//! Settings → Highlights: the dialogs that add and edit Highlight Rules and name Highlight
//! Sets, and the edits they make. The list of sets and rules is drawn in `settings.rs`.

use gpui::{HighlightStyle, StyledText};

use super::*;
use crate::highlight::{Rgb8, RuleDraft, SWATCHES, parse_hex, tint};
use crate::ui::widgets::labelled_checkbox;

/// Add a Highlight Rule to a set, or edit the one at `index`.
pub struct RuleForm {
    set: String,
    index: Option<usize>,
    pattern: Entity<InputState>,
    group: Entity<InputState>,
    foreground: Entity<InputState>,
    background: Entity<InputState>,
    /// A line of output to try the rule on; never saved.
    sample: Entity<InputState>,
    regex: bool,
    case_sensitive: bool,
    /// Fill behind the match: a tint of the text colour, or `background`'s colour.
    fill: bool,
    bold: bool,
    /// Why the last Save was refused.
    error: Option<String>,
    /// "Delete rule" was clicked once and now asks for a second click.
    confirm_delete: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

impl RuleForm {
    fn draft(&self, cx: &App) -> RuleDraft {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let colour = |input: &Entity<InputState>| Some(value(input)).filter(|v| !v.trim().is_empty());
        RuleDraft {
            text: value(&self.pattern),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            group: value(&self.group),
            foreground: colour(&self.foreground),
            fill: self.fill,
            background: colour(&self.background),
            bold: self.bold,
        }
    }
}

/// Create a Highlight Set, or rename the one with the id in `editing`.
pub struct SetForm {
    editing: Option<String>,
    name: Entity<InputState>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// Which colour field of the rule form a control belongs to.
#[derive(Clone, Copy)]
enum ColourField {
    Foreground,
    Background,
}

impl ColourField {
    fn input(self, form: &RuleForm) -> &Entity<InputState> {
        match self {
            Self::Foreground => &form.foreground,
            Self::Background => &form.background,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Foreground => "fg",
            Self::Background => "bg",
        }
    }
}

/// The line the preview starts with: the rule's own text when it is plain text, otherwise a
/// typical log line to try a pattern on.
fn sample_line(draft: &RuleDraft) -> String {
    if !draft.regex && !draft.text.trim().is_empty() {
        format!("Nov 16 09:41:07 web01 app[812]: {} while handling request", draft.text.trim())
    } else {
        "Nov 16 09:41:07 web01 app[812]: ERROR request failed, status=503 after 3 retries".to_string()
    }
}

fn new_input(value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<InputState> {
    let placeholder = placeholder.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
}

fn to_hsla(c: Rgb8) -> Hsla {
    crate::theme::hex(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
}

impl Workspace {
    fn rule_form(&mut self) -> Option<&mut RuleForm> {
        match &mut self.overlay {
            Some(Overlay::RuleForm(form)) => Some(form),
            _ => None,
        }
    }

    fn set_form(&mut self) -> Option<&mut SetForm> {
        match &mut self.overlay {
            Some(Overlay::SetForm(form)) => Some(form),
            _ => None,
        }
    }

    /// Open the rule form on rule `index` of `set`, or on a new rule for it.
    pub(super) fn open_rule_form(&mut self, set: &str, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(highlight_set) = self.highlights.iter().find(|s| s.id == set) else { return };
        let draft = match index.and_then(|i| highlight_set.keywords.get(i)) {
            Some(rule) => RuleDraft::from_keyword(rule),
            None if index.is_some() => return,
            // A new rule starts with the first colour, so it shows as soon as it matches.
            None => RuleDraft { foreground: Some(SWATCHES[0].0.to_string()), ..RuleDraft::default() },
        };
        let pattern = new_input(draft.text.clone(), "ERROR, or a regular expression such as \\bfail(ed)?\\b", window, cx);
        let group = new_input(draft.group.clone(), "1 or a name", window, cx);
        let foreground = new_input(draft.foreground.clone().unwrap_or_default(), "#RRGGBB", window, cx);
        let background = new_input(draft.background.clone().unwrap_or_default(), "#RRGGBB", window, cx);
        let sample = new_input(sample_line(&draft), "A line of output to try the rule on", window, cx);
        let mut subscriptions = vec![];
        for input in [&pattern, &group, &foreground, &background, &sample] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.save_rule(window, cx),
                // The preview follows every keystroke.
                InputEvent::Change => {
                    if let Some(form) = this.rule_form() {
                        form.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            }));
        }
        pattern.update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::RuleForm(Box::new(RuleForm {
            set: set.to_string(),
            index,
            pattern,
            group,
            foreground,
            background,
            sample,
            regex: draft.regex,
            case_sensitive: draft.case_sensitive,
            fill: draft.fill,
            bold: draft.bold,
            error: None,
            confirm_delete: None,
            _subscriptions: subscriptions,
        })));
        self.confirm_delete_set.clear();
        cx.notify();
    }

