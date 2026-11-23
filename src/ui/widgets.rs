//! Small building blocks shared by every screen: buttons, icons, labels.
//!
//! Type scale (px): 11 uppercase labels (+tracking), 12 meta, 13 UI text, 14 inputs and
//! terminal, 15 dialog titles. The logo is drawn, not set in a font. Radii are concentric: an inner element's
//! radius is the outer radius minus the padding between them.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, BoxShadow, ElementId, FontWeight, Hsla, IntoElement, ParentElement,
    SharedString, Stateful, Styled, Transformation, div, percentage, point, prelude::*, px, svg,
};

use std::time::{Duration, Instant};

use crate::theme::AppPalette;
use gpui::Focusable as _;
use gpui_component::input::{Input, InputState};
use gpui_component::tooltip::Tooltip;

/// `n` and a count noun with a plain plural: "1 host", "3 hosts".
pub fn plural(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

/// A compact warning shared by password fields and terminal password prompts.
pub fn caps_lock_indicator(id: impl Into<ElementId>, p: AppPalette) -> impl IntoElement {
    div()
        .id(id)
        .role(gpui::Role::Status)
        .aria_label("Caps Lock is on")
        .flex()
        .items_center()
        .gap(px(4.))
        .flex_shrink_0()
        .font_family(p.font_family)
        .text_size(px(11.))
        .text_color(p.warning)
        .child(icon_el("arrow-big-up-dash", 14., p.warning))
        .child("Caps Lock")
        .tooltip(|window, cx| Tooltip::new("Caps Lock is on").build(window, cx))
}

/// A password input with a live Caps Lock warning while the field has focus.
pub fn password_input(input: &gpui::Entity<InputState>, p: AppPalette) -> impl IntoElement {
    PasswordInput { input: input.clone(), palette: p }
}

#[derive(IntoElement)]
struct PasswordInput {
    input: gpui::Entity<InputState>,
    palette: AppPalette,
}

impl gpui::RenderOnce for PasswordInput {
    fn render(self, window: &mut gpui::Window, cx: &mut gpui::App) -> impl IntoElement {
        let focused = self.input.focus_handle(cx).is_focused(window) && window.is_window_active();
        let caps_lock = window.capslock().on;
        div()
            .w_full()
            .on_modifiers_changed(move |event, window, _| {
                if focused && event.capslock.on != caps_lock {
                    window.refresh();
                }
            })
            .child(Input::new(&self.input).when(focused && caps_lock, |input| {
                input.suffix(caps_lock_indicator(("caps-lock", self.input.entity_id()), self.palette))
            }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A 32px-high text button. Hit area is the full surface.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: ButtonKind, p: AppPalette) -> Stateful<gpui::Div> {
    let label = label.into();
    button_shell(id, kind, p).0.aria_label(label.clone()).child(label)
}

/// A text button with a leading icon, drawn in the label's colour.
pub fn icon_text_button(
    id: impl Into<ElementId>,
    icon: &str,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    p: AppPalette,
) -> Stateful<gpui::Div> {
    let (el, fg) = button_shell(id, kind, p);
    let label = label.into();
    el.aria_label(label.clone()).gap(px(8.)).child(icon_el(icon, 15., fg)).child(label)
}

/// A button's surface without its content, and the colour its content is drawn in.
fn button_shell(id: impl Into<ElementId>, kind: ButtonKind, p: AppPalette) -> (Stateful<gpui::Div>, Hsla) {
    let (bg, fg, hover, border) = match kind {
        ButtonKind::Primary => (p.brand, p.on_brand, p.interactive_surface(p.brand, p.on_brand, false), None),
        ButtonKind::Secondary => (p.secondary, p.on_secondary, p.interactive_surface(p.secondary, p.on_secondary, false), Some(p.border)),
        ButtonKind::Ghost => (gpui::transparent_black(), p.text_muted, p.hover, None),
        ButtonKind::Danger => (p.danger, p.on_danger, p.interactive_surface(p.danger, p.on_danger, false), None),
    };
    let pressed = p.interactive_surface(if kind == ButtonKind::Ghost { p.hover } else { bg }, fg, true);
    let el = div()
        .id(id)
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(8.))
        .bg(bg)
        .when_some(border, |el, b| el.border_1().border_color(b))
        .text_color(fg)
        .font_family(p.font_family)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(pressed));
    (el, fg)
}

/// A square icon-only button with a tooltip-free label for accessibility tooling.
/// A labelled tick box in a dialog. It is a Tab stop: Space toggles it and keeps the focus
/// there, so Tab carries on to the next control. `toggle` gets `true` when the mouse did it.
///
/// gpui turns Space or Enter released on a focused element into a keyboard click, so this
/// handles no keys itself: doing so would toggle once on key down and back on key up.
pub fn labelled_checkbox(
    id: impl Into<ElementId>, label: &'static str, ticked: bool, p: AppPalette,
    toggle: impl Fn(bool, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::CheckBox)
        .aria_label(label)
        .aria_toggled(if ticked { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .flex()
        .items_center()
        .gap(px(8.))
        .mt(px(2.))
        .px(px(4.))
        .mx(px(-4.))
        .py(px(2.))
        .rounded(p.corner(6.))
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .cursor_pointer()
        .on_click(move |event, window, cx| match event {
            // Enter isn't a checkbox key; leave it alone.
            gpui::ClickEvent::Keyboard(key) if key.button == gpui::KeyboardButton::Enter => {}
            gpui::ClickEvent::Keyboard(_) => toggle(false, window, cx),
            _ => toggle(true, window, cx),
        })
        .child(
            div()
                .size(px(16.))
                .flex_shrink_0()
                .rounded(p.corner(4.))
                .border_1()
                .border_color(if ticked { p.brand } else { p.border })
                .when(ticked, |el| el.bg(p.brand))
                .flex()
                .items_center()
                .justify_center()
                .when(ticked, |el| el.child(icon_el("check", 11., p.on_brand))),
        )
        .child(div().text_size(px(13.)).text_color(p.text).child(label))
}

pub fn icon_button(id: impl Into<ElementId>, icon: &str, p: AppPalette) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(icon.replace('-', " "))
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(7.))
        .text_color(p.text_muted)
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover).text_color(p.hover_text))
        .child(icon_el(icon, 16., p.text_muted))
}

