//! Settings → Import: bring Hosts over from Tabby, MobaXterm or the SSH config, or from a
//! Project File someone shared.
//!
//! Three steps on the page itself: find the file (another client's settings automatically,
//! or the user picks it), pick the groups and hosts to bring over, then review what comes over and
//! what doesn't before anything is saved. Reading and matching live in `crate::import` and
//! `crate::share`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::Toggled;

use super::settings::{group, settings_block};
use super::*;
use crate::import::{self, Applied, Duplicate, Found, FoundGroup, FoundHost, Plan, Target, mobaxterm, openssh, tabby};
use crate::model::{JumpHost, PortForward, StartupCommand};
use crate::share;
use crate::ui::widgets::plural;

/// Groups start folded when the import has more hosts than this, so the list stays short.
const UNFOLD_UP_TO: usize = 40;

/// Where the import is.
#[derive(Default)]
pub(super) enum ImportFlow {
    /// Nothing loaded yet.
    #[default]
    Start,
    /// The other client's settings aren't in the usual places: the user is asked to pick the file.
    NotFound(Source),
    /// The picked or found file couldn't be read, or has no supported sessions.
    Failed { from: Source, path: PathBuf, error: String },
    /// Tabby's configuration is encrypted with its vault: the user is shown how to switch
    /// that off and try the same file again.
    Encrypted { path: PathBuf },
    Pick(Selection),
    Review(Selection),
    Done(Applied),
}

/// What an import reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Tabby,
    MobaXterm,
    /// OpenSSH's `~/.ssh/config`.
    OpenSsh,
    /// A Project File made with "Share project…".
    ProjectFile,
}

impl Source {
    /// Where settings that don't come over stay, for "… stay in {}".
    fn name(self) -> &'static str {
        match self {
            Source::Tabby => "Tabby",
            Source::MobaXterm => "MobaXterm",
            Source::OpenSsh => "your SSH config",
            Source::ProjectFile => "the project file",
        }
    }
}

/// What was read, and what the user picked of it.
pub(super) struct Selection {
    from: Source,
    source: PathBuf,
    found: Found,
    plan: Plan,
    /// `picked[group][host]`.
    picked: Vec<Vec<bool>>,
    /// Groups unfolded in the list.
    open: HashSet<usize>,
}

impl Selection {
    fn picked_count(&self) -> usize {
        self.picked.iter().flatten().filter(|p| **p).count()
    }

    fn group_state(&self, g: usize) -> Toggled {
        let picked = &self.picked[g];
        match picked.iter().filter(|p| **p).count() {
            0 => Toggled::False,
            n if n == picked.len() => Toggled::True,
            _ => Toggled::Mixed,
        }
    }
}

/// Whether a Lucide icon of this name is bundled.
fn is_icon(name: &str) -> bool {
    all_icons().iter().any(|n| n == name)
}

/// The desktop protocol (when applicable) and `user@address:port`, without the default port.
fn login(host: &FoundHost) -> String {
    let user = host.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
    let port = if host.port == host.protocol.default_port() { String::new() } else { format!(":{}", host.port) };
    let protocol = if host.protocol.is_ssh() { String::new() } else { format!("{} · ", host.protocol.label()) };
    format!("{protocol}{user}{}{port}", host.address)
}

fn hosts_label(n: usize) -> String {
    if n == 1 { "1 host".into() } else { format!("{n} hosts") }
}

impl Workspace {
    /// "Import from Tabby", "Import from MobaXterm" or "Import from SSH config": read the
    /// client's settings if they are in the usual place (`~/.ssh/config` for SSH config), or
    /// ask for them (or, for MobaXterm, for exported sessions).
    fn start_client_import(&mut self, from: Source, find_config: fn() -> Option<PathBuf>, cx: &mut Context<Self>) {
        match find_config() {
            Some(path) => self.load_client(from, path, cx),
            None => {
                self.import = ImportFlow::NotFound(from);
                cx.notify();
            }
        }
    }

