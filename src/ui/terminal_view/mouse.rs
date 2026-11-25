//! The mouse: selecting, links, reporting to programs, the context menu, the cursor's blink,
//! and the minimap and scrollbar.

use super::*;

impl TerminalView {
    /// The viewport cell under `position`, clamped to the grid.
    pub(super) fn cell_at(&self, position: gpui::Point<Pixels>) -> (usize, usize) {
        let origin = self.origin.get();
        let (cw, ch) = self.cell.get();
        let size = self.grid_size();
        let x = (f32::from(position.x - origin.x) / cw).max(0.0) as usize;
        let y = (f32::from(position.y - origin.y) / ch).max(0.0) as usize;
        (x.min(size.cols.saturating_sub(1) as usize), y.min(size.rows.saturating_sub(1) as usize))
    }

    pub(super) fn grid_point(&self, position: gpui::Point<Pixels>) -> (GridPoint, Side) {
        let origin = self.origin.get();
        let (cw, _) = self.cell.get();
        let (col, row) = self.cell_at(position);
        let x = f32::from(position.x - origin.x) / cw;
        // Past the left edge the selection starts at the line's start, past the right edge it
        // takes the last column in.
        let side = if x <= 0.0 {
            Side::Left
        } else if x >= self.grid_size().cols as f32 {
            Side::Right
        } else if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.term.lock().grid().display_offset() as i32;
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side)
    }

    /// The link under the pointer, if any.
    ///
    /// `grid_point` locks the terminal itself, so it has to run before the lock here is taken:
    /// `link_at(&*self.term.lock(), self.grid_point(..))` locked it twice on the UI thread, and
    /// the terminal's mutex isn't reentrant, so the window hung for good.
    /// The rules colouring this terminal's output: none while its highlighting is off. The
    /// empty set is shared, so the grid sees the same one from frame to frame.
    pub(super) fn highlighter(&self, cx: &App) -> Arc<highlight::Highlighter> {
        static NONE: std::sync::LazyLock<Arc<highlight::Highlighter>> = std::sync::LazyLock::new(Default::default);
        match cx.try_global::<ActiveHighlights>() {
            Some(active) if self.highlight => active.0.clone(),
            _ => NONE.clone(),
        }
    }

    /// Whether a click with these keys opens a link at once: the chosen key, and Ctrl (Cmd on
    /// macOS) when a plain click opens links, so programs that take the mouse still allow it.
    pub(super) fn link_key_held(&self, m: &gpui::Modifiers) -> bool {
        match self.pointer.link_click {
            LinkClick::Ctrl | LinkClick::Click => m.control || m.platform,
            LinkClick::Alt => m.alt,
        }
    }

    pub(super) fn link_under(&self, position: gpui::Point<Pixels>) -> Option<TermLink> {
        let at = self.grid_point(position).0;
        link_at(&*self.term.lock(), at)
    }

    /// The mouse mode the program asked for, if clicks should go to it. Shift held down
    /// always means "select text" instead, as in xterm.
    pub(super) fn mouse_mode(&self, shift: bool) -> Option<MouseMode> {
        if shift || self.prompt.is_some() || self.sink.is_none() || self.is_disconnected() {
            return None;
        }
        let mouse = MouseMode::of(*self.term.lock().mode());
        mouse.reporting().then_some(mouse)
    }

    pub(super) fn report_mouse(
        &mut self,
        mode: MouseMode,
        button: keys::MouseButton,
        action: MouseAction,
        position: gpui::Point<Pixels>,
        modifiers: &gpui::Modifiers,
    ) {
        let cell = self.cell_at(position);
        // Motion is reported once per cell, not once per pixel.
        if action == MouseAction::Motion && self.mouse_cell == Some(cell) {
            return;
        }
        self.mouse_cell = Some(cell);
        let mods = Mods { ctrl: modifiers.control, alt: modifiers.alt, shift: false };
        if let Some(bytes) = keys::mouse_report(mode, button, action, cell.0, cell.1, mods) {
            self.write(bytes);
        }
    }

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A question is open: a click anywhere (its field included, since the card sits inside
        // the terminal) keeps the keyboard on the question and selects nothing behind it.
        // A right-click that reaches the terminal's wrapper opens its menu (see `context_menu`);
        // one that means something else stops here.
        if self.prompt.is_some() {
            if event.button == MouseButton::Right {
                cx.stop_propagation();
            }
            self.focus_prompt(window, cx);
            return;
        }
        window.focus(&self.focus, cx);
        // A right-click on the minimap is never the program's or a paste: it opens the menu.
        if event.button == MouseButton::Right {
            self.menu_on_minimap = std::mem::take(&mut self.minimap_right_press);
            if self.menu_on_minimap {
                self.menu_point = None;
                return;
            }
        }
        // The link key and click (Ctrl+click by default) open a link, even in a program that
        // took the mouse.
        self.link_press = None;
        if event.button == MouseButton::Left
            && self.link_key_held(&event.modifiers)
            && let Some(link) = self.link_under(event.position)
        {
            cx.open_url(&link.url);
            return;
        }
        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            // The program asked for the mouse, right button included; shift+right-click is
            // still the menu.
            if event.button == MouseButton::Right {
                cx.stop_propagation();
            }
            let button = report_button(event.button);
            self.mouse_pressed = Some(button);
            self.report_mouse(mode, button, MouseAction::Press, event.position, &event.modifiers);
            return;
        }
        if event.button == MouseButton::Right {
            self.menu_point = Some(self.grid_point(event.position).0);
        }
        match event.button {
            MouseButton::Left => {
                // A plain click opens a link on release, if it didn't select anything.
                if self.pointer.link_click == LinkClick::Click && event.click_count == 1 && !event.modifiers.modified() {
                    self.link_press = self.link_under(event.position);
                }
                let (pt, side) = self.grid_point(event.position);
                let kind = match event.click_count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                self.term.lock().selection = Some(Selection::new(kind, pt, side));
                self.selecting = true;
                self.drag_at = Some(event.position);
                cx.notify();
            }
            // A right-click that pastes (or copies); with Ctrl or Shift it opens the menu, as in PuTTY.
            MouseButton::Right if self.pointer.right_click != RightClick::Menu && !event.modifiers.control && !event.modifiers.shift => {
                cx.stop_propagation();
                let copied = self.pointer.right_click == RightClick::CopyOrPaste && self.copy_selection(cx);
                if copied {
                    self.term.lock().selection = None;
                    cx.notify();
                } else {
                    self.paste(&Paste, window, cx);
                }
            }
            MouseButton::Middle if self.pointer.middle_click_paste => {
                cx.stop_propagation();
                self.paste(&Paste, window, cx);
            }
            _ => {}
        }
    }

    /// The terminal's right-click menu: copy and paste, typing the selection into the shell,
    /// selecting everything, searching, and copying the whole output.
    pub(super) fn context_menu(view: gpui::WeakEntity<Self>, focus: FocusHandle, menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = view.upgrade() else { return menu };
        let (selected, can_type, hit, link, in_split) = {
            let this = this.read(cx);
            let highlighter = this.highlight.then(|| cx.try_global::<ActiveHighlights>().map(|h| h.0.clone())).flatten();
            let selected = this.term.lock().selection_to_string().is_some_and(|t| !t.is_empty());
            let hit = highlighter.zip(this.menu_point).and_then(|(h, at)| highlighted_at(&*this.term.lock(), &h, at));
            let link = this.menu_point.and_then(|at| link_at(&*this.term.lock(), at)).map(|l| l.url);
            (selected, this.sink.is_some() && !this.is_disconnected(), hit, link, this.in_split)
        };
        let menu = if this.read(cx).menu_on_minimap {
            menu.item(crate::ui::popup::item("Hide minimap", None, &view, |_, _, cx| cx.emit(TerminalEvent::HideMinimap)))
                .separator()
        } else {
            menu
        };
        let icon = |name: &str| Icon::empty().path(format!("icons/{name}.svg"));
        let run = |label: &str, f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            crate::ui::popup::item(label, None, &view, f)
        };
        // A highlighted word was right-clicked: its rule can be turned off from here.
        let menu = match hit {
            Some(hit) => {
                let label = format!("Unhighlight “{}”", ellipsize(&hit.matched, 32));
                let item = crate::ui::popup::item(label, None, &view, move |_, _, cx| {
                    cx.emit(TerminalEvent::Unhighlight { rule: hit.clone() })
                });
                menu.item(item).separator()
            }
            None => menu,
        };
        // A link was right-clicked: open or copy it.
        let menu = match link {
            Some(url) => {
                let copy = url.clone();
                menu.item(crate::ui::popup::item("Open link", Some("external-link"), &view, move |_, _, cx| cx.open_url(&url)))
                    .item(crate::ui::popup::item("Copy link", None, &view, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                    }))
                    .separator()
            }
            None => menu,
        };
        let menu = menu.action_context(focus)
            .menu_with_icon_and_disabled("Copy", icon("copy"), Box::new(Copy), !selected)
            .menu_with_icon_and_disabled("Paste", icon("clipboard-paste"), Box::new(Paste), !can_type)
            .item(
                run("Paste selection", |this, _, cx| this.paste_selection(cx))
                    .disabled(!(selected && can_type)),
            )
            .separator()
            .item(run("Select all", |this, _, cx| this.select_all(cx)))
            .menu_with_icon("Find in output…", icon("search"), Box::new(Find))
            .separator()
            .menu("Copy output", Box::new(crate::ui::workspace::CopyOutputText))
            .menu("Copy as .txt", Box::new(crate::ui::workspace::CopyOutputFile));
        if !in_split {
            return menu;
        }
        menu.separator().item(crate::ui::popup::item("Close split view", Some("columns-2"), &view, |_, _, cx| {
            cx.emit(TerminalEvent::CloseSplit)
        }))
    }

    /// Type the selected text into the shell, as if pasted.
    pub(super) fn paste_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.term.lock().selection_to_string().filter(|t| !t.is_empty()) else { return };
        self.paste_or_confirm(text, cx);
    }

    /// Select the whole output: the Scrollback and the screen down to its last written line
    /// (or the cursor's), not the blank rows below.
    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.keyboard_selection.select_all(&mut self.term.lock());
        cx.notify();
    }

    pub(super) fn select_output(&mut self, _: &SelectOutput, window: &mut Window, cx: &mut Context<Self>) {
        if self.windows_selection && self.focus.is_focused(window) && self.prompt.is_none()
            && !self.term.lock().mode().contains(TermMode::ALT_SCREEN)
        {
            self.select_all(cx);
        } else {
            // Keep the existing Copy output shortcut when keyboard selection is off.
            cx.propagate();
        }
    }

    pub fn set_cursor(&mut self, shape: crate::model::TerminalCursor, blinking: bool) {
        let mut config = crate::session::terminal_config(self.scrollback);
        config.semantic_escape_chars = format!("{}\t", self.pointer.word_separators);
        config.default_cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            shape: match shape {
                crate::model::TerminalCursor::Line => CursorShape::Beam,
                crate::model::TerminalCursor::Block => CursorShape::Block,
                crate::model::TerminalCursor::Underline => CursorShape::Underline,
            },
            blinking,
        };
        self.term.lock().set_options(config);
        self.reset_cursor();
    }

    pub(super) fn reset_cursor(&mut self) {
        self.cursor_epoch = Instant::now();
        self.cursor_timer = None;
    }

    /// Wake only at blink edges, and only while the terminal has the keyboard.
    pub(super) fn cursor_visible(&mut self, focused: bool, cx: &mut Context<Self>) -> bool {
        let blink = {
            let term = self.term.lock();
            focused && term.cursor_style().blinking && term.mode().contains(TermMode::SHOW_CURSOR)
        };
        if !blink {
            self.cursor_timer = None;
            return true;
        }
        let elapsed = self.cursor_epoch.elapsed();
        if self.cursor_timer.is_none() {
            let wait = Duration::from_millis(500 - (elapsed.as_millis() % 500) as u64);
            self.cursor_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    this.cursor_timer = None;
                    cx.notify();
                });
            }));
        }
        (elapsed.as_millis() / 500) % 2 == 0
    }

    /// Pointer motion over the terminal, for programs that asked for it. A selection follows
    /// the pointer through `drag_selection` instead, which hears it anywhere.
    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let link = if self.selecting || event.pressed_button.is_some() || self.prompt.is_some() {
            None
        } else {
            self.link_under(event.position)
        };
        if link != self.hover_link {
            self.hover_link = link;
            cx.notify();
        }
        if self.selecting {
            return;
        }
        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            let button = match event.pressed_button {
                Some(button) if self.mouse_pressed.is_some() => report_button(button),
                _ => keys::MouseButton::None,
            };
            if mode.wants(button, MouseAction::Motion) {
                self.report_mouse(mode, button, MouseAction::Motion, event.position, &event.modifiers);
            }
        }
    }

    /// Pointer motion anywhere in the window: a held minimap slider follows it (even off the
    /// minimap), and the overlay opens and closes with it.
    pub(super) fn minimap_hover(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.minimap_state.clone();
        // So does the scrollbar's thumb, shown while the minimap is off.
        if state.bar_dragging() || state.dragging() {
            if event.pressed_button == Some(MouseButton::Left) {
                if state.bar_dragging() {
                    crate::ui::minimap::bar_drag(&self.term, &state, event.position.y);
                } else {
                    crate::ui::minimap::drag(&self.term, &state, event.position.y);
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            // The release happened somewhere nothing reported it.
            state.bar_release();
            state.release();
            cx.notify();
        }
        self.minimap_track(event.position, event.pressed_button.is_some(), window, cx);
    }

    /// Something else has the pointer, or there is nothing for the overlay to show.
    pub(super) fn minimap_blocked(&self, window: &Window) -> bool {
        !self.minimap
            || !self.minimap_overlay
            || self.prompt.is_some()
            || self.route.is_some()
            || self.selecting
            || self.mouse_pressed.is_some()
            || !window.is_window_active()
            || {
                let term = self.term.lock();
                term.mode().contains(TermMode::ALT_SCREEN) || term.grid().history_size() == 0
            }
    }

    /// Open or close the overlay for the pointer at `position`. Merely passing over output
    /// never opens it, nor may a selection; once open it stays while the pointer is on it.
    pub(super) fn minimap_track(&mut self, position: gpui::Point<Pixels>, pressed: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::minimap::{HIDE_DELAY, Timer, hover_region};
        if !self.minimap_overlay {
            return;
        }
        let state = self.minimap_state.clone();
        let Some(bounds) = state.terminal() else {
            return;
        };
        let (at_edge, over_map) = hover_region(position, bounds, window.viewport_size().width, PADDING_X);
        let blocked = pressed || self.minimap_blocked(window);
        let now = Instant::now();
        let before = state.look();
        let timer = state.track(at_edge, over_map, blocked, now);
        if state.look() != before {
            cx.notify();
        }
        match timer {
            Some(Timer::Show(generation)) if self.minimap_delay.is_zero() => {
                if state.show(generation, now) {
                    cx.notify();
                }
            }
            Some(Timer::Show(generation)) => {
                let delay = self.minimap_delay;
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        let state = this.minimap_state.clone();
                        if !this.minimap_blocked(window) && window.is_window_hovered() {
                            if state.show(generation, Instant::now()) {
                                cx.notify();
                            }
                        } else if state.look().1 {
                            state.cancel_pending();
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Some(Timer::Hide(generation)) => {
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(HIDE_DELAY).await;
                    let _ = this.update_in(cx, |this, _, cx| {
                        if !this.minimap_state.dragging() && this.minimap_state.leave(generation, Instant::now()) {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            None => {}
        }
    }

    /// A press on the minimap. Returns whether the minimap took it.
    pub(crate) fn minimap_pressed(&mut self, y: Pixels, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A question is open: the terminal's own handler keeps the keyboard on it.
        if self.prompt.is_some() || self.route.is_some() {
            return false;
        }
        window.focus(&self.focus, cx);
        crate::ui::minimap::press(&self.term, &self.minimap_state, y);
        if self.minimap_overlay {
            self.minimap_state.open_now(Instant::now());
        }
        cx.notify();
        true
    }

    /// The left button came up. Returns whether it ended a drag of the minimap's slider. The
    /// overlay stays open; if the pointer is off it, it fades after the usual pause.
    pub(crate) fn minimap_released(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.minimap_state.dragging() {
            return false;
        }
        self.minimap_state.release();
        self.minimap_track(window.mouse_position(), false, window, cx);
        cx.notify();
        true
    }

    /// A press on the scrollbar shown while the minimap is off. Returns whether it took it.
    pub(crate) fn scrollbar_pressed(&mut self, y: Pixels, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A question is open: the terminal's own handler keeps the keyboard on it.
        if self.prompt.is_some() || self.route.is_some() {
            return false;
        }
        window.focus(&self.focus, cx);
        crate::ui::minimap::bar_press(&self.term, &self.minimap_state, y);
        cx.notify();
        true
    }

    /// The left button came up. Returns whether it let go of the scrollbar's thumb.
    pub(crate) fn scrollbar_released(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.minimap_state.bar_dragging() {
            return false;
        }
        self.minimap_state.bar_release();
        cx.notify();
        true
    }

    /// A click on the overlay's handle: open now, without waiting.
    pub(crate) fn minimap_open_now(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.minimap_blocked(window) {
            return false;
        }
        self.minimap_state.open_now(Instant::now());
        cx.notify();
        true
    }

    /// The pointer moved while a selection is being dragged, wherever it is: over the
    /// terminal, the sidebar, or outside the window (the window keeps the mouse while a button
    /// is held). Past the top or bottom of the grid the Scrollback scrolls along.
    pub(super) fn drag_selection(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The release was missed (it happened somewhere nothing reported it): stop here.
            self.selecting = false;
            self.drag_at = None;
            return;
        }
        self.drag_at = Some(event.position);
        self.extend_selection(event.position);
        if self.autoscroll_speed(event.position) != 0. && self.autoscroll.is_none() {
            // Step once per displayed frame, so it moves as smoothly as the display refreshes.
            self.autoscroll = Some((Instant::now(), 0.));
            cx.on_next_frame(window, |this, window, cx| this.autoscroll_step(window, cx));
        }
        cx.notify();
    }

    pub(super) fn extend_selection(&mut self, position: gpui::Point<Pixels>) {
        let (pt, side) = self.grid_point(position);
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(pt, side);
        }
    }

    /// Lines per second to scroll for a selection dragged to `position`: up (positive) above
    /// the grid, down below it, faster the further away; 0 over it.
    pub(super) fn autoscroll_speed(&self, position: gpui::Point<Pixels>) -> f32 {
        let top = f32::from(self.origin.get().y);
        let (_, ch) = self.cell.get();
        let bottom = top + self.grid_size().rows as f32 * ch;
        let y = f32::from(position.y);
        let speed = |distance: f32| ((1. + distance / ch) * AUTOSCROLL_SPEED).min(AUTOSCROLL_MAX_SPEED);
        if y < top {
            speed(top - y)
        } else if y >= bottom {
            -speed(y - bottom)
        } else {
            0.
        }
    }

    /// One frame of scrolling while a selection is held above or below the grid: as many
    /// lines as the time since the last frame allows, the rest carried to the next frame.
    pub(super) fn autoscroll_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((last, carry)) = self.autoscroll else { return };
        let (position, speed) = match self.drag_at {
            Some(position) if self.selecting => (position, self.autoscroll_speed(position)),
            _ => (Default::default(), 0.),
        };
        if speed == 0. {
            self.autoscroll = None;
            return;
        }
        let now = Instant::now();
        let carry = carry + speed * now.duration_since(last).min(AUTOSCROLL_MAX_STEP).as_secs_f32();
        let lines = carry.trunc();
        if lines != 0. {
            self.term.lock().scroll_display(Scroll::Delta(lines as i32));
            self.extend_selection(position);
            cx.notify();
        }
        self.autoscroll = Some((now, carry - lines));
        cx.on_next_frame(window, |this, window, cx| this.autoscroll_step(window, cx));
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A press the program saw gets its release, even if shift went down in between.
        if self.mouse_pressed.take().is_some() {
            if let Some(mode) = self.mouse_mode(false) {
                let button = report_button(event.button);
                self.report_mouse(mode, button, MouseAction::Release, event.position, &event.modifiers);
            }
            return;
        }
        if event.button != MouseButton::Left || !self.selecting {
            return;
        }
        self.selecting = false;
        self.drag_at = None;
        let selected = {
            let mut term = self.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
            term.selection.is_some()
        };
        if let Some(link) = self.link_press.take()
            && !selected
            && self.link_under(event.position).as_ref() == Some(&link)
        {
            cx.open_url(&link.url);
        } else if self.pointer.copy_on_select && selected {
            self.copy_selection(cx);
        }
        cx.notify();
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (_, ch) = self.cell.get();
        let dy = f32::from(event.delta.pixel_delta(px(ch)).y);
        // Touchpads send many small deltas: keep the remainder so they add up to lines.
        if matches!(event.touch_phase, TouchPhase::Started) || dy * self.scroll_remainder < 0. {
            self.scroll_remainder = 0.;
        }
        self.scroll_remainder += dy;
        let lines = (self.scroll_remainder / ch).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32 * ch;

        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            let button = if lines > 0 { keys::MouseButton::WheelUp } else { keys::MouseButton::WheelDown };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(mode, button, MouseAction::Press, event.position, &event.modifiers);
            }
            return;
        }
        let mode = *self.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            // Full-screen programs (less, man) that did not ask for the mouse get cursor
            // keys, unless they turned that off (DECRST 1007).
            if mode.contains(TermMode::ALTERNATE_SCROLL) && !event.modifiers.shift && self.sink.is_some() {
                let bytes = keys::alternate_scroll_bytes(lines.clamp(-10, 10), mode.contains(TermMode::APP_CURSOR));
                self.write(bytes);
            }
        } else {
            self.scroll_view(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Tell the program whether the terminal has focus, if it asked (DECSET 1004).
    pub(super) fn report_focus(&mut self, focused: bool) {
        if self.focus_reported == Some(focused) {
            return;
        }
        self.focus_reported = Some(focused);
        if self.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(keys::focus_bytes(focused).to_vec());
        }
    }
}
