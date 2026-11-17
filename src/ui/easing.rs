//! Easing curves that gpui doesn't ship.

/// A soft spring: quick start, about 1% past the end at 73%, then settling back. Sampled
/// every 1% of the way, as Zen Browser's compact sidebar defines it in CSS `linear()`.
const SPRING: [f32; 101] = [
    0., 0.002748, 0.010544, 0.022757, 0.038804, 0.058151, 0.080308, 0.104828, 0.131301, 0.159358,
    0.188662, 0.21891, 0.249828, 0.281172, 0.312724, 0.344288, 0.375693, 0.40679, 0.437447, 0.467549,
    0.497, 0.525718, 0.553633, 0.580688, 0.60684, 0.632052, 0.656298, 0.679562, 0.701831, 0.723104,
    0.743381, 0.76267, 0.780983, 0.798335, 0.814744, 0.830233, 0.844826, 0.858549, 0.87143, 0.883498,
    0.894782, 0.905314, 0.915125, 0.924247, 0.93271, 0.940547, 0.947787, 0.954463, 0.960603, 0.966239,
    0.971397, 0.976106, 0.980394, 0.984286, 0.987808, 0.990984, 0.993837, 0.99639, 0.998664, 1.000679,
    1.002456, 1.004011, 1.005363, 1.006528, 1.007522, 1.008359, 1.009054, 1.009618, 1.010065, 1.010405,
    1.010649, 1.010808, 1.01089, 1.010904, 1.010857, 1.010757, 1.010611, 1.010425, 1.010205, 1.009955,
    1.009681, 1.009387, 1.009077, 1.008754, 1.008422, 1.008083, 1.00774, 1.007396, 1.007052, 1.00671,
    1.006372, 1.00604, 1.005713, 1.005394, 1.005083, 1.004782, 1.004489, 1.004207, 1.003935, 1.003674,
    1.003423,
];

/// The spring the floated-out sidebar slides in with.
pub fn spring(t: f32) -> f32 {
    if t >= 1. {
        // The samples stop a hair past the end; the slide finishes exactly in place.
        return 1.;
    }
    let at = t.max(0.) * 100.;
    let i = at.floor() as usize;
    SPRING[i] + (SPRING[i + 1] - SPRING[i]) * (at - i as f32)
}

/// CSS `ease`, `cubic-bezier(0.25, 0.1, 0.25, 1)`.
pub fn ease(t: f32) -> f32 {
    if t <= 0. { 0. } else if t >= 1. { 1. } else { cubic_bezier(0.25, 0.1, 0.25, 1., t) }
}

/// The y of a CSS cubic Bézier at x = `t`, solved for its parameter by bisection.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let at = |a: f32, b: f32, s: f32| 3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3);
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.;
        if at(x1, x2, mid) < t { lo = mid } else { hi = mid }
    }
    at(y1, y2, (lo + hi) / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_at_zero_and_end_in_place() {
        for curve in [spring, ease] {
            assert_eq!(curve(0.), 0.);
            assert_eq!(curve(1.), 1.);
        }
    }

    #[test]
    fn spring_overshoots_slightly_then_settles() {
        assert!((spring(0.5) - 0.971397).abs() < 1e-5);
        assert!((spring(0.725) - 1.010897).abs() < 1e-5);
        assert!(spring(0.99) < spring(0.73));
    }

    #[test]
    fn ease_matches_css() {
        // Reference values of CSS `ease`.
        assert!((ease(0.5) - 0.8024).abs() < 1e-3);
        assert!((ease(0.25) - 0.4094).abs() < 1e-3);
    }
}
