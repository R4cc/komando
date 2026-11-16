//! A Remote Desktop's toolbar, above the picture (floating over it in full screen).
//!
//! Left, what you are looking at: the protocol, the remote screen's size and the zoom. Right,
//! in three groups from what you see to where it is shown: display (scaling, quality), input
//! (keys to send and pass on, view only), and full screen. Choices live in menus, so the bar
//! keeps the same layout whatever is chosen.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, ClickEvent, Context, FontWeight, SharedString, WeakEntity, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, Selectable as _, Sizable as _};

use super::DesktopView;
use crate::desktop::keys::{self, Code};
use crate::desktop::Protocol;
use crate::model::{Quality, Scaling, ViewerOptions};
use crate::theme::{AppPalette, Appearance};
use crate::ui::widgets::elevation;

const HEIGHT: f32 = 40.;

const ESCAPE: Code = Code { scancode: 0x01, keysym: 0xff1b };
const PRINT_SCREEN: Code = Code { scancode: 0xe037, keysym: 0xff61 };

fn icon(name: &str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}

/// Change the view's options from a menu.
fn set(view: &WeakEntity<DesktopView>, change: impl Fn(&mut ViewerOptions) + 'static) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, _, cx| {
        let _ = view.update(cx, |this, cx| this.change_options(&change, cx));
    }
}

/// Send a shortcut from a menu.
fn send(view: &WeakEntity<DesktopView>, keys: &'static [Code]) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, window, cx| {
        let _ = view.update(cx, |this, cx| {
            this.send_combo(keys);
            // Back to the desktop, so typing carries on there.
            window.focus(&this.focus, cx);
        });
    }
}

