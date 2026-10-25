//! The Extensions side bar view: the extensions installed here, and a search of the marketplace
//! (Open VSX) with a button to install what is found.

use std::time::Duration;

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent, ScrollHandle, Task, Window};
use rustcode_application::MarketExtension;
use rustcode_domain::{Editor, TextBuffer};

use crate::editor_view::{EditorView, Focus};
use crate::notifications::ToastLevel;

/// Typing waits this long for more typing before the marketplace is asked.
const DEBOUNCE: Duration = Duration::from_millis(350);

pub(crate) struct ExtensionsState {
    pub query: Editor,
    pub results: Vec<MarketExtension>,
    pub searching: bool,
    pub error: Option<String>,
    /// Bumped by every search, so a late answer to an old one is dropped.
    generation: u64,
    task: Option<Task<()>>,
    /// The id of the extension being installed.
    pub installing: Option<String>,
    pub scroll: ScrollHandle,
    /// The marketplace has been asked at least once.
    asked: bool,
}

impl ExtensionsState {
    pub fn new() -> Self {
        Self {
            query: Editor::new(TextBuffer::default()),
            results: Vec::new(),
            searching: false,
            error: None,
            generation: 0,
            task: None,
            installing: None,
            scroll: ScrollHandle::new(),
            asked: false,
        }
    }
}

impl EditorView {
    /// The view was opened: read what is installed, and show the popular extensions at first.
    pub(crate) fn extensions_opened(&mut self, cx: &mut Context<Self>) {
        self.installed_extensions = self.ext_host.installed_packages();
        self.set_focus(Focus::ExtensionsSearch, cx);
        if !self.extensions.asked {
            self.extensions_search_now(cx);
        }
    }

