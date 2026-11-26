//! Highlight Rules: compiling the sets and applying them to open terminals.

use super::*;

impl Workspace {
    // ---- highlights -------------------------------------------------------------------

    /// Whether Highlight Rules colour the Sessions of `item`: not when its Host or Project
    /// switched highlighting off.
    pub(super) fn highlights_item(&self, item: &ItemKey) -> bool {
        let project_on = |id: &str| self.library.project(id).is_none_or(|p| p.highlight);
        match item {
            ItemKey::Host { project, host } => project_on(project) && self.library.host(host).is_none_or(|h| h.highlight),
            ItemKey::Shell { project, .. } => project.as_deref().is_none_or(project_on),
        }
    }

    /// After a Host or Project changed: turn each open terminal's highlighting on or off.
    pub(super) fn refresh_session_highlights(&mut self, cx: &mut Context<Self>) {
        for entry in &self.sessions {
            let on = self.highlights_item(&entry.item());
            if let Some(view) = entry.ui.view.terminal() {
                view.update(cx, |v, cx| {
                    if v.highlight != on {
                        v.highlight = on;
                        cx.notify();
                    }
                });
            }
        }
        self.refresh_session_clipboard(cx);
    }

    /// Whether programs in the Sessions of `item` may copy to the clipboard (OSC 52): a Host's
    /// own choice, else Settings'. Local shells always may.
    pub(super) fn clipboard_for(&self, item: &ItemKey) -> crate::model::RemoteClipboard {
        use crate::model::RemoteClipboard;
        match item {
            ItemKey::Shell { .. } => RemoteClipboard::Allow,
            ItemKey::Host { host, .. } => match self.library.host(host).and_then(|h| h.clipboard) {
                Some(true) => RemoteClipboard::Allow,
                Some(false) => RemoteClipboard::Never,
                None => self.settings.remote_clipboard,
            },
        }
    }

    /// After the clipboard setting or a Host's choice changed: tell each open terminal.
    pub(super) fn refresh_session_clipboard(&mut self, cx: &mut Context<Self>) {
        for entry in &self.sessions {
            let policy = self.clipboard_for(&entry.item());
            if let Some(view) = entry.ui.view.terminal() {
                view.update(cx, |v, cx| v.set_clipboard(policy, cx));
            }
        }
    }

    /// "Always" or "Never" on a terminal's clipboard question: kept on its Host.
    pub(super) fn choose_clipboard(&mut self, id: usize, allow: bool, cx: &mut Context<Self>) {
        let Some(ItemKey::Host { host, .. }) = self.sessions.get(id).map(|entry| entry.item()) else { return };
        let mut library = self.library.clone();
        let Some(saved) = library.hosts.iter_mut().find(|h| &h.id == host) else { return };
        saved.clipboard = Some(allow);
        if let Err(err) = self.commit_library(library) {
            self.show_toast(format!("Could not save the choice. {err}"), cx);
        }
        self.refresh_session_clipboard(cx);
    }

    pub(super) fn apply_highlights(&mut self, cx: &mut Context<Self>) {
        let mut compiled = Highlighter::from_sets(&self.highlights, &self.settings.disabled_highlight_sets);
        self.highlight_skipped = compiled.skipped.clone();
        compiled.full_screen = self.settings.highlight_full_screen;
        if !self.settings.highlighting {
            compiled = Highlighter::default();
        }
        cx.set_global(ActiveHighlights(Arc::new(compiled)));
        cx.refresh_windows();
    }

    pub(super) fn save_highlights(&mut self, cx: &mut Context<Self>) {
        if self.highlights_file_locked {
            self.show_toast(format!("Highlights aren't saved until Komando restarts: {} couldn't be read.", highlight::FILE), cx);
        } else if let Err(err) = highlight::save(&self.data_dir, &self.highlights) {
            self.show_toast(format!("Could not save highlights: {err:#}"), cx);
        }
        self.apply_highlights(cx);
    }

    pub(super) fn active_rule_count(&self) -> usize {
        self.highlights
            .iter()
            .filter(|s| !self.settings.disabled_highlight_sets.contains(&s.id))
            .flat_map(|s| s.keywords.iter())
            .filter(|k| k.enabled)
            .count()
    }

    pub(super) fn toggle_highlight_set(&mut self, id: &str, cx: &mut Context<Self>) {
        let disabled = &mut self.settings.disabled_highlight_sets;
        if let Some(pos) = disabled.iter().position(|d| d == id) {
            disabled.remove(pos);
        } else {
            disabled.push(id.to_string());
        }
        self.save_settings(cx);
        self.apply_highlights(cx);
        cx.notify();
    }

    pub(super) fn toggle_highlight_rule(&mut self, set_id: &str, index: usize, cx: &mut Context<Self>) {
        if let Some(rule) = self.highlights.iter_mut().find(|s| s.id == set_id).and_then(|s| s.keywords.get_mut(index)) {
            rule.enabled = !rule.enabled;
        }
        self.save_highlights(cx);
        cx.notify();
    }

