use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, mem, path::PathBuf};

use tokio::fs;

use super::{PreviewData, Tab, Tabs, Watcher};
use crate::{core::{external, files::{File, FilesOp}, input::{InputOpt, InputPos}, manager::Folder, tasks::Tasks}, emit};

pub struct Manager {
	tabs:   Tabs,
	yanked: (bool, HashSet<PathBuf>),

	watcher:      Watcher,
	pub mimetype: HashMap<PathBuf, String>,
}

impl Manager {
	pub fn new() -> Self {
		Self {
			tabs:   Tabs::new(),
			yanked: Default::default(),

			watcher:  Watcher::start(),
			mimetype: Default::default(),
		}
	}

	pub fn refresh(&mut self) {
		self.watcher.trigger(&self.current().cwd);
		if let Some(p) = self.parent() {
			self.watcher.trigger(&p.cwd);
		}
		emit!(Hover);

		let mut to_watch = BTreeSet::new();
		for tab in self.tabs.iter() {
			to_watch.insert(tab.current.cwd.clone());
			if let Some(ref p) = tab.parent {
				to_watch.insert(p.cwd.clone());
			}
			if let Some(ref h) = tab.current.hovered {
				if h.meta.is_dir() {
					to_watch.insert(h.path());
				}
			}
		}
		self.watcher.watch(to_watch);
	}

	pub fn preview(&mut self) -> bool {
		let hovered = if let Some(h) = self.hovered() {
			h.clone()
		} else {
			return self.active_mut().preview.reset();
		};

		if hovered.meta.is_dir() {
			self.active_mut().preview.go(&hovered.path, "inode/directory");
			if self.active().history(&hovered.path).is_some() {
				emit!(Preview(hovered.path, PreviewData::Folder));
			}
		} else if let Some(mime) = self.mimetype.get(&hovered.path).cloned() {
			self.active_mut().preview.go(&hovered.path, &mime);
		} else {
			tokio::spawn(async move {
				if let Ok(mimes) = external::file(&[hovered.path()]).await {
					emit!(Mimetype(mimes));
				}
			});
		}
		false
	}

	pub fn yank(&mut self, cut: bool) -> bool {
		self.yanked.0 = cut;
		self.yanked.1.clear();
		self.yanked.1.extend(self.selected());
		false
	}

	#[inline]
	pub fn yanked(&self) -> &(bool, HashSet<PathBuf>) { &self.yanked }

	pub fn quit(&self, tasks: &Tasks) -> bool {
		let tasks = tasks.len();
		if tasks == 0 {
			emit!(Quit);
			return false;
		}

		tokio::spawn(async move {
			let result = emit!(Input(InputOpt {
				title:    format!("There are {} tasks running, sure to quit? (y/N)", tasks),
				value:    "".to_string(),
				position: InputPos::Top,
			}))
			.await;

			if let Ok(choice) = result {
				if choice.to_lowercase() == "y" {
					emit!(Quit);
				}
			}
		});
		false
	}

	pub fn close(&mut self, tasks: &Tasks) -> bool {
		if self.tabs.len() > 1 {
			return self.tabs.close(self.tabs.idx());
		}
		self.quit(tasks)
	}

	pub fn create(&self) -> bool {
		let cwd = self.current().cwd.clone();
		tokio::spawn(async move {
			let result = emit!(Input(InputOpt {
				title:    "Create:".to_string(),
				value:    "".to_string(),
				position: InputPos::Top,
			}))
			.await;

			if let Ok(name) = result {
				let path = cwd.join(&name);
				if name.ends_with('/') {
					fs::create_dir_all(path).await.ok();
				} else {
					fs::create_dir_all(path.parent().unwrap()).await.ok();
					fs::File::create(path).await.ok();
				}
			}
		});
		false
	}

	pub fn rename(&self) -> bool {
		if self.current().has_selected() {
			return self.bulk_rename();
		}

		let hovered = if let Some(h) = self.hovered() {
			h.path.clone()
		} else {
			return false;
		};

		tokio::spawn(async move {
			let result = emit!(Input(InputOpt {
				title:    "Rename:".to_string(),
				value:    hovered.file_name().unwrap().to_string_lossy().to_string(),
				position: InputPos::Hovered,
			}))
			.await;

			if let Ok(new) = result {
				let to = hovered.parent().unwrap().join(new);
				fs::rename(&hovered, to).await.ok();
			}
		});
		false
	}

