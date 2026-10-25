//! Flutter and Dart: run the app on a device in a terminal, hot reload it, and the usual
//! `flutter` and `dart` commands. The app runs in a real terminal tab (so its output, logs and
//! keys behave as in any terminal); hot reload is the `r` key that `flutter run` listens for.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent};
use rustcode_application::{AdapterKind, DebugCommand, LaunchConfig};

use crate::debug_view::shell_quote;
use crate::editor_view::EditorView;
use crate::git_extra::{GitPickAction, PickItem};
use crate::debug_view::DebugPhase;
use crate::text_layout::UI_FONT_SIZE;

/// What the program knows about Flutter now.
#[derive(Default)]
pub(crate) struct FlutterState {
    /// The device to run on (`flutter run -d`); none lets Flutter choose.
    pub device: Option<(String, String)>,
    /// The terminal tab that runs the app, by its number.
    pub session: Option<usize>,
    /// Devices are being listed.
    pub listing: bool,
    /// What `flutter emulators` printed and when: the list rarely changes, and each run starts a
    /// whole Dart tool, so it is reused for a while.
    pub(crate) emulators_seen: Option<(std::time::Instant, String)>,
    /// The app was started under the debugger, so hot reload goes through it.
    pub debugging: bool,
    /// Addresses `flutter run` printed: the DevTools of the app, and its Dart VM service.
    app_devtools: Option<String>,
    vm_service: Option<String>,
    /// A terminal tab that serves DevTools on its own (`dart devtools`), and the address it printed.
    pub devtools_session: Option<usize>,
    devtools_server: Option<String>,
    /// DevTools were asked for and wait for the server to say where it is.
    open_when_ready: bool,
    /// What the two terminals printed, not yet read: the tab and the text.
    pub output: Vec<(usize, String)>,
    /// The package of a folder (the nearest folder with a `pubspec.yaml`) and whether it is a
    /// Flutter one, found once per folder.
    packages: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<(PathBuf, bool)>>>,
}

/// The addresses a tool prints about itself: DevTools and the Dart VM service.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolUrls {
    pub devtools: Option<String>,
    pub vm_service: Option<String>,
}

/// Read the addresses `flutter run` and `dart devtools` print, from their output.
pub(crate) fn tool_urls(output: &str) -> ToolUrls {
    let mut found = ToolUrls::default();
    for line in rustcode_application::notebook::strip_ansi(output).lines() {
        let lower = line.to_lowercase();
        let Some(start) = line.find("http://").or_else(|| line.find("https://")) else { continue };
        let url = line[start..].split_whitespace().next().unwrap_or_default().trim_end_matches(['.', ',', ')']);
        // These tools serve on this computer. A page printed by a project's own program must not
        // send the user to another site, where it would run scripts.
        let rest = url.split("://").nth(1).unwrap_or_default();
        let local = ["127.0.0.1", "localhost", "[::1]"].iter().any(|host| rest.strip_prefix(host).is_some_and(|after| after.is_empty() || after.starts_with([':', '/', '?', '#'])));
        if url.is_empty() || !local {
            continue;
        }
        if lower.contains("devtools") {
            found.devtools = Some(url.to_owned());
        } else if lower.contains("vm service") || lower.contains("dart vm") {
            found.vm_service = Some(url.to_owned());
        }
    }
    found
}

/// `text` as a value in a query string.
fn query_value(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Height of the bar with the run buttons.
const BAR_HEIGHT: f32 = 28.0;
/// Numbers of the built-in lenses above `main`, past the ones a language server gives.
pub(crate) const RUN_LENS: usize = 1_000_000;
pub(crate) const DEBUG_LENS: usize = 1_000_001;

/// A device `flutter devices` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlutterDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
}

/// The devices in the output of `flutter devices --machine`. Shell start-up noise before the
/// JSON is skipped.
pub(crate) fn parse_devices(output: &str) -> Vec<FlutterDevice> {
    let Some(start) = output.find("[\n").or_else(|| output.find('[')) else { return Vec::new() };
    let Some(end) = output.rfind(']') else { return Vec::new() };
    if end < start {
        return Vec::new();
    }
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(&output[start..=end]) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|device| {
            let text = |key: &str| device.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
            Some(FlutterDevice { id: text("id")?, name: text("name").unwrap_or_default(), platform: text("targetPlatform").unwrap_or_default() })
        })
        .collect()
}

