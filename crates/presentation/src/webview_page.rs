//! What a page of an extension needs around it, apart from the system webview that shows it: the
//! colors and fonts it reads (`--vscode-*`), the script that gives it `acquireVsCodeApi`, the
//! files it may load, and the messages it sends back.

use std::path::{Path, PathBuf};

use gpui::{Keystroke, Modifiers};
use rustcode_domain::{Color, ColorTheme, ThemeKind};
use serde_json::{json, Value};

/// Address of the page and of the files it loads. Windows serves custom schemes as `http://<name>.localhost`.
pub(crate) const PAGE_SCHEME: &str = "rustcode-webview";
pub(crate) const RESOURCE_SCHEME: &str = "rustcode-resource";

pub(crate) fn page_origin() -> String {
    if cfg!(windows) {
        format!("http://{PAGE_SCHEME}.localhost")
    } else {
        format!("{PAGE_SCHEME}://localhost")
    }
}

pub(crate) fn resource_origin() -> String {
    if cfg!(windows) {
        format!("http://{RESOURCE_SCHEME}.localhost")
    } else {
        format!("{RESOURCE_SCHEME}://localhost")
    }
}

/// `scheme://host:port` of an address, for telling whether a navigation stays on the same site.
pub(crate) fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// The address the page is loaded from. `version` changes when the extension sets new HTML.
pub(crate) fn page_url(version: u64) -> String {
    format!("{}/index.html?v={version}", page_origin())
}

// ---- colors and fonts ------------------------------------------------------------------------------