    fn save_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.rule_form() else { return };
        let (set, index) = (form.set.clone(), form.index);
        let draft = form.draft(cx);
        let base = self.highlights.iter().find(|s| s.id == set).and_then(|s| index.and_then(|i| s.keywords.get(i)));
        let rule = match draft.build(base) {
            Ok(rule) => rule,
            Err(error) => {
                if let Some(form) = self.rule_form() {
                    form.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        let Some(highlight_set) = self.highlights.iter_mut().find(|s| s.id == set) else { return };
        match index.and_then(|i| highlight_set.keywords.get_mut(i)) {
            Some(slot) => *slot = rule,
            None => highlight_set.keywords.push(rule),
        }
        self.highlights_expanded = Some(set);
        self.save_highlights(cx);
        self.close_overlay(window, cx);
    }

    fn delete_highlight_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.rule_form() else { return };
        let (set, index) = (form.set.clone(), form.index);
        if let (Some(highlight_set), Some(index)) = (self.highlights.iter_mut().find(|s| s.id == set), index)
            && index < highlight_set.keywords.len()
        {
            highlight_set.keywords.remove(index);
            self.save_highlights(cx);
        }
        self.close_overlay(window, cx);
    }

    /// Move a rule one place up or down its set: earlier rules win.
    pub(super) fn move_highlight_rule(&mut self, set: &str, index: usize, up: bool, cx: &mut Context<Self>) {
        let Some(highlight_set) = self.highlights.iter_mut().find(|s| s.id == set) else { return };
        let Some(other) = (if up { index.checked_sub(1) } else { Some(index + 1) }) else { return };
        if other >= highlight_set.keywords.len() || index >= highlight_set.keywords.len() {
            return;
        }
        highlight_set.keywords.swap(index, other);
        self.save_highlights(cx);
        cx.notify();
    }

    /// Open the set form: renaming the set with id `editing`, or creating one.
    pub(super) fn open_set_form(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let current = editing.as_ref().and_then(|id| self.highlights.iter().find(|s| s.id == *id)).map(|s| s.name.clone());
        if editing.is_some() && current.is_none() {
            return;
        }
        let name = new_input(current.unwrap_or_default(), "My rules", window, cx);
        let subscription = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_set(window, cx);
            }
        });
        name.update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::SetForm(SetForm { editing, name, error: None, _subscriptions: vec![subscription] }));
        self.confirm_delete_set.clear();
        cx.notify();
    }

    fn save_set(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.set_form() else { return };
        let name = form.name.read(cx).value().trim().to_string();
        let editing = form.editing.clone();
        if name.is_empty() {
            if let Some(form) = self.set_form() {
                form.error = Some("Give the set a name.".into());
            }
            cx.notify();
            return;
        }
        let id = match editing.and_then(|id| self.highlights.iter_mut().find(|s| s.id == id)) {
            Some(set) => {
                set.name = name;
                set.id.clone()
            }
            None => {
                let set = highlight::new_set(&name);
                let id = set.id.clone();
                self.highlights.push(set);
                id
            }
        };
        self.highlights_expanded = Some(id);
        self.save_highlights(cx);
        self.close_overlay(window, cx);
    }

    pub(super) fn render_set_form(&self, form: &SetForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let title = if form.editing.is_some() { "Rename highlight set" } else { "New highlight set" };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(form_field(
                "Name",
                Some("A new set starts empty. Add rules to it, or import rules into Komando from a file."),
                Input::new(&form.name).into_any_element(),
                p,
            ))
            .into_any_element();
        let save = button("hl-set-save", if form.editing.is_some() { "Save" } else { "Create set" }, ButtonKind::Primary, p)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_set(window, cx)));
        form_dialog("hl-set-form", title, None, body, form.error.clone(), None, save.into_any_element(), p, cx)
    }

    pub(super) fn render_rule_form(&self, form: &RuleForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let editing = form.index.is_some();
        let title = if editing { "Edit highlight rule" } else { "Add highlight rule" };
        let set_name = self.highlights.iter().find(|s| s.id == form.set).map(|s| s.name.clone()).unwrap_or_default();
        let draft = form.draft(cx);

        let match_controls = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Input::new(&form.pattern).font_family(p.mono_family))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(segmented("hl-rule-kind", &[("Text", !form.regex), ("Regular expression", form.regex)], p, cx, |this, i, _, cx| {
                        if let Some(form) = this.rule_form() {
                            form.regex = i == 1;
                            form.error = None;
                        }
                        cx.notify();
                    }))
                    .child(self.form_checkbox("hl-rule-case", "Match case", form.case_sensitive, p, cx, |form| &mut form.case_sensitive)),
            )
            .when(form.regex, |el| el.child(hint("Rust regex syntax: like JavaScript's, without look-around or back-references.", p)))
            .into_any_element();

        // The rule's sentence, top to bottom: what it matches, then how it looks. Labels sit
        // in a column of their own so the controls line up and the form fits without scrolling.
        let body = div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(self.rule_preview(form, &draft, p, cx))
            .child(rule_row("Match", match_controls, p))
            .when(form.regex, |el| {
                el.child(rule_row(
                    "Group",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().w(px(160.)).flex_shrink_0().child(Input::new(&form.group).font_family(p.mono_family)))
                        .child(hint("Optional: colour only this capture group, by number or name.", p).flex_1().min_w_0())
                        .into_any_element(),
                    p,
                ))
            })
            .child(div().h(px(1.)).bg(p.border))
            .child(rule_row("Text", self.colour_picker(form, ColourField::Foreground, p, cx), p))
            .child(rule_row("Fill", self.fill_control(form, p, cx), p))
            .child(rule_row(
                "Style",
                self.form_checkbox("hl-rule-bold", "Bold", form.bold, p, cx, |form| &mut form.bold),
                p,
            ))
            .into_any_element();

        // Removing a rule can't be undone, so it takes a second click.
        let delete = editing.then(|| {
            let confirming = form.confirm_delete.is_some();
            button("hl-rule-delete", if confirming { "Click again to delete" } else { "Delete rule" }, if confirming { ButtonKind::Danger } else { ButtonKind::Ghost }, p)
                // Red even before the second click: it is the one thing here that can't be undone.
                .when(!confirming, |el| el.text_color(p.danger))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let Some(confirm) = this.rule_form().map(|form| form.confirm_delete) else { return };
                    match confirm {
                        // The second click of a double-click doesn't count as the confirmation.
                        Some(at) if at.elapsed() < Duration::from_millis(500) => {}
                        Some(_) => this.delete_highlight_rule(window, cx),
                        None => {
                            if let Some(form) = this.rule_form() {
                                form.confirm_delete = Some(Instant::now());
                            }
                            cx.notify();
                        }
                    }
                }))
                .into_any_element()
        });
        let save = button("hl-rule-save", if editing { "Save" } else { "Add rule" }, ButtonKind::Primary, p)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_rule(window, cx)));
        form_dialog("hl-rule-form", title, Some(set_name), body, form.error.clone(), delete, save.into_any_element(), p, cx)
    }

    /// A tick box in the rule form that flips one of its flags.
    fn form_checkbox(
        &self,
        id: &'static str,
        label: &'static str,
        ticked: bool,
        p: AppPalette,
        cx: &mut Context<Self>,
        flag: fn(&mut RuleForm) -> &mut bool,
    ) -> AnyElement {
        let workspace = cx.entity().downgrade();
        labelled_checkbox(id, label, ticked, p, move |_, _, cx| {
            let _ = workspace.update(cx, |this, cx| {
                if let Some(form) = this.rule_form() {
                    let value = flag(form);
                    *value = !*value;
                    form.error = None;
                }
                cx.notify();
            });
        })
        .into_any_element()
    }

    /// The sample line drawn as the terminal would draw it with this rule, and what the rule
    /// does to it.
    fn rule_preview(&self, form: &RuleForm, draft: &RuleDraft, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let appearance = cx.global::<Appearance>();
        let (term, font) = (appearance.term.clone(), appearance.font_family.clone());
        let line = form.sample.read(cx).value().to_string();
        let mut highlights = vec![];
        let status: (String, Hsla) = match draft.build(None) {
            Ok(rule) => {
                let styles = Highlighter::new([&rule]).styles_for_background(&line, term.dark).unwrap_or_default();
                // Neighbouring characters with the same style make one run.
                let mut start = 0;
                for end in 1..=styles.len() {
                    if end == styles.len() || styles[end] != styles[start] {
                        if let Some(style) = styles[start] {
                            highlights.push((
                                start..end,
                                HighlightStyle {
                                    color: style.fg.map(to_hsla),
                                    background_color: style.bg.map(to_hsla),
                                    font_weight: style.bold.then_some(FontWeight::BOLD),
                                    ..HighlightStyle::default()
                                },
                            ));
                        }
                        start = end;
                    }
                }
                match highlights.len() {
                    0 => ("No match in this line. Paste a line from your output to try the rule on it.".into(), p.text_muted),
                    1 => ("Colours 1 match in this line.".into(), p.text_muted),
                    n => (format!("Colours {n} matches in this line."), p.text_muted),
                }
            }
            Err(_) if draft.text.trim().is_empty() => ("Type what to match to see it coloured here.".into(), p.text_muted),
            Err(error) => (error, p.danger),
        };
        let shown = if line.is_empty() { " ".to_string() } else { line.clone() };
        let error = matches!(status.1, c if c == p.danger).then(|| status.0.clone());
        let count: SharedString = match (&error, highlights.len()) {
            (Some(_), _) => "".into(),
            (None, _) if draft.text.trim().is_empty() => "".into(),
            (None, 0) => "No match".into(),
            (None, 1) => "1 match".into(),
            (None, n) => format!("{n} matches").into(),
        };
        let term_bg = crate::theme::rgb_to_hsla(term.background);
        div()
            .id("hl-rule-preview")
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                // The line as the terminal draws it, over the sample it is drawn from: one card,
                // so the sample can't be mistaken for the pattern below.
                div()
                    .rounded(p.corner(10.))
                    .border_1()
                    .border_color(p.border)
                    .overflow_hidden()
                    .child(
                        div()
                            .id("hl-rule-preview-line")
                            .role(gpui::Role::Status)
                            .aria_label(format!("Preview: {}", status.0))
                            .px(px(14.))
                            .py(px(12.))
                            .bg(term_bg)
                            .text_color(crate::theme::rgb_to_hsla(term.foreground))
                            .font_family(font)
                            .text_size(px(13.))
                            .line_height(px(20.))
                            // Wrapped, not cut: a match near the end of the line stays in view.
                            .whitespace_normal()
                            .child(StyledText::new(shown).with_highlights(highlights)),
                    )
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(4.))
                            .border_t_1()
                            .border_color(p.border)
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child("Sample"))
                            .child(div().flex_1().min_w_0().child(Input::new(&form.sample).appearance(false)))
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text_muted).child(count)),
                    ),
            )
            .when_some(error, |el, error| el.child(div().text_size(px(12.)).text_color(p.danger).child(error)))
            .when(error_free_hint(&draft.text, &status.0), |el| el.child(hint(&status.0, p)))
            .into_any_element()
    }

    /// The fill's switch and, while it is on, what it is filled with.
    fn fill_control(&self, form: &RuleForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        // The switch, and its label as a larger target for the same thing.
        let toggle = || {
            cx.listener(|this: &mut Workspace, _: &ClickEvent, _, cx| {
                if let Some(form) = this.rule_form() {
                    form.fill = !form.fill;
                    form.error = None;
                }
                cx.notify();
            })
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(switch("hl-rule-fill".into(), "Fill behind the match", form.fill, p).on_click(toggle()))
                    .child(
                        div()
                            .id("hl-rule-fill-label")
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(p.text)
                            .on_click(toggle())
                            .child("Fill behind the match, like a highlighter"),
                    ),
            )
            .when(form.fill, |el| {
                el.child(self.colour_picker(form, ColourField::Background, p, cx))
                    .child(hint("A tint follows the text colour and keeps it readable on light and dark terminals.", p))
            })
            .into_any_element()
    }

    /// The first choice and the bundled set's colours, any other colour already chosen, and a
    /// field for typing a hex colour. The text colour's first choice is no colour; the fill's is
    /// a tint of the text colour, shown as it will look.
    fn colour_picker(&self, form: &RuleForm, field: ColourField, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let current_text = field.input(form).read(cx).value().to_string();
        let current = parse_hex(&current_text);
        let none = current_text.trim().is_empty();
        let pick = |value: &'static str| {
            cx.listener(move |this: &mut Workspace, _: &ClickEvent, window, cx| {
                let Some(form) = this.rule_form() else { return };
                form.error = None;
                let input = field.input(form).clone();
                input.update(cx, |f, cx| f.set_value(value, window, cx));
                cx.notify();
            })
        };
        let first = match field {
            ColourField::Foreground => swatch(SharedString::from(format!("hl-{}-none", field.id())), "No colour", None, none, p),
            ColourField::Background => {
                let dark = cx.global::<Appearance>().term.dark;
                let text = parse_hex(&form.foreground.read(cx).value());
                tint_swatch(tint(text, dark), none, p)
            }
        };
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.)).child(first.on_click(pick("")));
        for (hex, name) in SWATCHES {
            let selected = current.is_some() && current == parse_hex(hex);
            row = row.child(
                swatch(SharedString::from(format!("hl-{}-{hex}", field.id())), name, parse_hex(hex), selected, p).on_click(pick(hex)),
            );
        }
        // A colour of the user's own stays in view, chosen.
        if let Some(own) = current.filter(|c| SWATCHES.iter().all(|(hex, _)| parse_hex(hex) != Some(*c))) {
            row = row.child(swatch(SharedString::from(format!("hl-{}-own", field.id())), "Your colour", Some(own), true, p));
        }
        row.child(div().ml(px(6.)).w(px(116.)).child(Input::new(field.input(form)).font_family(p.mono_family))).into_any_element()
    }
}

