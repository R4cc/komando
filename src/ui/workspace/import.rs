//! Settings → Import: bring Hosts over from Tabby, or from a Project File someone shared.
//!
//! Three steps on the page itself: find the file (Tabby's configuration automatically, or the
//! user picks it), pick the groups and hosts to bring over, then review what comes over and
//! what doesn't before anything is saved. Reading and matching live in `crate::import` and
//! `crate::share`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::Toggled;

use super::settings::group;
use super::*;
use crate::import::{self, Applied, Duplicate, Found, FoundHost, Plan, Target, tabby};
use crate::model::JumpHost;
use crate::share;

/// Groups start folded when the import has more hosts than this, so the list stays short.
const UNFOLD_UP_TO: usize = 40;

/// Where the import is.
#[derive(Default)]
pub(super) enum ImportFlow {
    /// Nothing loaded yet.
    #[default]
    Start,
    /// Tabby isn't in the usual places: the user is asked to pick its file.
    NotFound,
    /// The picked or found file couldn't be read, or has no SSH profiles.
    Failed { from: Source, path: PathBuf, error: String },
    Pick(Selection),
    Review(Selection),
    Done(Applied),
}

/// What an import reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Tabby,
    /// A Project File made with "Share project…".
    ProjectFile,
}

impl Source {
    /// Where settings that don't come over stay, for "… stay in {}".
    fn name(self) -> &'static str {
        match self {
            Source::Tabby => "Tabby",
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

/// `user@address:port`, leaving out the default port.
fn login(host: &FoundHost) -> String {
    let user = host.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
    let port = if host.port == 22 { String::new() } else { format!(":{}", host.port) };
    format!("{user}{}{port}", host.address)
}

fn hosts_label(n: usize) -> String {
    if n == 1 { "1 host".into() } else { format!("{n} hosts") }
}

impl Workspace {
    /// "Import from Tabby": read Tabby's configuration if it is in the usual place, or ask
    /// for it.
    fn start_tabby_import(&mut self, cx: &mut Context<Self>) {
        match tabby::find_config() {
            Some(path) => self.load_tabby(path, cx),
            None => {
                self.import = ImportFlow::NotFound;
                cx.notify();
            }
        }
    }

    /// Ask for the file to import from: Tabby's configuration or a Project File.
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
                Source::Tabby => match tabby::config_in(&path) {
                    Ok(config) => this.load_tabby(config, cx),
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

    fn load_tabby(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.import = match tabby::read(&path, &is_icon) {
            Err(err) => ImportFlow::Failed { from: Source::Tabby, path, error: format!("{err:#}") },
            Ok(found) if found.groups.is_empty() => {
                let mut error = "There are no SSH profiles in this file.".to_string();
                for note in &found.notes {
                    error.push(' ');
                    error.push_str(note);
                }
                ImportFlow::Failed { from: Source::Tabby, path, error }
            }
            Ok(found) => self.selection(Source::Tabby, path, found),
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

    fn pick_all_import(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection_mut() {
            selection.picked.iter_mut().flatten().for_each(|p| *p = on);
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
            Source::Tabby => "Tabby".to_string(),
            Source::ProjectFile => selection.found.groups.first().map(|g| g.name.clone()).unwrap_or_default(),
        };
        if let Err(err) = self.commit_library(library) {
            self.show_toast(format!("Nothing was imported. {err}"), cx);
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
            ImportFlow::NotFound => self.import_start(Some(self.import_not_found(p, cx)), p, cx),
            ImportFlow::Failed { from, path, error } => self.import_start(Some(self.import_failed(*from, path, error, p, cx)), p, cx),
            ImportFlow::Pick(selection) => self.import_pick(selection, p, cx),
            ImportFlow::Review(selection) => self.import_review(selection, p, cx),
            ImportFlow::Done(applied) => self.import_done(applied, p, cx),
        }
    }

    fn import_start(&self, problem: Option<AnyElement>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let row = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(badge("terminal", p.brand, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Tabby"))
                    .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(
                        "Copies your SSH profiles' names, host names, ports, users, icons and login scripts (as startup \
                         commands). Profile groups become projects. Passwords and jump hosts stay in Tabby.",
                    )),
            )
            .child(
                button("import-tabby", "Import from Tabby", ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_tabby_import(cx))),
            )
            .into_any_element();
        let project_row = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(badge("folder-input", p.brand, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Komando project"))
                    .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(format!(
                        "A .{} file someone made with \"Share project…\". Brings over its hosts with their addresses, \
                         users, jump hosts and startup commands. Passwords and keys are never in it.",
                        share::EXTENSION
                    ))),
            )
            .child(
                button("import-project", "Choose project file…", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_import_file(Source::ProjectFile, window, cx))),
            )
            .into_any_element();
        let mut rows = vec![row, project_row];
        rows.extend(problem);
        group(p, rows)
    }

    fn import_not_found(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let place = if cfg!(windows) { "%APPDATA%\\tabby" } else { "Tabby's settings folder" };
        notice(
            "search-x",
            p.warning,
            "Komando couldn't find Tabby on this PC.",
            &format!(
                "Tabby keeps its profiles in config.yaml, in {place}, or in the data folder next to a \
                 portable Tabby.exe. Choose that file to import from it."
            ),
            button("import-choose", "Choose config.yaml…", ButtonKind::Secondary, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_import_file(Source::Tabby, window, cx)))
                .into_any_element(),
            p,
        )
    }

    fn import_failed(&self, from: Source, path: &Path, error: &str, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        notice(
            "circle-alert",
            p.danger,
            &format!("Couldn't import {}", path.display()),
            error,
            button("import-choose", "Choose another file…", ButtonKind::Secondary, p)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_import_file(from, window, cx)))
                .into_any_element(),
            p,
        )
    }