    /// Ask for the file to import from: another client's settings, or a Project File.
    fn choose_import_file(&mut self, from: Source, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| match from {
                Source::ProjectFile => this.load_project_file(path, cx),
                Source::MobaXterm | Source::OpenSsh => this.load_client(from, path, cx),
                Source::Tabby => match tabby::config_in(&path) {
                    Ok(config) => this.load_client(from, config, cx),
                    Err(err) => {
                        this.import = ImportFlow::Failed { from, path, error: format!("{err:#}") };
                        cx.notify();
                    }
                },
            });
        })
        .detach();
    }

    fn load_project_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.import = match share::read_file(&path, &is_icon) {
            Err(err) => ImportFlow::Failed { from: Source::ProjectFile, path, error: format!("{err:#}") },
            Ok(found) => self.selection(Source::ProjectFile, path, found),
        };
        cx.notify();
    }

    /// The pick step for what was read: hosts already in Komando start unpicked.
    fn selection(&self, from: Source, source: PathBuf, found: Found) -> ImportFlow {
        let plan = import::plan(&found, &self.library);
        let picked = plan.groups.iter().map(|g| g.hosts.iter().map(|d| !d.is_duplicate()).collect()).collect();
        let open = if found.host_count() <= UNFOLD_UP_TO { (0..found.groups.len()).collect() } else { HashSet::new() };
        ImportFlow::Pick(Selection { from, source, found, plan, picked, open })
    }

    /// Read another client's settings: Tabby's, MobaXterm's or OpenSSH's.
    fn load_client(&mut self, from: Source, path: PathBuf, cx: &mut Context<Self>) {
        let (read, none) = match from {
            Source::MobaXterm => (mobaxterm::read(&path), "There are no importable SSH, RDP or VNC sessions in this file."),
            Source::OpenSsh => (
                openssh::read(&path),
                "There are no hosts to import in this file. Komando imports Host blocks that name a host, not only patterns such as Host *.",
            ),
            _ => (tabby::read(&path, &is_icon), "There are no SSH profiles in this file."),
        };
        self.import = match read {
            Err(err) if err.is::<tabby::Encrypted>() => ImportFlow::Encrypted { path },
            Err(err) => ImportFlow::Failed { from, path, error: format!("{err:#}") },
            Ok(found) if found.groups.is_empty() => {
                let mut error = none.to_string();
                for note in &found.notes {
                    error.push(' ');
                    error.push_str(note);
                }
                ImportFlow::Failed { from, path, error }
            }
            Ok(found) => self.selection(from, path, found),
        };
        cx.notify();
    }

    fn selection_mut(&mut self) -> Option<&mut Selection> {
        match &mut self.import {
            ImportFlow::Pick(s) | ImportFlow::Review(s) => Some(s),
            _ => None,
        }
    }

    fn toggle_import_host(&mut self, g: usize, h: usize, cx: &mut Context<Self>) {
        if let Some(pick) = self.selection_mut().and_then(|s| s.picked.get_mut(g)?.get_mut(h)) {
            *pick = !*pick;
            cx.notify();
        }
    }

    /// Pick every host of a group, or none when all are picked already.
    fn toggle_import_group(&mut self, g: usize, cx: &mut Context<Self>) {
        let Some(selection) = self.selection_mut() else { return };
        let on = selection.group_state(g) != Toggled::True;
        if let Some(group) = selection.picked.get_mut(g) {
            group.iter_mut().for_each(|p| *p = on);
        }
        cx.notify();
    }

    /// "Pick all" leaves hosts already in Komando out, as the first selection does; they can
    /// still be picked one by one.
    fn pick_all_import(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection_mut() {
            for (picked, planned) in selection.picked.iter_mut().zip(&selection.plan.groups) {
                for (pick, duplicate) in picked.iter_mut().zip(&planned.hosts) {
                    *pick = on && !duplicate.is_duplicate();
                }
            }
            cx.notify();
        }
    }

    fn toggle_import_open(&mut self, g: usize, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection_mut() {
            if !selection.open.remove(&g) {
                selection.open.insert(g);
            }
            cx.notify();
        }
    }

    fn import_step(&mut self, review: bool, cx: &mut Context<Self>) {
        self.import = match std::mem::take(&mut self.import) {
            ImportFlow::Pick(s) | ImportFlow::Review(s) if review && s.picked_count() > 0 => ImportFlow::Review(s),
            ImportFlow::Pick(s) | ImportFlow::Review(s) => ImportFlow::Pick(s),
            other => other,
        };
        cx.notify();
    }

    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        self.import = ImportFlow::Start;
        cx.notify();
    }

    /// Add the picked hosts and save. A failed save changes nothing and stays on the review.
    fn run_import(&mut self, cx: &mut Context<Self>) {
        let ImportFlow::Review(selection) = &self.import else { return };
        let mut library = self.library.clone();
        // Matched again: the Library may have changed since the list was made.
        let plan = import::plan(&selection.found, &library);
        let applied = import::apply(&selection.found, &plan, &selection.picked, &mut library);
        let from = match selection.from {
            Source::Tabby | Source::MobaXterm | Source::OpenSsh => selection.from.name().to_string(),
            Source::ProjectFile => selection.found.groups.first().map(|g| g.name.clone()).unwrap_or_default(),
        };
        if let Err(err) = self.commit_library(library) {
            self.show_toast(format!("Nothing was imported: {err:#}"), cx);
            return;
        }
        self.show_toast(format!("Imported {} from {from}", hosts_label(applied.hosts)), cx);
        self.import = ImportFlow::Done(applied);
        cx.notify();
    }

    // ---- rendering ------------------------------------------------------------------------

    pub(super) fn settings_import(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        match &self.import {
            ImportFlow::Start => self.import_start(None, p, cx),
            ImportFlow::NotFound(from) => self.import_start(Some(self.import_not_found(*from, p, cx)), p, cx),
            ImportFlow::Failed { from, path, error } => self.import_start(Some(self.import_failed(*from, path, error, p, cx)), p, cx),
            ImportFlow::Encrypted { path } => self.import_start(Some(self.import_encrypted(path, p, cx)), p, cx),
            ImportFlow::Pick(selection) => self.import_pick(selection, p, cx),
            ImportFlow::Review(selection) => self.import_review(selection, p),
            ImportFlow::Done(applied) => self.import_done(applied, p, cx),
        }
    }

    /// The buttons that move the import on, pinned below the page so they stay in view however
    /// long the list of hosts is. None outside the pick and review steps.
    pub(super) fn settings_import_footer(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (selection, review) = match &self.import {
            ImportFlow::Pick(selection) => (selection, false),
            ImportFlow::Review(selection) => (selection, true),
            _ => return None,
        };
        let picked = selection.picked_count();
        let status = if review {
            "Nothing is saved until you import.".to_string()
        } else {
            format!("{picked} of {} picked", hosts_label(selection.found.host_count()))
        };
        let buttons = if review {
            vec![
                button("import-back", "Back", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_step(false, cx)))
                    .into_any_element(),
                button("import-run", format!("Import {}", hosts_label(picked)), ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_import(cx)))
                    .into_any_element(),
            ]
        } else {
            vec![
                button("import-cancel", "Cancel", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_import(cx)))
                    .into_any_element(),
                button("import-next", format!("Review {}", hosts_label(picked)), ButtonKind::Primary, p)
                    .when(picked == 0, |el| el.opacity(0.5).cursor_default().hover(|s| s).active(|s| s))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_step(true, cx)))
                    .into_any_element(),
            ]
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(p.text_muted).child(status))
                .children(buttons)
                .into_any_element(),
        )
    }

    /// Where an import can come from: the other SSH clients, then a shared project. A problem
    /// with the last attempt shows above them.
    fn import_start(&self, problem: Option<AnyElement>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let import = |id: &'static str, from: Source, find_config: fn() -> Option<PathBuf>| {
            button(id, "Import", ButtonKind::Secondary, p)
                .aria_label(format!("Import from {}", from.name()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.start_client_import(from, find_config, cx)))
                .into_any_element()
        };
        let clients = vec![
            source_row(
                "square-terminal",
                "Tabby",
                false,
                "SSH profiles with their icons, forwarded ports and login scripts. Profile groups become projects.".into(),
                import("import-tabby", Source::Tabby, tabby::find_config),
                p,
            ),
            source_row(
                "file-cog",
                "SSH config",
                false,
                "Hosts from ~/.ssh/config and the files it includes, with their jump hosts and forwards.".into(),
                import("import-openssh", Source::OpenSsh, openssh::find_config),
                p,
            ),
            source_row(
                "app-window",
                "MobaXterm",
                true,
                "SSH, RDP and VNC sessions. Session folders become projects.".into(),
                import("import-mobaxterm", Source::MobaXterm, mobaxterm::find_config),
                p,
            ),
        ];
        let project = source_row(
            "folder-input",
            "Komando project",
            false,
            format!("A .{} file made with \"Share project…\".", share::EXTENSION),
            button("import-project", "Choose file…", ButtonKind::Secondary, p)
                .aria_label("Choose a project file")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_import_file(Source::ProjectFile, window, cx)))
                .into_any_element(),
            p,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .children(problem)
            .child(settings_block("From another SSH client", p, group(p, clients)))
            .child(settings_block("From a shared project", p, group(p, vec![project])))
            .into_any_element()
    }

    fn import_not_found(&self, from: Source, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (title, body, choose) = match from {
            Source::OpenSsh => {
                let place = openssh::ssh_dir().map(|dir| dir.join("config").display().to_string()).unwrap_or_else(|| "~/.ssh/config".into());
                ("No SSH config found", format!("ssh reads its hosts from {place}, and there's no file there. If yours is elsewhere, choose it."), "Choose SSH config…")
            }
            Source::MobaXterm if cfg!(windows) => (
                "MobaXterm's sessions weren't found",
                "MobaXterm keeps them in MobaXterm.ini, in Documents\\MobaXterm, %APPDATA%\\MobaXterm or next to a portable \
                 MobaXterm.exe. You can also export them from its Sessions panel as a .mxtsessions file."
                    .to_string(),
                "Choose MobaXterm file…",
            ),
            Source::MobaXterm => (
                "MobaXterm's sessions aren't on this PC",
                "MobaXterm runs on Windows. Export your sessions from its Sessions panel as a .mxtsessions file and choose it here."
                    .to_string(),
                "Choose MobaXterm file…",
            ),
            _ => {
                let place = if cfg!(windows) { "%APPDATA%\\tabby" } else { "Tabby's settings folder" };
                (
                    "Tabby's settings weren't found",
                    format!("Tabby keeps its profiles in config.yaml, in {place} or in the data folder next to a portable Tabby.exe."),
                    "Choose config.yaml…",
                )
            }
        };
        problem_card(
            "search-x",
            p.warning,
            title,
            vec![body_text(body, p)],
            vec![
                button("import-choose", choose, ButtonKind::Secondary, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_import_file(from, window, cx)))
                    .into_any_element(),
                dismiss_button(p, cx),
            ],
            p,
        )
    }

    fn import_failed(&self, from: Source, path: &Path, error: &str, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let title = match from {
            Source::Tabby => "Couldn't import from Tabby",
            Source::MobaXterm => "Couldn't import from MobaXterm",
            Source::OpenSsh => "Couldn't import from your SSH config",
            Source::ProjectFile => "Couldn't import the project file",
        };
        problem_card(
            "circle-alert",
            p.danger,
            title,
            vec![body_text(error.to_string(), p), file_line(path, p)],
            vec![
                button("import-choose", "Choose another file…", ButtonKind::Secondary, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_import_file(from, window, cx)))
                    .into_any_element(),
                dismiss_button(p, cx),
            ],
            p,
        )
    }

    /// Tabby's configuration is encrypted: what that means and the steps to import anyway.
    fn import_encrypted(&self, path: &Path, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let step = |n: usize, text: &str| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .size(px(20.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.warning.opacity(if p.dark { 0.18 } else { 0.14 }))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.warning)
                        .child(n.to_string()),
                )
                .child(div().flex_1().min_w_0().text_size(px(12.5)).line_height(px(18.)).text_color(p.text).child(text.to_string()))
                .into_any_element()
        };
        let retry = path.to_path_buf();
        problem_card(
            "lock-keyhole",
            p.warning,
            "Your Tabby settings are encrypted",
            vec![
                body_text(
                    "Tabby's \"Encrypt config file\" option is on, so only Tabby can read your profiles. Switch it off \
                     while you import; you can switch it back on afterwards."
                        .into(),
                    p,
                ),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .py(px(2.))
                    .child(step(1, "In Tabby, open Settings → Vault."))
                    .child(step(2, "Switch off \"Encrypt config file\"."))
                    .child(step(3, "Come back here and choose Try again."))
                    .into_any_element(),
                file_line(path, p),
            ],
            vec![
                button("import-retry", "Try again", ButtonKind::Primary, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.load_client(Source::Tabby, retry.clone(), cx)))
                    .into_any_element(),
                button("import-choose", "Choose another file…", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_import_file(Source::Tabby, window, cx)))
                    .into_any_element(),
                dismiss_button(p, cx),
            ],
            p,
        )
    }

    /// The file the import reads, with a way to read another one instead.
    fn import_source(&self, selection: &Selection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let from = selection.from;
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon_el("file-text", 14., p.text_faint))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(p.text_muted).child(selection.source.display().to_string()))
            .child(
                button("import-other", "Choose another file…", ButtonKind::Ghost, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_import_file(from, window, cx))),
            )
            .into_any_element()
    }

    fn import_pick(&self, selection: &Selection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let groups = selection.found.groups.len();
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .px(px(2.))
                    .text_size(px(12.5))
                    .text_color(p.text_muted)
                    .child(format!("{} in {}", hosts_label(selection.found.host_count()), plural(groups, "project"))),
            )
            .child(
                button("import-all", "Pick all", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_all_import(true, cx))),
            )
            .child(
                button("import-none", "Pick none", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_all_import(false, cx))),
            );

        let mut list = div().flex().flex_col().rounded(p.corner(12.)).border_1().border_color(p.border).bg(p.surface);
        for (g, (found, planned)) in selection.found.groups.iter().zip(&selection.plan.groups).enumerate() {
            let open = selection.open.contains(&g);
            let count = selection.picked[g].iter().filter(|x| **x).count();
            let total = found.hosts.len();
            let (color, target) = match &planned.target {
                Target::Existing { id, name } => {
                    (self.library.project(id).map(|pr| project_color(&pr.color)).unwrap_or(p.brand), format!("Adds to {name}"))
                }
                Target::New => (p.text_muted, "New project".to_string()),
            };
            let hosts = if count == total { hosts_label(total) } else { format!("{count} of {}", hosts_label(total)) };
            let header = div()
                .id(("import-group", g))
                .role(gpui::Role::Button)
                .aria_label(match selection.from {
                    Source::Tabby => format!("Profile group: {}", found.name),
                    Source::MobaXterm => format!("Session folder: {}", found.name),
                    Source::OpenSsh => found.name.clone(),
                    Source::ProjectFile => format!("Project: {}", found.name),
                })
                .aria_expanded(open)
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(48.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(p.corner(8.))
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_import_open(g, cx)))
                .child(
                    checkbox(("import-group-check", g), &found.name, selection.group_state(g), p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_import_group(g, cx);
                        },
                    )),
                )
                .child(badge(found.icon.as_deref().unwrap_or("folder"), color, 24.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).truncate().child(found.name.clone()))
                        .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(format!("{hosts} · {target}"))),
                )
                .child(icon_el(if open { "chevron-down" } else { "chevron-right" }, 14., p.text_faint));
            let mut section = div().flex().flex_col().p(px(4.)).when(g > 0, |el| el.border_t_1().border_color(p.border)).child(header);
            if open {
                // Hosts start under the group's badge, so their tick boxes line up with it.
                let mut hosts = div().flex().flex_col().pl(px(26.));
                for (h, (host, duplicate)) in found.hosts.iter().zip(&planned.hosts).enumerate() {
                    hosts = hosts.child(self.import_host_row(selection.from, g, h, host, duplicate, selection.picked[g][h], p, cx));
                }
                section = section.child(hosts);
            }
            list = list.child(section);
        }

        let notes = (!selection.found.notes.is_empty())
            .then(|| div().flex().flex_col().gap(px(6.)).children(selection.found.notes.iter().map(|note| info_line("info", note, p))));
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.import_source(selection, p, cx))
            .child(div().flex().flex_col().gap(px(6.)).child(toolbar).child(list))
            .children(notes)
            .into_any_element()
    }

    /// One host in the pick list: its tick box, name and address, and what to know before
    /// picking it.
    #[allow(clippy::too_many_arguments)]
    fn import_host_row(
        &self,
        from: Source,
        g: usize,
        h: usize,
        host: &FoundHost,
        duplicate: &Duplicate,
        on: bool,
        p: AppPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut tags = div().flex().flex_shrink_0().max_w(px(280.)).gap(px(6.));
        match duplicate {
            Duplicate::No => {}
            Duplicate::InKomando { host: existing, project } if *existing == host.name => {
                tags = tags.child(tag(&format!("Already in {project}"), p.warning, p));
            }
            Duplicate::InKomando { host: existing, project } => {
                tags = tags.child(tag(&format!("Already in {project} as {existing}"), p.warning, p));
            }
            Duplicate::Earlier { host } => tags = tags.child(tag(&format!("Same as {host} above"), p.warning, p)),
        }
        if !host.left_out.is_empty() {
            let left_out = plural(host.left_out.len(), "setting");
            tags = tags.child(tag(&format!("{left_out} stay in {}", from.name()), p.text_muted, p));
        }
        div()
            .id(SharedString::from(format!("import-host-{g}-{h}")))
            .h(px(36.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(p.corner(8.))
            .cursor_pointer()
            .hover(|s| s.bg(p.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_import_host(g, h, cx)))
            .child(checkbox(
                SharedString::from(format!("import-host-check-{g}-{h}")),
                &host.name,
                if on { Toggled::True } else { Toggled::False },
                p,
            ))
            .child(icon_el(host.icon.as_deref().unwrap_or("server"), 15., if on { p.text_muted } else { p.text_faint }))
            .child(host_line(host, if on { p.text } else { p.text_muted }, FontWeight::NORMAL, p))
            .child(tags)
            .into_any_element()
    }

    fn import_review(&self, selection: &Selection, p: AppPalette) -> AnyElement {
        let picked = selection.picked_count();
        let mut projects = 0;
        let mut new_projects = 0;
        let mut any_left_out = false;
        let mut sections = div().flex().flex_col().gap(px(20.));
        let mut seen_new: Vec<String> = vec![];
        for (g, (found, planned)) in selection.found.groups.iter().zip(&selection.plan.groups).enumerate() {
            let hosts: Vec<&FoundHost> =
                found.hosts.iter().enumerate().filter(|(h, _)| selection.picked[g][*h]).map(|(_, host)| host).collect();
            if hosts.is_empty() {
                continue;
            }
            projects += 1;
            let mut creates = false;
            let target = match &planned.target {
                Target::Existing { name, .. } => format!("Adds to your project {name}"),
                Target::New if seen_new.iter().any(|n| n.eq_ignore_ascii_case(found.name.trim())) => "Adds to the new project above".to_string(),
                Target::New => {
                    creates = true;
                    new_projects += 1;
                    seen_new.push(found.name.trim().to_string());
                    match &found.default_user {
                        Some(user) => format!("New project · default user {user}"),
                        None => "New project".to_string(),
                    }
                }
            };
            let mut rows: Vec<AnyElement> = vec![];
            // A new project's own Jump Hosts and Startup Commands: every host added to it later
            // uses them, even when none of the imported hosts does.
            let defaults = if creates { project_defaults(found) } else { vec![] };
            if !defaults.is_empty() {
                let mut row = div().flex().flex_col().gap(px(4.)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(icon_el("folder", 15., p.text_muted))
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Project defaults")),
                );
                for (setting, detail) in defaults {
                    row = row.child(setting_row(None, setting, detail, p));
                }
                rows.push(row.into_any_element());
            }
            for host in hosts {
                let mut row = div().flex().flex_col().gap(px(4.)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(icon_el(host.icon.as_deref().unwrap_or("server"), 15., p.text_muted))
                        .child(host_line(host, p.text, FontWeight::MEDIUM, p)),
                );
                for (setting, detail) in comes_over(host) {
                    row = row.child(setting_row(None, setting, detail, p));
                }
                for left in &host.left_out {
                    any_left_out = true;
                    row = row.child(setting_row(Some(("x", p.danger)), left.setting.clone(), left.detail.clone(), p));
                }
                rows.push(row.into_any_element());
            }
            sections = sections.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .px(px(2.))
                            .flex()
                            .items_baseline()
                            .gap(px(10.))
                            .child(div().min_w_0().truncate().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(found.name.clone()))
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child(target)),
                    )
                    .child(group(p, rows)),
            );
        }

        // What stays behind as a whole.
        let mut left: Vec<AnyElement> = vec![];
        let unpicked: Vec<&Duplicate> = selection
            .plan
            .groups
            .iter()
            .enumerate()
            .flat_map(|(g, planned)| planned.hosts.iter().enumerate().filter(move |(h, _)| !selection.picked[g][*h]).map(|(_, d)| d))
            .collect();
        let duplicates = unpicked.iter().filter(|d| d.is_duplicate()).count();
        if duplicates > 0 {
            left.push(info_line("minus", &format!("{} already in Komando", hosts_label(duplicates)), p));
        }
        let others = unpicked.len() - duplicates;
        if others > 0 {
            left.push(info_line("minus", &format!("{} you didn't pick", hosts_label(others)), p));
        }
        left.push(info_line(
            "minus",
            match selection.from {
                Source::Tabby | Source::MobaXterm => "Passwords and keys. Komando asks for each password when you first connect.",
                Source::OpenSsh => "Keys, which stay in their files; add one in a host's settings. Komando asks for passwords when you first connect.",
                Source::ProjectFile => "Passwords and keys, which a shared project never holds. Komando asks for each password when you first connect.",
            },
            p,
        ));
        left.extend(selection.found.notes.iter().map(|note| info_line("minus", note, p)));

        let go = if picked == 1 { "goes" } else { "go" };
        let summary = match new_projects {
            0 => format!("{} {go} into {}.", hosts_label(picked), plural(projects, "existing project")),
            n if n == projects => format!("{} {go} into {}.", hosts_label(picked), plural(n, "new project")),
            n => format!("{} {go} into {projects} projects, {n} of them new.", hosts_label(picked)),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(summary))
                    .when(any_left_out, |el| {
                        el.child(div().text_size(px(12.5)).text_color(p.text_muted).child(format!(
                            "Settings marked with an X don't come over and stay in {}.",
                            selection.from.name()
                        )))
                    }),
            )
            .child(sections)
            .child(settings_block("Not imported", p, div().flex().flex_col().gap(px(6.)).px(px(2.)).children(left).into_any_element()))
            .into_any_element()
    }

    fn import_done(&self, applied: &Applied, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let mut into = vec![];
        if applied.new_projects > 0 {
            into.push(plural(applied.new_projects, "new project"));
        }
        if applied.extended_projects > 0 {
            into.push(plural(applied.extended_projects, "existing project"));
        }
        let row = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(badge("check", p.success, 32.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child(format!("Imported {}", hosts_label(applied.hosts))),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(p.text_muted)
                            .child(format!("Into {}. You'll find them in the sidebar.", into.join(" and "))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        button("import-again", "Import more", ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_import(cx))),
                    )
                    .child(
                        button("import-close", "Back to sessions", ButtonKind::Primary, p).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.import = ImportFlow::Start;
                                this.close_settings(window, cx);
                            },
                        )),
                    ),
            )
            .into_any_element();
        group(p, vec![row])
    }
}

