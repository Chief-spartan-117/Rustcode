//! The system webview that shows a page of an extension (`wry`): WebKit on macOS, WebView2 on
//! Windows. The page is a child view of the window, put over the area its tab leaves for it.
//! Where there is no such webview yet (Linux) a page cannot be shown, and `create` says so.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::webview_page::{PageMessage, PageTheme};

/// Everything a page is started with.
pub(crate) struct PageConfig {
    pub id: u64,
    pub enable_scripts: bool,
    pub roots: Vec<PathBuf>,
    pub theme: PageTheme,
    pub state: Option<Value>,
    pub html: String,
    /// An address to load instead of the page of `html` (a tool that serves its own).
    pub url: Option<String>,
}

/// What the address handlers need to answer: they run when the webview asks, not when the editor draws.
pub(crate) struct Shared {
    pub html: String,
    pub enable_scripts: bool,
    pub roots: Vec<PathBuf>,
}

pub(crate) type Inbox = Arc<Mutex<Vec<PageMessage>>>;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod system {
    use std::borrow::Cow;

    use gpui::{Bounds, Pixels, Window};
    use http::{Response, StatusCode};
    use serde_json::Value;
    use wry::dpi::{LogicalPosition, LogicalSize};
    use wry::{NewWindowResponse, Rect, WebView, WebViewBuilder};

    use super::{Inbox, PageConfig, Shared};
    use crate::webview_page::{self, PageMessage, PageTheme};

    pub(crate) struct NativePage {
        view: WebView,
        shared: std::sync::Arc<std::sync::Mutex<Shared>>,
        inbox: Inbox,
        version: u64,
        ready: bool,
        queued: Vec<Value>,
        visible: bool,
        last_bounds: Option<(i32, i32, i32, i32)>,
        /// The page's own view, to make a picture of.
        native: Option<crate::webview_mask::NativeView>,
    }

    fn reply(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
        Response::builder()
            .status(status)
            .header("Content-Type", content_type)
            .header("Access-Control-Allow-Origin", "*")
            .header("Cache-Control", "no-store")
            .body(Cow::Owned(body))
            .expect("a response with plain headers")
    }

    fn not_found() -> Response<Cow<'static, [u8]>> {
        reply(StatusCode::NOT_FOUND, "text/plain", b"not found".to_vec())
    }

    /// `bytes=from-to` of a Range header, against a file of `length` bytes.
    fn byte_range(header: &str, length: u64) -> Option<(u64, u64)> {
        let spec = header.strip_prefix("bytes=")?;
        let (from, to) = spec.split_once('-')?;
        let (from, to) = match (from.parse::<u64>().ok(), to.parse::<u64>().ok()) {
            (Some(from), Some(to)) => (from, to),
            (Some(from), None) => (from, length.saturating_sub(1)),
            (None, Some(last)) => (length.saturating_sub(last), length.saturating_sub(1)),
            (None, None) => return None,
        };
        (from <= to && from < length).then_some((from, to.min(length.saturating_sub(1))))
    }

    fn serve_resource(shared: &std::sync::Mutex<Shared>, request: &http::Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        let Some(file) = webview_page::resource_path(request.uri().path()) else { return not_found() };
        let roots = shared.lock().map(|s| s.roots.clone()).unwrap_or_default();
        if !webview_page::inside_roots(&file, &roots) {
            return reply(StatusCode::FORBIDDEN, "text/plain", b"outside the folders this page may read".to_vec());
        }
        let Ok(bytes) = std::fs::read(&file) else { return not_found() };
        let mime = webview_page::mime_type(&file);
        let range = request.headers().get("range").and_then(|h| h.to_str().ok()).and_then(|h| byte_range(h, bytes.len() as u64));
        match range {
            Some((from, to)) => {
                let total = bytes.len();
                let slice = bytes[from as usize..=to as usize].to_vec();
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("Content-Type", mime)
                    .header("Content-Range", format!("bytes {from}-{to}/{total}"))
                    .header("Accept-Ranges", "bytes")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Cow::Owned(slice))
                    .expect("a response with plain headers")
            }
            None => reply(StatusCode::OK, mime, bytes),
        }
    }

    fn serve_page(shared: &std::sync::Mutex<Shared>, request: &http::Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        if request.uri().path() != "/index.html" {
            return not_found();
        }
        let Ok(page) = shared.lock() else { return not_found() };
        let mut builder = Response::builder().status(StatusCode::OK).header("Content-Type", "text/html; charset=utf-8").header("Cache-Control", "no-store");
        // A page that did not ask for scripts gets none, whatever its markup says.
        if !page.enable_scripts {
            builder = builder.header("Content-Security-Policy", "script-src 'none'");
        }
        builder.body(Cow::Owned(page.html.clone().into_bytes())).expect("a response with plain headers")
    }

    fn open_outside(url: &str) {
        if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:")) {
            return;
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(url).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }

    impl NativePage {
        pub(crate) fn create(window: &Window, config: PageConfig) -> Result<Self, String> {
            // Test windows have no platform window to put a view in (asking for its handle panics).
            if cfg!(test) {
                return Err("there is no real window in tests".to_owned());
            }
            let shared = std::sync::Arc::new(std::sync::Mutex::new(Shared { html: config.html, enable_scripts: config.enable_scripts, roots: config.roots }));
            let inbox: Inbox = Default::default();
            let (page_shared, resource_shared) = (shared.clone(), shared.clone());
            let (ipc_inbox, nav_inbox) = (inbox.clone(), inbox.clone());
            let origin = webview_page::page_origin();
            let site = config.url.as_deref().and_then(webview_page::origin_of);
            let first_url = config.url.clone().unwrap_or_else(|| webview_page::page_url(1));
            let resource_origin = webview_page::resource_origin();
            let background = config.theme.background;
            let id = config.id.to_string();
            let builder = WebViewBuilder::new()
                .with_id(&id)
                .with_visible(false)
                .with_incognito(true)
                .with_background_color((background.r, background.g, background.b, 255))
                .with_devtools(cfg!(debug_assertions))
                .with_initialization_script_for_main_only(webview_page::init_script(&config.theme, config.state.as_ref()), true)
                .with_custom_protocol(webview_page::PAGE_SCHEME.into(), move |_, request| serve_page(&page_shared, &request))
                .with_custom_protocol(webview_page::RESOURCE_SCHEME.into(), move |_, request| serve_resource(&resource_shared, &request))
                .with_ipc_handler(move |request| {
                    if let Some(message) = webview_page::parse_page_message(request.body()) {
                        if let Ok(mut inbox) = ipc_inbox.lock() {
                            inbox.push(message);
                        }
                    }
                })
                .with_navigation_handler(move |url| {
                    if url.starts_with(&origin) || url.starts_with(&resource_origin) || url == "about:blank" || site.as_ref().is_some_and(|site| url.starts_with(site.as_str())) {
                        return true;
                    }
                    if let Some(command) = webview_page::parse_command_link(&url) {
                        if let Ok(mut inbox) = nav_inbox.lock() {
                            inbox.push(command);
                        }
                    } else {
                        open_outside(&url);
                    }
                    false
                })
                .with_new_window_req_handler(|url, _| {
                    open_outside(&url);
                    NewWindowResponse::Deny
                })
                .with_bounds(Rect { position: LogicalPosition::new(0, 0).into(), size: LogicalSize::new(1, 1).into() })
                .with_url(first_url);
            let view = builder.build_as_child(window).map_err(|error| error.to_string())?;
            #[cfg(target_os = "macos")]
            let native = crate::webview_mask::NativeView::newest_in(window);
            #[cfg(not(target_os = "macos"))]
            let native = None;
            Ok(Self { view, shared, inbox, version: 1, ready: false, queued: Vec::new(), visible: false, last_bounds: None, native })
        }

        pub(crate) fn set_html(&mut self, html: String) {
            if let Ok(mut shared) = self.shared.lock() {
                shared.html = html;
            }
            self.version += 1;
            self.ready = false;
            let _ = self.view.load_url(&webview_page::page_url(self.version));
        }

        pub(crate) fn set_access(&mut self, enable_scripts: bool, roots: Vec<std::path::PathBuf>) {
            let changed = self.shared.lock().map(|mut s| {
                let changed = s.enable_scripts != enable_scripts;
                s.enable_scripts = enable_scripts;
                s.roots = roots;
                changed
            });
            // Scripts are decided when the page is loaded.
            if changed.unwrap_or(false) {
                self.ready = false;
                self.version += 1;
                let _ = self.view.load_url(&webview_page::page_url(self.version));
            }
        }

        /// Send a message to the page, or keep it until the page has loaded.
        pub(crate) fn post(&mut self, message: Value) {
            if self.ready {
                self.deliver(&message);
            } else {
                self.queued.push(message);
            }
        }

        fn deliver(&self, message: &Value) {
            let _ = self.view.evaluate_script(&format!("window.__rustcode && window.__rustcode.deliver({message})"));
        }

        /// The page has loaded: what was posted before it is sent now.
        pub(crate) fn loaded(&mut self) {
            self.ready = true;
            for message in std::mem::take(&mut self.queued) {
                self.deliver(&message);
            }
        }

        pub(crate) fn take_messages(&self) -> Vec<PageMessage> {
            self.inbox.lock().map(|mut inbox| std::mem::take(&mut *inbox)).unwrap_or_default()
        }

        pub(crate) fn set_theme(&self, theme: &PageTheme) {
            let _ = self.view.evaluate_script(&format!("window.__rustcode && window.__rustcode.theme({})", webview_page::theme_json(theme)));
        }

        pub(crate) fn set_bounds(&mut self, bounds: Bounds<Pixels>) {
            let rounded = (
                f32::from(bounds.origin.x).round() as i32,
                f32::from(bounds.origin.y).round() as i32,
                f32::from(bounds.size.width).round().max(1.0) as i32,
                f32::from(bounds.size.height).round().max(1.0) as i32,
            );
            if self.last_bounds == Some(rounded) {
                return;
            }
            self.last_bounds = Some(rounded);
            let _ = self.view.set_bounds(Rect {
                position: LogicalPosition::new(rounded.0, rounded.1).into(),
                size: LogicalSize::new(rounded.2 as u32, rounded.3 as u32).into(),
            });
        }

        /// Ask for a picture of the page as it is now, to stand in for it while something covers it.
        pub(crate) fn request_snapshot(&self) {
            #[cfg(target_os = "macos")]
            if let (Some(native), Some((_, _, width, _))) = (self.native.as_ref(), self.last_bounds) {
                native.request_snapshot(width as f64);
            }
        }

        /// The picture asked for, as PNG, when it has come.
        pub(crate) fn take_snapshot(&self) -> Option<Vec<u8>> {
            #[cfg(target_os = "macos")]
            return self.native.as_ref().and_then(|native| native.take_snapshot());
            #[cfg(not(target_os = "macos"))]
            None
        }

        pub(crate) fn set_visible(&mut self, visible: bool) {
            if self.visible != visible {
                self.visible = visible;
                let _ = self.view.set_visible(visible);
                if !visible {
                    // A page that is out of sight must not keep the keyboard.
                    let _ = self.view.focus_parent();
                }
            }
        }

        pub(crate) fn focus(&self) {
            let _ = self.view.focus();
        }

        pub(crate) fn release_focus(&self) {
            let _ = self.view.focus_parent();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::byte_range;

        #[test]
        fn ranges_of_files_are_read() {
            assert_eq!(byte_range("bytes=0-9", 100), Some((0, 9)));
            assert_eq!(byte_range("bytes=90-", 100), Some((90, 99)));
            assert_eq!(byte_range("bytes=-10", 100), Some((90, 99)));
            assert_eq!(byte_range("bytes=50-500", 100), Some((50, 99)));
            assert_eq!(byte_range("bytes=200-300", 100), None);
            assert_eq!(byte_range("lines=1-2", 100), None);
        }
    }
}

