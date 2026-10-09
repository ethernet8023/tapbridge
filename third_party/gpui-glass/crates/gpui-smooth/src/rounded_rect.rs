//! Uniform Lisse/Figma squircle with Lisse's per-edge budget blend into capsules.
//! Port of curves/blend.ts at the revision recorded in THIRD_PARTY_NOTICES.md.
use crate::capsule::{End, TOLERANCE, flatten_cubic};
use std::f32::consts::FRAC_PI_4;

/// Closed clockwise contour in logical pixels, without a repeated final vertex.
/// Radius is clamped to half the short side; zero gives a rectangle. Smoothing
/// is clamped to [0, 1] and to the available space along each adjacent edge.
/// Invalid or nonpositive dimensions return an empty contour.
pub fn outline(width: f32, height: f32, radius: f32, smoothing: f32) -> Vec<[f32; 2]> {
    if ![width, height, radius, smoothing]
        .iter()
        .all(|v| v.is_finite())
        || width <= 0.
        || height <= 0.
    {
        return Vec::new();
    }
    let radius = radius.max(0.).min(width.min(height) * 0.5);
    if radius == 0. {
        return vec![[0., 0.], [width, 0.], [width, height], [0., height]];
    }
    if radius == width.min(height) * 0.5 {
        return crate::capsule::outline(width, height, smoothing);
    }
    let horizontal = End::new(radius, width * 0.5, smoothing);
    let vertical = End::new(radius, height * 0.5, smoothing);
    let mut points = vec![[horizontal.reach, 0.]];
    for (corner, u, v, arrival, departure) in [
        ([width, 0.], [-1., 0.], [0., 1.], &horizontal, &vertical),
        (
            [width, height],
            [0., -1.],
            [-1., 0.],
            &vertical,
            &horizontal,
        ),
        ([0., height], [1., 0.], [0., -1.], &horizontal, &vertical),
        ([0., 0.], [0., 1.], [1., 0.], &vertical, &horizontal),
    ] {
        let h = FRAC_PI_4 * arrival.smoothing;
        let v_angle = FRAC_PI_4 * departure.smoothing;
        let center = std::array::from_fn::<_, 2, _>(|k| corner[k] + (u[k] + v[k]) * radius);
        let start = std::array::from_fn(|k| corner[k] + u[k] * arrival.reach);
        let end = std::array::from_fn(|k| corner[k] + v[k] * departure.reach);
        let j1 =
            std::array::from_fn(|k| center[k] - v[k] * radius * h.cos() - u[k] * radius * h.sin());
        let j2 = std::array::from_fn(|k| {
            center[k] - u[k] * radius * v_angle.cos() - v[k] * radius * v_angle.sin()
        });
        points.push(start);
        flatten_cubic(
            [
                start,
                std::array::from_fn(|k| start[k] - u[k] * arrival.a),
                std::array::from_fn(|k| start[k] - u[k] * (arrival.a + arrival.b)),
                j1,
            ],
            &mut points,
            0,
        );
        let angle = (j1[1] - center[1]).atan2(j1[0] - center[0]);
        let sweep = std::f32::consts::FRAC_PI_2 - h - v_angle;
        if sweep > 1e-6 {
            let step = (2. * (1. - TOLERANCE / radius).clamp(-1., 1.).acos()).max(0.001);
            let steps = (sweep / step).ceil().max(1.) as usize;
            for i in 1..steps {
                let t = angle + sweep * i as f32 / steps as f32;
                points.push([center[0] + radius * t.cos(), center[1] + radius * t.sin()]);
            }
        }
        points.push(j2);
        flatten_cubic(
            [
                j2,
                std::array::from_fn(|k| end[k] - v[k] * (departure.a + departure.b)),
                std::array::from_fn(|k| end[k] - v[k] * departure.a),
                end,
            ],
            &mut points,
            0,
        );
    }
    points.pop();
    points.dedup_by(|a, b| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-5);
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;
    fn distance(p: [f32; 2], points: &[[f32; 2]]) -> f32 {
        points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
            .map(|(a, b)| {
                let d = [b[0] - a[0], b[1] - a[1]];
                let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1])
                    / (d[0] * d[0] + d[1] * d[1]).max(1e-12))
                .clamp(0., 1.);
                (p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1])
            })
            .fold(f32::INFINITY, f32::min)
    }
    #[test]
    fn circle_corners_and_capsule_limits() {
        let points = outline(300., 120., 25., 0.);
        for i in 0..100 {
            let t = TAU * i as f32 / 400.;
            assert!(distance([275. + 25. * t.cos(), 25. - 25. * t.sin()], &points) < 0.011);
        }
        assert_eq!(
            outline(300., 120., 999., 1.),
            crate::capsule::outline(300., 120., 1.)
        );
        assert_eq!(outline(300., 120., 0., 1.).len(), 4);
    }
    #[test]
    fn blend_is_continuous_at_capsule_and_roomy_boundaries() {
        for height in [100., 160.] {
            let a = outline(300., height, 50., 0.6);
            let b = outline(300., height + 0.001, 50., 0.6);
            for p in &a {
                assert!(distance(*p, &b) < 0.03);
            }
            for p in &b {
                assert!(distance(*p, &a) < 0.03);
            }
        }
    }
    #[test]
    fn smoothing_stays_bounded_for_different_aspect_ratios() {
        for (w, h) in [(120., 300.), (300., 120.), (100., 100.)] {
            for s in [0., 0.6, 1.] {
                for [x, y] in outline(w, h, 42., s) {
                    assert!(x >= -0.001 && x <= w + 0.001 && y >= -0.001 && y <= h + 0.001);
                }
            }
        }
        assert!(outline(0., 30., 5., 0.6).is_empty());
        assert!(outline(20., 30., f32::NAN, 0.6).is_empty());
    }
}
