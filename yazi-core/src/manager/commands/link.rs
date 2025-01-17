use yazi_config::keymap::Exec;

use crate::{manager::Manager, tasks::Tasks};

pub struct Opt {
	relative: bool,
	force:    bool,
}

impl From<&Exec> for Opt {
	fn from(e: &Exec) -> Self {
		Self { relative: e.named.contains_key("relative"), force: e.named.contains_key("force") }
	}
}

impl Manager {
	pub fn link(&mut self, opt: impl Into<Opt>, tasks: &Tasks) -> bool {
		let opt = opt.into() as Opt;
		let (cut, ref src) = self.yanked;
		!cut && tasks.file_link(src, self.cwd(), opt.relative, opt.force)
	}
}