/// VS Code color keys pages read as `var(--vscode-<key with dots as dashes>)`, with what a dark and
/// a light theme use when the theme does not say.
const COLORS: &[(&str, &str, &str)] = &[
    ("foreground", "#cccccc", "#616161"),
    ("disabledForeground", "#cccccc80", "#61616180"),
    ("descriptionForeground", "#ccccccb3", "#717171"),
    ("errorForeground", "#f48771", "#a1260d"),
    ("focusBorder", "#007fd4", "#0090f1"),
    ("contrastBorder", "#00000000", "#00000000"),
    ("widget.shadow", "#0000005c", "#00000029"),
    ("selection.background", "#264f78", "#add6ff"),
    ("icon.foreground", "#c5c5c5", "#424242"),
    ("textLink.foreground", "#3794ff", "#006ab1"),
    ("textLink.activeForeground", "#3794ff", "#006ab1"),
    ("textCodeBlock.background", "#0a0a0a66", "#dcdcdc66"),
    ("textBlockQuote.background", "#222222", "#f2f2f2"),
    ("textBlockQuote.border", "#007acc80", "#007acc80"),
    ("textPreformat.foreground", "#d7ba7d", "#a31515"),
    ("textSeparator.foreground", "#ffffff2e", "#0000002e"),
    ("editor.background", "#1e1e1e", "#ffffff"),
    ("editor.foreground", "#d4d4d4", "#000000"),
    ("editor.selectionBackground", "#264f78", "#add6ff"),
    ("editor.findMatchBackground", "#515c6a", "#a8ac94"),
    ("editor.findMatchHighlightBackground", "#ea5c0055", "#ea5c0055"),
    ("editor.lineHighlightBackground", "#ffffff0a", "#0000000a"),
    ("editorWidget.background", "#252526", "#f3f3f3"),
    ("editorWidget.foreground", "#cccccc", "#616161"),
    ("editorWidget.border", "#454545", "#c8c8c8"),
    ("editorError.foreground", "#f14c4c", "#e51400"),
    ("editorWarning.foreground", "#cca700", "#bf8803"),
    ("editorInfo.foreground", "#3794ff", "#1a85ff"),
    ("editorGroup.border", "#444444", "#e7e7e7"),
    ("editorLineNumber.foreground", "#858585", "#237893"),
    ("button.background", "#0e639c", "#007acc"),
    ("button.foreground", "#ffffff", "#ffffff"),
    ("button.hoverBackground", "#1177bb", "#0062a3"),
    ("button.secondaryBackground", "#3a3d41", "#5f6a79"),
    ("button.secondaryForeground", "#ffffff", "#ffffff"),
    ("button.secondaryHoverBackground", "#45494e", "#4c5561"),
    ("input.background", "#3c3c3c", "#ffffff"),
    ("input.foreground", "#cccccc", "#616161"),
    ("input.border", "#00000000", "#00000000"),
    ("input.placeholderForeground", "#a6a6a6", "#767676"),
    ("inputOption.activeBorder", "#007acc", "#007acc"),
    ("inputValidation.errorBackground", "#5a1d1d", "#f2dede"),
    ("inputValidation.errorBorder", "#be1100", "#be1100"),
    ("dropdown.background", "#3c3c3c", "#ffffff"),
    ("dropdown.foreground", "#f0f0f0", "#616161"),
    ("dropdown.border", "#3c3c3c", "#cecece"),
    ("badge.background", "#4d4d4d", "#c4c4c4"),
    ("badge.foreground", "#ffffff", "#333333"),
    ("list.activeSelectionBackground", "#04395e", "#0060c0"),
    ("list.activeSelectionForeground", "#ffffff", "#ffffff"),
    ("list.inactiveSelectionBackground", "#37373d", "#e4e6f1"),
    ("list.hoverBackground", "#2a2d2e", "#e8e8e8"),
    ("list.focusOutline", "#007fd4", "#0090f1"),
    ("sideBar.background", "#252526", "#f3f3f3"),
    ("sideBar.foreground", "#cccccc", "#616161"),
    ("sideBar.border", "#80808059", "#80808059"),
    ("sideBarTitle.foreground", "#bbbbbb", "#6f6f6f"),
    ("sideBarSectionHeader.background", "#80808033", "#80808033"),
    ("panel.background", "#1e1e1e", "#ffffff"),
    ("panel.border", "#80808059", "#80808059"),
    ("tab.activeBackground", "#1e1e1e", "#ffffff"),
    ("tab.inactiveBackground", "#2d2d2d", "#ececec"),
    ("scrollbarSlider.background", "#79797966", "#64646466"),
    ("scrollbarSlider.hoverBackground", "#646464b3", "#646464b3"),
    ("scrollbarSlider.activeBackground", "#bfbfbf66", "#00000099"),
    ("progressBar.background", "#0e70c0", "#0e70c0"),
    ("charts.foreground", "#cccccc", "#616161"),
    ("charts.red", "#f14c4c", "#e51400"),
    ("charts.blue", "#3794ff", "#1a85ff"),
    ("charts.yellow", "#cca700", "#bf8803"),
    ("charts.orange", "#d18616", "#d18616"),
    ("charts.green", "#89d185", "#388a34"),
    ("charts.purple", "#b180d7", "#652d90"),
    ("terminal.foreground", "#cccccc", "#333333"),
    ("terminal.background", "#1e1e1e", "#ffffff"),
    ("gitDecoration.modifiedResourceForeground", "#e2c08d", "#895503"),
    ("gitDecoration.deletedResourceForeground", "#c74e39", "#ad0707"),
    ("gitDecoration.addedResourceForeground", "#81b88b", "#587c0c"),
];

fn css_color(color: Color) -> String {
    if color.a == 255 {
        format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
    } else {
        format!("rgba({}, {}, {}, {:.3})", color.r, color.g, color.b, color.a as f32 / 255.0)
    }
}

/// `--vscode-` and the key with its dots as dashes: `editor.background` is `--vscode-editor-background`.
pub(crate) fn variable_name(key: &str) -> String {
    format!("--vscode-{}", key.replace('.', "-"))
}