/// An emulator `flutter emulators` lists: id, name, and platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlutterEmulator {
    pub id: String,
    pub name: String,
    pub platform: String,
}

/// The emulators in the text `flutter emulators` prints: lines of `id • name • maker • platform`.
pub(crate) fn parse_emulators(output: &str) -> Vec<FlutterEmulator> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('•').map(str::trim).collect();
            (parts.len() >= 4 && !parts[0].is_empty() && !parts[0].contains(' ')).then(|| FlutterEmulator {
                id: parts[0].to_owned(),
                name: parts[1].to_owned(),
                platform: parts[parts.len() - 1].to_owned(),
            })
        })
        .collect()
}

/// Platforms this computer can run Flutter apps on that the project at `folder` has no folder for.
fn platforms_to_enable(folder: &Path) -> Vec<&'static str> {
    let host: &[&str] = if cfg!(target_os = "macos") {
        &["web", "macos"]
    } else if cfg!(windows) {
        &["web", "windows"]
    } else {
        &["web", "linux"]
    };
    host.iter().copied().filter(|p| !folder.join(p).is_dir()).collect()
}

/// Whether `pubspec` (the text of a `pubspec.yaml`) is that of a Flutter app or package.
pub(crate) fn is_flutter_pubspec(pubspec: &str) -> bool {
    pubspec.lines().any(|line| {
        let line = line.trim();
        line == "sdk: flutter" || line.starts_with("flutter:") || line.starts_with("sdk: flutter ")
    })
}

/// The nearest folder at or above `start` with a `pubspec.yaml`.
pub(crate) fn pubspec_folder(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { Some(start) } else { start.parent() };
    while let Some(folder) = dir {
        if folder.join("pubspec.yaml").is_file() {
            return Some(folder.to_path_buf());
        }
        dir = folder.parent();
    }
    None
}

/// Whether `line` declares the `main` function of a Dart program.
pub(crate) fn declares_main(line: &str) -> bool {
    let line = line.trim_start();
    ["void main(", "Future<void> main(", "Future main(", "main("].iter().any(|start| line.starts_with(start))
}

impl EditorView {
    /// The package a Dart file belongs to, and whether it is a Flutter one.
    pub(crate) fn dart_context(&self, path: &Path) -> Option<(PathBuf, bool)> {
        let dir = path.parent()?.to_path_buf();
        if let Some(found) = self.flutter.packages.borrow().get(&dir) {
            return found.clone();
        }
        let found = pubspec_folder(&dir).map(|folder| {
            let flutter = self.package_is_flutter(&folder);
            (folder, flutter)
        });
        self.flutter.packages.borrow_mut().insert(dir, found.clone());
        found
    }

    /// The package of the active file, or of the workspace.
    pub(crate) fn dart_package(&self) -> Option<PathBuf> {
        let from_file = self.document.path.as_deref().and_then(pubspec_folder);
        from_file.or_else(|| self.shell.explorers.first().and_then(|tree| pubspec_folder(tree.root())))
    }

    fn package_is_flutter(&self, folder: &Path) -> bool {
        std::fs::read_to_string(folder.join("pubspec.yaml")).is_ok_and(|text| is_flutter_pubspec(&text))
    }

    /// Run `command` in a new terminal tab, in `folder`. The tab's number is returned.
    pub(crate) fn run_in_new_terminal(&mut self, folder: &Path, command: &str, cx: &mut Context<Self>) -> Option<usize> {
        self.new_terminal(cx);
        let serial = self.last_terminal_serial();
        self.terminal_text(&format!("cd {} && {command}\n", shell_quote(&folder.to_string_lossy())), cx);
        serial
    }