/// A sidebar row's × button: like `icon_button`, but it turns red under the pointer, icon
/// and all, since it disconnects or discards.
pub fn close_button(id: impl Into<ElementId>, size: f32, p: AppPalette) -> Stateful<gpui::Div> {
    let id: ElementId = id.into();
    let group = SharedString::from(format!("close-button-{id}"));
    div()
        .id(id)
        .group(group.clone())
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(7.))
        .cursor_pointer()
        .hover(move |s| s.bg(p.danger.opacity(0.14)))
        .child(
            svg()
                .path("icons/x.svg")
                .size(px(16.))
                .flex_shrink_0()
                .text_color(p.text_muted)
                .group_hover(group, move |s| s.text_color(p.danger)),
        )
}

/// A button that opens a menu (an `icon_button` a gpui-component Popover can hang a menu
/// from), so a ⋯ button looks and behaves like the buttons beside it.
#[derive(IntoElement)]
pub struct MenuTrigger {
    base: Stateful<gpui::Div>,
    open: bool,
}

impl MenuTrigger {
    pub fn new(base: Stateful<gpui::Div>) -> Self {
        Self { base, open: false }
    }
}

impl gpui_component::Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl gpui::RenderOnce for MenuTrigger {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        self.base
    }
}

/// The icon for a Local Shell's program: PowerShell 7, Windows PowerShell, Command Prompt, or
/// Tux for WSL and Unix shells. `None` for anything else, which gets the generic terminal icon.
pub fn shell_icon(program: &str) -> Option<&'static str> {
    let file = program.rsplit(['\\', '/']).next().unwrap_or(program).to_lowercase();
    let name = file.strip_suffix(".exe").unwrap_or(&file);
    match name {
        "pwsh" | "pwsh-preview" | "powershell" => Some("tabler:brand-powershell"),
        "cmd" => Some("tabler:terminal-2"),
        "wsl" => Some("simpleicons:linux"),
        // Git Bash and friends on Windows are not Linux; elsewhere these are the system's shell.
        "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh" if !file.ends_with(".exe") => Some("simpleicons:linux"),
        _ => None,
    }
}

/// Automatic shell accents stay legible on each theme; manual Host/Project icons use their chosen color.
pub fn shell_icon_color(icon: &str, p: AppPalette) -> Hsla {
    crate::icons::find_symbol(icon).and_then(|icon| icon.color(p.dark)).unwrap_or(p.text_muted)
}

type ShellFrames = Vec<(u16, std::sync::Arc<gpui::Image>)>;