/// What a host brings over besides its address and user, for the review: its Jump Hosts,
/// Port Forwards and Startup Commands, when the file says.
fn comes_over(host: &FoundHost) -> Vec<(String, String)> {
    let mut over = routes_and_commands(host.jump_hosts.as_deref(), host.port_forwards.as_deref(), host.startup_commands.as_deref());
    // It lets the server use your keys: worth seeing before it comes over.
    if host.forward_agent == Some(true) {
        over.push(("Agent forwarding".into(), "On: root on this host can use your SSH agent's keys while you're connected".into()));
    }
    if host.agent_off {
        over.push(("SSH agent".into(), "Not used".into()));
    }
    over
}

/// A new project's Jump Hosts, Port Forwards and Startup Commands from the file, for the review.
fn project_defaults(group: &FoundGroup) -> Vec<(String, String)> {
    routes_and_commands(Some(&group.jump_hosts), Some(&group.port_forwards), Some(&group.startup_commands))
}

fn routes_and_commands(
    jumps: Option<&[JumpHost]>,
    forwards: Option<&[PortForward]>,
    commands: Option<&[StartupCommand]>,
) -> Vec<(String, String)> {
    let mut over = vec![];
    if let Some(jumps) = jumps.filter(|j| !j.is_empty()) {
        let label = if jumps.len() == 1 { "Jump host" } else { "Jump hosts" };
        over.push((label.to_string(), jumps.iter().map(JumpHost::label).collect::<Vec<_>>().join(" → ")));
    }
    // One per line, with who can connect: a forward open to the network is worth seeing.
    if let Some(forwards) = forwards.filter(|f| !f.is_empty()) {
        let label = if forwards.len() == 1 { "Port forward" } else { "Port forwards" };
        let line = |f: &PortForward| if f.open_to_network { format!("{}, open to the network", f.label()) } else { f.label() };
        over.push((label.to_string(), forwards.iter().map(line).collect::<Vec<_>>().join("\n")));
    }
    // Each command in full, one per line: they run on connect, so nothing may hide in an
    // ellipsis.
    if let Some(commands) = commands.filter(|c| !c.is_empty()) {
        let label = if commands.len() == 1 { "Startup command" } else { "Startup commands" };
        over.push((label.to_string(), commands.iter().map(|c| c.label()).collect::<Vec<_>>().join("\n")));
    }
    over
}