/// Colors, fonts and kind of the theme in use, as a page sees them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PageTheme {
    pub variables: Vec<(String, String)>,
    /// `vscode-dark` or `vscode-light`, the class VS Code gives the page's body.
    pub class: &'static str,
    /// `dark` or `light`, for `data-vscode-theme-kind`.
    pub kind: &'static str,
    pub name: String,
    /// The background of the page before it paints.
    pub background: Color,
}

pub(crate) fn page_theme(theme: &ColorTheme, ui_font: &str, code_font: &str, code_size: f32) -> PageTheme {
    let dark = theme.kind == ThemeKind::Dark;
    let mut variables: Vec<(String, String)> = Vec::new();
    for (key, dark_default, light_default) in COLORS {
        let value = match theme.color(key) {
            Some(color) => css_color(color),
            None => match *key {
                // What the editor itself knows beats a fixed default.
                "editor.background" | "panel.background" | "tab.activeBackground" => css_color(theme.colors.background),
                "editor.foreground" => css_color(theme.colors.foreground),
                "editor.selectionBackground" | "selection.background" => css_color(theme.colors.selection),
                "editorWidget.background" => css_color(theme.colors.widget_background),
                "input.background" => css_color(theme.colors.input_background),
                "editor.lineHighlightBackground" => css_color(theme.colors.line_highlight),
                "editorLineNumber.foreground" => css_color(theme.colors.gutter_foreground),
                _ => (if dark { *dark_default } else { *light_default }).to_owned(),
            },
        };
        variables.push((variable_name(key), value));
    }
    let family = format!("{ui_font}, -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif");
    let mono = format!("{code_font}, Menlo, Consolas, 'Courier New', monospace");
    for (name, value) in [
        ("--vscode-font-family", family),
        ("--vscode-font-weight", "normal".to_owned()),
        ("--vscode-font-size", "13px".to_owned()),
        ("--vscode-editor-font-family", mono),
        ("--vscode-editor-font-weight", "normal".to_owned()),
        ("--vscode-editor-font-size", format!("{code_size}px")),
    ] {
        variables.push((name.to_owned(), value));
    }
    let background = theme.color("editor.background").unwrap_or(theme.colors.background);
    PageTheme {
        variables,
        class: if dark { "vscode-dark" } else { "vscode-light" },
        kind: if dark { "dark" } else { "light" },
        name: theme.name.clone(),
        background,
    }
}

// ---- the script every page starts with ---------------------------------------------------------------