impl DesktopView {
    pub(super) fn render_toolbar(&self, floating: bool, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.global::<Appearance>().app;
        let view = cx.entity().downgrade();
        let options = self.options;
        let protocol = self.protocol;
        let input = self.accepts_input();
        let fit_desktop = self.fit_desktop;

        // What you are looking at.
        let (w, h) = self.remote_size;
        let zoom = self.placement.get().map(|pl| pl.zoom.round() as u32).filter(|_| w > 0 && self.image.is_some());
        let status = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(tag(protocol.label(), p))
            .when(w > 0 && h > 0, |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(p.text_muted)
                        .child(format!("{w} × {h}")),
                )
            })
            .when_some(zoom, |el, zoom| {
                el.child(div().flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child(format!("{zoom}%")))
            })
            .when(options.view_only, |el| el.child(tag("View only", p)));

        // Display: how the picture fills the pane, and how much detail it has.
        let scale_menu = {
            let view = view.clone();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let mut menu = menu.min_w(px(240.)).label("Scaling");
                for (scaling, label) in [
                    (Scaling::Fit, "Fit to window"),
                    (Scaling::Actual, "Actual size (100%)"),
                    (Scaling::Stretch, "Stretch to fill"),
                ] {
                    menu = menu.item(
                        PopupMenuItem::new(label).checked(options.scaling == scaling).on_click(set(&view, move |o| o.scaling = scaling)),
                    );
                }
                if protocol == Protocol::Rdp {
                    let on = fit_desktop;
                    menu = menu.separator().item(
                        PopupMenuItem::new("Resize remote desktop to window")
                            .checked(on)
                            .on_click(set(&view, move |o| o.match_window = Some(!on))),
                    );
                }
                menu
            }
        };
        let quality_menu = {
            let view = view.clone();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let mut menu = menu.min_w(px(260.)).label("Picture quality · reconnects");
                for quality in Quality::ALL {
                    let row = move |_: &mut Window, _: &mut App| {
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(16.))
                            .child(quality.label())
                            .child(div().text_size(px(12.)).text_color(p.text_faint).child(quality.describe(protocol)))
                    };
                    menu = menu.item(
                        PopupMenuItem::element(row).checked(options.quality == quality).on_click(set(&view, move |o| o.quality = quality)),
                    );
                }
                menu
            }
        };

        // Input: keys this PC's keyboard can't send, which keys pass on, and view only.
        let keys_menu = {
            let view = view.clone();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                const CTRL_ALT_DEL: &[Code] = &[keys::CONTROL, keys::ALT, keys::DELETE];
                const WINDOWS: &[Code] = &[keys::WINDOWS];
                const TASK_MANAGER: &[Code] = &[keys::CONTROL, keys::SHIFT, ESCAPE];
                const PRINT: &[Code] = &[PRINT_SCREEN];
                let mut menu = menu.min_w(px(280.)).label("Send");
                for (label, keys) in [
                    ("Ctrl+Alt+Del", CTRL_ALT_DEL),
                    ("Windows key", WINDOWS),
                    ("Ctrl+Shift+Esc", TASK_MANAGER),
                    ("Print Screen", PRINT),
                ] {
                    menu = menu.item(PopupMenuItem::new(label).disabled(!input).on_click(send(&view, keys)));
                }
                menu.separator()
                    .label("Pass to the remote desktop")
                    .item(
                        PopupMenuItem::new("Windows key and shortcuts")
                            .checked(options.send_shortcuts)
                            .on_click(set(&view, |o| o.send_shortcuts = !o.send_shortcuts)),
                    )
                    .item(
                        PopupMenuItem::new("Media keys (play, pause, next)")
                            .checked(options.send_media_keys)
                            .on_click(set(&view, |o| o.send_media_keys = !o.send_media_keys)),
                    )
                    .item(
                        PopupMenuItem::new("Volume keys")
                            .checked(options.send_volume_keys)
                            .on_click(set(&view, |o| o.send_volume_keys = !o.send_volume_keys)),
                    )
            }
        };

        let scaling_icon = match options.scaling {
            Scaling::Fit => "shrink",
            Scaling::Actual => "scan",
            Scaling::Stretch => "expand",
        };
        let controls = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                Button::new("rd-scaling")
                    .ghost()
                    .small()
                    .icon(icon(scaling_icon))
                    .label(options.scaling.label())
                    .dropdown_caret(true)
                    .tooltip("Scaling")
                    .dropdown_menu(scale_menu),
            )
            .child(
                Button::new("rd-quality")
                    .ghost()
                    .small()
                    .icon(icon("gauge"))
                    .label(options.quality.label())
                    .dropdown_caret(true)
                    .tooltip("Picture quality")
                    .dropdown_menu(quality_menu),
            )
            .child(divider(p))
            .child(
                Button::new("rd-keys")
                    .ghost()
                    .small()
                    .icon(icon("keyboard"))
                    // A label keeps it a normal button: an icon-only one is a fixed square the
                    // caret spills out of.
                    .label("Keys")
                    .dropdown_caret(true)
                    .tooltip("Keyboard")
                    .dropdown_menu(keys_menu),
            )
            .child({
                let on = options.view_only;
                Button::new("rd-view-only")
                    .ghost()
                    .small()
                    .icon(icon(if on { "eye" } else { "mouse-pointer-2" }))
                    .selected(on)
                    .toggled(on)
                    .tooltip(if on { "View only: on. Click to control the desktop" } else { "View only: off. Click to only watch" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.change_options(|o| o.view_only = !on, cx)))
            })
            .child(divider(p))
            .child({
                Button::new("rd-fullscreen")
                    .ghost()
                    .small()
                    .icon(icon(if floating { "minimize-2" } else { "maximize-2" }))
                    .tooltip(if floating { "Exit full screen" } else { "Full screen" })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        window.toggle_fullscreen();
                        this.toolbar_out = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }))
            });

        let bar = div()
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .gap(px(12.))
            .pl(px(12.))
            .pr(px(6.))
            .text_size(px(12.5))
            .text_color(p.text)
            .child(status)
            .child(div().flex_1())
            .child(controls);

        if !floating {
            return bar.flex_shrink_0().w_full().border_b_1().border_color(p.border).bg(p.surface).into_any_element();
        }
        // Full screen: out of the way at the top edge until the pointer reaches it.
        let out = self.toolbar_out;
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .map(|el| {
                if out {
                    el.pt(px(8.)).child(
                        // Clicks on it stay with it, never reaching the desktop below.
                        bar.id("rd-toolbar-floating")
                            .occlude()
                            .rounded(p.corner(12.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface)
                            .shadow(elevation(p))
                            .min_w(px(520.)),
                    )
                } else {
                    // A small handle says where it is.
                    el.child(div().mt(px(3.)).w(px(56.)).h(px(4.)).rounded_full().bg(p.text_faint.opacity(0.5)))
                }
            })
            .into_any_element()
    }
}

/// A small outlined label: the protocol, or "View only".
fn tag(text: &'static str, p: AppPalette) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px(px(5.))
        .rounded(p.corner(4.))
        .border_1()
        .border_color(p.brand.opacity(0.45))
        .bg(p.brand_soft)
        .text_size(px(10.))
        .line_height(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.brand)
        .child(SharedString::from(text))
}

fn divider(p: AppPalette) -> impl IntoElement {
    div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(p.border)
}