/// One way to import on the start page: an icon, what it is and what it brings over, and the
/// button that starts it.
fn source_row(icon: &str, name: &str, experimental: bool, about: String, action: AnyElement, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .child(badge(icon, p.brand, 32.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name.to_string()))
                        .when(experimental, |el| el.child(tag("Experimental", p.warning, p))),
                )
                .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(about)),
        )
        .child(action)
        .into_any_element()
}

/// A host's name and, fainter, its address: the name keeps its room, the address is cut short
/// first.
fn host_line(host: &FoundHost, color: Hsla, weight: FontWeight, p: AppPalette) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_baseline()
        .gap(px(8.))
        .overflow_hidden()
        .child(div().min_w_0().flex_shrink(1.).truncate().text_size(px(13.)).font_weight(weight).text_color(color).child(host.name.clone()))
        .child(
            div()
                .min_w(px(48.))
                .flex_shrink(1.)
                .flex_basis(px(0.))
                .flex_grow(1.)
                .truncate()
                .text_size(px(12.))
                .text_color(p.text_faint)
                .child(login(host)),
        )
}

/// One setting a host or project brings over, or leaves out when it has an icon, in full:
/// wrapped, never cut short. The label lines up under the host's name.
fn setting_row(icon: Option<(&str, Hsla)>, setting: String, detail: String, p: AppPalette) -> gpui::Div {
    let left_out = icon.is_some();
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(div().w(px(15.)).flex_shrink_0().pt(px(2.)).flex().justify_center().children(icon.map(|(icon, color)| icon_el(icon, 12., color))))
        .child(div().w(px(132.)).flex_shrink_0().text_size(px(12.)).text_color(if left_out { p.text_faint } else { p.text_muted }).child(setting))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .text_size(px(12.))
                .text_color(if left_out { p.text_faint } else { p.text })
                .children(detail.lines().map(|line| div().child(line.to_string())).collect::<Vec<_>>()),
        )
}

