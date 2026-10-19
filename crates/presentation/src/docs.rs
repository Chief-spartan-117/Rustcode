//! Open documents, shared by every editor group.
//!
//! One document is *installed* on the view (`EditorView::document`), so editing code stays
//! single-document. It is the document of the active group's active tab. All other open
//! documents are parked here until a view needs them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustcode_application::IncrementalHighlighter;
use rustcode_domain::{Document, DocumentId, TextBuffer};

use gpui::Context;
use rustcode_domain::Editor;

use crate::editor_view::EditorView;

/// A parked document with its highlighter.
pub(crate) struct DocState {
    pub document: Document,
    pub highlighter: Option<IncrementalHighlighter>,
    /// Index of the theme the highlighter was built for.
    theme_index: usize,
}

pub(crate) struct DocSlot {
    pub id: DocumentId,
    /// `None` for the installed document, whose state is on the view.
    pub parked: Option<DocState>,
}

impl EditorView {
    pub(crate) fn make_highlighter(&self, title: &str) -> Option<IncrementalHighlighter> {
        // A notebook's text is JSON the cells write; coloring it would only cost time.
        if crate::notebook_view::is_notebook(title) {
            return None;
        }
        let language = self.syntax.detect_language(title)?;
        let session = self.syntax.start_session(&language)?;
        let mut highlighter = IncrementalHighlighter::new(session, self.themes[self.theme_index].clone());
        // An extension that lists the language's brackets decides which characters count.
        let listed = self.language_configs.get(language.as_str()).map(|c| &c.brackets).filter(|b| !b.is_empty());
        if let Some(pairs) = listed {
            let pairs = pairs.clone();
            highlighter.set_bracket_pairs(Some(pairs));
        }
        Some(highlighter)
    }

    pub(crate) fn allocate_document_id(&mut self) -> DocumentId {
        let id = DocumentId(self.next_document_id);
        self.next_document_id += 1;
        id
    }

    fn slot_index(&self, id: DocumentId) -> Option<usize> {
        self.docs.iter().position(|slot| slot.id == id)
    }

    /// Document and highlighter for `id`, wherever its state currently lives.
    pub(crate) fn doc_ref(&self, id: DocumentId) -> (&Document, Option<&IncrementalHighlighter>) {
        if id == self.document.id {
            return (&self.document, self.highlighter.as_ref());
        }
        let slot = &self.docs[self.slot_index(id).expect("open document")];
        let parked = slot.parked.as_ref().expect("document not installed is parked");
        (&parked.document, parked.highlighter.as_ref())
    }

    /// (title, dirty) of an open document.
    pub(crate) fn doc_label(&self, id: DocumentId) -> (String, bool) {
        let (document, _) = self.doc_ref(id);
        (document.title.clone(), document.is_dirty() || self.webview_dirty(id))
    }

    pub(crate) fn doc_path(&self, id: DocumentId) -> Option<&Path> {
        self.doc_ref(id).0.path.as_deref()
    }

    /// Register a new document. It is parked until a view shows it.
    pub(crate) fn add_document(&mut self, document: Document) -> DocumentId {
        let id = document.id;
        let highlighter = self.make_highlighter(&document.title);
        self.docs.push(DocSlot {
            id,
            parked: Some(DocState { document, highlighter, theme_index: self.theme_index }),
        });
        self.ensure_notebook(id);
        id
    }

    /// Install `id` as the active document, parking the current one.
    pub(crate) fn install_doc(&mut self, id: DocumentId) {
        if id == self.document.id {
            return;
        }
        self.highlight_task = None;
        self.highlight_busy = false;
        let outgoing = DocState {
            document: std::mem::replace(
                &mut self.document,
                Document::new(DocumentId(0), "", TextBuffer::default()),
            ),
            highlighter: self.highlighter.take(),
            theme_index: self.theme_index,
        };
        let outgoing_index = self.slot_index(outgoing.document.id);
        let incoming_index = self.slot_index(id).expect("open document");
        let incoming = self.docs[incoming_index].parked.take().expect("document is parked");
        if let Some(index) = outgoing_index {
            self.docs[index].parked = Some(outgoing);
        }
        self.document = incoming.document;
        self.highlighter = incoming.highlighter;
        // A theme change while parked leaves stale token colors.
        if incoming.theme_index != self.theme_index {
            if let Some(highlighter) = self.highlighter.as_mut() {
                highlighter.set_theme(self.themes[self.theme_index].clone());
            }
        }
        // Each file may have its own indentation (`.editorconfig`, language settings).
        self.sync_indentation(false);
    }