/// The fill's "Tint" choice: the tint itself with an "A" in the text colour on it, so it shows
/// what the match will look like rather than naming it.
fn tint_swatch((fill, text): (Rgb8, Option<Rgb8>), selected: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let name = "Tint of the text colour";
    div()
        .id("hl-bg-tint")
        .role(gpui::Role::RadioButton)
        .aria_label(name)
        .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .tooltip(move |window, cx| Tooltip::new(name).build(window, cx))
        .h(px(28.))
        .flex_shrink_0()
        .p(px(3.))
        .rounded_full()
        .border_2()
        .border_color(if selected { p.text } else { gpui::transparent_black() })
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .cursor_pointer()
        .child(
            div()
                .h_full()
                .px(px(9.))
                .rounded_full()
                .flex()
                .items_center()
                .gap(px(5.))
                .bg(to_hsla(fill))
                .border_1()
                .border_color(p.text.opacity(0.12))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(text.map(to_hsla).unwrap_or(p.text))
                .child("Tint"),
        )
}

/// A round colour to pick, with a ring when it is the one chosen. `None` is "no colour".
fn swatch(id: SharedString, name: &'static str, colour: Option<Rgb8>, selected: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::RadioButton)
        .aria_label(name)
        .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .tooltip(move |window, cx| Tooltip::new(name).build(window, cx))
        .size(px(28.))
        .flex_shrink_0()
        .p(px(3.))
        .rounded_full()
        .border_2()
        .border_color(if selected { p.text } else { gpui::transparent_black() })
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .cursor_pointer()
        .child(
            div()
                .size_full()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .map(|el| match colour {
                    Some(c) => el.bg(to_hsla(c)).border_1().border_color(p.text.opacity(0.12)),
                    None => el.border_1().border_color(p.border).child(icon_el("ban", 12., p.text_faint)),
                }),
        )
}

