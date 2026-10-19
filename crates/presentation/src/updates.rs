//! Looking for a newer release, fetching it, and switching to it: the title bar button, the
//! commands, and the usage events that go with them.

use std::sync::Arc;

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent, Task};
use rustcode_application::{repository_slug, NoTelemetry, NoUpdates, Release, StagedUpdate, Telemetry, UpdateService};

use crate::editor_view::EditorView;
use crate::notifications::ToastLevel;

/// Where the update stands.
#[derive(Clone)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    Available(Release),
    Downloading(Release),
    Ready(StagedUpdate),
}

pub(crate) struct UpdateUi {
    pub service: Arc<dyn UpdateService>,
    pub telemetry: Arc<dyn Telemetry>,
    pub version: String,
    pub state: UpdateState,
    pub task: Option<Task<()>>,
}

impl Default for UpdateUi {
    fn default() -> Self {
        Self {
            service: Arc::new(NoUpdates),
            telemetry: Arc::new(NoTelemetry),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            state: UpdateState::Idle,
            task: None,
        }
    }
}

impl EditorView {
    /// Give the view the means to update the program and to record usage events, and look for an
    /// update unless the settings say not to.
    pub fn set_updates(&mut self, service: Arc<dyn UpdateService>, telemetry: Arc<dyn Telemetry>, version: &str, cx: &mut Context<Self>) {
        self.updates.service = service;
        self.updates.telemetry = telemetry;
        self.updates.version = version.to_owned();
        self.track("app_start");
        // A downloaded update is only offered where this copy can really switch to it; else the
        // button would come back after every restart.
        let staged = self.updates.service.unavailable().is_none().then(|| self.updates.service.staged()).flatten();
        if let Some(staged) = staged {
            self.updates.state = UpdateState::Ready(staged);
            cx.notify();
        } else if self.config.settings.boolean("rustcode.update.check").unwrap_or(true) {
            self.check_for_updates(false, cx);
        }
    }

    /// Record a usage event, when the user turned that on.
    pub(crate) fn track(&self, event: &str) {
        if self.config.settings.boolean("rustcode.telemetry.enabled").unwrap_or(false) {
            self.updates.telemetry.record(event);
        }
    }

    /// The repository to ask: the setting, else the one this build was made for.
    fn update_repository(&self) -> Option<String> {
        let chosen = self.config.settings.string("rustcode.update.repository").filter(|s| !s.trim().is_empty());
        chosen.or(option_env!("RUSTCODE_UPDATE_REPO")).and_then(repository_slug)
    }

    /// Ask GitHub for a newer release. `manual` is a request of the user: it also reports
    /// "up to date" and what is in the way. A check at start-up stays silent about those.
    pub(crate) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(self.updates.state, UpdateState::Checking | UpdateState::Downloading(_)) {
            return;
        }
        if let Some(reason) = self.updates.service.unavailable() {
            if manual {
                self.notify_user(ToastLevel::Warning, format!("Cannot update: {reason}"), cx);
            }
            return;
        }
        let Some(repository) = self.update_repository() else {
            if manual {
                self.notify_user(ToastLevel::Warning, "Set `rustcode.update.repository` (owner/name) to say where releases are published.".to_owned(), cx);
            }
            return;
        };
        self.updates.state = UpdateState::Checking;
        self.track("update_check");
        let (service, current) = (self.updates.service.clone(), self.updates.version.clone());
        self.updates.task = Some(cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { service.latest(&repository, &current) }).await;
            this.update(cx, |this, cx| {
                this.updates.state = UpdateState::Idle;
                match found {
                    Ok(Some(release)) => {
                        if manual {
                            this.notify_user(ToastLevel::Info, format!("RustCode {} is available.", release.version), cx);
                        }
                        this.updates.state = UpdateState::Available(release);
                    }
                    Ok(None) if manual => this.notify_user(ToastLevel::Info, "RustCode is up to date.".to_owned(), cx),
                    Ok(None) => {}
                    Err(error) if manual => this.notify_user(ToastLevel::Error, format!("Could not look for updates: {error}"), cx),
                    Err(error) => this.log(format!("update check: {error}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn download_update(&mut self, release: Release, cx: &mut Context<Self>) {
        self.updates.state = UpdateState::Downloading(release.clone());
        let service = self.updates.service.clone();
        self.updates.task = Some(cx.spawn(async move |this, cx| {
            let fetched = cx.background_executor().spawn(async move { service.download(&release) }).await;
            this.update(cx, |this, cx| {
                match fetched {
                    Ok(staged) => {
                        this.track("update_downloaded");
                        this.updates.state = UpdateState::Ready(staged);
                    }
                    Err(error) => {
                        this.updates.state = UpdateState::Idle;
                        this.notify_user(ToastLevel::Error, format!("Update failed: {error}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_update(&mut self, staged: StagedUpdate, cx: &mut Context<Self>) {
        match self.updates.service.install_on_exit(&staged) {
            Ok(()) => {
                self.track("update_installed");
                cx.quit();
            }
            Err(error) => self.notify_user(ToastLevel::Error, format!("Update failed: {error}"), cx),
        }
    }

    /// What the title bar button does for the state the update is in.
    pub(crate) fn update_button_pressed(&mut self, cx: &mut Context<Self>) {
        match self.updates.state.clone() {
            UpdateState::Available(release) => self.download_update(release, cx),
            UpdateState::Ready(staged) => self.install_update(staged, cx),
            UpdateState::Idle | UpdateState::Checking | UpdateState::Downloading(_) => {}
        }
    }

    /// The button for an update that can be fetched or switched to; nothing when there is none.
    pub(crate) fn render_update_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (label, enabled) = match &self.updates.state {
            UpdateState::Available(release) => (format!("Update to {}", release.version), true),
            UpdateState::Downloading(release) => (format!("Downloading {}…", release.version), false),
            UpdateState::Ready(staged) => (format!("Restart to update to {}", staged.version), true),
            UpdateState::Idle | UpdateState::Checking => return None,
        };
        let (background, foreground, hover) = (self.theme.button.background, self.theme.button.foreground, self.theme.button.hover);
        Some(
            div()
                .id("update-button")
                .h(px(22.))
                .px(px(10.))
                .flex()
                .items_center()
                .flex_none()
                .rounded(px(4.))
                .text_size(px(12.))
                .bg(background)
                .text_color(foreground)
                .when(enabled, |b| {
                    b.cursor_pointer().hover(move |style| style.bg(hover)).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.update_button_pressed(cx);
                        }),
                    )
                })
                .child(label),
        )
    }
}