/// A tick box, 16×16: ticked, empty, or a dash when only some of what it stands for is ticked.
fn checkbox(id: impl Into<gpui::ElementId>, label: &str, state: Toggled, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let on = state != Toggled::False;
    div()
        .id(id)
        .role(gpui::Role::CheckBox)
        .aria_label(SharedString::from(label.to_string()))
        .aria_toggled(state)
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(4.))
        .cursor_pointer()
        .when(on, |el| el.bg(p.brand))
        .when(!on, |el| el.border_1().border_color(p.text_faint))
        .when(state == Toggled::True, |el| el.child(icon_el("check", 12., p.on_brand)))
        .when(state == Toggled::Mixed, |el| el.child(icon_el("minus", 12., p.on_brand)))
}

/// A small rounded label after a row, in `color`.
fn tag(text: &str, color: Hsla, p: AppPalette) -> impl IntoElement {
    div()
        .px(px(7.))
        .py(px(1.))
        .rounded(p.corner(6.))
        .bg(color.opacity(if p.dark { 0.14 } else { 0.12 }))
        .text_size(px(11.5))
        .text_color(color)
        .min_w_0()
        .truncate()
        .child(text.to_string())
}

/// A faint icon and a line of text, for notes about the import.
fn info_line(icon: &str, text: &str, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().pt(px(3.)).child(icon_el(icon, 12., p.text_faint)))
        .child(div().flex_1().text_size(px(12.5)).line_height(px(18.)).text_color(p.text_muted).child(text.to_string()))
        .into_any_element()
}