    /// Start the app under the debugger, on the chosen device.
    pub(crate) fn flutter_debug(&mut self, cx: &mut Context<Self>) {
        if self.debug.phase != DebugPhase::Idle {
            self.set_status("a debug session is already running");
            cx.notify();
            return;
        }
        let Some(folder) = self.dart_package() else {
            self.set_status("no pubspec.yaml found: open a Dart or Flutter project");
            cx.notify();
            return;
        };
        let flutter = self.package_is_flutter(&folder);
        // The open file when it is Dart, else the package's own `main`.
        let program = self
            .document
            .path
            .clone()
            .filter(|p| p.extension().is_some_and(|e| e == "dart"))
            .unwrap_or_else(|| folder.join("lib").join("main.dart"));
        let kind = if flutter { AdapterKind::Flutter } else { AdapterKind::Dart };
        let name = format!("{}: {}", if flutter { "Flutter" } else { "Dart" }, program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let mut config = LaunchConfig::new(name, kind, program.to_string_lossy(), folder);
        if let (true, Some((id, _))) = (flutter, self.flutter.device.as_ref()) {
            config.tool_args = vec!["-d".to_owned(), id.clone()];
        }
        self.save_all(cx);
        self.debug.phase = DebugPhase::Starting;
        self.debug.console.clear();
        self.show_panel_tab(crate::shell::PanelTab::Debug, cx);
        self.flutter.debugging = true;
        self.begin_session(config, cx);
    }

    /// Run the app (or the Dart program) of the open file.
    pub(crate) fn run_app(&mut self, cx: &mut Context<Self>) {
        let flutter = self.dart_package().is_some_and(|folder| self.package_is_flutter(&folder));
        if flutter {
            self.flutter_run(cx);
        } else {
            self.dart_run_file(cx);
        }
    }

    /// Whether an app started from here runs now, in a terminal or under the debugger.
    fn app_running(&self) -> bool {
        self.flutter_alive() || (self.flutter.debugging && self.debug.phase != DebugPhase::Idle)
    }

    /// `flutter run` on the chosen device.
    pub(crate) fn flutter_run(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.dart_package() else {
            self.set_status("no pubspec.yaml found: open a Flutter project");
            cx.notify();
            return;
        };
        // A running app is stopped first, so there is one at a time.
        if self.flutter_alive() {
            self.flutter_send(b"q", cx);
        }
        let device = self.flutter.device.as_ref().map(|(id, _)| format!(" -d {}", shell_quote(id))).unwrap_or_default();
        self.flutter.session = self.run_in_new_terminal(&folder, &format!("flutter run{device}"), cx);
        self.set_status("flutter run: r hot reloads, R restarts, q quits");
    }

    /// Take in what the terminals of the app and of DevTools printed: the addresses in it.
    pub(crate) fn flutter_read_output(&mut self, cx: &mut Context<Self>) {
        for (serial, text) in std::mem::take(&mut self.flutter.output) {
            let urls = tool_urls(&text);
            if self.flutter.session == Some(serial) {
                if urls.devtools.is_some() {
                    self.flutter.app_devtools = urls.devtools;
                }
                if urls.vm_service.is_some() {
                    self.flutter.vm_service = urls.vm_service;
                }
            } else if self.flutter.devtools_session == Some(serial) && urls.devtools.is_some() {
                self.flutter.devtools_server = urls.devtools;
            }
        }
        if self.flutter.open_when_ready && self.flutter.devtools_server.is_some() {
            self.flutter.open_when_ready = false;
            self.flutter_show_devtools(cx);
        }
    }

    /// Open DevTools in a tab: the address the running app printed, or a `dart devtools` server of
    /// its own (started in a terminal tab) connected to the app.
    pub(crate) fn flutter_open_devtools(&mut self, cx: &mut Context<Self>) {
        if self.flutter.app_devtools.is_some() && self.flutter_alive() || self.flutter.devtools_server.is_some() && self.devtools_server_alive() {
            self.flutter_show_devtools(cx);
            return;
        }
        let folder = self.dart_package().or_else(|| self.shell.explorers.first().map(|t| t.root().to_path_buf())).unwrap_or_default();
        self.flutter.devtools_server = None;
        self.flutter.open_when_ready = true;
        self.flutter.devtools_session = self.run_in_new_terminal(&folder, "dart devtools --no-launch-browser", cx);
        self.name_last_terminal("DevTools");
        self.set_status("starting DevTools...");
        cx.notify();
    }

    fn devtools_server_alive(&self) -> bool {
        self.flutter.devtools_session.is_some_and(|serial| self.terminal_alive(serial))
    }

    fn flutter_show_devtools(&mut self, cx: &mut Context<Self>) {
        let url = match (&self.flutter.app_devtools, self.flutter_alive()) {
            (Some(url), true) => url.clone(),
            _ => {
                let Some(server) = self.flutter.devtools_server.clone() else { return };
                match self.flutter.vm_service.as_ref().filter(|_| self.flutter_alive()) {
                    Some(vm) => format!("{}/?uri={}", server.trim_end_matches('/'), query_value(vm)),
                    None => server,
                }
            }
        };
        self.open_url_page("Flutter DevTools", url, cx);
    }

    /// Whether the terminal tab that runs the app is still there.
    fn flutter_alive(&self) -> bool {
        self.flutter.session.is_some_and(|serial| self.terminal_alive(serial))
    }

    fn flutter_send(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        match self.flutter.session {
            Some(serial) => self.terminal_write_to(serial, bytes, cx),
            None => false,
        }
    }

    /// Hot reload (`r`) or hot restart (`R`) of the running app.
    pub(crate) fn flutter_hot_reload(&mut self, restart: bool, cx: &mut Context<Self>) {
        // Under the debugger the adapter does it.
        if self.flutter.debugging && self.debug.phase != DebugPhase::Idle {
            self.debugger.command(if restart { DebugCommand::HotRestart } else { DebugCommand::HotReload });
            self.set_status(if restart { "hot restart" } else { "hot reload" });
            cx.notify();
            return;
        }
        let sent = self.flutter_alive() && self.flutter_send(if restart { b"R" } else { b"r" }, cx);
        if !sent {
            self.set_status("no Flutter app is running: use Flutter: Run App");
        } else {
            self.set_status(if restart { "hot restart" } else { "hot reload" });
        }
        cx.notify();
    }

    pub(crate) fn flutter_stop(&mut self, cx: &mut Context<Self>) {
        if self.flutter.debugging && self.debug.phase != DebugPhase::Idle {
            self.stop_debugging(cx);
            return;
        }
        if !(self.flutter_alive() && self.flutter_send(b"q", cx)) {
            self.set_status("no Flutter app is running");
        }
        cx.notify();
    }

    /// Saving a Dart file reloads the running app.
    pub(crate) fn flutter_after_save(&mut self, cx: &mut Context<Self>) {
        if self.document.title.ends_with(".dart") && self.flutter_alive() {
            self.flutter_send(b"r", cx);
        }
    }

    /// A `flutter` or `dart` command in a new terminal tab.
    pub(crate) fn dart_command(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(folder) = self.dart_package() else {
            self.set_status("no pubspec.yaml found: open a Dart or Flutter project");
            cx.notify();
            return;
        };
        // `flutter pub` for Flutter packages, `dart pub` for the rest.
        let command = if self.package_is_flutter(&folder) { command.to_owned() } else { command.replacen("flutter ", "dart ", 1) };
        self.run_in_new_terminal(&folder, &command, cx);
    }

    /// Run the open Dart file with `dart run`.
    pub(crate) fn dart_run_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone().filter(|p| p.extension().is_some_and(|e| e == "dart")) else {
            self.set_status("open a .dart file to run");
            cx.notify();
            return;
        };
        let folder = pubspec_folder(&path).or_else(|| path.parent().map(Path::to_path_buf)).unwrap_or_default();
        self.run_in_new_terminal(&folder, &format!("dart run {}", shell_quote(&path.to_string_lossy())), cx);
    }