    /// The text of the search box changed: ask the marketplace after a pause.
    pub(crate) fn extensions_query_changed(&mut self, cx: &mut Context<Self>) {
        self.extensions.generation += 1;
        let generation = self.extensions.generation;
        self.extensions.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.extensions.generation == generation {
                    this.extensions_search_now(cx);
                }
            })
            .ok();
        }));
    }

    pub(crate) fn extensions_search_now(&mut self, cx: &mut Context<Self>) {
        self.extensions.asked = true;
        self.extensions.generation += 1;
        let generation = self.extensions.generation;
        let query = self.extensions.query.text().trim().to_owned();
        let host = self.ext_host.clone();
        self.extensions.searching = true;
        self.extensions.error = None;
        cx.notify();
        self.extensions.task = Some(cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { host.search_marketplace(&query) }).await;
            this.update(cx, |this, cx| {
                if this.extensions.generation != generation {
                    return;
                }
                this.extensions.searching = false;
                match found {
                    Ok(list) => this.extensions.results = list,
                    Err(why) => {
                        this.extensions.results.clear();
                        this.extensions.error = Some(why);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Download `extension` from the marketplace and install it.
    pub(crate) fn extensions_install(&mut self, extension: MarketExtension, cx: &mut Context<Self>) {
        if self.extensions.installing.is_some() {
            return;
        }
        self.extensions.installing = Some(extension.id.clone());
        self.set_status(format!("installing {}...", extension.name));
        cx.notify();
        let host = self.ext_host.clone();
        let name = extension.name.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { host.install_from_marketplace(&extension) }).await;
            this.update(cx, |this, cx| {
                this.extensions.installing = None;
                match outcome {
                    Ok(message) => {
                        this.load_extension_contributions(cx);
                        this.installed_extensions = this.ext_host.installed_packages();
                        this.set_status(message.clone());
                        this.notify_user(ToastLevel::Info, message, cx);
                    }
                    Err(why) => {
                        this.set_status(format!("cannot install {name}: {why}"));
                        this.notify_user(ToastLevel::Error, format!("Cannot install {name}: {why}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_extensions_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.side_bar.foreground, theme.gutter_text);
        let input_width = self.side_panel_width(window) - 24.0;

        // One row of the list: a picture, a name and a line about it, who made it, and a button.
        let row = |id: (&'static str, usize),
                   icon: Option<std::path::PathBuf>,
                   name: String,
                   description: String,
                   publisher: String,
                   button: gpui::AnyElement| {
            let letter = name.chars().next().map_or('?', |c| c.to_ascii_uppercase()).to_string();
            let picture = match icon {
                Some(path) => gpui::img(path).w(px(42.)).h(px(42.)).flex_none().into_any_element(),
                None => div()
                    .w(px(42.))
                    .h(px(42.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(theme.widget_background)
                    .text_size(px(18.))
                    .text_color(dim)
                    .child(letter)
                    .into_any_element(),
            };
            div()
                .id(id)
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .flex()
                .flex_row()
                .gap(px(10.))
                .text_color(text)
                .hover(move |style| style.bg(hover))
                .child(picture)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.)).text_color(dim).child(description))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(px(12.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(dim).child(publisher))
                                .child(button),
                        ),
                )
                .into_any_element()
        };

        let section = |title: &'static str, count: Option<usize>| {
            div()
                .h(px(28.))
                .flex_none()
                .px(px(12.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(text)
                .child(crate::icons::icon("chevron-down", 12., dim))
                .child(title)
                .child(div().flex_1())
                .children(count.map(|n| crate::shell::count_badge_inline(n, theme.badge.background, theme.badge.foreground)))
                .into_any_element()
        };

        let mut rows: Vec<gpui::AnyElement> = vec![section("Installed", Some(self.installed_extensions.len()))];
        for (index, package) in self.installed_extensions.iter().enumerate() {
            let gear = div()
                .id(("extension-uninstall", index))
                .w(px(22.))
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.uninstall_extension(index, cx)
                    }),
                )
                .child(crate::icons::icon("trash", 14., dim))
                .into_any_element();
            rows.push(row(("extension-installed", index), package.icon.clone(), package.name.clone(), package.description.clone(), package.publisher.clone(), gear));
        }

        let installed_ids: Vec<&str> = self.installed_extensions.iter().map(|p| p.id.as_str()).collect();
        let title = if self.extensions.query.text().trim().is_empty() { "Popular" } else { "Marketplace" };
        rows.push(section(title, (!self.extensions.results.is_empty()).then_some(self.extensions.results.len())));
        if self.extensions.searching {
            rows.push(div().px(px(12.)).py(px(4.)).text_color(dim).child("Searching...").into_any_element());
        } else if let Some(error) = &self.extensions.error {
            rows.push(div().px(px(12.)).py(px(4.)).text_color(theme.markers.error).child(error.clone()).into_any_element());
        } else if self.extensions.results.is_empty() && self.extensions.asked {
            rows.push(div().px(px(12.)).py(px(4.)).text_color(dim).child("No extensions found").into_any_element());
        }
        for (index, found) in self.extensions.results.iter().enumerate() {
            let installed = installed_ids.iter().any(|id| id.eq_ignore_ascii_case(&found.id));
            let installing = self.extensions.installing.as_deref() == Some(found.id.as_str());
            let label = if installed { "Installed" } else if installing { "Installing..." } else { "Install" };
            let enabled = !installed && self.extensions.installing.is_none();
            let extension = found.clone();
            let install = div()
                .id(("extension-install", index))
                .px(px(10.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .text_size(px(12.))
                .bg(if enabled { theme.button.background } else { theme.widget_background })
                .text_color(if enabled { theme.button.foreground } else { dim })
                .when(enabled, |b| b.cursor_pointer().hover(|style| style.opacity(0.9)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        if enabled {
                            this.extensions_install(extension.clone(), cx)
                        }
                    }),
                )
                .child(label)
                .into_any_element();
            rows.push(row(("extension-found", index), None, found.name.clone(), found.description.clone(), found.publisher.clone(), install));
        }

        self.side_panel_frame("extensions-panel", cx)
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.side_bar.title_foreground)
                    .child("Extensions")
                    .child(div().flex_1())
                    .child(self.icon_button("extensions-refresh", "refresh", false, cx, |this, cx| {
                        this.installed_extensions = this.ext_host.installed_packages();
                        this.extensions_search_now(cx)
                    })),
            )
            .child(div().flex_none().px(px(12.)).pb(px(8.)).child(self.input(
                "extensions-input",
                &self.extensions.query,
                self.focus == Focus::ExtensionsSearch,
                Focus::ExtensionsSearch,
                11,
                input_width,
                window,
                cx,
            )))
            .child(self.scroll_area(
                8,
                &self.extensions.scroll,
                div()
                    .id("extensions-list")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.extensions.scroll)
                    .children(rows),
                cx,
            ))
    }
}