/// Runs before the page's own scripts: `acquireVsCodeApi`, the theme, and what a page does not
/// handle itself (shortcuts reach the editor).
pub(crate) fn init_script(theme: &PageTheme, state: Option<&Value>) -> String {
    let theme_json = theme_json(theme);
    let state_json = state.map_or("null".to_owned(), |s| s.to_string());
    format!(
        r#"(() => {{
  if (window.__rustcode) return;
  const post = (message) => window.ipc.postMessage(JSON.stringify(message));
  const stateKey = '__rustcode_state';
  let state = {state_json};
  try {{
    // A page that is loaded again (new HTML) keeps the state it saved.
    const saved = window.sessionStorage.getItem(stateKey);
    if (saved !== null) state = JSON.parse(saved);
  }} catch (error) {{}}
  let acquired = false;
  const defaults = `
    html {{ scrollbar-color: var(--vscode-scrollbarSlider-background) transparent; }}
    body {{ background-color: transparent; color: var(--vscode-editor-foreground); font-family: var(--vscode-font-family);
      font-weight: var(--vscode-font-weight); font-size: var(--vscode-font-size); margin: 0; padding: 0 20px; }}
    img, video {{ max-width: 100%; max-height: 100%; }}
    a, a code {{ color: var(--vscode-textLink-foreground); }}
    a:hover {{ color: var(--vscode-textLink-activeForeground); }}
    a:focus, input:focus, select:focus, textarea:focus {{ outline: 1px solid -webkit-focus-ring-color; outline-offset: -1px; }}
    code {{ font-family: var(--vscode-editor-font-family); color: var(--vscode-textPreformat-foreground); }}
    pre code {{ color: inherit; }}
    ::selection {{ background-color: var(--vscode-editor-selectionBackground); }}
    blockquote {{ background: var(--vscode-textBlockQuote-background); border-color: var(--vscode-textBlockQuote-border); }}
    kbd {{ background-color: var(--vscode-textCodeBlock-background); border: 1px solid var(--vscode-widget-shadow); border-radius: 3px; padding: 1px 3px; }}
  `;
  // Typed arrays travel as {{$bin: type, data: base64}} in both directions.
  const toBase64 = (bytes) => {{
    let text = '';
    for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(text);
  }};
  const encode = (value, depth = 0) => {{
    if (value === null || typeof value !== 'object' || depth > 50) return value;
    if (value instanceof ArrayBuffer) return {{ $bin: 'ArrayBuffer', data: toBase64(new Uint8Array(value)) }};
    if (ArrayBuffer.isView(value)) {{
      const type = value instanceof DataView ? 'Uint8Array' : value.constructor.name;
      return {{ $bin: type, data: toBase64(new Uint8Array(value.buffer, value.byteOffset, value.byteLength)) }};
    }}
    if (typeof value.toJSON === 'function') return encode(value.toJSON(), depth + 1);
    if (Array.isArray(value)) return value.map((item) => encode(item, depth + 1));
    const out = {{}};
    for (const key of Object.keys(value)) out[key] = encode(value[key], depth + 1);
    return out;
  }};
  const decode = (value, depth = 0) => {{
    if (value === null || typeof value !== 'object' || depth > 50) return value;
    if (Array.isArray(value)) return value.map((item) => decode(item, depth + 1));
    if (typeof value.$bin === 'string' && typeof value.data === 'string' && Object.keys(value).length === 2) {{
      const text = atob(value.data);
      const bytes = new Uint8Array(text.length);
      for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i);
      const Type = value.$bin === 'ArrayBuffer' ? null : window[value.$bin];
      return Type ? new Type(bytes.buffer) : bytes.buffer;
    }}
    const out = {{}};
    for (const key of Object.keys(value)) out[key] = decode(value[key], depth + 1);
    return out;
  }};
  const applyTheme = (theme) => {{
    const root = document.documentElement;
    for (const [name, value] of theme.variables) root.style.setProperty(name, value);
    root.setAttribute('data-vscode-theme-kind', 'vscode-' + theme.kind);
    root.setAttribute('data-vscode-theme-name', theme.name);
    const mark = () => {{
      if (!document.body) return;
      document.body.classList.remove('vscode-light', 'vscode-dark');
      document.body.classList.add(theme.class);
      document.body.setAttribute('data-vscode-theme-kind', 'vscode-' + theme.kind);
      document.body.setAttribute('data-vscode-theme-name', theme.name);
    }};
    mark();
    document.addEventListener('DOMContentLoaded', mark);
    // The body is marked as soon as it exists, before the page's own scripts look at it.
    if (!document.body) {{
      const watcher = new MutationObserver(() => {{
        if (document.body) {{
          watcher.disconnect();
          mark();
        }}
      }});
      watcher.observe(document.documentElement, {{ childList: true }});
    }}
  }};
  const addDefaults = () => {{
    try {{
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(defaults);
      document.adoptedStyleSheets = [sheet, ...document.adoptedStyleSheets];
    }} catch (error) {{
      const style = document.createElement('style');
      style.textContent = defaults;
      (document.head || document.documentElement).prepend(style);
    }}
  }};
  applyTheme({theme_json});
  addDefaults();
  window.__rustcode = {{
    deliver: (message) => window.dispatchEvent(new MessageEvent('message', {{ data: decode(message) }})),
    theme: applyTheme,
  }};
  window.acquireVsCodeApi = () => {{
    if (acquired) throw new Error('An instance of the VS Code API has already been acquired');
    acquired = true;
    return Object.freeze({{
      postMessage: (message) => post({{ t: 'm', d: message === undefined ? null : encode(message) }}),
      setState: (next) => {{
        state = next;
        try {{ window.sessionStorage.setItem(stateKey, JSON.stringify(next === undefined ? null : next)); }} catch (error) {{}}
        post({{ t: 's', d: next === undefined ? null : next }});
        return next;
      }},
      getState: () => state,
    }});
  }};
  // Shortcuts the page did not use belong to the editor (open the palette, close the tab, ...).
  window.addEventListener('keydown', (event) => {{
    if (event.defaultPrevented || !(event.metaKey || event.ctrlKey || event.altKey)) return;
    const typing = event.target && (event.target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName));
    if (!event.altKey && (event.metaKey || event.ctrlKey) && 'acvx'.includes(event.key.toLowerCase()) && !event.shiftKey) return;
    // Undo and redo are the page's own while text is being typed.
    if (typing && 'zy'.includes(event.key.toLowerCase()) && (event.metaKey || event.ctrlKey)) return;
    post({{ t: 'k', key: event.key, ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey, meta: event.metaKey }});
    event.preventDefault();
  }});
  window.addEventListener('focus', () => post({{ t: 'f', d: true }}));
  window.addEventListener('blur', () => post({{ t: 'f', d: false }}));
  window.addEventListener('load', () => post({{ t: 'r' }}));
}})();"#
    )
}

