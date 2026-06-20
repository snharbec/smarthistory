// The PALETTE thread-local. Defined in its own module so that
// `thread_local!`'s scoping rules (which scope the resulting static
// to the enclosing module) don't prevent it from being read from
// the sibling `styles` module.
use std::cell::RefCell;

use super::Palette;

thread_local! {
    pub static PALETTE: RefCell<Palette> = RefCell::new(Palette::builtin());
}
