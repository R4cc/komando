//! The welcome tour: a short guide in a dialog the first time Komando starts.
//!
//! It walks through the settings worth choosing straight away, creates the master password,
//! and shows what is easy to miss: importing from another client, the minimap, remote desktops
//! and a handful of smaller features. Every step can be skipped, and the whole tour too (✕,
//! Escape or "Skip tour"); either way it is not shown again. Settings → About replays it.
//!
//! It is offered only on a fresh install: once a Vault, a Project or a Host exists, Komando was
//! already in use, and the tour is marked as seen without being shown.

use super::*;
use super::settings::group;
use super::vault::{field_note, no_recovery_warning};
use crate::model::MinimapPlacement;

/// The tour's steps, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Step {
    Essentials,
    Vault,
    Import,
    Minimap,
    RemoteDesktop,
    More,
}

impl Step {
    pub(super) const ALL: [Step; 6] =
        [Self::Essentials, Self::Vault, Self::Import, Self::Minimap, Self::RemoteDesktop, Self::More];

    fn icon(self) -> &'static str {
        match self {
            Self::Essentials => "settings-2",
            Self::Vault => "lock-keyhole",
            Self::Import => "import",
            Self::Minimap => "map",
            Self::RemoteDesktop => "monitor",
            Self::More => "sparkles",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Essentials => "Welcome to Komando",
            Self::Vault => "Create your master password",
            Self::Import => "Bring your hosts with you",
            Self::Minimap => "The minimap",
            Self::RemoteDesktop => "Remote desktops over RDP and VNC",
            Self::More => "A few more things",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Essentials => "First, the settings worth choosing right away. Everything here can be changed later in Settings.",
            Self::Vault => "Server passwords, key files and desktop passwords are kept in an encrypted vault on this PC, locked with one master password.",
            Self::Import => "Coming from another SSH client? Import your hosts instead of typing them in again.",
            Self::Minimap => "All of a session's output in miniature, beside the terminal, so you can see where you are and jump anywhere.",
            Self::RemoteDesktop => "A host can have a graphical desktop as well as its terminal, opened right next to it.",
            Self::More => "Smaller things that make everyday work faster.",
        }
    }
}

/// Where the tour is, and its master password fields.
pub(super) struct Tour {
    step: usize,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    error: Option<String>,
}

/// What the usage ping is, in the tour and Settings → About (see `crate::usage`).
pub(super) const USAGE_PING_HINT: &str = "Once a day, Komando sends an anonymous ping so the developers can estimate how many people use it. Only the version and operating system: no hosts, commands or anything about you.";

/// Lock-after choices offered in the tour: a shorter list than Settings → Security's.
const TOUR_AUTO_LOCK: [(u32, &str); 5] = [(15, "15 min"), (60, "1 h"), (8 * 60, "8 h"), (24 * 60, "1 day"), (0, "Until restart")];

/// Whether this start should show the tour: never seen, and nothing set up yet. `None` means
/// Komando was used before this version introduced the tour, so it is only marked as seen.
pub(super) fn should_offer(settings: &Settings, library: &Library, vault_exists: bool) -> Option<bool> {
    if settings.onboarded {
        return Some(false);
    }
    if vault_exists || !library.projects.is_empty() || !library.hosts.is_empty() {
        return None;
    }
    Some(true)
}

