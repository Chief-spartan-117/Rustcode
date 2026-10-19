//! Links a language server finds in the text (URLs, paths in comments): underlined, and opened
//! with a modifier click.

use std::path::{Path, PathBuf};

use gpui::Context;
use rustcode_application::LinkEntry;
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;

/// The path a `file://` link names, with `%20` and the like decoded.
pub(crate) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            out.push(u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

impl EditorView {
    pub(crate) fn links_ready(&mut self, doc: DocumentId, revision: u64, links: Vec<LinkEntry>, cx: &mut Context<Self>) {
        self.features.links.insert(doc, (revision, links));
        cx.notify();
    }

    /// Links of document `doc` that are for its text as it is now.
    pub(crate) fn links_of(&self, doc: DocumentId) -> &[LinkEntry] {
        match self.features.links.get(&doc) {
            Some((revision, links)) if self.doc_ref(doc).0.editor.revision() == *revision => links.as_slice(),
            _ => &[],
        }
    }

    /// The link at a line and column of the open file.
    pub(crate) fn link_at(&self, line: usize, column: usize) -> Option<&LinkEntry> {
        self.links_of(self.document.id).iter().find(|l| {
            l.target.is_some() && l.line == l.end_line && l.line == line && (l.column..l.end_column).contains(&column)
        })
    }

    /// Open what a link points at: a web address in the browser, a file in the editor.
    pub(crate) fn open_link(&mut self, target: &str, cx: &mut Context<Self>) {
        if let Some(path) = file_uri_path(target) {
            self.open_path(&path, cx);
        } else if target.contains("://") {
            cx.open_url(target);
        } else {
            // A path next to the open file.
            let base = self.document.path.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
            self.open_path(&base.join(target), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_links_become_paths() {
        assert_eq!(file_uri_path("file:///w/my%20dir/a.rs"), Some(PathBuf::from("/w/my dir/a.rs")));
        assert_eq!(file_uri_path("http://x"), None);
    }
}
