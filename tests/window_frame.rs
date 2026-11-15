//! Opt-in native hit testing; select a display with KOMANDO_TEST_DISPLAY (zero-based).
#![cfg(windows)]

use std::sync::{Arc, Mutex};

use gpui::{App, AppContext, Bounds, Render, TitlebarOptions, WindowBounds, WindowControlArea, WindowOptions, div, prelude::*, px, size};
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

struct TitleBar;

impl Render for TitleBar {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().child(
            div().h(px(40.)).w_full().flex().window_control_area(WindowControlArea::Drag)
                .child(div().flex_1())
                .children([WindowControlArea::Min, WindowControlArea::Max, WindowControlArea::Close].map(|area| {
                    div().w(px(46.)).h_full().occlude().window_control_area(area)
                })),
        )
    }
}

#[test]
#[ignore = "opens a native window; set KOMANDO_TEST_DISPLAY to the display to test"]
fn maximized_mica_title_bar_reaches_work_area_top() {
    let display_index: usize = std::env::var("KOMANDO_TEST_DISPLAY").expect("KOMANDO_TEST_DISPLAY").parse().unwrap();
    let result = Arc::new(Mutex::new(None));
    let report = result.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        let display = cx.displays().into_iter().nth(display_index).expect("test display");
        let bounds = Bounds::centered(Some(display.id()), size(px(800.), px(600.)), cx);
        cx.open_window(WindowOptions {
            display_id: Some(display.id()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Komando frame test".into()),
                appears_transparent: true,
                ..Default::default()
            }),
            ..Default::default()
        }, |window, cx| {
            assert!(komando::platform::set_backdrop(window, true), "Mica must be available for this regression");
            let hwnd = komando::platform::window_handle(window).unwrap();
            let scale = window.scale_factor();
            window.on_next_frame(move |_, cx| {
                cx.spawn(async move |cx| {
                    let checked = cx.background_executor().spawn(async move { probe(hwnd, scale) }).await;
                    *report.lock().unwrap() = Some(checked);
                    cx.update(|cx| cx.quit());
                }).detach();
            });
            cx.new(|_| TitleBar)
        }).unwrap();
        cx.activate(true);
    });
    let checked = result.lock().unwrap().take().expect("native probe completed");
    assert!(checked.is_ok(), "{}", checked.unwrap_err());
}

fn probe(handle: isize, scale: f32) -> Result<(), String> {
    unsafe {
        let hwnd = HWND(handle as *mut _);
        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        assert!(GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info).as_bool());
        let mut origin = POINT::default();
        assert!(ClientToScreen(hwnd, &mut origin).as_bool());
        let x = info.rcWork.left + (info.rcWork.right - info.rcWork.left) / 2;
        for offset in [0, 1, 2, 4, 8, 16] {
            let y = info.rcWork.top + offset;
            let hit = hit_test(hwnd, x, y);
            if hit != HTCAPTION as isize {
                return Err(format!("Top + {offset}px: expected HTCAPTION ({HTCAPTION}), got {hit}; client top {}, work area top {}", origin.y, info.rcWork.top));
            }
        }
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
        for (button_from_right, expected) in [(0, HTCLOSE), (1, HTMAXBUTTON), (2, HTMINBUTTON)] {
            let x = origin.x + client.right - ((button_from_right as f32 + 0.5) * 46. * scale) as i32;
            let hit = hit_test(hwnd, x, origin.y + (16. * scale) as i32);
            if hit != expected as isize {
                return Err(format!("Caption button {button_from_right}: expected {expected}, got {hit}"));
            }
        }
        let _ = ShowWindow(hwnd, SW_RESTORE);
        std::thread::sleep(std::time::Duration::from_millis(300));
        origin = POINT::default();
        assert!(ClientToScreen(hwnd, &mut origin).as_bool());
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
        let hit = hit_test(hwnd, origin.x + client.right / 2, origin.y);
        if hit != HTTOP as isize {
            return Err(format!("Restored top edge must resize: expected HTTOP ({HTTOP}), got {hit}"));
        }
        Ok(())
    }
}

unsafe fn hit_test(hwnd: HWND, x: i32, y: i32) -> isize {
    let point = LPARAM((((y as u32 & 0xffff) << 16) | (x as u32 & 0xffff)) as isize);
    unsafe {
        // GPUI hit tests the pointer position supplied by the preceding mouse move.
        SendMessageW(hwnd, WM_NCMOUSEMOVE, Some(WPARAM(HTCAPTION as usize)), Some(point));
        SendMessageW(hwnd, WM_NCHITTEST, None, Some(point)).0
    }
}