impl Workspace {
    /// Open the tour at its first step: on the first start, or from Settings → About.
    pub(super) fn start_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("New master password"));
        let confirm = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Repeat master password"));
        for input in [&password, &confirm] {
            let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.tour_create_vault(window, cx);
                }
            });
            self._subscriptions.push(sub);
        }
        self.unlock = None;
        self.overlay = Some(Overlay::Tour(Tour { step: 0, password, confirm, error: None }));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The tour was closed, finished or skipped: don't offer it again.
    pub(super) fn tour_closed(&mut self, cx: &mut Context<Self>) {
        if !self.settings.onboarded {
            self.settings.onboarded = true;
            self.save_settings(cx);
            // Held back while the tour was open, where it can be switched off.
            self.send_usage_ping(cx);
        }
    }

    fn tour_mut(&mut self) -> Option<&mut Tour> {
        match self.overlay.as_mut() {
            Some(Overlay::Tour(tour)) => Some(tour),
            _ => None,
        }
    }

    fn tour_go(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        let needs_password = !self.vault_exists;
        let Some(tour) = self.tour_mut() else { return };
        tour.step = step.min(Step::ALL.len() - 1);
        tour.error = None;
        if Step::ALL[tour.step] == Step::Vault && needs_password {
            let password = tour.password.clone();
            password.update(cx, |input, cx| input.focus(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Close the tour, then open a Settings page or a form when asked to.
    fn tour_finish(&mut self, then: TourThen, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay(window, cx);
        match then {
            TourThen::Nothing => {}
            TourThen::Settings(section) => self.open_settings(section, window, cx),
            TourThen::AddConnection => {
                self.close_settings(window, cx);
                self.open_new_host(String::new(), window, cx);
            }
        }
    }

    fn tour_create_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if Vault::exists(&self.data_dir) {
            self.vault_exists = true;
            cx.notify();
            return;
        }
        let Some((password_input, confirm_input)) = self.tour_mut().map(|t| (t.password.clone(), t.confirm.clone())) else { return };
        let password = password_input.read(cx).value().to_string();
        let confirm = confirm_input.read(cx).value().to_string();
        let result = if password != confirm {
            Err(anyhow::anyhow!("The two passwords don't match."))
        } else {
            Vault::create(&self.data_dir, &password)
        };
        match result {
            Ok(vault) => {
                // Don't keep the password in the fields once it has done its job.
                for input in [password_input, confirm_input] {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
                if let Some(tour) = self.tour_mut() {
                    tour.error = None;
                }
                self.vault = Some(vault);
                self.vault_exists = true;
                self.last_activity = SystemTime::now();
                self.sync_remembered_unlock_with_notice(cx);
                window.focus(&self.focus, cx);
            }
            Err(err) => {
                if let Some(tour) = self.tour_mut() {
                    tour.error = Some(err.to_string());
                }
            }
        }
        cx.notify();
    }

    pub(super) fn render_tour(&self, tour: &Tour, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let step = Step::ALL[tour.step];
        let count = Step::ALL.len();
        let last = tour.step + 1 == count;

        let header = div()
            .flex()
            .items_start()
            .gap(px(14.))
            .px(px(28.))
            .pt(px(24.))
            .pb(px(18.))
            // Room for the dialog's close button.
            .pr(px(52.))
            .child(badge(step.icon(), p.brand, 40.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.brand)
                            .child(format!("WELCOME TOUR · {} OF {count}", tour.step + 1)),
                    )
                    .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(step.title()))
                    .child(div().text_size(px(13.)).line_height(px(19.)).text_color(p.text_muted).child(step.subtitle())),
            );

        let body = match step {
            Step::Essentials => self.tour_essentials(p, cx),
            Step::Vault => self.tour_vault(tour, p, cx),
            Step::Import => self.tour_import(p, cx),
            Step::Minimap => self.tour_minimap(p, cx),
            Step::RemoteDesktop => self.tour_remote_desktop(p),
            Step::More => self.tour_more(p, cx),
        };

        // One dot per step; the current one is a longer bar. A click goes to that step.
        let dots = div().flex_1().flex().justify_center().items_center().gap(px(6.)).children((0..count).map(|i| {
            let current = i == tour.step;
            div()
                .id(("tour-dot", i))
                .role(gpui::Role::Button)
                .aria_label(format!("Step {}: {}", i + 1, Step::ALL[i].title()))
                .h(px(6.))
                .w(px(if current { 20. } else { 6. }))
                .rounded_full()
                .cursor_pointer()
                .bg(if current { p.brand } else if i < tour.step { p.brand.opacity(0.45) } else { p.border })
                .hover(|s| s.opacity(0.8))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.tour_go(i, window, cx)))
        }));

        let creating = step == Step::Vault && !self.vault_exists;
        let back = (tour.step > 0).then(|| {
            let to = tour.step - 1;
            button("tour-back", "Back", ButtonKind::Ghost, p)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.tour_go(to, window, cx)))
        });
        let next_step = tour.step + 1;
        let primary = if creating {
            button("tour-create-vault", "Create vault", ButtonKind::Primary, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.tour_create_vault(window, cx)))
        } else if last {
            button("tour-done", "Start using Komando", ButtonKind::Primary, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.tour_finish(TourThen::Nothing, window, cx)))
        } else {
            icon_text_button("tour-next", "arrow-right", "Next", ButtonKind::Primary, p)
                .flex_row_reverse()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.tour_go(next_step, window, cx)))
        };
        // Creating a vault is optional: Komando asks for one when something first needs it.
        let later = creating.then(|| {
            button("tour-vault-later", "Not now", ButtonKind::Secondary, p)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.tour_go(next_step, window, cx)))
        });
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(14.))
            .border_t_1()
            .border_color(p.border)
            .child(
                div().w(px(170.)).flex().child(
                    button("tour-skip", if last { "Close" } else { "Skip tour" }, ButtonKind::Ghost, p)
                        .ml(px(-6.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.tour_finish(TourThen::Nothing, window, cx))),
                ),
            )
            .child(dots)
            .child(div().w(px(170.)).flex().justify_end().gap(px(8.)).children(back).children(later).child(primary));

        div()
            .id("tour")
            .role(gpui::Role::Dialog)
            .aria_label("Welcome tour")
            .tab_group()
            .w(px(640.))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(div().id(("tour-body", tour.step)).flex_1().min_h_0().overflow_y_scroll().px(px(28.)).pb(px(24.)).child(body))
            .child(footer)
            .into_any_element()
    }

    fn tour_essentials(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.settings;
        let app = s.app_theme;
        let theme = segmented(
            "tour-theme",
            &[("Dark", app == AppThemeSetting::Dark), ("Light", app == AppThemeSetting::Light), ("System", app == AppThemeSetting::System)],
            p,
            cx,
            |this, i, window, cx| {
                this.set_app_theme([AppThemeSetting::Dark, AppThemeSetting::Light, AppThemeSetting::System][i], window, cx);
            },
        );
        let history = s.command_history;
        let putty = s.pointer.putty_style();
        let paste = s.pointer.confirm_multiline_paste;
        let tray = s.minimize_to_tray;
        let updates = s.check_for_updates;
        let usage = s.usage_ping;
        let mut rows = vec![
            tour_row(
                p,
                "palette",
                "Theme",
                if cfg!(windows) {
                    "Dark, light, or follow Windows. More themes in Settings → Appearance."
                } else {
                    "Dark, light, or follow the system. More themes in Settings → Appearance."
                },
                theme,
            ),
            tour_row(
                p,
                "scroll-text",
                "Keep command history",
                "Saves the commands you type in each host's sessions on this PC, with their date, time and address: your own record of what you ran where. Stored in plain text. Right-click a host to see it.",
                Self::setting_switch("tour-history", "Keep command history", history, p, cx, |s, on| s.command_history = on, Self::apply_terminal_settings),
            ),
            tour_row(
                p,
                "clipboard-paste",
                "Copy on select, paste on right-click",
                "As in PuTTY: selecting text copies it, a right-click pastes. Off, use Ctrl+Shift+C and Ctrl+Shift+V.",
                Self::setting_switch(
                    "tour-putty",
                    "Copy on select, paste on right-click",
                    putty,
                    p,
                    cx,
                    |s, on| s.pointer.set_putty_style(on),
                    Self::apply_terminal_settings,
                ),
            ),
            tour_row(
                p,
                "triangle-alert",
                "Ask before pasting several lines",
                "A pasted block runs line by line on the server. Komando shows it to you first.",
                Self::setting_switch(
                    "tour-paste",
                    "Ask before pasting several lines",
                    paste,
                    p,
                    cx,
                    |s, on| s.pointer.confirm_multiline_paste = on,
                    Self::apply_terminal_settings,
                ),
            ),
            tour_row(
                p,
                "refresh-cw",
                "Check for updates",
                "Look for a new version on GitHub when Komando starts. Nothing is installed without asking.",
                Self::setting_switch("tour-updates", "Check for updates", updates, p, cx, |s, on| s.check_for_updates = on, Self::save_settings_and_notify),
            ),
            tour_row(
                p,
                "chart-no-axes-column",
                "Anonymous daily ping",
                USAGE_PING_HINT,
                Self::setting_switch("tour-usage", "Anonymous daily ping", usage, p, cx, |s, on| s.usage_ping = on, Self::save_settings_and_notify),
            ),
        ];
        // The tray icon is Windows only (see `Settings` → Behaviour).
        if cfg!(windows) {
            rows.insert(
                4,
                tour_row(
                    p,
                    "minimize-2",
                    "Minimize to the notification area",
                    "A minimized Komando leaves the taskbar and waits in the notification area, sessions still connected.",
                    Self::setting_switch(
                        "tour-tray",
                        "Minimize to the notification area",
                        tray,
                        p,
                        cx,
                        |s, on| s.minimize_to_tray = on,
                        Self::apply_minimize_to_tray,
                    ),
                ),
            );
        }
        group(p, rows)
    }

    fn tour_vault(&self, tour: &Tour, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let minutes = self.settings.auto_lock_minutes;
        let auto_lock = segmented(
            "tour-auto-lock",
            &TOUR_AUTO_LOCK.map(|(m, label)| (label, m == minutes)),
            p,
            cx,
            |this, i, _, cx| {
                this.settings.auto_lock_minutes = TOUR_AUTO_LOCK[i].0;
                this.save_settings(cx);
                this.sync_remembered_unlock_with_notice(cx);
                cx.notify();
            },
        );
        let auto_lock = group(
            p,
            vec![
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(tour_text(
                        p,
                        "Lock the vault after",
                        "How long the vault stays unlocked without use. Open sessions stay connected when it locks.",
                    ))
                    .child(div().flex().child(auto_lock))
                    .into_any_element(),
            ],
        );

        let state = if self.vault_exists {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(16.))
                .py(px(14.))
                .rounded(p.corner(12.))
                .bg(p.success.opacity(0.1))
                .border_1()
                .border_color(p.success.opacity(0.35))
                .child(icon_el("circle-check-big", 20., p.success))
                .child(tour_text(
                    p,
                    "Your vault is ready",
                    "Tick \"Remember\" when Komando asks for a server's password, and it is saved here. Lock it any time with Ctrl+Shift+L.",
                ))
                .into_any_element()
        } else {
            let message = match &tour.error {
                Some(error) => field_note("circle-alert", error.clone(), p.danger),
                None => field_note("key-round", "At least 8 characters. This is Komando's password, not a server's.", p.text_faint),
            };
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(div().px(px(3.)).child(password_input(&tour.password, p)))
                .child(div().px(px(3.)).child(password_input(&tour.confirm, p)))
                .child(message)
                .child(no_recovery_warning(p))
                .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(
                    "Not now? Komando asks for one the first time it saves a password or key.",
                ))
                .into_any_element()
        };
        div().flex().flex_col().gap(px(16.)).child(state).child(auto_lock).into_any_element()
    }

    fn tour_import(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let source = |icon: &str, name: &str, about: &str| {
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(div().pt(px(2.)).child(badge(icon, p.brand, 30.)))
                .child(tour_text(p, name, about))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(group(
                p,
                vec![
                    source(
                        "terminal",
                        "Tabby",
                        "SSH profiles with their names, addresses, ports, users, icons and login scripts. Profile groups become projects.",
                    ),
                    source(
                        "square-terminal",
                        "MobaXterm",
                        "SSH, RDP and VNC sessions with their names, addresses, ports and users. Session folders become projects.",
                    ),
                    source(
                        "folder-input",
                        "A shared project file",
                        "A .komando file a colleague made with Share project…: a project and its hosts, without passwords.",
                    ),
                ],
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        icon_text_button("tour-open-import", "import", "Import hosts now", ButtonKind::Primary, p).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.tour_finish(TourThen::Settings(SettingsSection::Import), window, cx)
                            }),
                        ),
                    )
                    .child(div().flex_1().min_w_0().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(
                        "You pick what comes over and review it before anything is saved. Later: Settings → Import.",
                    )),
            )
            .into_any_element()
    }

    fn tour_minimap(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let on = self.settings.minimap;
        let placement = self.settings.minimap_placement;
        let placements = [(MinimapPlacement::Beside, "Beside output"), (MinimapPlacement::Over, "Over output")];
        let placement_choice = segmented(
            "tour-minimap-placement",
            &placements.map(|(value, label)| (label, value == placement)),
            p,
            cx,
            move |this, i, _, cx| {
                this.settings.minimap_placement = placements[i].0;
                this.apply_terminal_settings(cx);
            },
        );
        let placement_hint = match placement {
            MinimapPlacement::Beside => "A column of its own, always shown, as in VS Code. Output never runs underneath it.",
            MinimapPlacement::Over => "The terminal keeps its full width. Rest the pointer on the thin handle at its right edge to open the minimap.",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(minimap_preview(p))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(p.text_muted)
                    .child(bullet(p, "Each line of output is a thin stripe in its own colours, highlights included, so errors and long blocks stand out."))
                    .child(bullet(p, "The lighter box is what's on screen: drag it to scroll, or click anywhere on the minimap to jump there."))
                    .child(bullet(p, "Search results show on it too, so you can see at a glance where every match is.")),
            )
            .child(group(
                p,
                vec![
                    tour_row(
                        p,
                        "map",
                        "Show the minimap",
                        "Beside every terminal.",
                        Self::setting_switch("tour-minimap", "Show the minimap", on, p, cx, |s, on| s.minimap = on, Self::apply_terminal_settings),
                    ),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .when(!on, |el| el.opacity(0.5))
                        .child(tour_text(p, "Where it goes", placement_hint))
                        .child(div().flex().child(placement_choice))
                        .into_any_element(),
                ],
            ))
            .into_any_element()
    }

    fn tour_remote_desktop(&self, p: AppPalette) -> AnyElement {
        let step = |n: usize, title: &str, about: &str| {
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(24.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(p.brand_soft)
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.active_text)
                        .child(n.to_string()),
                )
                .child(tour_text(p, title, about))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(desktop_preview(p))
            .child(group(
                p,
                vec![
                    step(1, "Choose a remote desktop", "Right-click the host or open its ⋯ menu. RDP and VNC are offered by default; choose Set up RDP… or Set up VNC… to enter its settings. Switch off a protocol in Edit host… → Remote desktop to hide it from the menu."),
                    step(
                        2,
                        "Set its port and password",
                        "RDP for Windows and xrdp, VNC for Linux desktops and macOS screen sharing. Connect straight to the address, or through the host's SSH session so nothing travels unencrypted. A password typed there is saved in your vault.",
                    ),
                    step(
                        3,
                        "Open it beside the terminal",
                        "Right-click the host or open its ⋯ menu and choose Open RDP or Open VNC. The desktop gets a row under the host; click that row or the host to switch between desktop and terminal. Drag the desktop's row out of the sidebar to give it a window of its own, or choose what a click on the host opens in Edit host… → Remote desktop.",
                    ),
                ],
            ))
            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(
                "Scaling, picture quality, full screen and keys such as Ctrl+Alt+Del are in the toolbar above the desktop. Defaults for every host: Settings → Remote desktop.",
            ))
            .into_any_element()
    }

    fn tour_more(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let tip = |icon: &str, title: &str, keys: Option<&str>, about: &str| {
            div()
                .w(px(284.))
                .flex()
                .items_start()
                .gap(px(10.))
                .p(px(12.))
                .rounded(p.corner(10.))
                .border_1()
                .border_color(p.border)
                .bg(p.surface)
                .child(div().pt(px(1.)).child(icon_el(icon, 16., p.brand)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(title.to_string()))
                                .children(keys.map(|keys| kbd(keys, p))),
                        )
                        .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(about.to_string())),
                )
        };
        let tips = div()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .child(tip("search", "Quick connect", Some("Ctrl+Shift+K"), "Find any host or local shell by name, address, user or project. Ctrl+Enter opens another session."))
            .child(tip("plus", "Several sessions", Some("Ctrl+Shift+T"), "Ctrl+click a host, or hover it and click +, for another terminal on it. Each gets a row under the host; right-click one to rename it."))
            .child(tip("list-tree", "Sidebar by keyboard", Some("Ctrl+Shift+H"), "Arrows move and fold, Enter connects, Ctrl+Enter opens another session, F2 renames, Shift+F10 opens the menu, Escape goes back."))
            .child(tip("folder-tree", "File browser", Some("Ctrl+Shift+E"), "Browse, upload and download files over SFTP beside an SSH session."))
            .child(tip("columns-2", "Split view", None, "Drag a host onto the terminal to open it beside the session you're in."))
            .child(tip("highlighter", "Highlights", None, "Errors, warnings, addresses and more are coloured in the output. Edit the rules in Settings → Highlights."))
            .child(tip("waypoints", "Jump hosts", None, "Reach hosts behind a bastion: add jump hosts in a host's or a project's form."))
            .child(tip("terminal", "Startup commands", None, "Lines Komando types once the shell is ready, such as sudo -i, set per project or host."))
            .child(tip("share-2", "Share a project", None, "Right-click a project and choose Share project… to hand its hosts to a colleague, without passwords."))
            .child(tip("keyboard", "Settings", Some("Ctrl+,"), "Everything in this tour, and more. Replay the tour from Settings → About."));
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(tips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        icon_text_button("tour-add-host", "plus", "Add your first connection", ButtonKind::Secondary, p).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| this.tour_finish(TourThen::AddConnection, window, cx),
                        )),
                    )
                    .child(
                        icon_text_button("tour-import", "import", "Import hosts", ButtonKind::Secondary, p).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| this.tour_finish(TourThen::Settings(SettingsSection::Import), window, cx),
                        )),
                    ),
            )
            .into_any_element()
    }
}