    /// `flutter create`: the command is typed for the user to finish with the project's name.
    pub(crate) fn flutter_create(&mut self, cx: &mut Context<Self>) {
        let folder = self.shell.explorers.first().map(|t| t.root().to_path_buf()).unwrap_or_default();
        self.new_terminal(cx);
        self.terminal_text(&format!("cd {} && flutter create ", shell_quote(&folder.to_string_lossy())), cx);
    }

    /// List the devices (connected ones, emulators that can be started, platforms that can be
    /// added to the project) and let the user pick one.
    pub(crate) fn flutter_pick_device(&mut self, cx: &mut Context<Self>) {
        if self.flutter.listing {
            return;
        }
        self.flutter.listing = true;
        self.set_status("listing devices...");
        cx.notify();
        let package = self.dart_package();
        let known_emulators = self.flutter.emulators_seen.as_ref().filter(|(at, _)| at.elapsed() < EMULATORS_KEPT).map(|(_, text)| text.clone());
        let task = cx.background_executor().spawn(async move {
            let devices = run_flutter(&["devices", "--machine", "--device-timeout", DEVICE_WAIT]);
            let (emulators, fresh) = match known_emulators {
                Some(text) => (text, false),
                None => (run_flutter(&["emulators"]).unwrap_or_default(), true),
            };
            (devices, emulators, fresh)
        });
        cx.spawn(async move |this, cx| {
            let (devices, emulators, fresh) = task.await;
            this.update(cx, |this, cx| {
                this.flutter.listing = false;
                if fresh && !emulators.is_empty() {
                    this.flutter.emulators_seen = Some((std::time::Instant::now(), emulators.clone()));
                }
                match devices {
                    Err(why) => this.set_status(why),
                    Ok(text) => {
                        this.status = None;
                        let mut picks: Vec<PickItem> = parse_devices(&text)
                            .into_iter()
                            .map(|d| PickItem {
                                hint: Some(format!("Connected Devices · {}", d.platform)),
                                label: d.name.clone(),
                                action: GitPickAction::FlutterDevice(d.id, d.name),
                            })
                            .collect();
                        for e in parse_emulators(&emulators) {
                            picks.push(PickItem {
                                label: format!("Start {}", e.name),
                                hint: Some(format!("Offline Emulators · {}", e.platform)),
                                action: GitPickAction::FlutterLaunchEmulator(e.id),
                            });
                        }
                        picks.push(PickItem {
                            label: "Create Android emulator".to_owned(),
                            hint: Some("Offline Emulators".to_owned()),
                            action: GitPickAction::FlutterCreateEmulator,
                        });
                        for platform in package.as_deref().map(platforms_to_enable).unwrap_or_default() {
                            picks.push(PickItem {
                                label: format!("Enable {platform} for this project"),
                                hint: Some("Other Available Platforms".to_owned()),
                                action: GitPickAction::FlutterEnablePlatform(platform.to_owned()),
                            });
                        }
                        this.open_git_pick(picks, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Start emulator `id` and list the devices again when it is up.
    pub(crate) fn flutter_launch_emulator(&mut self, id: String, cx: &mut Context<Self>) {
        self.set_status(format!("starting emulator {id}..."));
        let task = cx.background_executor().spawn({
            let id = id.clone();
            async move { run_flutter(&["emulators", "--launch", &id]) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.set_status(format!("emulator {id} started; pick it from the device list once it is up")),
                    Err(why) => this.set_status(why),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The bar above a Dart file: run, debug, the device, and the controls of a running app.
    pub(crate) fn render_flutter_bar(&self, gi: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let group = &self.groups[gi];
        let path = self.doc_path(group.tabs[group.active].doc)?.to_path_buf();
        if path.extension().is_none_or(|e| e != "dart") {
            return None;
        }
        let (_, flutter) = self.dart_context(&path)?;
        let theme = &self.theme;
        let running = self.app_running();
        let hover = theme.lists.hover;
        let button = |id: &'static str, icon: &'static str, label: &'static str, color: gpui::Rgba, on_click: fn(&mut EditorView, &mut Context<EditorView>)| {
            div()
                .id((id, gi))
                .h(px(BAR_HEIGHT - 6.))
                .px(px(8.))
                .gap(px(5.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .cursor_pointer()
                .text_color(theme.text)
                .hover(move |style| style.bg(hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        on_click(this, cx)
                    }),
                )
                .child(crate::icons::icon(icon, 13., color))
                .children((!label.is_empty()).then_some(label))
        };
        let green = gpui::rgb(0x89d185);
        let blue = gpui::rgb(0x75beff);
        let device = self.flutter.device.as_ref().map_or_else(
            || if self.flutter.listing { "Looking for devices...".to_owned() } else { "No device".to_owned() },
            |(_, name)| name.clone(),
        );
        let app = if flutter { "App" } else { "File" };
        let run = button("flutter-run", "debug-start", if flutter { "Run App" } else { "Run" }, green, |this, cx| this.run_app(cx));
        let debug = button("flutter-debug", "bug", if flutter { "Debug App" } else { "Debug" }, blue, |this, cx| this.flutter_debug(cx));
        let _ = app;
        Some(
            div()
                .id(("flutter-bar", gi))
                .h(px(BAR_HEIGHT))
                .flex_none()
                .px(px(8.))
                .gap(px(2.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(UI_FONT_SIZE))
                .child(run)
                .child(debug)
                .children(flutter.then(|| {
                    div().w(px(1.)).h(px(14.)).mx(px(6.)).bg(theme.border)
                }))
                .children(flutter.then(|| {
                    button("flutter-device", "device", "", theme.text, |this, cx| this.flutter_pick_device(cx)).child(device)
                }))
                .children(running.then(|| div().w(px(1.)).h(px(14.)).mx(px(6.)).bg(theme.border)))
                .children(running.then(|| button("flutter-reload", "refresh", "Hot Reload", green, |this, cx| this.flutter_hot_reload(false, cx))))
                .children(running.then(|| button("flutter-restart", "debug-restart", "Hot Restart", green, |this, cx| this.flutter_hot_reload(true, cx))))
                .children(running.then(|| button("flutter-devtools", "graph", "DevTools", blue, |this, cx| this.flutter_open_devtools(cx))))
                .children(running.then(|| button("flutter-stop", "debug-stop", "Stop", theme.markers.error, |this, cx| this.flutter_stop(cx))))
                .into_any_element(),
        )
    }

    /// The device, at the right of the status bar, for a Flutter project.
    pub(crate) fn render_flutter_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let in_flutter = match self.document.path.as_deref().filter(|p| p.extension().is_some_and(|e| e == "dart")) {
            Some(path) => self.dart_context(path).is_some_and(|(_, flutter)| flutter),
            None => self.shell.explorers.first().is_some_and(|tree| self.dart_context(&tree.root().join("pubspec.yaml")).is_some_and(|(_, flutter)| flutter)),
        };
        if !in_flutter {
            return None;
        }
        let hover = self.theme.status_bar.hover;
        let color = self.theme.status_bar.foreground;
        let name = self.flutter.device.as_ref().map_or("No device", |(_, name)| name.as_str()).to_owned();
        Some(
            div()
                .id("status-flutter-device")
                .px(px(8.))
                .gap(px(5.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _w, cx| this.flutter_pick_device(cx)))
                .child(crate::icons::icon("device", 12., color))
                .child(name)
                .into_any_element(),
        )
    }

    pub(crate) fn flutter_select_device(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        self.set_status(format!("flutter device: {name}"));
        self.flutter.device = Some((id, name));
        cx.notify();
    }
}

/// Run `command` in the user's login shell, which knows the `PATH` a terminal has (a desktop
/// program often starts with a short one). The standard output, or why it failed.
/// How long the list of emulators is reused.
const EMULATORS_KEPT: std::time::Duration = std::time::Duration::from_secs(300);
/// A tool that has not answered by now is stopped, so it does not sit in memory for good.
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// Seconds `flutter devices` waits for devices to appear. Its own wait is 10 s, which makes every listing slow.
const DEVICE_WAIT: &str = "3";

/// The `flutter` program: on the PATH or where it is usually put. Looking is much cheaper than
/// starting a login shell, which runs all of the user's start-up files first.
fn flutter_program() -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let usual = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from).into_iter().chain(["flutter/bin", "development/flutter/bin", "fvm/default/bin"].map(|d| home.join(d)));
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    on_path.into_iter().chain(usual).map(|dir| dir.join("flutter")).find(|p| p.is_file())
}

/// Run `flutter` with `args`: directly when it can be found, else through the user's login shell.
fn run_flutter(args: &[&str]) -> Result<String, String> {
    match flutter_program() {
        Some(program) => {
            let mut command = std::process::Command::new(program);
            command.args(args);
            run_tool(command, &format!("flutter {}", args.join(" ")))
        }
        None => run_login_shell(&format!("flutter {}", args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "))),
    }
}

fn run_login_shell(command: &str) -> Result<String, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let mut run = std::process::Command::new(shell);
    run.args(["-lic", command]);
    run_tool(run, command)
}

fn run_tool(mut command: std::process::Command, label: &str) -> Result<String, String> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {label}: {e}"))?;
    let pid = child.id();
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(child.wait_with_output());
    });
    let output = match answer.recv_timeout(TOOL_TIMEOUT) {
        Ok(result) => result.map_err(|e| format!("cannot run {label}: {e}"))?,
        Err(_) => {
            // The tool and everything it started: it leads its own process group.
            let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).status();
            return Err(format!("{label} did not answer in {} seconds", TOOL_TIMEOUT.as_secs()));
        }
    };
    if output.stdout.is_empty() && !output.status.success() {
        return Err(format!("{label} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_addresses_a_tool_prints_are_found_by_what_the_line_says() {
        let run = "\x1b[1mLaunching\x1b[0m\nA Dart VM Service on macOS is available at: http://127.0.0.1:61458/abc=/\n\
                   The Flutter DevTools debugger and profiler on macOS is available at: http://127.0.0.1:9101?uri=http://127.0.0.1:61458/abc=/\n";
        assert_eq!(
            tool_urls(run),
            ToolUrls {
                devtools: Some("http://127.0.0.1:9101?uri=http://127.0.0.1:61458/abc=/".into()),
                vm_service: Some("http://127.0.0.1:61458/abc=/".into()),
            }
        );
        assert_eq!(tool_urls("Serving DevTools at http://127.0.0.1:9100.\n").devtools.as_deref(), Some("http://127.0.0.1:9100"));
        assert_eq!(tool_urls("nothing to see: http://example.com"), ToolUrls::default());
        assert_eq!(query_value("http://127.0.0.1:1/a=/"), "http%3A%2F%2F127.0.0.1%3A1%2Fa%3D%2F");
    }

    #[test]
    fn devices_are_read_from_the_machine_output_after_shell_noise() {
        let output = "Last login: today\n[\n  {\"name\": \"iPhone 15\", \"id\": \"ABC-1\", \"targetPlatform\": \"ios\", \"emulator\": true},\n  {\"name\": \"macOS (desktop)\", \"id\": \"macos\", \"targetPlatform\": \"darwin\"}\n]\n";
        let devices = parse_devices(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0], FlutterDevice { id: "ABC-1".into(), name: "iPhone 15".into(), platform: "ios".into() });
        assert_eq!(devices[1].id, "macos");
        assert!(parse_devices("nothing here").is_empty());
        assert!(parse_devices("[]").is_empty());
    }

    #[test]
    fn a_pubspec_tells_flutter_from_plain_dart() {
        assert!(is_flutter_pubspec("name: app\ndependencies:\n  flutter:\n    sdk: flutter\n"));
        assert!(!is_flutter_pubspec("name: tool\ndependencies:\n  args: ^2.0.0\n"));
    }

    #[test]
    fn the_package_is_the_nearest_folder_with_a_pubspec() {
        let dir = std::env::temp_dir().join(format!("rustcode-pubspec-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib/src")).unwrap();
        std::fs::write(dir.join("pubspec.yaml"), "name: x").unwrap();
        assert_eq!(pubspec_folder(&dir.join("lib/src/a.dart")), Some(dir.clone()));
        assert_eq!(pubspec_folder(&dir.join("lib")), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
