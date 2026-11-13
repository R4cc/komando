//! Updates in the sidebar footer and Settings → About.
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

/// The sidebar's update button while an update is offered or downloading.
const UPDATE_ICON: &str = "circle-fading-arrow-up";

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

    /// Download, install and restart from one click. A Windows helper finishes an MSI
    /// installation after this process has exited, then reopens the program.
    pub(super) fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = &self.update else { return };
        let release = release.clone();
        let kind = update::install_kind();
        let asset = match &kind {
            InstallKind::Portable { .. } => release.exe.clone(),
            InstallKind::Installer { .. } => release.msi.clone(),
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
        let data_dir = self.data_dir.clone();
        let task = cx.background_spawn(async move {
            update::download(&asset, &dest, progress)?;
            match &kind {
                InstallKind::Portable { exe } => update::replace_exe(exe, &dest)?,
                InstallKind::Installer { all_users } => update::run_installer(&dest, *all_users, &data_dir)?,
                InstallKind::Unsupported => anyhow::bail!("Updates install on Windows only."),
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
                    this.restart_for_update(cx);
                }
                // The helper is ready and waiting for this process to quit.
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

    /// A compact update action beside Settings. The footprint stays fixed while downloading.
    pub(super) fn render_update_button(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (icon, label, downloading) = match &self.update {
            // Not "download": the Downloads entry sits right above, and the two must not look
            // alike. Lucide's update icon, an arrow up out of a fading circle.
            UpdateState::Available(release) => {
                (UPDATE_ICON, format!("Update to Komando {} and restart", release.version_text()), false)
            }
            UpdateState::Downloading { .. } => (UPDATE_ICON, "Downloading update".into(), true),
            UpdateState::Ready(_) => ("rotate-cw", "Restart to finish updating Komando".into(), false),
            UpdateState::Failed(_) => ("circle-alert", "Update failed. Open update settings".into(), false),
            _ => return None,
        };
        let color = if matches!(self.update, UpdateState::Failed(_)) { p.danger } else { p.brand };
        let owner = cx.entity().downgrade();
        Some(
            div()
                .id("sidebar-update")
                .role(gpui::Role::Button)
                .aria_label(label)
                .tab_index(if downloading { -1 } else { 0 })
                .focus_visible(move |s| s.border_2().border_color(p.brand))
                .relative()
                .size(px(32.))
                .flex_shrink_0()
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .bg(if matches!(self.update, UpdateState::Failed(_)) { p.danger.opacity(0.10) } else { p.brand_soft })
                .when(!downloading, |el| el.cursor_pointer().hover(|s| s.bg(p.active)))
                .child(icon_el(icon, 16., color))
                // The same ring as the Downloads entry: around the icon, filling clockwise, or
                // turning until the size is known.
                .when(downloading, |el| {
                    el.child(div().absolute().top(px(3.)).left(px(3.)).child(progress_ring(
                        "update-ring",
                        self.download_fraction(),
                        26.,
                        2.,
                        p.active,
                        p.brand,
                    )))
                })
                .tooltip(move |window, cx| {
                    let owner = owner.clone();
                    Tooltip::element(move |_, cx| {
                        owner.upgrade().map(|this| this.read(cx).render_update_tooltip(p)).unwrap_or_else(|| div().into_any_element())
                    })
                    .bg(p.surface)
                    .border_color(p.border)
                    .p(px(14.))
                    .rounded(px(12.))
                    .build(window, cx)
                })
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.activate_update_button(window, cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        cx.stop_propagation();
                        this.activate_update_button(window, cx);
                    }
                }))
                .into_any_element(),
        )
    }

    fn activate_update_button(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.update {
            UpdateState::Available(release) if update::install_kind() == InstallKind::Unsupported => cx.open_url(&release.url),
            UpdateState::Available(_) => self.install_update(cx),
            UpdateState::Ready(_) => self.restart_for_update(cx),
            UpdateState::Failed(_) => self.open_settings(SettingsSection::About, window, cx),
            _ => {}
        }
    }

    fn download_fraction(&self) -> Option<f32> {
        match &self.update {
            UpdateState::Downloading { done, total, .. } if *total > 0 => {
                Some(done.load(Ordering::Relaxed).min(*total) as f32 / *total as f32)
            }
            _ => None,
        }
    }

    fn render_update_tooltip(&self, p: AppPalette) -> AnyElement {
        let (major, minor, patch) = update::current_version();
        let (title, detail, action, release) = match &self.update {
            UpdateState::Available(release) => (
                format!("Komando {} is available", release.version_text()),
                format!("You have {major}.{minor}.{patch}"),
                "Click to update and restart. Open sessions will close.",
                Some(release),
            ),
            UpdateState::Downloading { release, .. } => (
                format!("Downloading Komando {}", release.version_text()),
                format!("{}% downloaded", (self.download_fraction().unwrap_or(0.) * 100.).round() as u32),
                "Komando will install the update and restart automatically.",
                None,
            ),
            UpdateState::Ready(release) => (
                format!("Komando {} is ready", release.version_text()),
                "The download has been checked and installed.".into(),
                "Click to restart Komando. Open sessions will close.",
                None,
            ),
            UpdateState::Failed(message) => {
                ("The update didn't finish".into(), message.clone(), "Click to review the error and try again.", None)
            }
            _ => return div().into_any_element(),
        };
        div()
            .w(px(300.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(p.text_muted)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                    .child(div().child(detail)),
            )
            .when_some(release, |el, release| {
                let notes = release.highlights();
                el.when(!notes.is_empty(), |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).child("What's new"))
                            .children(notes.into_iter().map(|note| {
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(div().flex_shrink_0().text_color(p.text_faint).child("•"))
                                    .child(div().flex_1().min_w_0().line_clamp(3).text_ellipsis().child(note))
                            }))
                            .child(div().text_size(px(11.)).text_color(p.text_faint).child("Full release notes in Settings → About.")),
                    )
                })
            })
            .child(div().pt(px(10.)).border_t_1().border_color(p.border).text_size(px(11.)).child(action))
            .into_any_element()
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
                    InstallKind::Portable { .. } => {
                        "Komando downloads and checks the update, installs it and restarts automatically. Open sessions close."
                    }
                    InstallKind::Installer { all_users: true } => {
                        "Windows asks for administrator rights. Komando closes, installs the update and reopens automatically. Open sessions close."
                    }
                    InstallKind::Installer { all_users: false } => {
                        "Komando downloads and checks the update, closes to install it and reopens automatically. Open sessions close."
                    }
                    InstallKind::Unsupported => "Updates install on Windows only.",
                };
                let control = if kind == InstallKind::Unsupported {
                    let url = release.url.clone();
                    button("update-open", "Open release page", ButtonKind::Secondary, p)
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element()
                } else {
                    button("update-install", "Update and restart", ButtonKind::Primary, p)
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
                "The update didn't finish".into(),
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
                this.save_settings(cx);
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