/// Native small-icon frames first, large artwork last. Sources are in assets/shells/README.md.
fn shell_frames(name: &str) -> Option<&'static ShellFrames> {
    use std::sync::{Arc, OnceLock};
    static IMAGES: OnceLock<Vec<(&'static str, ShellFrames)>> = OnceLock::new();
    let images = IMAGES.get_or_init(|| {
        let png = |bytes: &[u8]| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()));
        vec![
            ("shell:pwsh", vec![
                (16, png(include_bytes!("../../assets/shells/pwsh-16.png"))),
                (20, png(include_bytes!("../../assets/shells/pwsh-20.png"))),
                (24, png(include_bytes!("../../assets/shells/pwsh-24.png"))),
                (32, png(include_bytes!("../../assets/shells/pwsh-32.png"))),
                (40, png(include_bytes!("../../assets/shells/pwsh-40.png"))),
                (48, png(include_bytes!("../../assets/shells/pwsh-48.png"))),
                (64, png(include_bytes!("../../assets/shells/pwsh-64.png"))),
                (256, png(include_bytes!("../../assets/shells/pwsh.png"))),
            ]),
            ("shell:powershell", vec![
                (16, png(include_bytes!("../../assets/shells/powershell-16.png"))),
                (32, png(include_bytes!("../../assets/shells/powershell-32.png"))),
                (256, png(include_bytes!("../../assets/shells/powershell.png"))),
            ]),
            ("shell:cmd", vec![
                (16, png(include_bytes!("../../assets/shells/cmd-16.png"))),
                (32, png(include_bytes!("../../assets/shells/cmd-32.png"))),
                (48, png(include_bytes!("../../assets/shells/cmd-48.png"))),
                (256, png(include_bytes!("../../assets/shells/cmd.png"))),
            ]),
            ("shell:linux", vec![(32, png(include_bytes!("../../assets/shells/linux.png")))]),
        ]
    });
    images.iter().find(|(n, _)| *n == name).map(|(_, frames)| frames)
}

fn shell_frame(frames: &ShellFrames, physical_size: f32) -> &(u16, std::sync::Arc<gpui::Image>) {
    frames.iter().find(|(size, _)| f32::from(*size) >= physical_size.ceil()).unwrap_or_else(|| frames.last().unwrap())
}

#[derive(IntoElement)]
struct ShellArtwork {
    frames: &'static ShellFrames,
    size: f32,
}

impl gpui::RenderOnce for ShellArtwork {
    fn render(self, window: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        // Choose at render time so moving between monitors also updates the source artwork.
        let (_, image) = shell_frame(self.frames, self.size * window.scale_factor());
        gpui::img(image.clone()).size(px(self.size)).flex_shrink_0()
    }
}

/// Whether an icon name is full-colour artwork rather than a Lucide icon drawn in one colour.
pub fn is_artwork(name: &str) -> bool {
    name.starts_with("shell:") || crate::icons::find(name).is_some()
}

/// A Lucide icon by name (e.g. "server"), drawn in `color`. `shell:` and `devicon:` IDs
/// preserve their full-color artwork instead.
pub fn icon_el(name: &str, size: f32, color: Hsla) -> AnyElement {
    if let Some(icon) = crate::icons::find_symbol(name) {
        return svg().data(icon.svg).size(px(size)).flex_shrink_0().text_color(color).into_any_element();
    }
    if let Some(icon) = crate::icons::find(name) {
        return gpui::img(icon.image()).size(px(size)).object_fit(gpui::ObjectFit::Contain).flex_shrink_0().into_any_element();
    }
    if let Some(frames) = shell_frames(name) {
        return ShellArtwork { frames, size }.into_any_element();
    }
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .into_any_element()
}

/// A turning `loader-circle`, for work that is under way (connecting, loading a folder).
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> AnyElement {
    svg()
        .path("icons/loader-circle.svg")
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .with_animation(id, Animation::new(std::time::Duration::from_millis(900)).repeat(), |icon, delta| {
            icon.with_transformation(Transformation::rotate(percentage(delta)))
        })
        .into_any_element()
}

/// The least time a spinner shows, so work that takes milliseconds doesn't flash it.
pub const MIN_SPIN: Duration = Duration::from_millis(500);
/// How long the check stays after work succeeds, then how long it takes to fade out.
const DONE_HOLD: Duration = Duration::from_millis(1200);
const DONE_FADE: Duration = Duration::from_millis(800);
/// A wait shown in place of content appears only when it lasts longer than this.
const WAIT_DELAY: Duration = Duration::from_millis(300);
const WAIT_FADE_IN: Duration = Duration::from_millis(200);

/// Work under way, shown as a spinner and, once it succeeds, as a check that fades out.
/// The spinner stays at least `MIN_SPIN`, so quick work reads as "working, done" rather
/// than a flicker. A failure clears it at once: the error says what happened.
///
/// The phase follows the clock, so the owner calls `schedule_redraws` (`finish` does it)
/// to render again when the spinner turns into the check and when the check is gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    started: Option<Instant>,
    ended: Option<Instant>,
    failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusyPhase {
    Idle,
    Working,
    Done,
}

impl Busy {
    /// Work is under way. Work that follows other work, even one still finishing its
    /// spinner's minimum time, carries on the same spinner.
    pub fn start(&mut self) {
        if self.is_working() {
            return;
        }
        if self.done_times().is_some_and(|(check, _)| Instant::now() < check) {
            self.ended = None;
        } else {
            *self = Self { started: Some(Instant::now()), ended: None, failed: false };
        }
    }