	fn bulk_rename(&self) -> bool { false }

	pub fn selected(&self) -> Vec<PathBuf> {
		self.current().selected().or_else(|| self.hovered().map(|h| vec![h.path()])).unwrap_or_default()
	}

	pub async fn mimetypes(&mut self, files: &[PathBuf]) -> Vec<Option<String>> {
		let todo =
			files.iter().filter(|&p| !self.mimetype.contains_key(p)).cloned().collect::<Vec<_>>();

		if let Ok(mimes) = external::file(&todo).await {
			self.mimetype.extend(mimes);
		}

		files.into_iter().map(|p| self.mimetype.get(p).cloned()).collect()
	}

	pub fn update_read(&mut self, op: FilesOp) -> bool {
		let path = op.path();
		let cwd = self.current().cwd.clone();
		let hovered = self.hovered().map(|h| h.path());

		let mut b = if cwd == path && !self.current().in_search {
			self.current_mut().update(op)
		} else if matches!(self.parent(), Some(p) if p.cwd == path) {
			self.active_mut().parent.as_mut().unwrap().update(op)
		} else {
			self
				.active_mut()
				.history
				.entry(path.to_path_buf())
				.or_insert_with(|| Folder::new(&path))
				.update(op);

			matches!(self.hovered(), Some(h) if h.path == path)
		};

		b |= self.active_mut().parent.as_mut().map_or(false, |p| p.hover(&cwd));
		b |= hovered.as_ref().map_or(false, |h| self.current_mut().hover(h));

		if hovered != self.hovered().map(|h| h.path()) {
			emit!(Hover);
		}
		b
	}

	pub fn update_ioerr(&mut self, op: FilesOp) -> bool {
		let path = op.path();
		let op = FilesOp::read_empty(&path);

		if path == self.current().cwd {
			self.current_mut().update(op);
		} else if matches!(self.parent(), Some(p) if p.cwd == path) {
			self.active_mut().parent.as_mut().unwrap().update(op);
		} else {
			return false;
		}

		self.active_mut().leave();
		true
	}

	pub fn update_search(&mut self, op: FilesOp) -> bool {
		let path = op.path();
		if self.current().in_search && self.current().cwd == path {
			return self.current_mut().update(op);
		}

		let rep = mem::replace(self.current_mut(), Folder::new_search(&path));
		if !rep.in_search {
			self.active_mut().history.insert(path, rep);
		}
		self.current_mut().update(op);
		true
	}

	pub fn update_mimetype(&mut self, mut mimes: BTreeMap<PathBuf, String>, tasks: &Tasks) -> bool {
		mimes.retain(|f, m| self.mimetype.get(f) != Some(m));
		if mimes.is_empty() {
			return false;
		}

		tasks.precache_image(&mimes);
		tasks.precache_video(&mimes);

		self.mimetype.extend(mimes);
		self.preview();
		true
	}

	pub fn update_preview(&mut self, path: PathBuf, data: PreviewData) -> bool {
		let hovered = if let Some(ref h) = self.current().hovered {
			h.path()
		} else {
			return self.active_mut().preview.reset();
		};

		if hovered != path {
			return false;
		}

		let preview = &mut self.active_mut().preview;
		preview.path = path;
		preview.data = data;
		true
	}
}

impl Manager {
	#[inline]
	pub fn tabs(&self) -> &Tabs { &self.tabs }

	#[inline]
	pub fn tabs_mut(&mut self) -> &mut Tabs { &mut self.tabs }

	#[inline]
	pub fn active(&self) -> &Tab { self.tabs.active() }

	#[inline]
	pub fn active_mut(&mut self) -> &mut Tab { self.tabs.active_mut() }

	#[inline]
	pub fn current(&self) -> &Folder { &self.tabs.active().current }

	#[inline]
	pub fn current_mut(&mut self) -> &mut Folder { &mut self.tabs.active_mut().current }

	#[inline]
	pub fn parent(&self) -> Option<&Folder> { self.tabs.active().parent.as_ref() }

	#[inline]
	pub fn hovered(&self) -> Option<&File> { self.tabs.active().current.hovered.as_ref() }
}
