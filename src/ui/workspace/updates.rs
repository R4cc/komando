//! Updates in Settings → About: check GitHub for a newer release, download it and install it.
//! The work itself is in `crate::update`; this is the state the page shows.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use super::settings::VERSION;
use crate::update::{self, InstallKind, Release};

pub(super) enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading { release: Release, done: Arc<AtomicU64>, total: u64 },
    /// The new program is in place (portable copy); a restart starts it.
    Ready(Release),
    Failed(String),
}

impl Workspace {
    /// Ask GitHub for the latest release. `quiet` (the check at startup) keeps failures off
    /// the page: being offline isn't worth a message.
    pub(super) fn check_for_updates(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready(_)) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        let task = cx.background_spawn(async { update::fetch_latest() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.update = match result {
                    Ok(release) if release.is_newer() => UpdateState::Available(release),
                    Ok(_) => UpdateState::UpToDate,
                    Err(_) if quiet => UpdateState::Idle,
                    Err(err) => UpdateState::Failed(format!("{err:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Download the available release and install it: swap the program file of a portable
    /// copy (then offer a restart), or run the installer and quit so it can replace us.
    pub(super) fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = &self.update else { return };
        let release = release.clone();
        let kind = update::install_kind();
        let asset = match &kind {
            InstallKind::Portable { .. } => release.exe.clone(),
            InstallKind::Installer => release.msi.clone(),
            InstallKind::Unsupported => None,
        };
        let Some(asset) = asset else {
            self.update = UpdateState::Failed(match kind {
                InstallKind::Unsupported => "Updates install on Windows only.".into(),
                _ => format!("Release {} has no file for this kind of install.", release.tag),
            });
            cx.notify();
            return;
        };
        let dest = update::download_path(&kind, &release);
        let done = Arc::new(AtomicU64::new(0));
        self.update = UpdateState::Downloading { release: release.clone(), done: done.clone(), total: asset.size };
        cx.notify();

        let progress = done.clone();
        let task = cx.background_spawn(async move {
            update::download(&asset, &dest, progress)?;
            match &kind {
                InstallKind::Portable { exe } => update::replace_exe(exe, &dest)?,
                _ => update::run_installer(&dest)?,
            }
            anyhow::Ok(kind)
        });
        // Redraw the progress bar while the download runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.update, UpdateState::Downloading { .. })
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(InstallKind::Portable { .. }) => {
                    this.update = UpdateState::Ready(release);
                    cx.notify();
                }
                // The installer is running and needs our files: quit.
                Ok(_) => cx.quit(),
                Err(err) => {
                    this.update = UpdateState::Failed(format!("{err:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Start the new version and quit this one.
    pub(super) fn restart_for_update(&mut self, cx: &mut Context<Self>) {
        let result = std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe| update::relaunch(&exe));
        match result {
            Ok(()) => cx.quit(),
            Err(err) => {
                self.update = UpdateState::Failed(format!("{err:#}"));
                cx.notify();
            }
        }
    }

    pub(super) fn update_available(&self) -> bool {
        matches!(self.update, UpdateState::Available(_) | UpdateState::Ready(_))
    }

    /// The Updates group on the About page.
    pub(super) fn render_updates(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let mb = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.0);
        let (label, hint, hint_color, control): (String, String, Hsla, AnyElement) = match &self.update {
            UpdateState::Idle => (
                format!("Komando {VERSION}"),
                "Check GitHub for a newer version.".into(),
                p.text_muted,
                button("update-check", "Check for updates", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.check_for_updates(false, cx)))
                    .into_any_element(),
            ),
            UpdateState::Checking => (
                format!("Komando {VERSION}"),
                "Checking GitHub…".into(),
                p.text_muted,
                button("update-checking", "Checking…", ButtonKind::Secondary, p).opacity(0.6).into_any_element(),
            ),
            UpdateState::UpToDate => (
                format!("Komando {VERSION}"),
                "You have the latest version.".into(),
                p.text_muted,
                button("update-check", "Check again", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.check_for_updates(false, cx)))
                    .into_any_element(),
            ),
            UpdateState::Available(release) => {
                let kind = update::install_kind();
                let how = match kind {
                    InstallKind::Portable { .. } => "Komando downloads it, checks it and swaps it in; a restart finishes the update.",
                    InstallKind::Installer => {
                        "Komando downloads the installer and starts it. Windows asks for administrator rights, then the installer replaces this version and Komando closes."
                    }
                    InstallKind::Unsupported => "Updates install on Windows only.",
                };
                let control = if kind == InstallKind::Unsupported {
                    let url = release.url.clone();
                    button("update-open", "Open release page", ButtonKind::Secondary, p)
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element()
                } else {
                    button("update-install", "Download and install", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.install_update(cx)))
                        .into_any_element()
                };
                let (a, b, c) = update::current_version();
                (
                    format!("Version {} is available", release.version_text()),
                    format!("You have {a}.{b}.{c}. {how}"),
                    p.text_muted,
                    control,
                )
            }
            UpdateState::Downloading { release, done, total } => {
                let done = done.load(Ordering::Relaxed).min(*total);
                let fraction = if *total > 0 { done as f32 / *total as f32 } else { 0. };
                (
                    format!("Downloading {}", release.version_text()),
                    format!("{} of {} MB", mb(done), mb(*total)),
                    p.text_muted,
                    div()
                        .w(px(180.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(p.active)
                        .child(div().h_full().rounded_full().bg(p.brand).w(gpui::relative(fraction)))
                        .into_any_element(),
                )
            }
            UpdateState::Ready(release) => (
                format!("Version {} is ready", release.version_text()),
                "Restart Komando to use it. Open sessions close; connect again after the restart.".into(),
                p.text_muted,
                button("update-restart", "Restart now", ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.restart_for_update(cx)))
                    .into_any_element(),
            ),
            UpdateState::Failed(message) => (
                "The update didn't work".into(),
                message.clone(),
                p.danger,
                button("update-retry", "Try again", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.check_for_updates(false, cx)))
                    .into_any_element(),
            ),
        };
        let notes = match &self.update {
            UpdateState::Available(release) | UpdateState::Downloading { release, .. } | UpdateState::Ready(release) => {
                Some(release.url.clone())
            }
            _ => None,
        };
        let auto = self.settings.check_for_updates;
        let status_row = div()
            .flex()
            .items_center()
            .gap(px(24.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label))
                    .child(div().text_size(px(12.)).line_height(px(17.)).text_color(hint_color).child(hint))
                    .when_some(notes, |el, url| {
                        el.child(
                            div()
                                .id("update-notes")
                                .text_size(px(12.))
                                .text_color(p.brand)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(move |_, _, cx| cx.open_url(&url))
                                .child("What's new"),
                        )
                    }),
            )
            .child(div().flex_shrink_0().child(control))
            .into_any_element();
        let auto_row = div()
            .flex()
            .items_center()
            .gap(px(24.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Check automatically"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(p.text_muted)
                            .child("Look for a new version when Komando starts. Nothing is downloaded without asking."),
                    ),
            )
            .child(switch("update-auto".into(), "Check for updates automatically", auto, p).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.settings.check_for_updates = !auto;
                let _ = this.settings.save(&this.data_dir);
                cx.notify();
            })))
            .into_any_element();
        div()
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.background)
            .flex()
            .flex_col()
            .child(div().px(px(18.)).py(px(14.)).border_b_1().border_color(p.border).child(status_row))
            .child(div().px(px(18.)).py(px(14.)).child(auto_row))
            .into_any_element()
    }
}