    /// Part of the work under way failed: no check when it ends.
    pub fn fail(&mut self) {
        if self.is_working() {
            self.failed = true;
        }
    }

    /// The work ended: a check follows if it succeeded, nothing if it failed.
    pub fn finish<T: 'static>(&mut self, ok: bool, cx: &mut gpui::Context<T>) {
        if !self.is_working() {
            return;
        }
        if !ok || self.failed {
            self.clear();
            return;
        }
        self.ended = Some(Instant::now());
        self.schedule_redraws(cx);
    }

    /// Show nothing, at once.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn phase(&self) -> BusyPhase {
        if self.started.is_none() {
            return BusyPhase::Idle;
        }
        let Some((check, gone)) = self.done_times() else { return BusyPhase::Working };
        let now = Instant::now();
        if now < check {
            BusyPhase::Working
        } else if now < gone {
            BusyPhase::Done
        } else {
            BusyPhase::Idle
        }
    }

    /// Render `cx`'s view again when the spinner turns into the check and when the check is gone.
    pub fn schedule_redraws<T: 'static>(&self, cx: &mut gpui::Context<T>) {
        let Some((check, gone)) = self.done_times() else { return };
        for at in [check, gone] {
            // A little late rather than early, so the phase has changed by then.
            let wait = at.saturating_duration_since(Instant::now()) + Duration::from_millis(15);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    fn is_working(&self) -> bool {
        self.started.is_some() && self.ended.is_none()
    }

    /// When the check appears and when it's gone, once the work has ended.
    fn done_times(&self) -> Option<(Instant, Instant)> {
        let (started, ended) = (self.started?, self.ended?);
        let check = ended.max(started + MIN_SPIN);
        Some((check, check + DONE_HOLD + DONE_FADE))
    }
}

/// Content for `BusyPhase::Done`. `draw` gets the check's opacity: 1 while it holds,
/// then falling to 0, so the check and anything beside it fade out together.
pub fn busy_done(id: impl Into<ElementId>, draw: impl Fn(f32) -> AnyElement + 'static) -> AnyElement {
    let total = (DONE_HOLD + DONE_FADE).as_secs_f32();
    let hold = DONE_HOLD.as_secs_f32() / total;
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .with_animation(id, Animation::new(DONE_HOLD + DONE_FADE), move |el, delta| {
            let fade = ((delta - hold) / (1. - hold)).clamp(0., 1.);
            el.child(draw(1. - gpui::ease_in_out(fade)))
        })
        .into_any_element()
}

/// A wait shown in place of content (a placeholder with a spinner): it fades in only once
/// the wait passes `WAIT_DELAY`, so content that arrives sooner replaces nothing visible.
/// Keep `id` the same across consecutive waits, so the next one doesn't start over.
pub fn after_delay<E: Styled + IntoElement + 'static>(id: impl Into<ElementId>, el: E) -> AnyElement {
    let total = (WAIT_DELAY + WAIT_FADE_IN).as_secs_f32();
    let delay = WAIT_DELAY.as_secs_f32() / total;
    el.with_animation(id, Animation::new(WAIT_DELAY + WAIT_FADE_IN), move |el, delta| {
        el.opacity(((delta - delay) / (1. - delay)).clamp(0., 1.))
    })
    .into_any_element()
}

/// A ring around an icon showing how far something is: a faint full circle, and over it an
/// arc from 12 o'clock clockwise to `fraction` (0 to 1). `None`: the amount isn't known, so a
/// quarter arc turns instead. `size` is the ring's outer diameter.
pub fn progress_ring(id: impl Into<ElementId>, fraction: Option<f32>, size: f32, stroke: f32, track: Hsla, color: Hsla) -> AnyElement {
    let paint = move |bounds: gpui::Bounds<gpui::Pixels>, start: f32, sweep: f32, window: &mut gpui::Window| {
        let center = bounds.center();
        let radius = (size - stroke) / 2.;
        if let Some(path) = arc_path(center, radius, stroke, 0., 1.) {
            window.paint_path(path, track);
        }
        if let Some(path) = arc_path(center, radius, stroke, start, sweep) {
            window.paint_path(path, color);
        }
    };
    let id = id.into();
    match fraction {
        // The fill glides to the progress reported, which comes in steps (a redraw every so
        // often, a buffer of bytes at a time), instead of jumping from one to the next.
        Some(fraction) => div()
            .size(px(size))
            .child(
                gpui::canvas(
                    move |_, window, cx| GlidingFill::shown(id, fraction.clamp(0., 1.), window, cx),
                    move |bounds, shown, window, _| paint(bounds, 0., shown, window),
                )
                .size(px(size)),
            )
            .into_any_element(),
        None => div()
            .size(px(size))
            .with_animation(id, Animation::new(std::time::Duration::from_millis(1000)).repeat(), move |el, delta| {
                el.child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| paint(bounds, delta, 0.25, window)).size(px(size)))
            })
            .into_any_element(),
    }
}

