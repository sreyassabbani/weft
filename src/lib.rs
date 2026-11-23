//! weft: a language server for Obsidian-style Markdown notes, and its own
//! shorter syntax for them (`parse::Syntax`).

pub mod changes;
pub mod complete;
pub mod ctx;
pub mod dates;
pub mod diagnose;
pub mod hover;
pub mod parse;
pub mod refs;
pub mod rename;
pub mod server;
pub mod symbols;
pub mod target;
pub mod text;
pub mod vault;
