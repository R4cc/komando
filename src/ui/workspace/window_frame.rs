use gpui::{
    App, Bounds, BoxShadow, Canvas, ContentMask, Hsla, Styled, Window, canvas, point, px, size,
};
#[cfg(not(windows))]
use gpui::{BorderStyle, outline};

use crate::theme::{AppPalette, hex};

/// A paint-only frame so the title bar, resize edges, and sidebar still receive pointer input.
pub(super) fn render(palette: AppPalette, window: &mut Window, cx: &mut App) -> Option<Canvas<()>> {
    let visible = !window.is_maximized() && !window.is_fullscreen();
    // Keep the frame near silver, with a little warmth or coolness from the app canvas.
    let (silver, line_alpha, glow_alpha) = if palette.dark {
        (0xdce0e5, 0.24, 0.16)
    } else {
        (0x5e6470, 0.22, 0.12)
    };
    let color = hex(silver).blend(Hsla {
        a: 0.10,
        ..palette.canvas
    });

    #[cfg(windows)]
    {
        // DWM needs an opaque color. Blend against the canvas to match the translucent stroke.
        let border = visible.then(|| {
            let canvas = Hsla {
                a: 1.,
                ..palette.canvas
            };
            crate::theme::packed(canvas.blend(color.opacity(line_alpha)))
        });
        let applied = window.use_state(cx, |_, _| std::cell::Cell::new(None));
        if applied.read(cx).replace(Some(border)) != Some(border) {
            window.defer(cx, move |window, _| {
                crate::platform::set_native_border_color(window, border)
            });
        }
    }

    if !visible {
        return None;
    }

    // The compositor traces the hairline exactly. Use Windows' usual window radius for
    // the much softer glow, which is painted inside the client area.
    #[cfg(windows)]
    let corners = gpui::Corners::all(px(8.));
    #[cfg(not(windows))]
    let corners = gpui_component::window_corner_radii(window, cx);

    Some(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let glow = BoxShadow::new(px(0.), px(0.), color.opacity(glow_alpha))
                    .blur_radius(px(3.))
                    .inset();
                #[cfg(not(windows))]
                let stroke = outline(bounds, color.opacity(line_alpha), BorderStyle::Solid)
                    .corner_radii(corners);

                // Limit the blur to the perimeter so repaints do not shade the whole window.
                let edge = px(12.)
                    .min(bounds.size.width / 2.)
                    .min(bounds.size.height / 2.);
                let middle = bounds.size.height - edge * 2.;
                let strips = [
                    Bounds::new(bounds.origin, size(bounds.size.width, edge)),
                    Bounds::new(
                        point(bounds.left(), bounds.bottom() - edge),
                        size(bounds.size.width, edge),
                    ),
                    Bounds::new(
                        point(bounds.left(), bounds.top() + edge),
                        size(edge, middle),
                    ),
                    Bounds::new(
                        point(bounds.right() - edge, bounds.top() + edge),
                        size(edge, middle),
                    ),
                ];
                for bounds_mask in strips {
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: bounds_mask,
                        }),
                        |window| {
                            window.paint_inset_shadows(
                                bounds,
                                corners,
                                std::slice::from_ref(&glow),
                            );
                            #[cfg(not(windows))]
                            window.paint_quad(stroke.clone());
                        },
                    );
                }
            },
        )
        .absolute()
        .inset_0(),
    )
}