/// Whether a picture of a page can stand in for it while the window draws something over it, or the
/// page has to be hidden then.
pub(crate) const CAN_SHOW_STAND_IN: bool = cfg!(target_os = "macos");

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod system {
    use gpui::{Bounds, Pixels, Window};
    use serde_json::Value;

    use super::PageConfig;
    use crate::webview_page::{PageMessage, PageTheme};

    pub(crate) struct NativePage;

    impl NativePage {
        pub(crate) fn create(_window: &Window, _config: PageConfig) -> Result<Self, String> {
            Err("pages of extensions are not shown on this system yet".to_owned())
        }
        pub(crate) fn set_html(&mut self, _html: String) {}
        pub(crate) fn set_access(&mut self, _enable_scripts: bool, _roots: Vec<std::path::PathBuf>) {}
        pub(crate) fn post(&mut self, _message: Value) {}
        pub(crate) fn loaded(&mut self) {}
        pub(crate) fn take_messages(&self) -> Vec<PageMessage> {
            Vec::new()
        }
        pub(crate) fn set_theme(&self, _theme: &PageTheme) {}
        pub(crate) fn set_bounds(&mut self, _bounds: Bounds<Pixels>) {}
        pub(crate) fn request_snapshot(&self) {}
        pub(crate) fn take_snapshot(&self) -> Option<Vec<u8>> {
            None
        }
        pub(crate) fn set_visible(&mut self, _visible: bool) {}
        pub(crate) fn focus(&self) {}
        pub(crate) fn release_focus(&self) {}
    }
}

pub(crate) use system::NativePage;
