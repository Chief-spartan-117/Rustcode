//! Pages of extensions in tabs: webview panels (`createWebviewPanel`) and custom editors
//! (`registerCustomEditorProvider`).
//!
//! A page is a tab like a picture is: a document with nothing in it, shown by something else. The
//! tab measures the room it leaves; the system webview (`webview_native.rs`) is put over it.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{canvas, div, prelude::*, px, AnyElement, Bounds, Context, Keystroke, Pixels, SharedString, Task, Window};
use rustcode_application::{CustomEditorInfo, ServiceCommand, WebviewOptions, WebviewSpec};
use rustcode_domain::{Document, DocumentId, TextBuffer};
use serde_json::Value;

use crate::editor_view::EditorView;
use crate::webview_native::{NativePage, PageConfig};
use crate::webview_page::{self, PageMessage};

/// Pages the window starts itself (custom editors) are numbered from here; the host's are below.
const APP_WEBVIEWS: u64 = 1 << 40;
/// How often what pages sent, and what extensions said to them, is picked up.
const PUMP: std::time::Duration = std::time::Duration::from_millis(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebviewKind {
    /// Made by `createWebviewPanel`, of this view type.
    Panel { view_type: String },
    /// A file shown by a custom editor. `text` is true when the document holds the file's text.
    CustomEditor { view_type: String, text: bool },
    /// A view of the side bar.
    View,
    /// A page the window opened itself (an output of a notebook, a tool's own page): no
    /// extension is behind it.
    Local,
}

pub(crate) struct WebviewTab {
    /// The number the host knows the page by.
    pub id: u64,
    pub kind: WebviewKind,
    pub options: WebviewOptions,
    pub html: String,
    pub page: Option<NativePage>,
    /// Why the page cannot be shown, when it cannot.
    pub failed: Option<String>,
    /// The room the tab leaves for the page, written while drawing.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A custom editor with changes its extension has not saved.
    pub dirty: bool,
    /// Its extension keeps a history that undo and redo can walk.
    pub undoable: bool,
    pub icon: Option<PathBuf>,
    pub state: Option<Value>,
    /// The page has the keyboard.
    pub focused: bool,
    /// Give the page the keyboard when it shows.
    pub wants_focus: bool,
    /// What the host was last told: shown, and has the keyboard.
    reported: Option<(bool, bool)>,
    /// Messages for the page before there was one.
    early: Vec<Value>,
    /// A picture of the page, drawn in its place while the window has something over it, and
    /// whether one was asked for.
    pub shot: Option<std::sync::Arc<gpui::RenderImage>>,
    shot_asked: bool,
    /// An address the page loads instead of HTML.
    pub url: Option<String>,
    /// A side bar view's own title, description and badge.
    pub title: Option<String>,
    pub description: Option<String>,
    pub badge: Option<u64>,
}

impl WebviewTab {
    fn new(id: u64, kind: WebviewKind, options: WebviewOptions, icon: Option<PathBuf>) -> Self {
        Self {
            id,
            kind,
            options,
            html: String::new(),
            page: None,
            failed: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            dirty: false,
            undoable: false,
            icon,
            state: None,
            focused: false,
            wants_focus: false,
            reported: None,
            early: Vec::new(),
            shot: None,
            shot_asked: false,
            url: None,
            title: None,
            description: None,
            badge: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct WebviewState {
    pub tabs: HashMap<DocumentId, WebviewTab>,
    /// Pages in side bar views, by view id.
    pub views: HashMap<String, WebviewTab>,
    next_app: u64,
    /// Shortcuts pages passed on, to be run in the window.
    pending_keys: Vec<Keystroke>,
    /// The theme (by index) the pages were last given.
    themed: Option<usize>,
    task: Option<Task<()>>,
    /// Ids of the "Open With" commands, by view type.
    pub open_with: HashMap<String, &'static str>,
    /// View types whose panels extensions can fill again after a restart.
    pub serializers: std::collections::HashSet<String>,
    /// Icons of tabs, drawn once (an SVG is turned into a picture).
    icons: std::cell::RefCell<HashMap<PathBuf, Option<std::sync::Arc<gpui::RenderImage>>>>,
}

impl WebviewState {
    pub(crate) fn doc_of(&self, id: u64) -> Option<DocumentId> {
        self.tabs.iter().find(|(_, tab)| tab.id == id).map(|(doc, _)| *doc)
    }

    /// The page the host calls `id`, in a tab or in a side bar view.
    pub(crate) fn by_id_mut(&mut self, id: u64) -> Option<&mut WebviewTab> {
        let in_tabs = self.tabs.values().any(|tab| tab.id == id);
        if in_tabs {
            self.tabs.values_mut().find(|tab| tab.id == id)
        } else {
            self.views.values_mut().find(|tab| tab.id == id)
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.views.is_empty()
    }
}

/// The folders a page may read from when its extension named none: the workspace and the
/// extensions' own.
fn default_roots(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = folders.to_vec();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".rustcode").join("extensions"));
        roots.push(home.join(".vscode").join("extensions"));
    }
    roots
}

impl EditorView {
    pub(crate) fn is_webview_doc(&self, doc: DocumentId) -> bool {
        self.webviews.tabs.contains_key(&doc)
    }

    /// Whether a page's tab is all there is of the document: extensions are not told of it as a file.
    pub(crate) fn hidden_from_extensions(&self, doc: DocumentId) -> bool {
        self.is_app_page(doc) || self.webviews.tabs.get(&doc).is_some_and(|tab| !matches!(tab.kind, WebviewKind::CustomEditor { text: true, .. }))
    }

    /// The icon an extension gave the page in tab `doc`, as a small picture; `None` when it gave none
    /// that can be drawn (the tab then has the usual file icon).
    pub(crate) fn webview_tab_icon(&self, doc: DocumentId) -> Option<AnyElement> {
        let path = self.webviews.tabs.get(&doc)?.icon.clone()?;
        let image = self
            .webviews
            .icons
            .borrow_mut()
            .entry(path.clone())
            .or_insert_with(|| {
                let bytes = std::fs::read(&path).ok()?;
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
                    crate::media_view::rasterize_svg(&bytes, 2.0)
                } else {
                    decode_picture(&bytes)
                }
            })
            .clone()?;
        Some(gpui::img(image).w(px(14.)).h(px(14.)).flex_none().into_any_element())
    }

    /// The tab's text is unsaved changes of a custom editor that keeps its own data.
    pub(crate) fn webview_dirty(&self, doc: DocumentId) -> bool {
        self.webviews.tabs.get(&doc).is_some_and(|tab| tab.dirty)
    }

    fn webview_roots(&self, options: &WebviewOptions) -> Vec<PathBuf> {
        options.local_roots.clone().unwrap_or_else(|| default_roots(&self.folders()))
    }

    fn set_doc_title(&mut self, doc: DocumentId, title: &str) {
        if self.document.id == doc {
            self.document.title = title.to_owned();
        } else if let Some(parked) = self.docs.iter_mut().find(|slot| slot.id == doc).and_then(|slot| slot.parked.as_mut()) {
            parked.document.title = title.to_owned();
        }
    }

    // ---- opening ---------------------------------------------------------------------------

    /// A page an extension asked for: a tab in the group it named.
    pub(crate) fn open_webview_panel(&mut self, spec: WebviewSpec, cx: &mut Context<Self>) {
        let id = self.allocate_document_id();
        let doc = self.add_document(Document::new(id, spec.title.clone(), TextBuffer::default()));
        self.read_only_docs.insert(doc);
        let mut tab = WebviewTab::new(spec.id, WebviewKind::Panel { view_type: spec.view_type.clone() }, spec.options.clone(), spec.icon.clone());
        tab.wants_focus = !spec.preserve_focus;
        self.webviews.tabs.insert(doc, tab);
        self.start_webview_pump(cx);
        self.place_webview_tab(doc, spec.column, spec.preserve_focus, cx);
    }

    fn place_webview_tab(&mut self, doc: DocumentId, column: i32, preserve_focus: bool, cx: &mut Context<Self>) {
        let previous = (self.active_group, self.groups[self.active_group].active);
        let named = (column >= 1)
            .then(|| self.layout.leaves().get(column as usize - 1).copied())
            .flatten()
            .and_then(|id| self.group_index(id));
        match (column, named) {
            (-2, _) | (1.., None) => self.open_doc_beside_active(doc, cx),
            (_, Some(group)) => {
                self.focus_view(group, self.groups[group].active, cx);
                self.open_doc_in_active_group(doc, cx);
            }
            _ => self.open_doc_in_active_group(doc, cx),
        }
        if preserve_focus {
            self.focus_view(previous.0, previous.1, cx);
        }
    }

    /// Show `html` in a tab of its own (the markup of a notebook output). Scripts run; the page
    /// reads no files.
    pub(crate) fn open_html_page(&mut self, title: &str, html: String, cx: &mut Context<Self>) {
        self.open_local_page(title, Some(html), None, cx);
    }

    /// Show the page at `url` in a tab of its own (a tool that serves a page, like DevTools).
    pub(crate) fn open_url_page(&mut self, title: &str, url: String, cx: &mut Context<Self>) {
        self.open_local_page(title, None, Some(url), cx);
    }

    fn open_local_page(&mut self, title: &str, html: Option<String>, url: Option<String>, cx: &mut Context<Self>) {
        let id = self.allocate_document_id();
        let doc = self.add_document(Document::new(id, title.to_owned(), TextBuffer::default()));
        self.read_only_docs.insert(doc);
        self.webviews.next_app += 1;
        let options = WebviewOptions { enable_scripts: true, enable_command_uris: false, local_roots: Some(Vec::new()) };
        let mut tab = WebviewTab::new(APP_WEBVIEWS + self.webviews.next_app, WebviewKind::Local, options, None);
        tab.html = html.unwrap_or_default();
        tab.url = url;
        tab.wants_focus = true;
        self.webviews.tabs.insert(doc, tab);
        self.start_webview_pump(cx);
        self.open_doc_in_active_group(doc, cx);
    }

    /// The custom editor offered for `path` that opens it without being asked.
    pub(crate) fn default_custom_editor(&self, path: &Path) -> Option<CustomEditorInfo> {
        let name = path.file_name()?.to_string_lossy();
        self.ext_ui.contributed.custom_editors.iter().find(|e| e.default_priority && e.matches(&name)).cloned()
    }

    /// Open `path` with the custom editor `info`.
    pub(crate) fn open_custom_editor(&mut self, path: &Path, info: &CustomEditorInfo, cx: &mut Context<Self>) {
        if !path.is_file() {
            self.set_status(format!("cannot find {}", path.display()));
            cx.notify();
            return;
        }
        let existing = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path));
        if let Some(doc) = existing.filter(|doc| self.webviews.tabs.contains_key(doc)) {
            self.open_doc_in_active_group(doc, cx);
            return;
        }
        // The text of the file when it is text; the tab shows the page either way.
        let (doc, text) = match existing {
            Some(doc) => (doc, crate::media_view::media_kind(path).is_none()),
            None => {
                let id = self.allocate_document_id();
                match crate::media_view::media_kind(path).is_none().then(|| self.open_document.execute(id, path)).and_then(Result::ok) {
                    Some(document) => (self.add_document(document), true),
                    None => {
                        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let document = Document::new(id, title, TextBuffer::default()).with_path(path.to_path_buf());
                        (self.add_document(document), false)
                    }
                }
            }
        };
        self.webviews.next_app += 1;
        let id = APP_WEBVIEWS + self.webviews.next_app;
        let kind = WebviewKind::CustomEditor { view_type: info.view_type.clone(), text };
        let mut tab = WebviewTab::new(id, kind, WebviewOptions::default(), None);
        tab.wants_focus = true;
        self.webviews.tabs.insert(doc, tab);
        self.log(format!("opened {} with {}", path.display(), info.display_name));
        self.start_webview_pump(cx);
        self.open_doc_in_active_group(doc, cx);
        let folders = self.folders();
        self.ext_host.custom_editor_open(id, &info.view_type, path, &folders);
    }

    /// Files that were open before the extensions were read (the one given at the start, the ones of
    /// the last session) are shown with their custom editors too.
    pub(crate) fn use_custom_editors_for_open_files(&mut self, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = self.docs.iter().filter_map(|slot| self.doc_path(slot.id).map(Path::to_path_buf)).collect();
        for path in files {
            let shown = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path.as_path())).is_some_and(|doc| self.is_webview_doc(doc));
            if shown {
                continue;
            }
            if let Some(info) = self.default_custom_editor(&path) {
                self.open_custom_editor(&path, &info, cx);
            }
        }
    }

    /// Open the file of the active tab with another editor: a custom one by its view type, or the
    /// text editor (`text`).
    pub(crate) fn reopen_with(&mut self, view_type: &str, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(path) = self.doc_path(doc).map(Path::to_path_buf) else {
            self.set_status("this tab is not a file");
            cx.notify();
            return;
        };
        let custom = self.ext_ui.contributed.custom_editors.iter().find(|e| e.view_type == view_type).cloned();
        if custom.is_none() && view_type != "text" {
            return;
        }
        if self.doc_label(doc).1 {
            self.set_status("save the file before opening it with another editor");
            cx.notify();
            return;
        }
        if let Some(tab) = self.webviews.tabs.remove(&doc) {
            self.ext_host.webview_closed(tab.id);
        }
        self.close_tabs_of(doc, cx);
        match custom {
            Some(info) => self.open_custom_editor(&path, &info, cx),
            None => self.open_path_as_text(&path, cx),
        }
    }

    /// Open a file in the text editor, whatever a custom editor would do with it.
    fn open_path_as_text(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.skip_custom_editors = true;
        self.open_path(path, cx);
        self.skip_custom_editors = false;
    }

    /// Add the "Open With" commands of the custom editors to the palette.
    pub(crate) fn register_open_with_commands(&mut self) {
        let editors: Vec<(String, String)> = self.ext_ui.contributed.custom_editors.iter().map(|e| (e.view_type.clone(), e.display_name.clone())).collect();
        if editors.is_empty() {
            return;
        }
        let mut entries = editors;
        entries.push(("text".to_owned(), "Text Editor".to_owned()));
        for (view_type, name) in entries {
            if self.webviews.open_with.contains_key(&view_type) {
                continue;
            }
            // Ids live as long as the program; there are few of them.
            let id: &'static str = Box::leak(format!("rustcode.openWith.{view_type}").into_boxed_str());
            self.registry.register(rustcode_application::CommandId(id), &format!("Reopen Editor With: {name}"), None);
            self.webviews.open_with.insert(view_type, id);
        }
    }

    // ---- what extensions say ----------------------------------------------------------------

    fn webview_doc(&self, id: u64) -> Option<DocumentId> {
        self.webviews.doc_of(id)
    }

    pub(crate) fn webview_set_html(&mut self, id: u64, html: String) {
        if let Some(tab) = self.webviews.by_id_mut(id) {
            tab.html = html.clone();
            if let Some(page) = tab.page.as_mut() {
                page.set_html(html);
            }
        }
    }

    pub(crate) fn webview_set_title(&mut self, id: u64, title: &str) {
        if let Some(doc) = self.webview_doc(id) {
            self.set_doc_title(doc, title);
        }
    }

    pub(crate) fn webview_set_icon(&mut self, id: u64, icon: Option<PathBuf>) {
        if let Some(tab) = self.webviews.by_id_mut(id) {
            tab.icon = icon;
        }
    }

    pub(crate) fn webview_set_options(&mut self, id: u64, options: WebviewOptions) {
        let roots = self.webview_roots(&options);
        if let Some(tab) = self.webviews.by_id_mut(id) {
            if let Some(page) = tab.page.as_mut() {
                page.set_access(options.enable_scripts, roots);
            }
            tab.options = options;
        }
    }

    pub(crate) fn webview_post(&mut self, id: u64, message: Value) {
        if let Some(tab) = self.webviews.by_id_mut(id) {
            match tab.page.as_mut() {
                Some(page) => page.post(message),
                None => tab.early.push(message),
            }
        }
    }

    /// A side bar view's title, description and badge, as its extension set them.
    pub(crate) fn webview_view_info(&mut self, id: u64, title: Option<String>, description: Option<String>, badge: Option<u64>) {
        if let Some(tab) = self.webviews.views.values_mut().find(|t| t.id == id) {
            tab.title = title;
            tab.description = description;
            tab.badge = badge;
        }
    }

    pub(crate) fn webview_reveal(&mut self, id: u64, _column: Option<i32>, preserve_focus: bool, cx: &mut Context<Self>) {
        // A view of the side bar: show the container that holds it.
        if let Some(view) = self.webviews.views.iter().find(|(_, t)| t.id == id).map(|(v, _)| v.clone()) {
            self.run_service_command(&ServiceCommand { command: "rustcode.focusView".into(), title: None, arguments: vec![Value::String(view)] }, cx);
            return;
        }
        let Some(doc) = self.webview_doc(id) else { return };
        let Some((group, tab)) = self.groups.iter().enumerate().find_map(|(gi, g)| g.tabs.iter().position(|t| t.doc == doc).map(|ti| (gi, ti))) else { return };
        if let Some(tab) = self.webviews.tabs.get_mut(&doc) {
            tab.wants_focus = !preserve_focus;
        }
        if !preserve_focus {
            self.focus_view(group, tab, cx);
        } else {
            self.groups[group].active = tab;
        }
        cx.notify();
    }

    pub(crate) fn webview_dispose(&mut self, id: u64, cx: &mut Context<Self>) {
        // A view of the side bar: its page goes, and is made again the next time the view is shown.
        if let Some(view) = self.webviews.views.iter().find(|(_, t)| t.id == id).map(|(v, _)| v.clone()) {
            self.webviews.views.remove(&view);
            cx.notify();
            return;
        }
        let Some(doc) = self.webview_doc(id) else { return };
        // The extension closed it: no need to tell it so.
        self.forget_webview(doc);
        self.close_tabs_of(doc, cx);
    }

    pub(crate) fn webview_set_dirty(&mut self, id: u64, dirty: bool, undoable: bool) {
        if let Some(tab) = self.webviews.by_id_mut(id) {
            tab.dirty = dirty;
            tab.undoable |= undoable;
        }
    }

    /// Undo or redo, when the page in front is a custom editor with a history of its own. True
    /// when it was one (and the request went to its extension).
    pub(crate) fn undo_in_custom_editor(&mut self, redo: bool) -> bool {
        let Some(tab) = self.webviews.tabs.get(&self.document.id).filter(|t| t.undoable) else { return false };
        self.ext_host.custom_editor_history(tab.id, redo);
        true
    }

    /// Forget the page of a document without telling the host.
    fn forget_webview(&mut self, doc: DocumentId) {
        self.webviews.tabs.remove(&doc);
    }

    /// Close every tab that shows `doc`.
    fn close_tabs_of(&mut self, doc: DocumentId, cx: &mut Context<Self>) {
        self.save_active_view();
        let mut found: Vec<(crate::groups::GroupId, usize)> = Vec::new();
        for group in &self.groups {
            for (index, tab) in group.tabs.iter().enumerate() {
                if tab.doc == doc {
                    found.push((group.id, index));
                }
            }
        }
        // From the back, so earlier positions stay right.
        for (group, index) in found.into_iter().rev() {
            if self.group_index(group).is_some_and(|gi| index < self.groups[gi].tabs.len()) {
                self.take_tab(group, index);
            }
        }
        self.settle_after_removal(cx);
    }

    /// Pages whose tabs are gone are closed; the host hears of it.
    pub(crate) fn drop_unreferenced_webviews(&mut self) {
        let open: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).collect();
        let gone: Vec<DocumentId> = self.webviews.tabs.keys().filter(|doc| !open.contains(doc)).copied().collect();
        for doc in gone {
            if let Some(tab) = self.webviews.tabs.remove(&doc) {
                if tab.kind == WebviewKind::Local {
                    continue;
                }
                if tab.dirty {
                    self.ext_host.custom_editor_revert(tab.id);
                }
                self.ext_host.webview_closed(tab.id);
            }
        }
    }

    /// Save a custom editor that keeps its own data (not the text of a file). True when it was one.
    pub(crate) fn save_custom_editor(&mut self, doc: DocumentId) -> bool {
        let Some(tab) = self.webviews.tabs.get(&doc).filter(|t| matches!(t.kind, WebviewKind::CustomEditor { text: false, .. })) else { return false };
        self.ext_host.custom_editor_save(tab.id, None);
        self.set_status("saving...");
        true
    }

    // ---- showing ---------------------------------------------------------------------------------

    fn start_webview_pump(&mut self, cx: &mut Context<Self>) {
        if self.webviews.task.is_some() {
            return;
        }
        self.webviews.task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(PUMP).await;
            if this.update(cx, |this, cx| this.pump_webviews(cx)).is_err() {
                break;
            }
        }));
    }

    /// Take what pages sent and what extensions said to them.
    fn pump_webviews(&mut self, cx: &mut Context<Self>) {
        if self.webviews.is_empty() {
            return;
        }
        self.poll_extension_services(cx);
        self.take_page_pictures(cx);
        let mut focus_doc: Option<DocumentId> = None;
        let mut commands: Vec<ServiceCommand> = Vec::new();
        let mut keys: Vec<Keystroke> = Vec::new();
        let mut handle = |tab: &mut WebviewTab, host: &dyn rustcode_application::ExtensionHost, focused: &mut bool| {
            let Some(page) = tab.page.as_mut() else { return };
            for message in page.take_messages() {
                match message {
                    PageMessage::Post(value) if tab.kind != WebviewKind::Local => host.webview_message(tab.id, &value),
                    PageMessage::Post(_) => {}
                    PageMessage::State(value) => tab.state = Some(value),
                    PageMessage::Ready => page.loaded(),
                    PageMessage::Focus(has) => {
                        tab.focused = has;
                        *focused |= has;
                    }
                    PageMessage::Key(key) => {
                        let undo = (key.meta || key.ctrl) && !key.alt && matches!(key.key.to_lowercase().as_str(), "z" | "y");
                        if undo && tab.undoable {
                            host.custom_editor_history(tab.id, key.key.eq_ignore_ascii_case("y") || key.shift);
                        } else if !undo {
                            keys.extend(webview_page::keystroke_of(&key));
                        }
                    }
                    PageMessage::Command { command, arguments } if tab.options.enable_command_uris => {
                        commands.push(ServiceCommand { command, title: None, arguments });
                    }
                    PageMessage::Command { .. } => {}
                }
            }
        };
        let host = self.ext_host.clone();
        for (doc, tab) in self.webviews.tabs.iter_mut() {
            let mut focused = false;
            handle(tab, host.as_ref(), &mut focused);
            if focused {
                focus_doc = Some(*doc);
            }
        }
        for tab in self.webviews.views.values_mut() {
            handle(tab, host.as_ref(), &mut false);
        }
        self.webviews.pending_keys.extend(keys);
        for command in commands {
            self.run_service_command(&command, cx);
        }
        // A click in a page makes its tab the one the keyboard is meant for.
        if let Some(doc) = focus_doc {
            if let Some((group, tab)) = self.groups.iter().enumerate().find_map(|(gi, g)| g.tabs.iter().position(|t| t.doc == doc).map(|ti| (gi, ti))) {
                if self.document.id != doc || self.active_group != group {
                    self.groups[group].active = tab;
                    self.focus_view(group, tab, cx);
                }
            }
        }
        if !self.webviews.pending_keys.is_empty() {
            cx.notify();
        }
    }

    /// Run the shortcuts pages passed on. Needs the window, so it is deferred from drawing.
    pub(crate) fn run_page_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.webviews.pending_keys.is_empty() {
            return;
        }
        let keys = std::mem::take(&mut self.webviews.pending_keys);
        cx.defer_in(window, move |_, window, cx| {
            for key in keys {
                window.dispatch_keystroke(key, cx);
            }
        });
    }

    /// Whether something drawn by the window itself is open over the editor area: a native page
    /// would cover it.
    fn page_covered(&self) -> bool {
        !crate::webview_native::CAN_SHOW_STAND_IN && (self.palette.open || self.menu_open.is_some() || !self.toasts.is_empty() || self.shell.context_menu.is_some())
    }

    /// A part of something drawn over the page: it measures itself while the window is painted.
    pub(crate) fn overlay_probe(&self) -> AnyElement {
        let list = self.overlays.clone();
        canvas(|bounds, _, _| bounds, move |bounds, _, _, _| list.borrow_mut().push(crate::webview_mask::rect_of(bounds)))
            .absolute()
            .size_full()
            .into_any_element()
    }

    /// The last thing painted: once everything has said where it is, the pages that something
    /// lies over are replaced by their pictures, and the others are shown.
    pub(crate) fn overlay_reader(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        canvas(
            |bounds, _, _| bounds,
            move |_, _, window, cx| {
                let entity = entity.clone();
                window.defer(cx, move |_, cx| {
                    entity.update(cx, |this, cx| this.cover_pages(cx));
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// A page with something of the window over it is shown as a picture, which the window draws
    /// under the palette or menu and which gets the clicks; the live page is out of sight until the
    /// cover is gone.
    fn cover_pages(&mut self, cx: &mut Context<Self>) {
        if self.webviews.is_empty() || !crate::webview_native::CAN_SHOW_STAND_IN {
            return;
        }
        let overlays = self.overlays.borrow().clone();
        let mut changed = false;
        for tab in self.webviews.tabs.values_mut().chain(self.webviews.views.values_mut()) {
            let Some(page) = tab.page.as_mut() else { continue };
            let bounds = crate::webview_mask::rect_of(tab.bounds.get());
            let covered = !crate::webview_mask::holes_in(bounds, &overlays).is_empty();
            if covered && tab.shot.is_none() && !tab.shot_asked {
                page.request_snapshot();
                tab.shot_asked = true;
            } else if !covered && (tab.shot_asked || tab.shot.is_some()) {
                tab.shot = None;
                tab.shot_asked = false;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Take the pictures that were asked for, once the system has made them.
    fn take_page_pictures(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for tab in self.webviews.tabs.values_mut().chain(self.webviews.views.values_mut()) {
            if !tab.shot_asked || tab.shot.is_some() {
                continue;
            }
            let Some(png) = tab.page.as_ref().and_then(|page| page.take_snapshot()) else { continue };
            tab.shot = decode_picture(&png);
            changed |= tab.shot.is_some();
        }
        if changed {
            cx.notify();
        }
    }

    /// Part of drawing the window: pages in tabs that are not in view go out of sight, the host
    /// hears what is shown, and a new theme reaches the pages.
    pub(crate) fn sync_native_webviews(&mut self) {
        if self.webviews.is_empty() {
            return;
        }
        let covered = self.page_covered();
        let shown: Vec<(DocumentId, bool)> = self
            .groups
            .iter()
            .enumerate()
            .filter_map(|(gi, g)| g.tabs.get(g.active).map(|t| (t.doc, gi == self.active_group)))
            .collect();
        let side_views: Vec<String> = self.shown_webview_views();
        let theme_index = self.theme_index;
        let new_theme = (self.webviews.themed != Some(theme_index)).then(|| self.page_theme());
        let host = self.ext_host.clone();
        let refresh = |tab: &mut WebviewTab, in_view: bool, active: bool| {
            if let Some(page) = tab.page.as_mut() {
                if !in_view || covered {
                    page.set_visible(false);
                }
                if let Some(theme) = &new_theme {
                    page.set_theme(theme);
                }
            }
            let state = (in_view, in_view && active);
            if tab.reported != Some(state) {
                tab.reported = Some(state);
                host.webview_visibility(tab.id, state.0, state.1);
            }
        };
        for (doc, tab) in self.webviews.tabs.iter_mut() {
            let in_view = shown.iter().any(|(d, _)| d == doc);
            let active = shown.iter().any(|(d, is_active)| d == doc && *is_active);
            refresh(tab, in_view, active);
        }
        for (view, tab) in self.webviews.views.iter_mut() {
            refresh(tab, side_views.contains(view), true);
        }
        self.webviews.themed = Some(theme_index);
    }

    fn page_theme(&self) -> webview_page::PageTheme {
        webview_page::page_theme(&self.themes[self.theme_index], crate::text_layout::ui_font(), crate::text_layout::code_font(), crate::text_layout::font_size())
    }

    /// The window's own click: a page that had the keyboard gives it back.
    pub(crate) fn release_page_focus(&mut self) {
        for tab in self.webviews.tabs.values_mut().chain(self.webviews.views.values_mut()) {
            if tab.focused {
                tab.focused = false;
                if let Some(page) = tab.page.as_ref() {
                    page.release_focus();
                }
            }
        }
    }

    /// Called after the tab measured itself: make the page, put it over the room, show it.
    fn place_webview(&mut self, doc: DocumentId, bounds: Bounds<Pixels>, mask: Bounds<Pixels>, window: &mut Window) {
        let shown = self.groups.iter().any(|g| g.tabs.get(g.active).is_some_and(|t| t.doc == doc));
        let (covered, folders, theme) = (self.page_covered(), self.folders(), self.page_theme());
        if let Some(tab) = self.webviews.tabs.get_mut(&doc) {
            ensure_page(tab, window, &folders, theme, shown && !covered && mask.intersect(&bounds) == bounds, bounds);
        }
    }

    /// The same for the page of a side bar view.
    fn place_view_webview(&mut self, view: &str, bounds: Bounds<Pixels>, mask: Bounds<Pixels>, window: &mut Window) {
        let (covered, folders, theme) = (self.page_covered(), self.folders(), self.page_theme());
        let shown = self.shown_webview_views().iter().any(|v| v == view);
        if let Some(tab) = self.webviews.views.get_mut(view) {
            ensure_page(tab, window, &folders, theme, shown && !covered && mask.intersect(&bounds) == bounds, bounds);
        }
    }

    /// The pages of side bar views that are on screen: the container is shown and the view is open.
    fn shown_webview_views(&self) -> Vec<String> {
        let Some(container) = self.shown_container() else { return Vec::new() };
        self.views_in(&container)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.webviews.views.contains_key(id) && !self.ext_ui.is_collapsed(&format!("view:{id}")))
            .collect()
    }

    /// Ask the extensions to fill the views of the shown container that are drawn with a page.
    pub(crate) fn open_visible_webview_views(&mut self, cx: &mut Context<Self>) {
        let Some(container) = self.shown_container() else { return };
        let folders = self.folders();
        for (view, _) in self.views_in(&container) {
            if !self.ext_ui.contributed.views.iter().any(|v| v.id == view && v.webview) || self.webviews.views.contains_key(&view) {
                continue;
            }
            self.webviews.next_app += 1;
            let id = APP_WEBVIEWS + self.webviews.next_app;
            self.webviews.views.insert(view.clone(), WebviewTab::new(id, WebviewKind::View, WebviewOptions::default(), None));
            self.start_webview_pump(cx);
            self.ext_host.webview_view_open(id, &view, &folders);
        }
    }

    /// The part of a side bar for a view drawn with a page.
    pub(crate) fn render_webview_view(&self, view: &str, sole: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let Some(tab) = self.webviews.views.get(view) else {
            return div().px(px(12.)).py(px(4.)).text_color(theme.gutter_text).child("Loading...").into_any_element();
        };
        let (bounds_cell, entity, message, view_id, shot) = (tab.bounds.clone(), cx.entity(), tab.failed.clone(), view.to_owned(), tab.shot.clone());
        div()
            .id(SharedString::from(format!("webview-view:{view}")))
            .relative()
            .flex_none()
            .w_full()
            .when(sole, |d| d.flex_1().min_h(px(200.)))
            .when(!sole, |d| d.h(px(300.)))
            .text_color(theme.gutter_text)
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        bounds_cell.set(bounds);
                        let mask = window.content_mask().bounds;
                        let (entity, view_id) = (entity.clone(), view_id.clone());
                        window.defer(cx, move |window, cx| {
                            entity.update(cx, |this, _| this.place_view_webview(&view_id, bounds, mask, window));
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(shot.map(|image| gpui::img(image).absolute().size_full().object_fit(gpui::ObjectFit::Fill)))
            .children(message.map(|text| div().p(px(12.)).child(format!("This view cannot be shown: {text}"))))
            .into_any_element()
    }

    /// The area of a page's tab.
    pub(crate) fn render_webview(&self, gi: usize, cx: &mut Context<Self>) -> AnyElement {
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let Some(tab) = self.webviews.tabs.get(&doc) else { return div().into_any_element() };
        let theme = &self.theme;
        let bounds_cell = tab.bounds.clone();
        let entity = cx.entity();
        let (message, shot) = (tab.failed.clone(), tab.shot.clone());
        div()
            .id(("webview", group.id.0 as usize))
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .text_color(theme.gutter_text)
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        bounds_cell.set(bounds);
                        let mask = window.content_mask().bounds;
                        let entity = entity.clone();
                        window.defer(cx, move |window, cx| {
                            entity.update(cx, |this, _| this.place_webview(doc, bounds, mask, window));
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(shot.map(|image| gpui::img(image).absolute().size_full().object_fit(gpui::ObjectFit::Fill)))
            .children(message.map(|text| div().p(px(24.)).child(format!("This page cannot be shown: {text}"))))
            .into_any_element()
    }
}

impl EditorView {
    /// The pages to open again at the next start: panels whose extension can fill them from the
    /// state they kept.
    pub(crate) fn panel_snapshot(&self, order: &[crate::groups::GroupId]) -> Vec<rustcode_application::PanelState> {
        let mut panels = Vec::new();
        for (index, id) in order.iter().enumerate() {
            let Some(gi) = self.group_index(*id) else { continue };
            let group = &self.groups[gi];
            for (ti, tab) in group.tabs.iter().enumerate() {
                let Some(page) = self.webviews.tabs.get(&tab.doc) else { continue };
                let WebviewKind::Panel { view_type } = &page.kind else { continue };
                if !self.webviews.serializers.contains(view_type) {
                    continue;
                }
                panels.push(rustcode_application::PanelState {
                    view_type: view_type.clone(),
                    title: self.doc_label(tab.doc).0,
                    state: page.state.clone().unwrap_or(Value::Null),
                    group: index,
                    active: ti == group.active,
                });
            }
        }
        panels
    }

    /// Open a panel of the last run again, in the group it was in; its extension fills it.
    pub(crate) fn restore_panel(&mut self, panel: &rustcode_application::PanelState, group: crate::groups::GroupId, cx: &mut Context<Self>) {
        let Some(gi) = self.group_index(group) else { return };
        // A group that had nothing but a stand-in for "nothing open" shows only the page.
        if let Some(stand_in) = self.placeholder_doc {
            if self.groups[gi].tabs.len() == 1 && self.groups[gi].tabs[0].doc == stand_in {
                self.groups[gi].tabs.clear();
                self.placeholder_doc = None;
            }
        }
        let id = self.allocate_document_id();
        let doc = self.add_document(Document::new(id, panel.title.clone(), TextBuffer::default()));
        self.read_only_docs.insert(doc);
        self.webviews.next_app += 1;
        let page_id = APP_WEBVIEWS + self.webviews.next_app;
        let mut tab = WebviewTab::new(page_id, WebviewKind::Panel { view_type: panel.view_type.clone() }, WebviewOptions::default(), None);
        if !panel.state.is_null() {
            tab.state = Some(panel.state.clone());
        }
        // Only a view type that was saved can be restored: it had a serializer.
        self.webviews.serializers.insert(panel.view_type.clone());
        self.webviews.tabs.insert(doc, tab);
        self.groups[gi].tabs.push(crate::groups::GroupTab { doc, view: crate::groups::ViewState::default() });
        if panel.active {
            self.groups[gi].active = self.groups[gi].tabs.len() - 1;
        }
        self.start_webview_pump(cx);
        let folders = self.folders();
        self.ext_host.webview_restore(page_id, &panel.view_type, &panel.title, &panel.state, &folders);
    }
}

/// A picture file (PNG, JPEG, ...) as an image the window can draw: blue first, with the color
/// already scaled by transparency.
fn decode_picture(bytes: &[u8]) -> Option<std::sync::Arc<gpui::RenderImage>> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    let mut pixels = decoded.into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        let (r, g, b) = (pixel[0] as u32 * alpha / 255, pixel[1] as u32 * alpha / 255, pixel[2] as u32 * alpha / 255);
        pixel[0] = b as u8;
        pixel[1] = g as u8;
        pixel[2] = r as u8;
    }
    let buffer = image::ImageBuffer::from_raw(width, height, pixels)?;
    Some(std::sync::Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(buffer), 1))))
}

/// Make the page of `tab` when it is first to be shown, put it over `bounds`, and show or hide it.
fn ensure_page(tab: &mut WebviewTab, window: &mut Window, folders: &[PathBuf], theme: webview_page::PageTheme, show: bool, bounds: Bounds<Pixels>) {
    if tab.page.is_none() && tab.failed.is_none() && show {
        let config = PageConfig {
            id: tab.id,
            enable_scripts: tab.options.enable_scripts,
            roots: tab.options.local_roots.clone().unwrap_or_else(|| default_roots(folders)),
            theme,
            state: tab.state.clone(),
            html: tab.html.clone(),
            url: tab.url.clone(),
        };
        match NativePage::create(window, config) {
            Ok(mut page) => {
                for message in std::mem::take(&mut tab.early) {
                    page.post(message);
                }
                tab.page = Some(page);
            }
            Err(message) => tab.failed = Some(message),
        }
    }
    if let Some(page) = tab.page.as_mut() {
        page.set_bounds(bounds);
        // While the window has something over the page, its picture is drawn instead.
        page.set_visible(show && tab.shot.is_none());
        if show && std::mem::take(&mut tab.wants_focus) {
            page.focus();
            tab.focused = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_may_read_the_workspace_and_the_extensions_by_default() {
        let roots = default_roots(&[PathBuf::from("/work")]);
        assert_eq!(roots[0], PathBuf::from("/work"));
        assert!(roots.iter().any(|r| r.ends_with(".rustcode/extensions")));
    }
}
