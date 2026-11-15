//! Opt-in native coverage for the production dropdown, including its real menu events.
#![cfg(windows)]

use std::sync::{Arc, Mutex};
use gpui::{App, AppContext, Bounds, Render, WindowBounds, WindowOptions, div, px, size, prelude::*};
use komando::theme::{AppPalette, DEFAULT_THEME};
use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

struct Picker {
    selected: Arc<Mutex<Option<&'static str>>>,
}

impl Render for Picker {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let p = AppPalette::dark();
        let selected = self.selected.clone();
        div().size_full().bg(p.canvas).p(px(20.)).child(
            div().w(px(420.)).child(komando::ui::theme_picker::dropdown(
                true, komando::theme::theme(true, DEFAULT_THEME), p,
                move |id, _, _| *selected.lock().unwrap() = Some(id),
            )),
        )
    }
}

#[test]
#[ignore = "opens a native window; set KOMANDO_TEST_DISPLAY to the display to test"]
fn dropdown_renders_opens_and_selects_a_named_theme_by_keyboard() {
    let display_index: usize = std::env::var("KOMANDO_TEST_DISPLAY").expect("KOMANDO_TEST_DISPLAY").parse().unwrap();
    let selected = Arc::new(Mutex::new(None));
    let report = selected.clone();
    gpui_platform::application().with_assets(gpui_kit_assets::AllAssets).run(move |cx: &mut App| {
        komando::init(cx);
        let fonts = cx.text_system().all_font_names();
        for t in komando::theme::themes(false).iter().chain(komando::theme::themes(true)) {
            assert!(fonts.iter().any(|name| name == t.font_sans), "UI font {} must be bundled or available", t.font_sans);
            assert!(fonts.iter().any(|name| name == t.font_mono), "terminal font {} must be bundled", t.font_mono);
            let font = cx.text_system().resolve_font(&gpui::font(t.font_mono));
            let a = cx.text_system().advance(font, px(14.), 'i').unwrap().width;
            let b = cx.text_system().advance(font, px(14.), 'W').unwrap().width;
            assert!((f32::from(a - b)).abs() < 0.01, "{} must be fixed width", t.font_mono);
        }
        gpui_component::Theme::update(cx, |theme| AppPalette::dark().apply_components(theme));
        let display = cx.displays().into_iter().nth(display_index).expect("test display");
        let bounds = Bounds::centered(Some(display.id()), size(px(600.), px(600.)), cx);
        cx.open_window(WindowOptions {
            display_id: Some(display.id()), window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions { appears_transparent: true, ..Default::default() }),
            ..Default::default()
        }, |window, cx| {
            let picker = cx.new(|_| Picker { selected: report });
            let hwnd = komando::platform::window_handle(window).unwrap();
            unsafe { let _ = ShowWindow(HWND(hwnd as *mut _), SW_SHOW); }
            let scale = window.scale_factor();
            window.on_next_frame(move |_, cx| {
                cx.spawn(async move |cx| {
                    cx.background_executor().spawn(async move {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        unsafe {
                            let hwnd = HWND(hwnd as *mut _);
                            let x = (50. * scale) as i32;
                            let y = (35. * scale) as i32;
                            let point = LPARAM(((y << 16) | x) as isize);
                            SendMessageW(hwnd, WM_MOUSEMOVE, None, Some(point));
                            SendMessageW(hwnd, WM_LBUTTONDOWN, Some(WPARAM(1)), Some(point));
                            SendMessageW(hwnd, WM_LBUTTONUP, None, Some(point));
                            std::thread::sleep(std::time::Duration::from_millis(250));
                            for key in [0x28, 0x28, 0x28, 0x0d] {
                                PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(key), LPARAM(0)).unwrap();
                                PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(key), LPARAM(0)).unwrap();
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }).await;
                    cx.update(|cx| cx.quit());
                }).detach();
            });
            cx.new(|cx| gpui_component::Root::new(picker, window, cx))
        }).unwrap();
        cx.activate(true);
    });
    assert_eq!(*selected.lock().unwrap(), Some("noctua"));
}