/// A thin progress bar: a faint track, and over it a fill from the left to `fraction` (0 to 1)
/// that glides to new progress like `progress_ring`'s arc. `None`: the amount isn't known, so
/// a short segment sweeps along the track instead, or rests at its start with reduced motion.
pub fn progress_bar(id: impl Into<ElementId>, fraction: Option<f32>, height: f32, track: Hsla, color: Hsla, reduce_motion: bool) -> AnyElement {
    /// How much of the track the sweeping segment covers.
    const SEGMENT: f32 = 0.3;
    let radius = px(height / 2.);
    let paint = move |bounds: gpui::Bounds<gpui::Pixels>, from: f32, to: f32, window: &mut gpui::Window| {
        window.paint_quad(gpui::fill(bounds, track).corner_radii(radius));
        let width = f32::from(bounds.size.width);
        let (start, end) = (from.clamp(0., 1.) * width, to.clamp(0., 1.) * width);
        if end - start >= 0.5 {
            let filled = gpui::Bounds::new(point(bounds.origin.x + px(start), bounds.origin.y), gpui::size(px(end - start), bounds.size.height));
            window.paint_quad(gpui::fill(filled, color).corner_radii(radius));
        }
    };
    let id = id.into();
    let bar = div().w_full().h(px(height));
    match fraction {
        Some(fraction) => bar
            .child(
                gpui::canvas(
                    move |_, window, cx| GlidingFill::shown(id, fraction.clamp(0., 1.), window, cx),
                    move |bounds, shown, window, _| paint(bounds, 0., shown, window),
                )
                .size_full(),
            )
            .into_any_element(),
        None if reduce_motion => {
            bar.child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| paint(bounds, 0., SEGMENT, window)).size_full()).into_any_element()
        }
        None => bar
            .with_animation(id, Animation::new(Duration::from_millis(1400)).repeat(), move |el, delta| {
                // In from beyond the left edge and out past the right one, so it never jumps.
                let start = -SEGMENT + (1. + SEGMENT) * gpui::ease_in_out(delta);
                el.child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| paint(bounds, start, start + SEGMENT, window)).size_full())
            })
            .into_any_element(),
    }
}

/// How full a determinate `progress_ring` or `progress_bar` is drawn, kept from frame to frame.
struct GlidingFill {
    shown: f32,
    at: std::time::Instant,
}

impl GlidingFill {
    /// How long the fill takes to cover about two thirds of the way to new progress. Close to
    /// how often progress is redrawn, so a steady download fills at a steady pace.
    const LAG: f32 = 0.18;

    /// The fraction to draw this frame for a ring showing `target`, asking for the next frame
    /// while it is still on its way there.
    fn shown(id: ElementId, target: f32, window: &mut gpui::Window, cx: &mut gpui::App) -> f32 {
        let state = window.use_keyed_state(id, cx, |_, _| GlidingFill { shown: 0., at: std::time::Instant::now() });
        let now = std::time::Instant::now();
        let shown = state.update(cx, |fill, _| {
            let elapsed = now.duration_since(fill.at).as_secs_f32();
            fill.at = now;
            fill.shown = glide(fill.shown, target, elapsed, Self::LAG);
            fill.shown
        });
        // Reduced motion: no glide, the ring shows the progress as it comes.
        if cx.reduce_motion() {
            state.update(cx, |fill, _| fill.shown = target);
            return target;
        }
        if shown != target {
            window.request_animation_frame();
        }
        shown
    }
}

/// Move `shown` towards `target` after `elapsed` seconds, covering `1 - 1/e` of the way every
/// `lag` seconds. It never runs backwards (progress that restarts is shown at once) and lands
/// on `target` once less than a thousandth is left, so it comes to rest.
fn glide(shown: f32, target: f32, elapsed: f32, lag: f32) -> f32 {
    if target <= shown {
        return target;
    }
    let next = shown + (target - shown) * (1. - (-elapsed.max(0.) / lag).exp());
    if target - next < 0.001 { target } else { next }
}

