//! Composition root: wires infrastructure adapters into use cases and starts the UI.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, Focusable, TitlebarOptions, WindowBounds,
    WindowOptions,
};
use rustcode_application::{LoadTheme, OpenDocument, SampleDocument, SaveDocument};
use rustcode_domain::{ColorTheme, Document, DocumentId};
mod assets;

use rustcode_application::{IconTheme, NullSessionStore};
use rustcode_infrastructure::{
    load_icon_theme, parse_icon_theme, CargoCheck, Git2Provider, GithubUpdates, JsonSessionStore,
    LocalTelemetry, NotebookKernels, NotifyWatcher, PtyShell, RipgrepSearcher, StdFileSystem,
    TextMateHighlighter, VsCodeThemeLoader,
};
use rustcode_presentation::{EditorServices, EditorView};

/// Where the window's size and position are kept between runs.
fn window_state_file() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("rustcode").join("window.txt"))
}

fn encode_window(bounds: &WindowBounds) -> String {
    let (kind, b) = match bounds {
        WindowBounds::Windowed(b) => ("windowed", b),
        WindowBounds::Maximized(b) => ("maximized", b),
        WindowBounds::Fullscreen(b) => ("fullscreen", b),
    };
    format!(
        "{kind} {} {} {} {}",
        f32::from(b.origin.x),
        f32::from(b.origin.y),
        f32::from(b.size.width),
        f32::from(b.size.height)
    )
}

fn decode_window(text: &str) -> Option<WindowBounds> {
    let mut parts = text.split_whitespace();
    let kind = parts.next()?;
    let n: Vec<f32> = parts.map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let [x, y, w, h] = n[..] else { return None };
    if w < 200.0 || h < 150.0 || !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let bounds = Bounds {
        origin: gpui::point(px(x), px(y)),
        size: size(px(w), px(h)),
    };
    match kind {
        "windowed" => Some(WindowBounds::Windowed(bounds)),
        "maximized" => Some(WindowBounds::Maximized(bounds)),
        "fullscreen" => Some(WindowBounds::Fullscreen(bounds)),
        _ => None,
    }
}

/// The last window state, or maximized on the first run so the editor gets the whole screen.
fn initial_window_bounds(cx: &App) -> WindowBounds {
    let saved = window_state_file()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .and_then(|text| decode_window(&text));
    saved.unwrap_or_else(|| {
        WindowBounds::Maximized(Bounds::centered(None, size(px(1100.), px(750.)), cx))
    })
}

const SAMPLE_LINES: usize = 100_000;

/// Command line: `rustcode [FILE|FOLDER] [--theme THEME.json] [--chrome windows|linux|macos]`.
/// `--install-language-servers [all|NAME...]` installs language servers and exits.
/// `--extensions DIR` adds a folder of VS Code extensions (default: `~/.rustcode/extensions`);
/// their commands show in the command palette and need Node.js. `--chrome` previews another platform's menu and title bar layout. `--icon-theme FILE` loads a
/// VS Code icon theme instead of the bundled one. `--font NAME` sets the interface font and
/// `--code-font NAME` the font for code and the terminal (a monospace one, so columns line up).
struct Args {
    file: Option<PathBuf>,
    theme: Option<PathBuf>,
    icon_theme: Option<PathBuf>,
    font: Option<String>,
    code_font: Option<String>,
    chrome: Option<String>,
    /// Folders of extensions, from `--extensions DIR` (repeatable).
    extensions: Vec<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = Args {
        file: None,
        theme: None,
        icon_theme: None,
        font: None,
        code_font: None,
        chrome: None,
        extensions: Vec::new(),
    };
    let mut iter = std::env::args_os().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--theme" {
            args.theme = iter.next().map(PathBuf::from);
            if args.theme.is_none() {
                eprintln!("--theme needs a path");
                std::process::exit(2);
            }
        } else if arg == "--icon-theme" {
            args.icon_theme = iter.next().map(PathBuf::from);
            if args.icon_theme.is_none() {
                eprintln!("--icon-theme needs a path");
                std::process::exit(2);
            }
        } else if arg == "--font" || arg == "--code-font" {
            let Some(name) = iter.next().map(|v| v.to_string_lossy().into_owned()) else {
                eprintln!("{} needs a font family name", arg.to_string_lossy());
                std::process::exit(2);
            };
            if arg == "--font" {
                args.font = Some(name);
            } else {
                args.code_font = Some(name);
            }
        } else if arg == "--extensions" {
            match iter.next() {
                Some(dir) => args.extensions.push(PathBuf::from(dir)),
                None => {
                    eprintln!("--extensions needs a folder");
                    std::process::exit(2);
                }
            }
        } else if arg == "--chrome" {
            args.chrome = iter.next().map(|v| v.to_string_lossy().into_owned());
            if !matches!(args.chrome.as_deref(), Some("windows" | "linux" | "macos")) {
                eprintln!("--chrome needs windows, linux or macos");
                std::process::exit(2);
            }
        } else {
            args.file = Some(PathBuf::from(arg));
        }
    }
    args
}