/// The frame of a highlight dialog: title (and the set it belongs to), a scrolling body, and a
/// footer with the error, an optional action on the left, Cancel and the main action.
#[allow(clippy::too_many_arguments)]
fn form_dialog(
    id: &'static str,
    title: &'static str,
    subtitle: Option<String>,
    body: AnyElement,
    error: Option<String>,
    left: Option<AnyElement>,
    primary: AnyElement,
    p: AppPalette,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    div()
        .id(id)
        .role(gpui::Role::Dialog)
        .aria_label(title)
        .tab_group()
        .w(px(600.))
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .p(px(24.))
                .pb(px(18.))
                .pr(px(52.))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                .when_some(subtitle, |el, subtitle| {
                    el.child(div().text_size(px(12.5)).text_color(p.text_muted).truncate().child(format!("In {subtitle}")))
                }),
        )
        .child(div().id(SharedString::from(format!("{id}-body"))).flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pb(px(22.)).child(body))
        .child(
            div()
                .p(px(16.))
                .border_t_1()
                .border_color(p.border)
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(10.))
                .when_some(error, |el, error| {
                    el.child(div().id(SharedString::from(format!("{id}-error"))).role(gpui::Role::Alert).text_size(px(12.5)).text_color(p.danger).child(error))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(left)
                        .child(div().flex_1())
                        .child(
                            button(SharedString::from(format!("{id}-cancel")), "Cancel", ButtonKind::Ghost, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx))),
                        )
                        .child(primary),
                ),
        )
        .into_any_element()
}

/// A row of the rule form: its label in a column of its own, the control beside it.
fn rule_row(label: &'static str, control: AnyElement, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(16.))
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                // On the first line of the control beside it (a 28-32px control).
                .pt(px(6.))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}

/// A line of help under a control.
fn hint(text: &str, p: AppPalette) -> gpui::Div {
    div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(text.to_string())
}

/// The preview's own words worth showing under it: what to do next while nothing matches.
fn error_free_hint(pattern: &str, status: &str) -> bool {
    !pattern.trim().is_empty() && status.starts_with("No match")
}