/// A stroked circular arc starting `start` turns clockwise from 12 o'clock and running
/// `sweep` turns, built from cubic Béziers of at most a quarter turn each.
fn arc_path(center: gpui::Point<gpui::Pixels>, radius: f32, stroke: f32, start: f32, sweep: f32) -> Option<gpui::Path<gpui::Pixels>> {
    if sweep <= 0.001 {
        return None;
    }
    let at = |turns: f32| {
        let angle = turns * std::f32::consts::TAU;
        (f32::from(center.x) + radius * angle.sin(), f32::from(center.y) - radius * angle.cos())
    };
    let segments = (sweep * 4.).ceil().max(1.) as usize;
    let step = sweep / segments as f32;
    // Control points lie along the tangents, k·radius from each end of a segment.
    let k = 4. / 3. * (step * std::f32::consts::TAU / 4.).tan() * radius;
    let mut path = gpui::PathBuilder::stroke(px(stroke));
    let (x, y) = at(start);
    path.move_to(point(px(x), px(y)));
    for i in 0..segments {
        let (a, b) = (start + step * i as f32, start + step * (i + 1) as f32);
        let ((ax, ay), (bx, by)) = (at(a), at(b));
        let (ta, tb) = (a * std::f32::consts::TAU, b * std::f32::consts::TAU);
        // Clockwise on screen: the tangent at angle t is (cos t, sin t).
        let c1 = point(px(ax + k * ta.cos()), px(ay + k * ta.sin()));
        let c2 = point(px(bx - k * tb.cos()), px(by - k * tb.sin()));
        path.cubic_bezier_to(point(px(bx), px(by)), c1, c2);
    }
    path.build().ok()
}

/// Islands are the rounded panels the window is made of (sidebar, terminal).
/// Rows inside them have an 8px radius and sit 8px from the edge: 8 + 8 = 16.
pub const ISLAND_RADIUS: f32 = 16.;
pub const ISLAND_GAP: f32 = 8.;

pub fn island(p: AppPalette) -> gpui::Div {
    div()
        .rounded(p.corner(ISLAND_RADIUS))
        .bg(p.background)
        .border_1()
        .border_color(p.border)
}

/// How a control shows keyboard focus: a 2px ring painted inside its edge. Widening the
/// border instead would shrink the content box, so the label would shift as Tab moves.
pub fn focus_outline(s: gpui::StyleRefinement, color: Hsla) -> gpui::StyleRefinement {
    s.border_color(color).shadow(vec![BoxShadow {
        color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: true,
    }])
}

/// Small uppercase section label. Copy stays in natural case; it is uppercased here.
pub fn section_label(text: &str, p: AppPalette) -> impl IntoElement {
    tracked(&text.to_uppercase(), 10.5, FontWeight::SEMIBOLD, 0.6, p.text_faint, p.font_family)
}

/// Width of the logo per pixel of height.
const LOGO_ASPECT: f32 = 988. / 167.;

/// The borderless Komando wordmark, using the theme's text color for contrast.
pub fn wordmark(height: f32, color: Hsla) -> impl IntoElement {
    svg()
        .path(crate::LOGO_SVG)
        .h(px(height))
        .w(px((height * LOGO_ASPECT).round()))
        .flex_shrink_0()
        .text_color(color)
}

fn tracked(text: &str, size: f32, weight: FontWeight, tracking: f32, color: Hsla, family: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(tracking))
        .font_family(family)
        .text_size(px(size))
        .line_height(px((size * 1.1).ceil()))
        .font_weight(weight)
        .text_color(color)
        .children(text.chars().map(|c| div().child(c.to_string())))
}

/// A coloured square with an icon: how Projects and Hosts are told apart at a glance.
pub fn badge(icon: &str, color: Hsla, size: f32) -> impl IntoElement {
    let letter = icon.strip_prefix("letter:").map(|s| s.to_string());
    div()
        .size(px(size))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px((size * 0.28).round()))
        .bg(color.opacity(0.16))
        .text_color(color)
        // A shell's own icon is coloured already: shown as it is, without the tinted square.
        .when(is_artwork(icon), |el| el.bg(gpui::transparent_black()))
        .map(|el| match letter {
            None if is_artwork(icon) => el.child(icon_el(icon, (size * 0.85).round(), color)),
            Some(letter) => el
                .text_size(px(size * 0.5))
                .font_weight(FontWeight::BOLD)
                .child(letter),
            None => el.child(icon_el(icon, (size * 0.6).round(), color)),
        })
}

/// A small status dot: green connected, amber connecting, grey disconnected.
pub fn status_dot(color: Hsla) -> impl IntoElement {
    div().size(px(7.)).rounded_full().flex_shrink_0().bg(color)
}

/// A small ✕ drawn for the corner of the connection bars: thicker strokes than the 24px
/// `x` icon, which turns into a hairline at this size.
const TIMED_OUT_MARK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path d="M3.5 3.5l7 7M10.5 3.5l-7 7" fill="none" stroke="black" stroke-width="2.6" stroke-linecap="round"/></svg>"#;

/// How long the ✕ of a timed-out connection takes to settle in.
const TIMED_OUT_ENTER: Duration = Duration::from_millis(260);

/// One pass of the searching bars, including the rest before the next.
const SEARCH_CYCLE: Duration = Duration::from_millis(1600);