    /// Turn a rule's fill on or off. Off keeps a colour chosen for it, so on brings it back;
    /// a rule without one is tinted with its text colour.
    pub(super) fn toggle_highlight_fill(&mut self, set_id: &str, index: usize, cx: &mut Context<Self>) {
        if let Some(rule) = self.highlights.iter_mut().find(|s| s.id == set_id).and_then(|s| s.keywords.get_mut(index)) {
            let on = rule.fill() != highlight::Fill::Off;
            rule.background = Some(!on);
            // Switching on a colour that can't be read (Tabby's "0") would do nothing: tint instead.
            if !on && rule.fill() == highlight::Fill::Off {
                rule.background_color = None;
            }
        }
        self.save_highlights(cx);
        cx.notify();
    }

    /// Turn off the rule that coloured a word right-clicked in a terminal. It stays in its
    /// set, off, so it can be turned back on in Settings → Highlights.
    pub(super) fn unhighlight(&mut self, hit: &highlight::RuleHit, cx: &mut Context<Self>) {
        let source = &hit.source;
        let Some(rule) = self.highlights.iter_mut().find(|s| s.id == source.set).and_then(|s| s.keywords.get_mut(source.index))
        else {
            return;
        };
        if !rule.enabled {
            return;
        }
        rule.enabled = false;
        // A rule written for just this word, or one that also colours other text.
        let just_this = !rule.is_regex() && rule.text.eq_ignore_ascii_case(hit.matched.trim());
        self.save_highlights(cx);
        let word = hit.matched.trim();
        let message = if just_this {
            format!("“{word}” is no longer highlighted. Turn it back on in Settings → Highlights.")
        } else {
            format!("Turned off the rule that highlighted “{word}”, along with what else it matched. Turn it back on in Settings → Highlights.")
        };
        self.show_toast_for(message, Duration::from_secs(6), cx);
        cx.notify();
    }

    pub(super) fn delete_highlight_set(&mut self, id: &str, cx: &mut Context<Self>) {
        self.highlights.retain(|s| s.id != id);
        self.settings.disabled_highlight_sets.retain(|d| d != id);
        self.save_settings(cx);
        self.save_highlights(cx);
        cx.notify();
    }

    pub(super) fn import_highlights(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                let mut imported = vec![];
                for path in &paths {
                    match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| highlight::parse(&t)) {
                        Ok(sets) => imported.extend(sets),
                        Err(err) => {
                            this.show_toast(format!("{}: {err:#}", path.display()), cx);
                            return;
                        }
                    }
                }
                let rules: usize = imported.iter().map(|s| s.keywords.len()).sum();
                let (added, replaced) = highlight::merge(&mut this.highlights, imported);
                this.save_highlights(cx);
                let plural = crate::ui::widgets::plural;
                let mut message = format!("Imported {}, {}", plural(added + replaced, "set"), plural(rules, "rule"));
                if replaced > 0 {
                    message.push_str(&format!(" ({replaced} replaced)"));
                }
                if !this.highlight_skipped.is_empty() {
                    message.push_str(&format!(". {} rule(s) can't be used", this.highlight_skipped.len()));
                }
                this.show_toast(message, cx);
            });
        })
        .detach();
    }

    pub(super) fn export_highlight_set(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(set) = self.highlights.iter().find(|s| s.id == id).cloned() else { return };
        let dir = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
        let name = format!("{}.json", sanitize(&set.name));
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let result = highlight::export(&set).and_then(|json| Ok(std::fs::write(&path, json)?));
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.show_toast(format!("Exported {} to {}", set.name, path.display()), cx),
                Err(err) => this.show_toast(format!("Could not export: {err:#}"), cx),
            });
        })
        .detach();
    }

    /// "Share project…": save the Project as a Project File to send to someone. It holds
    /// where its Hosts are and how to reach them, never a password or key.
    pub(super) fn share_project(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.library.project(project_id) else { return };
        let Some(text) = crate::share::write(&self.library, project_id) else {
            self.show_toast("Could not share the project.", cx);
            return;
        };
        let project_name = project.name.clone();
        let hosts = self.library.hosts_of(project).count();
        let dir = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
        let name = format!("{}.{}", sanitize(&project_name), crate::share::EXTENSION);
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.show_toast_for(
                    format!(
                        "Saved {project_name} with {} to {}. It has no passwords or keys; import it under Settings → Import.",
                        if hosts == 1 { "1 host".to_string() } else { format!("{hosts} hosts") },
                        path.display()
                    ),
                    Duration::from_secs(8),
                    cx,
                ),
                Err(err) => this.show_toast(format!("Could not save {}: {err}", path.display()), cx),
            });
        })
        .detach();
    }
}