/// What to show once the tour closes.
enum TourThen {
    Nothing,
    Settings(SettingsSection),
    AddConnection,
}

/// A label with its explanation below.
fn tour_text(p: AppPalette, label: &str, hint: &str) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()))
        .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(hint.to_string()))
}

/// An icon, label and explanation on the left, the control on the right.
fn tour_row(p: AppPalette, icon: &str, label: &str, hint: &str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .child(div().flex_shrink_0().child(icon_el(icon, 16., p.text_muted)))
        .child(tour_text(p, label, hint))
        .child(div().flex_shrink_0().pl(px(10.)).child(control))
        .into_any_element()
}

fn bullet(p: AppPalette, text: &str) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().flex_shrink_0().mt(px(7.)).size(px(4.)).rounded_full().bg(p.brand))
        .child(div().flex_1().min_w_0().child(text.to_string()))
}

/// Terminal output with a minimap beside it, drawn with plain shapes: lines of log output on
/// the left, the same kind of lines as coloured stripes on the right, and the box over the part
/// that's on screen.
fn minimap_preview(p: AppPalette) -> AnyElement {
    // (text, colour): None is ordinary output.
    let error = Some(p.danger);
    let ok = Some(p.success);
    let lines: [(&str, Option<Hsla>); 7] = [
        ("$ journalctl -u nginx --since today", Some(p.brand)),
        ("09:12:01 web01 nginx[812]: Started nginx", ok),
        ("09:14:22 web01 nginx[812]: GET /health 200", None),
        ("09:15:03 web01 nginx[812]: upstream timed out", error),
        ("09:15:04 web01 nginx[812]: GET /api/orders 504", error),
        ("09:16:40 web01 nginx[812]: GET /health 200", None),
        ("09:17:12 web01 nginx[812]: GET /static/app.js 200", None),
    ];
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(3.))
        .p(px(12.))
        .font_family(p.mono_family)
        .text_size(px(11.5))
        .children(lines.iter().map(|(line, color)| {
            div().truncate().text_color(color.unwrap_or(p.text_muted)).child(line.to_string())
        }));
    // A fixed pattern of stripe widths and colours, so it looks like real output.
    const PATTERN: [(u8, u8); 18] = [
        (34, 1), (26, 2), (40, 0), (31, 0), (22, 3), (38, 3), (29, 0), (44, 0), (18, 0),
        (36, 2), (27, 0), (41, 0), (24, 3), (33, 0), (39, 0), (20, 1), (30, 0), (43, 0),
    ];
    let stripes = (0..48).map(|i| {
        let (width, kind) = PATTERN[(i * 7) % PATTERN.len()];
        let color = match kind {
            1 => p.brand,
            2 => p.success,
            3 => p.danger,
            _ => p.text_muted.opacity(0.55),
        };
        div().h(px(2.)).w(px(width as f32)).bg(color)
    });
    let minimap = div()
        .relative()
        .flex_shrink_0()
        .w(px(68.))
        .h_full()
        .border_l_1()
        .border_color(p.border)
        .px(px(8.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .children(stripes)
        // The part on screen.
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(58.))
                .h(px(44.))
                .bg(p.text.opacity(0.08))
                .border_y_1()
                .border_color(p.text.opacity(0.2)),
        );
    div()
        .h(px(160.))
        .flex()
        .rounded(p.corner(10.))
        .border_1()
        .border_color(p.border)
        .bg(p.background)
        .overflow_hidden()
        .child(text)
        .child(minimap)
        .into_any_element()
}