/// The icon theme from `--icon-theme`, or the one built into the program.
fn load_icons(path: Option<&PathBuf>) -> IconTheme {
    match path {
        Some(path) => load_icon_theme(path).unwrap_or_else(|err| {
            eprintln!("{err}");
            std::process::exit(1);
        }),
        None => parse_icon_theme(assets::ICON_THEME, &|icon_path| {
            // The bundled icons are served by `assets::Assets` under `icons/<file>`.
            let name = icon_path.rsplit('/').next().unwrap_or(icon_path);
            format!("icons/{name}")
        })
        .expect("the bundled icon theme is valid"),
    }
}

/// Show the application icon in the dock. A bundled app gets its icon from the bundle; this covers
/// running the plain binary.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use cocoa::appkit::{NSApp, NSApplication, NSImage};
    use cocoa::base::nil;
    use cocoa::foundation::NSData;

    let png = assets::APP_ICON_PNG;
    // SAFETY: plain AppKit calls on the main thread, after the application exists. The data is
    // copied by `dataWithBytes`, so the static slice need not outlive the call.
    unsafe {
        let data = NSData::dataWithBytes_length_(nil, png.as_ptr().cast(), png.len() as u64);
        let image = NSImage::initWithData_(NSImage::alloc(nil), data);
        if image != nil {
            NSApp().setApplicationIconImage_(image);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon() {}

/// The themes the program starts with: the one asked for, then the ones that come with it.
fn bundled_themes(first: ColorTheme) -> Vec<ColorTheme> {
    let mut themes = vec![first, ColorTheme::light_plus()];
    for text in [include_str!("../assets/themes/one_dark.json"), include_str!("../assets/themes/astro.json")] {
        match VsCodeThemeLoader.parse_text(text) {
            Ok(theme) => themes.push(theme),
            Err(err) => eprintln!("bundled theme: {err}"),
        }
    }
    themes
}

fn load_theme(path: Option<&PathBuf>) -> ColorTheme {
    let Some(path) = path else {
        return ColorTheme::dark_plus();
    };
    LoadTheme::new(Arc::new(VsCodeThemeLoader))
        .execute(path)
        .unwrap_or_else(|err| {
            eprintln!("{err}");
            std::process::exit(1);
        })
}

/// Files that mark the top of a project.
const PROJECT_MARKERS: [&str; 6] = [
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    ".hg",
];

/// The nearest folder at or above `start` that looks like the top of a project, or `start`.
fn project_root(start: &std::path::Path) -> PathBuf {
    start
        .ancestors()
        .find(|dir| {
            PROJECT_MARKERS
                .iter()
                .any(|marker| dir.join(marker).exists())
        })
        .unwrap_or(start)
        .to_path_buf()
}

/// Folder shown in the explorer: the given folder, the project around the given file, or the
/// working directory.
fn workspace_root(target: Option<&PathBuf>) -> PathBuf {
    let cwd = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match target {
        Some(path) if path.is_dir() => path.canonicalize().unwrap_or_else(|_| path.clone()),
        Some(path) => path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(project_root))
            .unwrap_or_else(cwd),
        None => cwd(),
    }
}

/// User settings and key binding files: VS Code's own, then RustCode's, so RustCode's win.
fn config_files() -> rustcode_presentation::ConfigFiles {
    let vscode = dirs::config_dir().map(|dir| dir.join("Code").join("User"));
    let own = dirs::home_dir().map(|dir| dir.join(".rustcode"));
    let find = |name: &str| -> Vec<PathBuf> {
        [vscode.as_ref(), own.as_ref()]
            .into_iter()
            .flatten()
            .map(|d| d.join(name))
            .collect()
    };
    rustcode_presentation::ConfigFiles {
        settings: find("settings.json"),
        keybindings: find("keybindings.json"),
    }
}

/// The first font of `editor.fontFamily` that this machine has.
fn settings_code_font(
    files: &rustcode_presentation::ConfigFiles,
    root: &std::path::Path,
    installed: &[String],
) -> Option<String> {
    let mut settings = rustcode_application::Settings::default();
    let workspace = root.join(".vscode/settings.json");
    for path in files.settings.iter().chain(std::iter::once(&workspace)) {
        if let Some(found) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| rustcode_application::Settings::parse(&t).ok())
        {
            settings.overlay(&found);
        }
    }
    settings
        .editor()
        .font_families
        .into_iter()
        .find_map(|name| {
            installed
                .iter()
                .find(|f| f.eq_ignore_ascii_case(&name))
                .cloned()
        })
}

