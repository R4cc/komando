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
        let menu = PopupMenu::build(window, cx, build);
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, move |this: &mut V, _, _: &DismissEvent, _, cx| {
            *field(this) = Popup::default();
            cx.notify();
        });
        Popup { menu: Some((menu, position)), _subscription: Some(subscription) }
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_some()
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

/// A menu entry that runs `run` on a view.
pub fn item<V: 'static>(
    label: impl Into<String>,
    icon: &str,
    view: &WeakEntity<V>,
    run: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label.into()).icon(Icon::empty().path(format!("icons/{icon}.svg"))).on_click(
        move |_, window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| run(this, window, cx));
        },
    )
}
