//! Lisse's smoothed capsule: cubic shoulders on the flat sides, circular end caps.
//! Ported from capsule.ts and corner-params.ts at Lisse revision
//! 173846978ed806e14c78154ec914a9645a991932 (MIT; see THIRD_PARTY_NOTICES.md and ../../LICENSES/Lisse.txt).
//! This runs only when building a contour, never in the fragment shader.
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, SQRT_2};

#[derive(Debug)]
pub(crate) struct End {
    radius: f32,
    pub(crate) smoothing: f32,
    pub(crate) reach: f32,
    pub(crate) a: f32,
    pub(crate) b: f32,
    c: f32,
    d: f32,
}

impl End {
    pub(crate) fn new(radius: f32, long_half: f32, smoothing: f32) -> Self {
        // Lisse gives each end half the long side. When the shoulder runs out
        // of room, reduce smoothing; a fully rounded square remains a circle.
        let smoothing = smoothing
            .clamp(0., 1.)
            .min((long_half / radius - 1.).max(0.));
        let reach = radius * (1. + smoothing);
        let beta = FRAC_PI_4 * smoothing;
        let arc_length = (FRAC_PI_4 * (1. - smoothing)).sin() * radius * SQRT_2;
        let c = radius * (beta * 0.5).tan() * beta.cos();
        let d = c * beta.tan();
        let b = ((reach - arc_length - c - d) / 3.).max(0.);
        Self {
            radius,
            smoothing,
            reach,
            a: 2. * b,
            b,
            c,
            d,
        }
    }

    fn shoulder(&self, width: f32) -> [[f32; 2]; 4] {
        let start = width - self.reach;
        [
            [start, 0.],
            [start + self.a, 0.],
            [start + self.a + self.b, 0.],
            [start + self.a + self.b + self.c, self.d],
        ]
    }
}

fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

pub(crate) const TOLERANCE: f32 = 0.01; // logical pixels, below the distance atlas's resolution

pub(crate) fn flatten_cubic(curve: [[f32; 2]; 4], points: &mut Vec<[f32; 2]>, depth: u32) {
    let [a, b, c, d] = curve;
    let chord = [d[0] - a[0], d[1] - a[1]];
    let length = chord[0].hypot(chord[1]);
    let error = |p: [f32; 2]| {
        ((p[0] - a[0]) * chord[1] - (p[1] - a[1]) * chord[0]).abs() / length.max(1e-9)
    };
    if depth >= 16 || (length <= 4. && error(b).max(error(c)) <= TOLERANCE) {
        points.push(d);
        return;
    }
    let ab = midpoint(a, b);
    let bc = midpoint(b, c);
    let cd = midpoint(c, d);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let center = midpoint(abc, bcd);
    flatten_cubic([a, ab, abc, center], points, depth + 1);
    flatten_cubic([center, bcd, cd, d], points, depth + 1);
}

/// A clockwise closed outline in logical pixels (without a repeated final vertex).
/// Zero smoothing is a circular capsule; one matches Lisse's full smoothing.
/// Horizontal and vertical capsules share the same geometry. A square is a circle.
/// Returns an empty contour for nonfinite inputs or nonpositive dimensions.
pub fn outline(width: f32, height: f32, smoothing: f32) -> Vec<[f32; 2]> {
    if !width.is_finite()
        || !height.is_finite()
        || !smoothing.is_finite()
        || width <= 0.
        || height <= 0.
    {
        return Vec::new();
    }
    let w = width.max(height);
    let h = width.min(height);
    let end = End::new(h * 0.5, w * 0.5, smoothing);
    let shoulder = end.shoulder(w);
    let mut quarter = vec![shoulder[0]];
    if end.smoothing > 0. {
        flatten_cubic(shoulder, &mut quarter, 0);
    }
    let angle = FRAC_PI_2 - FRAC_PI_4 * end.smoothing;
    let step = (2. * (1. - TOLERANCE / end.radius).clamp(-1., 1.).acos()).max(0.001);
    let steps = (angle / step).ceil().max(1.) as usize;
    for i in 1..=steps {
        let t = -angle * (1. - i as f32 / steps as f32);
        quarter.push([
            w - end.radius + end.radius * t.cos(),
            end.radius + end.radius * t.sin(),
        ]);
    }
    let mut right = quarter.clone();
    right.extend(quarter.iter().rev().skip(1).map(|p| [p[0], h - p[1]]));
    let mut points = vec![[end.reach, 0.]];
    points.extend(right.iter().copied());
    points.extend(right.iter().map(|p| [w - p[0], h - p[1]]));
    points.pop(); // implicit closing segment
    points.dedup_by(|a, b| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-5);
    if height > width {
        for p in &mut points {
            p.swap(0, 1);
        }
        points.reverse(); // transposition reverses winding
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_lisse_reference_shoulders() {
        // Independent anchors from Lisse's capsule.test.ts / Sketch reference.
        let end = End::new(50., 150., 0.5);
        let expected = [
            [225., 0.],
            [248.2971, 0.],
            [259.9456, 0.],
            [269.1342, 3.806],
        ];
        for (actual, expected) in end.shoulder(300.).iter().zip(expected) {
            for axis in 0..2 {
                assert!((actual[axis] - expected[axis]).abs() < 0.0001);
            }
        }
        let full = End::new(50., 150., 1.).shoulder(300.);
        assert!((full[3][0] - 285.3553).abs() < 0.0001);
        assert!((full[3][1] - 14.6447).abs() < 0.0001);
    }

    #[test]
    fn capsules_stay_bounded_and_rotate_without_changing_shape() {
        for smoothing in [0., 0.5, 1.] {
            let horizontal = outline(300., 100., smoothing);
            let vertical = outline(100., 300., smoothing);
            for (a, b) in horizontal.iter().zip(vertical.iter().rev()) {
                assert_eq!(*a, [b[1], b[0]]);
                assert!((0. ..=300.).contains(&a[0]) && (0. ..=100.).contains(&a[1]));
            }
            assert!(horizontal.contains(&[300., 50.]));
            assert!(horizontal.contains(&[0., 50.]));
        }
    }

    #[test]
    fn smoothing_uses_flat_side_budget_and_preserves_circular_square() {
        for smoothing in [0., 0.5, 1.] {
            for p in outline(100., 100., smoothing) {
                assert!(((p[0] - 50.).hypot(p[1] - 50.) - 50.).abs() < 0.0001);
            }
        }
        assert_eq!(outline(110., 100., 0.5), outline(110., 100., 1.));
        assert_ne!(outline(300., 100., 0.), outline(300., 100., 1.));
    }
}