/// A paragraph inside a problem card.
fn body_text(text: String, p: AppPalette) -> AnyElement {
    div().text_size(px(12.5)).line_height(px(18.)).text_color(p.text_muted).child(text).into_any_element()
}

/// Closes a problem card and goes back to the list of sources.
fn dismiss_button(p: AppPalette, cx: &mut Context<Workspace>) -> AnyElement {
    button("import-dismiss", "Dismiss", ButtonKind::Ghost, p)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_import(cx)))
        .into_any_element()
}

/// A problem with what to do about it, above the import sources: an icon and title, what
/// it means, and the buttons that deal with it.
fn problem_card(icon: &str, color: Hsla, title: &str, body: Vec<AnyElement>, actions: Vec<AnyElement>, p: AppPalette) -> AnyElement {
    div()
        .rounded(p.corner(12.))
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(if p.dark { 0.08 } else { 0.06 }))
        .px(px(18.))
        .py(px(16.))
        .flex()
        .items_start()
        .gap(px(14.))
        .child(badge(icon, color, 32.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().pt(px(6.)).text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()))
                .children(body)
                .child(div().pt(px(4.)).flex().flex_wrap().gap(px(8.)).children(actions)),
        )
        .into_any_element()
}

/// The file a problem is about, on one line, cut short when long.
fn file_line(path: &Path, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(icon_el("file-text", 13., p.text_faint))
        .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(p.text_faint).child(path.display().to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_tabby_icons_map_to_is_bundled() {
        for (fa, lucide) in tabby::ICON_MAP {
            assert!(is_icon(lucide), "{fa} maps to {lucide}, which isn't bundled");
        }
        for icon in ["import", "square-terminal", "search-x", "circle-alert", "file-text", "check", "minus", "x", "info", "folder-input", "share-2", "lock-keyhole", "file-cog", "app-window", "folder", "server"] {
            assert!(is_icon(icon), "{icon}");
        }
    }
}
