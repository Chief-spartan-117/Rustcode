use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustcode_domain::{ColorTheme, Document, DocumentId, TextBuffer};

use crate::ports::{DirEntry, FileError, FileReader, FileWriter, ThemeLoadError, ThemeLoader};

/// Use case: open a file from disk as a [`Document`].
pub struct OpenDocument {
    reader: Arc<dyn FileReader>,
}

impl OpenDocument {
    pub fn new(reader: Arc<dyn FileReader>) -> Self {
        Self { reader }
    }

    /// Directory entries for path completion: directories first, then files, each by name.
    /// An unreadable directory lists as empty.
    pub fn list_dir(&self, path: &Path) -> Vec<DirEntry> {
        let mut entries = self.reader.list_dir(path).unwrap_or_default();
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        entries
    }

    pub fn execute(&self, id: DocumentId, path: &Path) -> Result<Document, FileError> {
        let text = self.reader.read_to_string(path)?;
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Document::new(id, title, TextBuffer::from_text(&text)).with_path(path.to_path_buf()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveDocumentError {
    #[error("document has no file path")]
    NoPath,
    #[error(transparent)]
    Write(#[from] FileError),
}

/// Use case: write a [`Document`] back to its file and mark it clean.
pub struct SaveDocument {
    writer: Arc<dyn FileWriter>,
}

impl SaveDocument {
    pub fn new(writer: Arc<dyn FileWriter>) -> Self {
        Self { writer }
    }

    pub fn execute(&self, document: &mut Document) -> Result<(), SaveDocumentError> {
        let path = document.path.clone().ok_or(SaveDocumentError::NoPath)?;
        self.writer.write(&path, &document.editor.text())?;
        document.mark_saved();
        Ok(())
    }

    /// Write the document to `path` and make that its file. The title follows the file name.
    /// On failure the document keeps its previous path.
    pub fn save_as(&self, document: &mut Document, path: PathBuf) -> Result<(), SaveDocumentError> {
        self.writer.write(&path, &document.editor.text())?;
        document.title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        document.path = Some(path);
        document.mark_saved();
        Ok(())
    }
}

/// Use case: load a color theme from a file.
pub struct LoadTheme {
    loader: Arc<dyn ThemeLoader>,
}

impl LoadTheme {
    pub fn new(loader: Arc<dyn ThemeLoader>) -> Self {
        Self { loader }
    }

    pub fn execute(&self, path: &Path) -> Result<ColorTheme, ThemeLoadError> {
        self.loader.load(path)
    }
}

/// Use case: build a large synthetic document for the scroll benchmark.
pub struct SampleDocument;

impl SampleDocument {
    pub fn execute(id: DocumentId, lines: usize) -> Document {
        let mut text = String::with_capacity(lines * 60);
        for n in 1..=lines {
            text.push_str(&format!(
                "fn generated_{n}(input: usize) -> usize {{ input.wrapping_mul({n}) }}\n"
            ));
        }
        Document::new(id, "sample.rs", TextBuffer::from_text(&text))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemoryFs(Mutex<Vec<(PathBuf, String)>>);

    impl FileWriter for MemoryFs {
        fn write(&self, path: &Path, contents: &str) -> Result<(), FileError> {
            self.0
                .lock()
                .unwrap()
                .push((path.to_path_buf(), contents.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn save_writes_text_and_clears_dirty() {
        let fs = Arc::new(MemoryFs::default());
        let mut doc = Document::new(DocumentId(1), "a.txt", TextBuffer::from_text("hi"))
            .with_path(PathBuf::from("/a.txt"));
        doc.editor.insert_text("x");
        assert!(doc.is_dirty());

        SaveDocument::new(fs.clone()).execute(&mut doc).unwrap();

        assert!(!doc.is_dirty());
        assert_eq!(fs.0.lock().unwrap()[0].1, "xhi");
    }

    #[test]
    fn save_as_sets_path_title_and_clears_dirty() {
        let fs = Arc::new(MemoryFs::default());
        let mut doc = Document::new(DocumentId(1), "Untitled-1", TextBuffer::from_text("hi"));
        doc.editor.insert_text("x");
        SaveDocument::new(fs.clone()).save_as(&mut doc, PathBuf::from("/dir/n.rs")).unwrap();
        assert_eq!(doc.title, "n.rs");
        assert_eq!(doc.path, Some(PathBuf::from("/dir/n.rs")));
        assert!(!doc.is_dirty());
        assert_eq!(fs.0.lock().unwrap()[0].0, PathBuf::from("/dir/n.rs"));
    }

    #[test]
    fn save_without_path_fails() {
        let mut doc = Document::new(DocumentId(1), "a", TextBuffer::default());
        let err = SaveDocument::new(Arc::new(MemoryFs::default()))
            .execute(&mut doc)
            .unwrap_err();
        assert!(matches!(err, SaveDocumentError::NoPath));
    }
}
