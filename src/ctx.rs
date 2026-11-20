//! What every handler needs: the vault and the negotiated encoding.

use lsp_types::{Location, Position, Range, Url};

use crate::text::{Encoding, Lines, Span};
use crate::vault::Vault;

pub struct Ctx<'a> {
    pub vault: &'a Vault,
    pub enc: Encoding,
}

impl Ctx<'_> {
    pub fn uri(&self, rel: &str) -> Url {
        Url::from_file_path(self.vault.path(rel)).expect("absolute path")
    }

    pub fn range(&self, rel: &str, span: Span) -> Range {
        Lines::new(self.vault.text(rel).unwrap_or("")).range(span, self.enc)
    }

    pub fn location(&self, rel: &str, span: Span) -> Location {
        Location::new(self.uri(rel), self.range(rel, span))
    }

    pub fn start_of(&self, rel: &str) -> Location {
        Location::new(self.uri(rel), Range::new(Position::new(0, 0), Position::new(0, 0)))
    }

    /// The line and byte offset of a position in `rel`.
    pub fn byte_of(&self, rel: &str, pos: Position) -> (u32, u32) {
        Lines::new(self.vault.text(rel).unwrap_or("")).byte_of(pos, self.enc)
    }
}