/// A host's title bar with its Terminal, RDP and VNC buttons above a desktop, drawn with plain
/// shapes.
fn desktop_preview(p: AppPalette) -> AnyElement {
    let chip = |icon: &str, label: &str, active: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .px(px(10.))
            .rounded(p.corner(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active { p.active_text } else { p.text_muted })
            .when(active, |el| el.bg(p.brand_soft))
            .child(icon_el(icon, 13., if active { p.active_text } else { p.text_muted }))
            .child(label.to_string())
    };
    let bar = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .h(px(38.))
        .border_b_1()
        .border_color(p.border)
        .child(icon_el("server", 14., p.brand))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("win-app01"))
        .child(div().flex_1())
        .child(chip("square-terminal", "Terminal", false))
        .child(chip("monitor", "RDP", true))
        .child(chip("monitor", "VNC", false));
    // A desktop: a wallpaper with a window and a taskbar.
    let desktop = div()
        .flex_1()
        .relative()
        .bg(p.brand.opacity(0.18))
        .child(
            div()
                .absolute()
                .top(px(16.))
                .left(px(28.))
                .w(px(190.))
                .h(px(72.))
                .rounded(p.corner(6.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .child(div().h(px(14.)).border_b_1().border_color(p.border).bg(p.muted)),
        )
        .child(
            div()
                .absolute()
                .top(px(34.))
                .left(px(250.))
                .w(px(150.))
                .h(px(60.))
                .rounded(p.corner(6.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .child(div().h(px(14.)).border_b_1().border_color(p.border).bg(p.muted)),
        )
        .child(div().absolute().left_0().right_0().bottom_0().h(px(14.)).bg(p.surface.opacity(0.9)));
    div()
        .h(px(160.))
        .flex()
        .flex_col()
        .rounded(p.corner(10.))
        .border_1()
        .border_color(p.border)
        .bg(p.background)
        .overflow_hidden()
        .child(bar)
        .child(desktop)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_gets_the_tour() {
        assert_eq!(should_offer(&Settings::default(), &Library::default(), false), Some(true));
    }

    #[test]
    fn a_tour_seen_once_is_not_offered_again() {
        let settings = Settings { onboarded: true, ..Settings::default() };
        assert_eq!(should_offer(&settings, &Library::default(), false), Some(false));
    }

    #[test]
    fn an_install_already_in_use_skips_the_tour() {
        assert_eq!(should_offer(&Settings::default(), &Library::default(), true), None);
        let mut library = Library::default();
        library.projects.push(Project::new("Atlas", "blue", "server"));
        assert_eq!(should_offer(&Settings::default(), &library, false), None);
    }
}
