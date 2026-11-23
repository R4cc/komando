//! A menu opened by a left click (a "⋯" or a split button), drawn like the right-click menus.

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, Focusable as _, IntoElement as _, ParentElement as _, Pixels, Point,
    Subscription, WeakEntity, Window, anchored, deferred, px,
};
use gpui_component::Icon;
use gpui_component::menu::{PopupMenu, PopupMenuItem};

#[derive(Default)]
pub struct Popup {
    menu: Option<(Entity<PopupMenu>, Point<Pixels>)>,
    _subscription: Option<Subscription>,
}

impl Popup {
    /// Open a menu at `position` (window coordinates). `field` finds this `Popup` in the view,
    /// so the menu can clear itself when it is dismissed.
    pub fn open<V: 'static>(
        position: Point<Pixels>,
        field: fn(&mut V) -> &mut Popup,
        window: &mut Window,
        cx: &mut Context<V>,
        build: impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu,
    ) -> Popup {
        // Opened from a button or a list: the keyboard goes back there once the menu closes.
        let previous = window.focused(cx);
        let menu = PopupMenu::build(window, cx, build);
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, move |this: &mut V, menu, _: &DismissEvent, window, cx| {
            // Unless the chosen entry moved it on, to a field or dialog it opened.
            if menu.focus_handle(cx).contains_focused(window, cx)
                && let Some(previous) = &previous
            {
                window.focus(previous, cx);
            }
            *field(this) = Popup::default();
            cx.notify();
        });
        Popup { menu: Some((menu, position)), _subscription: Some(subscription) }
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Whether the keyboard is in the open menu.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.menu.as_ref().is_some_and(|(menu, _)| menu.focus_handle(cx).contains_focused(window, cx))
    }

    pub fn render(&self) -> Option<AnyElement> {
        let (menu, position) = self.menu.clone()?;
        Some(
            deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu))
                .with_priority(10)
                .into_any_element(),
        )
    }
}

/// A menu entry that runs `run` on a view. Give familiar actions an icon when it
/// helps them stand out among the surrounding labels.
pub fn item<V: 'static>(
    label: impl Into<String>,
    icon: Option<&str>,
    view: &WeakEntity<V>,
    run: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> PopupMenuItem {
    let view = view.clone();
    let item = PopupMenuItem::new(label.into());
    let item = match icon {
        Some(icon) => item.icon(Icon::empty().path(format!("icons/{icon}.svg"))),
        None => item,
    };
    item.on_click(move |_, window, cx: &mut App| {
        let _ = view.update(cx, |this, cx| run(this, window, cx));
    })
}