    /// Forget documents no tab shows any more. Never the installed one.
    pub(crate) fn drop_unreferenced_docs(&mut self) {
        let referenced: Vec<DocumentId> = self
            .groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(|t| t.doc))
            .collect();
        let installed = self.document.id;
        self.docs.retain(|slot| slot.id == installed || referenced.contains(&slot.id));
        self.drop_unreferenced_notebooks();
        self.drop_unreferenced_webviews();
        self.drop_unreferenced_app_pages();
    }

    /// Number of tabs (in any group) showing document `id`.
    pub(crate) fn doc_refs(&self, id: DocumentId) -> usize {
        self.groups.iter().flat_map(|g| g.tabs.iter()).filter(|t| t.doc == id).count()
    }

    /// Documents with more lines than this are tokenized as far as they are looked at, not all at
    /// once: working through a million lines costs seconds and gigabytes.
    const LAZY_TOKENIZING_LINES: usize = 20_000;
    /// A file this long is colored down to this line and not further: tokens come in order, so
    /// reaching the end of a huge file would take longer than anyone waits for.
    const LAST_TOKENIZED_LINE: usize = 100_000;
    /// Lines below the last one in view that are tokenized ahead of the scrolling.
    const TOKENIZE_AHEAD: usize = 400;

    /// The documents in view (the active tab of every group), with the line up to which each is
    /// looked at: the last line shown plus some, or `usize::MAX` when all of it is wanted.
    fn visible_docs(&self) -> Vec<(DocumentId, usize)> {
        let mut visible: Vec<(DocumentId, usize)> = Vec::new();
        for group in &self.groups {
            let Some(tab) = group.tabs.get(group.active) else { continue };
            let lines = self.doc_ref(tab.doc).0.editor.buffer().line_count();
            let limit = if lines > Self::LAZY_TOKENIZING_LINES {
                let top = -f32::from(group.scroll.0.borrow().base_handle.offset().y);
                let height = f32::from(group.bounds.get().size.height).max(1200.0);
                (((top + height) / crate::text_layout::line_height()).ceil() as usize).saturating_add(Self::TOKENIZE_AHEAD).min(Self::LAST_TOKENIZED_LINE)
            } else {
                usize::MAX
            };
            match visible.iter_mut().find(|(doc, _)| *doc == tab.doc) {
                // Shown twice: as far as the lower of the two goes.
                Some((_, known)) => *known = (*known).max(limit),
                None => visible.push((tab.doc, limit)),
            }
        }
        visible.sort_by_key(|(id, _)| id.0);
        visible
    }

    /// Tokenize the documents in view. Returns true when all of them are tokenized as far as
    /// they are looked at.
    pub(crate) fn advance_visible(&mut self, budget: Duration) -> bool {
        let mut done = true;
        for (id, limit) in self.visible_docs() {
            if id == self.document.id {
                if let Some(h) = self.highlighter.as_mut() {
                    done &= h.advance_to(self.document.editor.buffer(), budget, limit);
                }
            } else if let Some(index) = self.slot_index(id) {
                if let Some(parked) = self.docs[index].parked.as_mut() {
                    if let Some(h) = parked.highlighter.as_mut() {
                        done &= h.advance_to(parked.document.editor.buffer(), budget, limit);
                    }
                }
            }
        }
        done
    }

    /// Whether what is in view still has to be tokenized (scrolling brings more into view).
    pub(crate) fn highlighting_pending(&self) -> bool {
        self.visible_docs().into_iter().any(|(id, limit)| {
            let (document, highlighter) = self.doc_ref(id);
            highlighter.is_some_and(|h| !h.is_idle_to(document.editor.buffer(), limit))
        })
    }

    /// Show a file in the active group, reusing the document when it is already open.
    pub(crate) fn open_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        // A custom editor an extension offers for the file wins, unless the file is already open as text.
        if !self.skip_custom_editors {
            if let Some(info) = self.default_custom_editor(path) {
                let open = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path));
                if open.is_none_or(|doc| self.is_webview_doc(doc)) {
                    self.open_custom_editor(path, &info, cx);
                    return;
                }
            }
        }
        if crate::media_view::media_kind(path).is_some() {
            self.open_media(path, cx);
            return;
        }
        let open = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path));
        if let Some(id) = open {
            self.open_doc_in_active_group(id, cx);
            return;
        }
        let id = self.allocate_document_id();
        match self.open_document.execute(id, path) {
            Ok(document) => {
                self.log(format!("opened {}", path.display()));
                let id = self.add_document(document);
                self.open_doc_in_active_group(id, cx);
                if let Some(why) = self.notebook_problem.take() {
                    self.set_status(why);
                }
                self.refresh_quick_diffs();
            }
            Err(err) => {
                let message = err.to_string();
                // A font, an archive, a program: not text.
                let message = if message.contains("valid UTF-8") {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    format!("{name} is a binary file, not text, so it is not opened in the editor")
                } else {
                    message
                };
                self.set_status(message);
                cx.notify();
            }
        }
    }

    pub(crate) fn save_as_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let settings = self.tidy_settings(self.document.id);
        Self::tidy_before_save(&settings, &mut self.document);
        match self.save_document.save_as(&mut self.document, path) {
            Ok(()) => {
                // The new name may select another grammar.
                self.highlight_task = None;
                self.highlight_busy = false;
                self.highlighter = self.make_highlighter(&self.document.title);
                self.update_highlights(cx);
                self.set_status(format!("saved {}", self.document.title));
                self.after_save(cx);
            }
            Err(err) => self.set_status(err.to_string()),
        }
        cx.notify();
    }

    /// Show document `id` as it is on disk now, and call it saved.
    pub(crate) fn reload_document_from_disk(&mut self, id: DocumentId, cx: &mut Context<Self>) {
        let Some(path) = self.doc_path(id).map(Path::to_path_buf) else { return };
        let Ok(fresh) = self.open_document.execute(id, &path) else { return };
        let text = fresh.editor.buffer().text();
        if self.doc_ref(id).0.editor.buffer().text() != text {
            self.edit_document_by_id(id, cx, |e| {
                e.select_all();
                e.insert_text(&text);
            });
        }
        self.refresh_notebook(id);
        if id == self.document.id {
            self.document.mark_saved();
        } else if let Some(index) = self.slot_index(id) {
            if let Some(parked) = self.docs[index].parked.as_mut() {
                parked.document.mark_saved();
            }
        }
        cx.notify();
    }

    /// Change document `id` through its editor, whether or not it is the installed one, and keep
    /// highlighting and the cursors of every view of it in step. The change is one undo step when
    /// `f` makes it so.
    pub(crate) fn edit_document_by_id(&mut self, id: DocumentId, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        if id == self.document.id {
            self.run_edit(cx, f);
            return;
        }
        let Some(index) = self.slot_index(id) else { return };
        let Some(parked) = self.docs[index].parked.as_mut() else { return };
        f(&mut parked.document.editor);
        let changes = parked.document.editor.take_changes();
        let char_edits = parked.document.editor.take_char_edits();
        if let Some(highlighter) = parked.highlighter.as_mut() {
            highlighter.apply_changes(&changes);
        }
        self.shift_folds(id, &changes);
        self.shift_semantic(id, &changes);
        self.shift_breakpoints(id, &changes);
        self.map_views_of(id, &char_edits, false);
        cx.notify();
    }
}
