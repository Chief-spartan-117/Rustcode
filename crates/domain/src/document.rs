use std::path::PathBuf;

use crate::{Editor, TextBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentId(pub u64);

/// A named text document, optionally backed by a file.
#[derive(Debug)]
pub struct Document {
    pub id: DocumentId,
    pub title: String,
    pub path: Option<PathBuf>,
    pub editor: Editor,
    saved_revision: u64,
}

impl Document {
    pub fn new(id: DocumentId, title: impl Into<String>, buffer: TextBuffer) -> Self {
        let editor = Editor::new(buffer);
        Self {
            id,
            title: title.into(),
            path: None,
            saved_revision: editor.revision(),
            editor,
        }
    }

    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.revision() != self.saved_revision
    }

    /// The revision that was last written to disk. It moves each time the file is saved.
    pub fn saved_revision(&self) -> u64 {
        self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.editor.revision();
    }
}