    /// The file the import reads, with a way to read another one instead.
    fn import_source(&self, selection: &Selection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let from = selection.from;
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(icon_el("file-text", 15., p.text_faint))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(p.text_muted).child(selection.source.display().to_string()))
            .child(
                button("import-other", "Choose another file…", ButtonKind::Ghost, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose_import_file(from, window, cx))),
            )
            .into_any_element()
    }

    fn import_pick(&self, selection: &Selection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let total = selection.found.host_count();
        let picked = selection.picked_count();
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .child(format!("{picked} of {} picked", hosts_label(total))),
            )
            .child(
                button("import-all", "Pick all", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_all_import(true, cx))),
            )
            .child(
                button("import-none", "Pick none", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_all_import(false, cx))),
            );

        let mut groups = div().flex().flex_col().gap(px(10.));
        for (g, (found, planned)) in selection.found.groups.iter().zip(&selection.plan.groups).enumerate() {
            let open = selection.open.contains(&g);
            let count = selection.picked[g].iter().filter(|x| **x).count();
            let (color, target) = match &planned.target {
                Target::Existing { id, name } => (
                    self.library.project(id).map(|pr| project_color(&pr.color)).unwrap_or(p.brand),
                    format!("adds to your project {name}"),
                ),
                Target::New => (p.text_muted, "new project".to_string()),
            };
            let header = div()
                .id(("import-group", g))
                .role(gpui::Role::Button)
                .aria_label(match selection.from {
                    Source::Tabby => format!("Profile group: {}", found.name),
                    Source::ProjectFile => format!("Project: {}", found.name),
                })
                .aria_expanded(open)
                .tab_index(0)
                .focus_visible(move |s| s.border_2().border_color(p.brand))
                .h(px(52.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(10.))
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
                .child(badge(found.icon.as_deref().unwrap_or("folder"), color, 28.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).truncate().child(found.name.clone()))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.text_faint)
                                .truncate()
                                .child(format!("{count} of {} · {target}", hosts_label(found.hosts.len()))),
                        ),
                )
                .child(icon_el(if open { "chevron-down" } else { "chevron-right" }, 14., p.text_faint));
            let mut card = div().flex().flex_col().rounded(px(12.)).border_1().border_color(p.border).bg(p.background).p(px(4.)).child(header);
            if open {
                let mut list = div().flex().flex_col().pl(px(26.)).pb(px(4.));
                for (h, (host, duplicate)) in found.hosts.iter().zip(&planned.hosts).enumerate() {
                    let on = selection.picked[g][h];
                    let mut tags = div().flex().flex_shrink_0().gap(px(6.));
                    match duplicate {
                        Duplicate::No => {}
                        Duplicate::InKomando { host, project } => {
                            tags = tags.child(tag(&format!("Already in Komando: {host} in {project}"), p.warning, p));
                        }
                        Duplicate::Earlier { host } => tags = tags.child(tag(&format!("Same as {host} above"), p.warning, p)),
                    }
                    if !host.left_out.is_empty() {
                        let n = host.left_out.len();
                        tags = tags.child(tag(
                            &format!("{n} setting{} stay in {}", if n == 1 { "" } else { "s" }, selection.from.name()),
                            p.text_muted,
                            p,
                        ));
                    }
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("import-host-{g}-{h}")))
                            .h(px(40.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .rounded(px(8.))
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
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(13.))
                                            .text_color(if on { p.text } else { p.text_muted })
                                            .child(host.name.clone()),
                                    )
                                    .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.text_faint).child(login(host))),
                            )
                            .child(tags),
                    );
                }
                card = card.child(list);
            }
            groups = groups.child(card);
        }

        let notes = (!selection.found.notes.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(selection.found.notes.iter().map(|note| info_line("info", note, p)))
        });
        let actions = div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(
                button("import-cancel", "Cancel", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_import(cx))),
            )
            .child(
                button("import-next", format!("Review {}", hosts_label(picked)), ButtonKind::Primary, p)
                    .when(picked == 0, |el| el.opacity(0.5).cursor_default())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_step(true, cx))),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.import_source(selection, p, cx))
            .child(toolbar)
            .child(groups)
            .children(notes)
            .child(actions)
            .into_any_element()
    }

    fn import_review(&self, selection: &Selection, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let picked = selection.picked_count();
        let mut projects = 0;
        let mut new_projects = 0;
        let mut sections = div().flex().flex_col().gap(px(14.));
        let mut seen_new: Vec<String> = vec![];
        for (g, (found, planned)) in selection.found.groups.iter().zip(&selection.plan.groups).enumerate() {
            let hosts: Vec<&FoundHost> =
                found.hosts.iter().enumerate().filter(|(h, _)| selection.picked[g][*h]).map(|(_, host)| host).collect();
            if hosts.is_empty() {
                continue;
            }
            projects += 1;
            let target = match &planned.target {
                Target::Existing { name, .. } => format!("Adds to your project {name}"),
                Target::New if seen_new.iter().any(|n| n.eq_ignore_ascii_case(found.name.trim())) => "Adds to the new project above".to_string(),
                Target::New => {
                    new_projects += 1;
                    seen_new.push(found.name.trim().to_string());
                    match &found.default_user {
                        Some(user) => format!("New project · default user {user}"),
                        None => "New project".to_string(),
                    }
                }
            };
            let mut rows: Vec<AnyElement> = vec![];
            for host in hosts {
                let mut row = div().flex().flex_col().gap(px(6.)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(icon_el("check", 15., p.success))
                        .child(icon_el(host.icon.as_deref().unwrap_or("server"), 15., p.text_muted))
                        .child(div().flex_shrink_0().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(host.name.clone()))
                        .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.text_muted).child(login(host))),
                );
                for (setting, detail) in comes_over(host) {
                    row = row.child(
                        div()
                            .pl(px(25.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon_el("check", 13., p.success))
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text).child(setting))
                            .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.text_faint).child(detail)),
                    );
                }
                for left in &host.left_out {
                    row = row.child(
                        div()
                            .pl(px(25.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon_el("x", 13., p.danger))
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text).child(left.setting.clone()))
                            .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.text_faint).child(left.detail.clone()))
                            .child(div().flex_1())
                            .child(div().flex_shrink_0().text_size(px(11.5)).text_color(p.text_faint).child("not imported")),
                    );
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
                            .flex()
                            .items_baseline()
                            .gap(px(10.))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(found.name.clone()))
                            .child(div().text_size(px(12.)).text_color(p.text_faint).child(target)),
                    )
                    .child(group(p, rows)),
            );
        }

        // What stays behind as a whole.
        let mut left: Vec<AnyElement> = vec![];
        let unpicked: Vec<(&FoundHost, &Duplicate)> = selection
            .found
            .groups
            .iter()
            .zip(&selection.plan.groups)
            .enumerate()
            .flat_map(|(g, (found, planned))| {
                found.hosts.iter().zip(&planned.hosts).enumerate().filter(move |(h, _)| !selection.picked[g][*h]).map(|(_, x)| x)
            })
            .collect();
        let duplicates = unpicked.iter().filter(|(_, d)| d.is_duplicate()).count();
        if duplicates > 0 {
            left.push(info_line("x", &format!("{} already in Komando, skipped", hosts_label(duplicates)), p));
        }
        let others = unpicked.len() - duplicates;
        if others > 0 {
            left.push(info_line("x", &format!("{} you didn't pick", hosts_label(others)), p));
        }
        left.push(info_line(
            "x",
            match selection.from {
                Source::Tabby => {
                    "Passwords and keys. Komando asks for the password the first time you connect and can save it in its vault; \
                     a key file can be added in the host's settings."
                }
                Source::ProjectFile => {
                    "Passwords and keys: a shared project never holds them. Komando asks for each password, the jump hosts' \
                     included, the first time you connect and can save it in its vault; a key file can be added in the host's \
                     settings."
                }
            },
            p,
        ));
        left.extend(selection.found.notes.iter().map(|note| info_line("x", note, p)));

        let summary = match new_projects {
            0 => format!("{} go into {} existing project{}.", hosts_label(picked), projects, if projects == 1 { "" } else { "s" }),
            n if n == projects => format!("{} go into {n} new project{}.", hosts_label(picked), if n == 1 { "" } else { "s" }),
            n => format!("{} go into {projects} projects, {n} of them new.", hosts_label(picked)),
        };
        let actions = div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(
                button("import-back", "Back", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_step(false, cx))),
            )
            .child(
                button("import-run", format!("Import {}", hosts_label(picked)), ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_import(cx))),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Review the import"))
                    .child(div().text_size(px(12.5)).text_color(p.text_muted).child(match selection.from {
                        Source::Tabby => format!("{summary} Settings marked with an X were set in Tabby and don't come over."),
                        Source::ProjectFile => {
                            format!("{summary} Each host keeps the user, jump hosts and startup commands it has in the shared project.")
                        }
                    })),
            )
            .child(sections)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(section_label("Not imported", p))
                    .child(div().flex().flex_col().gap(px(6.)).children(left)),
            )
            .child(actions)
            .into_any_element()
    }

    fn import_done(&self, applied: &Applied, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let mut detail = vec![];
        if applied.new_projects > 0 {
            detail.push(format!("{} new project{}", applied.new_projects, if applied.new_projects == 1 { "" } else { "s" }));
        }
        if applied.extended_projects > 0 {
            detail.push(format!(
                "{} existing project{}",
                applied.extended_projects,
                if applied.extended_projects == 1 { "" } else { "s" }
            ));
        }
        let row = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(badge("check", p.success, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child(format!("Imported {}", hosts_label(applied.hosts))),
                    )
                    .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(format!(
                        "Into {}. They're in the sidebar; the first connection asks for each password.",
                        detail.join(" and ")
                    ))),
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

/// What a host brings over besides its address and user, for the review: its Jump Hosts and
/// Startup Commands, when the file says.
fn comes_over(host: &FoundHost) -> Vec<(String, String)> {
    let mut over = vec![];
    if let Some(jumps) = host.jump_hosts.as_ref().filter(|j| !j.is_empty()) {
        let label = if jumps.len() == 1 { "Jump host" } else { "Jump hosts" };
        over.push((label.to_string(), jumps.iter().map(JumpHost::label).collect::<Vec<_>>().join(" → ")));
    }
    if let Some(commands) = host.startup_commands.as_ref().filter(|c| !c.is_empty()) {
        let label = if commands.len() == 1 { "Startup command" } else { "Startup commands" };
        over.push((label.to_string(), commands.iter().map(|c| c.label()).collect::<Vec<_>>().join(" · ")));
    }
    over
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
        .focus_visible(move |s| s.border_2().border_color(p.brand))
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
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
        .rounded(px(6.))
        .bg(color.opacity(if p.dark { 0.14 } else { 0.12 }))
        .text_size(px(11.5))
        .text_color(color)
        .whitespace_nowrap()
        .child(text.to_string())
}

/// An icon and a line of text, for notes about the import.
fn info_line(icon: &str, text: &str, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().pt(px(2.)).child(icon_el(icon, 13., if icon == "x" { p.danger } else { p.text_faint })))
        .child(div().flex_1().text_size(px(12.5)).line_height(px(18.)).text_color(p.text_muted).child(text.to_string()))
        .into_any_element()
}

/// A problem with what to do about it: icon, title and explanation, and a button.
fn notice(icon: &str, color: Hsla, title: &str, body: &str, action: AnyElement, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(div().pt(px(2.)).child(icon_el(icon, 16., color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(title.to_string()))
                .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(body.to_string())),
        )
        .child(div().flex_shrink_0().child(action))
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
        for icon in ["import", "search-x", "circle-alert", "file-text", "check", "minus", "x", "info", "folder-input", "share-2"] {
            assert!(is_icon(icon), "{icon}");
        }
    }
}