fn load_document(fs: &Arc<StdFileSystem>, target: Option<PathBuf>) -> Document {
    let id = DocumentId(1);
    match target {
        Some(path) if path.is_dir() => {
            Document::new(id, "Untitled-1", rustcode_domain::TextBuffer::default())
        }
        Some(path) => OpenDocument::new(fs.clone())
            .execute(id, &path)
            .unwrap_or_else(|err| {
                eprintln!("{err}");
                std::process::exit(1);
            }),
        None => SampleDocument::execute(id, SAMPLE_LINES),
    }
}

/// `rustcode --install-language-servers [all|NAME...]`: download and install language servers
/// (into `~/.rustcode/servers`, or `RUSTCODE_SERVERS_DIR`), then exit. For installers and scripts;
/// the program also installs a server by itself the first time a file needs it. Exits with the
/// number of servers that could not be installed (at most 100).
fn install_servers_and_exit() {
    let names: Vec<String> = std::env::args()
        .skip_while(|a| a != "--install-language-servers")
        .skip(1)
        .collect();
    let failed =
        rustcode_infrastructure::install_language_servers(&names, &mut |line| println!("{line}"));
    std::process::exit(failed.min(100) as i32);
}


fn main() {
    if std::env::args().any(|a| a == "--install-language-servers") {
        install_servers_and_exit();
    }
    // A newer program brings the servers it installed up to date, in the background.
    rustcode_infrastructure::refresh_installed_servers(env!("CARGO_PKG_VERSION"));
    let fs = Arc::new(StdFileSystem);
    let args = parse_args();
    let chrome = args.chrome.clone();
    // A folder with no file named: nothing is open, so the empty state shows.
    let show_empty = args.file.as_ref().is_some_and(|path| path.is_dir());
    let (ui_font, code_font) = (
        args.font
            .clone()
            .unwrap_or_else(|| assets::UI_FONT.to_owned()),
        args.code_font
            .clone()
            .unwrap_or_else(|| assets::CODE_FONT.to_owned()),
    );
    let theme = load_theme(args.theme.as_ref());
    let root = workspace_root(args.file.as_ref());
    let session: Arc<dyn rustcode_application::SessionStore> =
        match JsonSessionStore::in_user_data_dir() {
            Some(store) => Arc::new(store),
            None => Arc::new(NullSessionStore),
        };
    // With no file named, reopen what was open last time here.
    let restore_session = args.file.is_none() && session.load(&root).is_some();
    let document = if restore_session {
        Document::new(
            DocumentId(1),
            "Untitled-1",
            rustcode_domain::TextBuffer::default(),
        )
    } else {
        load_document(&fs, args.file)
    };
    let extension_dirs: Vec<PathBuf> = args
        .extensions
        .iter()
        .cloned()
        .chain(rustcode_infrastructure::NodeExtensionHost::default_dirs())
        .collect();
    let extension_host: Arc<dyn rustcode_application::ExtensionHost> = Arc::new(
        rustcode_infrastructure::NodeExtensionHost::new(&extension_dirs),
    );
    let config = config_files();
    let settings_root = root.clone();
    let highlighter = Arc::new(TextMateHighlighter::new().unwrap_or_else(|err| {
        eprintln!("TextMate engine unavailable: {err}");
        std::process::exit(1);
    }));
    let services = EditorServices {
        save_document: Arc::new(SaveDocument::new(fs.clone())),
        open_document: Arc::new(OpenDocument::new(fs.clone())),
        highlighter: highlighter.clone(),
        file_reader: fs.clone(),
        file_ops: fs.clone(),
        file_writer: fs,
        watcher: Arc::new(NotifyWatcher),
        session,
        searcher: Arc::new(RipgrepSearcher),
        git: Arc::new(Git2Provider),
        pty: Arc::new(PtyShell),
        diagnostics: Arc::new(CargoCheck),
        formatters: Arc::new(rustcode_infrastructure::ProgramFormatRunner::new()),
        linters: Arc::new(rustcode_infrastructure::ProgramLintRunner::new()),
        file_lister: Arc::new(rustcode_infrastructure::WalkFileLister),
        config_files: config.clone(),
        extension_host: extension_host.clone(),
        // Extensions answer language questions first; the language servers answer the rest.
        language_servers: Arc::new(rustcode_application::ExtensionLanguageServers::new(
            Arc::new(rustcode_infrastructure::LspHost::default()),
            extension_host,
        )),
        debugger: Arc::new(rustcode_infrastructure::DapHost::default()),
        icon_theme: load_icons(args.icon_theme.as_ref()),
        restore_session,
        workspace_root: root,
    };
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            // The bundled fonts first, so the names below resolve even where Inter is not installed.
            let fonts = assets::FONTS
                .iter()
                .map(|bytes| std::borrow::Cow::Borrowed(*bytes))
                .collect();
            if let Err(err) = cx.text_system().add_fonts(fonts) {
                eprintln!("cannot load the bundled fonts: {err}");
            }
            // `editor.fontFamily` picks the code font unless `--code-font` was given.
            let code_font = if args.code_font.is_some() {
                code_font.clone()
            } else {
                let installed = cx.text_system().all_font_names();
                settings_code_font(&config, &settings_root, &installed)
                    .unwrap_or_else(|| code_font.clone())
            };
            rustcode_presentation::configure_fonts(&ui_font, &code_font);
            rustcode_presentation::init(cx);
            set_dock_icon();
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = initial_window_bounds(cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(bounds),
                        // Windows has no native title bar here: the view draws its own with the menus.
                        titlebar: Some(TitlebarOptions {
                            title: Some("RustCode".into()),
                            // On macOS too: the view's bar then sits beside the traffic lights, with the project chip.
                            appears_transparent: cfg!(any(
                                target_os = "windows",
                                target_os = "macos"
                            )) || chrome.as_deref() == Some("windows"),
                            traffic_light_position: Some(gpui::point(px(12.), px(12.))),
                            ..Default::default()
                        }),
                        // Lets Linux desktops match the window to its launcher and icon.
                        app_id: Some("rustcode".to_owned()),
                        ..Default::default()
                    },
                    |window, cx| {
                        let view = cx.new(|cx| {
                            EditorView::new(
                                document,
                                services,
                                bundled_themes(theme),
                                cx,
                            )
                        });
                        if let Some(chrome) = chrome.as_deref() {
                            view.update(cx, |view, _| match chrome {
                                "windows" => view.set_window_chrome(true, true),
                                "linux" => view.set_window_chrome(true, false),
                                _ => view.set_window_chrome(false, false),
                            });
                        }
                        view.update(cx, |view, _| {
                            view.set_notebook_kernels(Arc::new(NotebookKernels))
                        });
                        view.update(cx, |view, cx| {
                            let version = env!("CARGO_PKG_VERSION");
                            view.set_updates(
                                Arc::new(GithubUpdates::new(version)),
                                Arc::new(LocalTelemetry::new(version)),
                                version,
                                cx,
                            )
                        });
                        if show_empty {
                            view.update(cx, |view, _| view.show_empty_state());
                        }
                        window.focus(&view.focus_handle(cx));
                        view
                    },
                )
                .expect("failed to open window");
            // Keep the window's size and position, so the next run opens the same way.
            cx.spawn(async move |cx| {
                let mut last = String::new();
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let Ok(now) =
                        window.update(cx, |_, window, _| encode_window(&window.window_bounds()))
                    else {
                        break;
                    };
                    if now != last {
                        if let Some(file) = window_state_file() {
                            let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file));
                            let _ = std::fs::write(file, &now);
                        }
                        last = now;
                    }
                }
            })
            .detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_state_round_trips() {
        let text = "maximized 10 20 1100 750";
        assert_eq!(encode_window(&decode_window(text).unwrap()), text);
        assert!(decode_window("windowed 0 0 10 10").is_none());
        assert!(decode_window("garbage").is_none());
    }

    #[test]
    fn a_file_opens_the_project_around_it() {
        let root = std::env::temp_dir().join(format!("rustcode-root-{}", std::process::id()));
        std::fs::create_dir_all(root.join("crates/a/src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        assert_eq!(project_root(&root.join("crates/a/src")), root);
        // A nearer marker wins.
        std::fs::write(root.join("crates/a/Cargo.toml"), "").unwrap();
        assert_eq!(
            project_root(&root.join("crates/a/src")),
            root.join("crates/a")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn without_a_marker_the_folder_itself_is_used() {
        let dir = std::env::temp_dir().join(format!("rustcode-bare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The temp directory is not inside a project on a normal machine.
        if !dir
            .ancestors()
            .any(|d| PROJECT_MARKERS.iter().any(|m| d.join(m).exists()))
        {
            assert_eq!(project_root(&dir), dir);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