pub(crate) fn theme_json(theme: &PageTheme) -> String {
    json!({
        "variables": theme.variables,
        "class": theme.class,
        "kind": theme.kind,
        "name": theme.name,
    })
    .to_string()
}

// ---- files -----------------------------------------------------------------------------------------------

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(high << 4 | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file a request path names (`/Users/me/ext/media/main.js`, or `/C:/ext/main.js` on Windows).
pub(crate) fn resource_path(url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path);
    let decoded = decoded.split(['?', '#']).next().unwrap_or("");
    if decoded.is_empty() {
        return None;
    }
    let path = if cfg!(windows) { decoded.trim_start_matches('/').replace('/', "\\") } else { decoded.to_owned() };
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// Whether `file` is inside one of `roots`. Both are resolved first, so `..` and links do not lead out.
pub(crate) fn inside_roots(file: &Path, roots: &[PathBuf]) -> bool {
    let Ok(file) = file.canonicalize() else { return false };
    roots.iter().any(|root| root.canonicalize().is_ok_and(|root| file.starts_with(root)))
}

pub(crate) fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("txt" | "md" | "log") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

// ---- what pages send ---------------------------------------------------------------------------------------

/// A shortcut the page passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageKey {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PageMessage {
    /// `postMessage` of the page's API.
    Post(Value),
    /// `setState`.
    State(Value),
    Key(PageKey),
    /// The page got or lost the keyboard.
    Focus(bool),
    /// The page finished loading.
    Ready,
    /// A `command:` link was followed.
    Command { command: String, arguments: Vec<Value> },
}

pub(crate) fn parse_page_message(body: &str) -> Option<PageMessage> {
    let value: Value = serde_json::from_str(body).ok()?;
    Some(match value["t"].as_str()? {
        "m" => PageMessage::Post(value["d"].clone()),
        "s" => PageMessage::State(value["d"].clone()),
        "k" => PageMessage::Key(PageKey {
            key: value["key"].as_str()?.to_owned(),
            ctrl: value["ctrl"].as_bool() == Some(true),
            alt: value["alt"].as_bool() == Some(true),
            shift: value["shift"].as_bool() == Some(true),
            meta: value["meta"].as_bool() == Some(true),
        }),
        "f" => PageMessage::Focus(value["d"].as_bool() == Some(true)),
        "r" => PageMessage::Ready,
        _ => return None,
    })
}

