//! Undoable drawing transforms and curve editing tools.

mod edit;
mod transform;

use crate::commands::CommandRegistry;

pub fn register(r: &mut CommandRegistry) {
    transform::register(r);
    edit::register(r);
}