/// How lit searching bar `index` is, `delta` of the way through a cycle: each bar fades in
/// a step after the one before it, all four fade out together, and the row rests dim.
fn searching_level(index: usize, delta: f32) -> f32 {
    let ease = |x: f32| {
        let x = x.clamp(0., 1.);
        x * x * (3. - 2. * x)
    };
    let on = 0.08 + index as f32 * 0.16;
    ease((delta - on) / 0.08) * (1. - ease((delta - 0.8) / 0.12))
}

/// SSH connection quality, with a stable footprint and a hover target larger than the bars.
/// Local shells and Remote Desktops keep their ordinary connection status dot.
pub fn connection_indicator(
    status: &crate::session::Status,
    hops: &[crate::session::latency::HopLatency],
    reduce_motion: bool,
    p: AppPalette,
) -> impl IntoElement {
    use crate::session::Status;
    use crate::session::latency::{ReplyState, weakest_hop};

    let overdue = hops.iter().any(|hop| matches!(hop.state, ReplyState::Slow | ReplyState::Unresponsive));
    // Until every hop has finished its warm-up the bars search, like a phone looking for
    // signal: an early reading would look worse than the connection really is.
    let searching = match status {
        Status::Connecting => true,
        Status::Connected => !overdue && (hops.is_empty() || hops.iter().any(|hop| hop.state == ReplyState::Measuring)),
        Status::Disconnected(_) => false,
    };
    // The weakest hop drives the bars, including an overdue Jump Host reply. All samples
    // measure from this computer; adding them together would count the early hops twice.
    let weakest = weakest_hop(hops);
    let (bars, color) = match (status, weakest) {
        (Status::Connected, Some(hop)) => {
            let color = match hop.state {
                ReplyState::Measuring | ReplyState::Unavailable => p.text_faint,
                ReplyState::Slow => p.warning,
                ReplyState::Unresponsive => p.danger,
                ReplyState::Responsive => match hop.bars() {
                    3..=4 => p.success,
                    2 => p.warning,
                    _ => p.danger,
                },
            };
            (hop.bars(), color)
        }
        _ => (0, p.text_faint),
    };
    // The final hop's round trip includes the whole route through any Jump Hosts.
    let target = hops.iter().max_by_key(|hop| hop.hop).and_then(|hop| hop.round_trip);
    let timed_out = status.timed_out();
    let tip: SharedString = match (status, weakest.map(|hop| hop.state)) {
        (Status::Connecting, _) => "Connecting…".into(),
        _ if timed_out => "Connection timed out".into(),
        (Status::Disconnected(_), _) => "Disconnected".into(),
        _ if searching => "Measuring latency…".into(),
        (_, Some(ReplyState::Unresponsive)) => "No reply for over 5 s".into(),
        (_, Some(ReplyState::Slow)) => "Waiting for a reply…".into(),
        _ => match target {
            Some(rtt) if rtt < Duration::from_millis(1) => "<1 ms".into(),
            Some(rtt) => format!("{} ms", rtt.as_millis()).into(),
            None => "Latency unavailable".into(),
        },
    };
    let label = match (status, weakest) {
        _ if timed_out => "SSH connection: timed out".to_string(),
        (Status::Disconnected(_), _) => "SSH connection: disconnected".to_string(),
        _ if searching => "SSH connection: measuring latency".to_string(),
        (_, Some(hop)) => format!("SSH connection: {bars} of 4 bars · {}", hop.description(Instant::now())),
        _ => "SSH connection: latency unavailable".to_string(),
    };
    let bar = |index: usize| div().w(px(3.)).h(px(4. + index as f32 * 3.)).rounded(px(1.));
    let meter = div().h(px(13.)).flex().items_end().gap(px(2.));
    let meter = if searching && !reduce_motion {
        // The bars light up one after another, shortest first, hold, then fade out together.
        meter
            .with_animation("connection-searching", Animation::new(SEARCH_CYCLE).repeat(), move |el, delta| {
                el.children((0..4).map(|index| {
                    let lit = searching_level(index, delta);
                    bar(index).bg(p.text_muted.opacity(0.22 + 0.78 * lit))
                }))
            })
            .into_any_element()
    } else if searching {
        meter.children((0..4).map(|index| bar(index).bg(p.text_muted.opacity(0.35)))).into_any_element()
    } else if timed_out {
        // Like a phone without service: the bars go quiet and a ✕ sits in the empty corner
        // above the short ones, settling in from slightly smaller.
        let mark = svg()
            .data(TIMED_OUT_MARK)
            .absolute()
            .left(px(-1.))
            .top(px(-1.))
            .size(px(7.))
            .text_color(p.danger);
        let mark = if reduce_motion {
            mark.into_any_element()
        } else {
            mark.with_animation(
                "connection-timed-out",
                Animation::new(TIMED_OUT_ENTER).with_easing(gpui::ease_out_quint()),
                |mark, t| {
                    let scale = 0.55 + 0.45 * t;
                    mark.opacity(t).with_transformation(Transformation::scale(gpui::size(scale, scale)))
                },
            )
            .into_any_element()
        };
        meter
            .relative()
            .children((0..4).map(|index| bar(index).bg(p.text_faint.opacity(0.28))))
            .child(mark)
            .into_any_element()
    } else {
        meter
            .children((0..4).map(|index| bar(index).bg(if index < bars as usize { color } else { color.opacity(0.25) })))
            .into_any_element()
    };
    div()
        .id("connection-quality")
        .role(gpui::Role::Status)
        .aria_label(label)
        .w(px(22.))
        .h(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(meter)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_ring_glides_towards_new_progress_and_comes_to_rest() {
        use super::glide;
        let lag = 0.2;
        // One frame in, it has moved part of the way, not all of it.
        let first = glide(0.1, 0.5, 1. / 60., lag);
        assert!(first > 0.1 && first < 0.2, "{first}");
        // Frame by frame it gets there, never overshooting, and stops exactly on it.
        let mut shown = 0.1;
        let mut frames = 0;
        while shown != 0.5 {
            let next = glide(shown, 0.5, 1. / 60., lag);
            assert!(next > shown && next <= 0.5);
            shown = next;
            frames += 1;
            assert!(frames < 120, "still moving after two seconds");
        }
        // Progress that starts over shows at once, and a long gap (the window was hidden) lands.
        assert_eq!(glide(0.8, 0.2, 1. / 60., lag), 0.2);
        assert_eq!(glide(0.2, 0.9, 5., lag), 0.9);
    }

    use super::*;

    #[test]
    fn searching_bars_fill_from_the_shortest_and_loop_without_a_jump() {
        for index in 0..4 {
            assert_eq!(searching_level(index, 0.), 0.);
            assert_eq!(searching_level(index, 1.), 0.);
            assert_eq!(searching_level(index, 0.78), 1.);
        }
        // Mid-fill, every shorter bar is at least as lit as a taller one.
        for delta in [0.2, 0.35, 0.5, 0.6] {
            let levels: Vec<f32> = (0..4).map(|index| searching_level(index, delta)).collect();
            assert!(levels.windows(2).all(|pair| pair[0] >= pair[1]), "{delta}: {levels:?}");
            assert!(levels[0] == 1. && levels[3] < 1.);
        }
    }

    #[test]
    fn the_timed_out_mark_decodes_in_the_actual_svg_renderer() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(gpui_kit_assets::AllAssets));
        renderer.render_single_frame(TIMED_OUT_MARK, 1.).unwrap();
    }

    #[test]
    fn shell_artwork_uses_native_frames_for_the_display_scale() {
        let pwsh = shell_frames("shell:pwsh").unwrap();
        for (scale, expected) in [(1., 16), (1.25, 20), (1.5, 24), (2., 32), (2.5, 40), (3., 48), (4., 64)] {
            assert_eq!(shell_frame(pwsh, 15. * scale).0, expected);
        }
        for name in ["shell:pwsh", "shell:powershell", "shell:cmd"] {
            let frames = shell_frames(name).unwrap();
            assert_eq!(shell_frame(frames, 15.).0, 16);
            assert_eq!(shell_frame(frames, 30.).0, 32);
            assert_eq!(shell_frame(frames, 128.).0, 256);
            assert_eq!(shell_frame(frames, 512.).0, 256);
            assert!(frames.windows(2).all(|pair| pair[0].0 < pair[1].0));
        }
        assert_eq!(shell_frame(shell_frames("shell:linux").unwrap(), 15.).0, 32);
    }

    #[test]
    fn shells_get_their_own_icons() {
        assert_eq!(shell_icon("pwsh.exe"), Some("tabler:brand-powershell"));
        assert_eq!(shell_icon(r"C:\Program Files\PowerShell\7\pwsh.exe"), Some("tabler:brand-powershell"));
        assert_eq!(shell_icon("powershell.exe"), Some("tabler:brand-powershell"));
        assert_eq!(shell_icon("CMD.EXE"), Some("tabler:terminal-2"));
        assert_eq!(shell_icon("wsl.exe"), Some("simpleicons:linux"));
        assert_eq!(shell_icon("/bin/bash"), Some("simpleicons:linux"));
        // Git Bash on Windows isn't Linux.
        assert_eq!(shell_icon(r"C:\Program Files\Git\bin\bash.exe"), None);
        assert_eq!(shell_icon("nu.exe"), None);
        for name in ["shell:pwsh", "shell:powershell", "shell:cmd", "shell:linux"] {
            assert!(shell_frames(name).is_some(), "{name}");
        }
        // Every default shell on this platform has one.
        for shell in crate::model::default_shells() {
            assert!(shell_icon(&shell.program).is_some(), "{}", shell.program);
        }
    }
}