/// A `command:id?args` link: the command and its arguments (a JSON list, or one value).
pub(crate) fn parse_command_link(url: &str) -> Option<PageMessage> {
    let rest = url.strip_prefix("command:")?;
    let (command, query) = rest.split_once('?').map_or((rest, ""), |(c, q)| (c, q));
    let command = percent_decode(command);
    if command.is_empty() {
        return None;
    }
    let arguments = match serde_json::from_str::<Value>(&percent_decode(query)) {
        Ok(Value::Array(list)) => list,
        Ok(other) => vec![other],
        Err(_) => Vec::new(),
    };
    Some(PageMessage::Command { command, arguments })
}

/// The keystroke the editor knows a page's key press by.
pub(crate) fn keystroke_of(key: &PageKey) -> Option<Keystroke> {
    let name = match key.key.as_str() {
        "ArrowUp" => "up".to_owned(),
        "ArrowDown" => "down".to_owned(),
        "ArrowLeft" => "left".to_owned(),
        "ArrowRight" => "right".to_owned(),
        "Enter" => "enter".to_owned(),
        "Escape" => "escape".to_owned(),
        "Tab" => "tab".to_owned(),
        "Backspace" => "backspace".to_owned(),
        "Delete" => "delete".to_owned(),
        "Home" => "home".to_owned(),
        "End" => "end".to_owned(),
        "PageUp" => "pageup".to_owned(),
        "PageDown" => "pagedown".to_owned(),
        " " => "space".to_owned(),
        // Modifier keys alone are no shortcut.
        "Control" | "Meta" | "Alt" | "Shift" | "CapsLock" | "Dead" | "Unidentified" => return None,
        other if other.len() >= 2 && other.starts_with('F') && other[1..].chars().all(|c| c.is_ascii_digit()) => other.to_lowercase(),
        other if other.chars().count() == 1 => other.to_lowercase(),
        _ => return None,
    };
    Some(Keystroke {
        modifiers: Modifiers { control: key.ctrl, alt: key.alt, shift: key.shift, platform: key.meta, function: false },
        key: name,
        key_char: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_requests_are_decoded_and_kept_inside_the_roots() {
        let root = std::env::temp_dir().join(format!("rustcode-page-{}", std::process::id()));
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("media/a b.js"), "x").unwrap();
        let outside = std::env::temp_dir().join(format!("rustcode-page-outside-{}.txt", std::process::id()));
        std::fs::write(&outside, "secret").unwrap();
        let roots = [root.clone()];

        let url_path = format!("{}/media/a%20b.js", root.display());
        let file = resource_path(&url_path).unwrap();
        assert!(file.ends_with("media/a b.js"));
        assert!(inside_roots(&file, &roots));
        // A path that climbs out of the folder, a file elsewhere, and a file that is not there.
        let climbing = resource_path(&format!("{}/media/../../{}", root.display(), outside.file_name().unwrap().to_string_lossy())).unwrap();
        assert!(!inside_roots(&climbing, &roots));
        assert!(!inside_roots(&outside, &roots));
        assert!(!inside_roots(&root.join("media/none.js"), &roots));
        assert_eq!(resource_path("relative/file.js"), None);
        assert_eq!(resource_path(""), None);
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&outside).ok();
    }

    #[test]
    fn the_site_of_an_address_is_its_scheme_host_and_port() {
        assert_eq!(origin_of("http://127.0.0.1:9100/?uri=ws%3A%2F%2Fx").as_deref(), Some("http://127.0.0.1:9100"));
        assert_eq!(origin_of("https://example.com").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("about:blank"), None);
    }

    #[test]
    fn kinds_of_files_get_their_types() {
        assert_eq!(mime_type(Path::new("a/main.JS")), "text/javascript; charset=utf-8");
        assert_eq!(mime_type(Path::new("x.woff2")), "font/woff2");
        assert_eq!(mime_type(Path::new("x.unknown")), "application/octet-stream");
    }

    #[test]
    fn the_theme_gives_pages_their_colors_and_fonts() {
        let mut theme = ColorTheme::dark_plus();
        theme.set_color("button.background", Color::rgb(0x112233));
        theme.set_color("textLink.foreground", Color { r: 1, g: 2, b: 3, a: 128 });
        let page = page_theme(&theme, "Inter", "Menlo", 12.0);
        let find = |name: &str| page.variables.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        assert_eq!(find("--vscode-button-background").as_deref(), Some("#112233"));
        assert_eq!(find("--vscode-textLink-foreground").as_deref(), Some("rgba(1, 2, 3, 0.502)"));
        assert!(find("--vscode-editor-background").is_some());
        assert_eq!(find("--vscode-editor-font-size").as_deref(), Some("12px"));
        assert!(find("--vscode-font-family").unwrap().starts_with("Inter,"));
        assert_eq!((page.class, page.kind), ("vscode-dark", "dark"));
        let light = page_theme(&ColorTheme::light_plus(), "Inter", "Menlo", 12.0);
        assert_eq!(light.class, "vscode-light");
        assert_eq!(variable_name("editor.findMatch"), "--vscode-editor-findMatch");
    }

    #[test]
    fn the_start_script_holds_the_state_and_the_theme() {
        let page = page_theme(&ColorTheme::dark_plus(), "Inter", "Menlo", 12.0);
        let script = init_script(&page, Some(&json!({"count": 3})));
        assert!(script.contains("let state = {\"count\":3};"));
        assert!(script.contains("--vscode-editor-background"));
        assert!(script.contains("acquireVsCodeApi"));
        assert!(init_script(&page, None).contains("let state = null;"));
    }

    #[test]
    fn what_pages_send_is_read() {
        assert_eq!(parse_page_message(r#"{"t":"m","d":{"a":[1,2]}}"#), Some(PageMessage::Post(json!({"a": [1, 2]}))));
        assert_eq!(parse_page_message(r#"{"t":"s","d":"x"}"#), Some(PageMessage::State(json!("x"))));
        assert_eq!(parse_page_message(r#"{"t":"f","d":true}"#), Some(PageMessage::Focus(true)));
        assert_eq!(parse_page_message(r#"{"t":"r"}"#), Some(PageMessage::Ready));
        assert_eq!(parse_page_message("not json"), None);
        assert_eq!(parse_page_message(r#"{"t":"zz"}"#), None);
        let key = parse_page_message(r#"{"t":"k","key":"P","ctrl":false,"alt":false,"shift":true,"meta":true}"#);
        let Some(PageMessage::Key(key)) = key else { panic!("{key:?}") };
        let stroke = keystroke_of(&key).unwrap();
        assert_eq!(stroke.key, "p");
        assert!(stroke.modifiers.platform && stroke.modifiers.shift && !stroke.modifiers.control);
        assert_eq!(keystroke_of(&PageKey { key: "ArrowLeft".into(), ctrl: false, alt: true, shift: false, meta: false }).unwrap().key, "left");
        assert_eq!(keystroke_of(&PageKey { key: "Meta".into(), ctrl: false, alt: false, shift: false, meta: true }), None);
        assert_eq!(keystroke_of(&PageKey { key: "F12".into(), ctrl: false, alt: false, shift: false, meta: false }).unwrap().key, "f12");
    }

    #[test]
    fn command_links_carry_their_arguments() {
        assert_eq!(
            parse_command_link("command:ext.open?%5B%22a%22%2C2%5D"),
            Some(PageMessage::Command { command: "ext.open".into(), arguments: vec![json!("a"), json!(2)] })
        );
        assert_eq!(
            parse_command_link("command:ext.one?%7B%22k%22%3A1%7D"),
            Some(PageMessage::Command { command: "ext.one".into(), arguments: vec![json!({"k": 1})] })
        );
        assert_eq!(parse_command_link("command:ext.plain"), Some(PageMessage::Command { command: "ext.plain".into(), arguments: vec![] }));
        assert_eq!(parse_command_link("https://example.com"), None);
        assert_eq!(parse_command_link("command:"), None);
    }
}
