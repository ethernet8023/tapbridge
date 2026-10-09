//! A cached signed distance + normal atlas from ANY closed polygon, including concave ones.
//! A segment BVH computes subpixel distances; a cached harmonic direction field
//! removes interior nearest-edge discontinuities without changing coverage.
//! The shader never evaluates a polygon or runs a per-pixel path loop.
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Rectangle,
    Capsule,
    Rounded,
    Pebble,
    Notched,
    Flower,
}
impl Shape {
    pub const ALL: [Self; 4] = [Self::Rounded, Self::Pebble, Self::Notched, Self::Flower];
    pub fn name(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Capsule => "Capsule",
            Self::Rounded => "Smooth",
            Self::Pebble => "Pebble",
            Self::Notched => "Notched",
            Self::Flower => "Flower",
        }
    }
}

pub fn outline(shape: Shape, w: f32, h: f32, smoothing: f32) -> Vec<[f32; 2]> {
    if shape == Shape::Rectangle {
        return vec![[0., 0.], [w, 0.], [w, h], [0., h]];
    }
    if shape == Shape::Capsule {
        return gpui_smooth::capsule::outline(w, h, smoothing);
    }
    if shape == Shape::Rounded {
        let r = (h * 0.5).min(w * 0.5);
        let power = 2.0 + smoothing * 3.0;
        let mut points = Vec::with_capacity(132);
        for (cx, cy, start) in [
            (w - r, r, -TAU / 4.),
            (w - r, h - r, 0.),
            (r, h - r, TAU / 4.),
            (r, r, TAU / 2.),
        ] {
            for i in 0..=32 {
                let t = start + (i as f32 / 32.) * TAU / 4.;
                points.push([
                    cx + r * t.cos().signum() * t.cos().abs().powf(2. / power),
                    cy + r * t.sin().signum() * t.sin().abs().powf(2. / power),
                ]);
            }
        }
        return points;
    }
    (0..192)
        .map(|i| {
            let t = TAU * i as f32 / 192.;
            let k = match shape {
                Shape::Pebble => 0.90 + 0.07 * (3. * t + 0.8).sin() + 0.025 * (2. * t).cos(),
                Shape::Notched => {
                    0.96 - 0.33 * ((t.sin().abs() - 0.75) / 0.25).clamp(0., 1.).powi(2)
                }
                Shape::Flower => 0.82 + 0.16 * (5. * t).cos(),
                _ => 1.,
            };
            [
                w * (0.5 + 0.49 * k * t.cos()),
                h * (0.5 + 0.49 * k * t.sin()),
            ]
        })
        .collect()
}

pub struct DistanceAtlas {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u16>,
}
pub const PAD: f32 = 3.;
pub const RESOLUTION: f32 = 2.;

/// Polygon coordinates are logical pixels. Call only on shape/size changes, never per frame.
pub fn distance_atlas(points: &[[f32; 2]], width: f32, height: f32) -> DistanceAtlas {
    distance_atlas_padded(points, width, height, PAD)
}

/// Build a distance field with enough exterior support for effects such as shadows.
/// Padding is in logical pixels and must also be used when mapping texture coordinates.
pub fn distance_atlas_padded(
    points: &[[f32; 2]],
    width: f32,
    height: f32,
    pad: f32,
) -> DistanceAtlas {
    let pad = pad.max(PAD);
    let w = ((width + pad * 2.) * RESOLUTION).ceil() as usize;
    let h = ((height + pad * 2.) * RESOLUTION).ceil() as usize;
    let mut mask = vec![false; w * h];
    let mut cuts = Vec::with_capacity(points.len());
    for y in 0..h {
        let py = (y as f32 + 0.5) / RESOLUTION - pad;
        cuts.clear();
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            if (a[1] > py) != (b[1] > py) {
                cuts.push(a[0] + (py - a[1]) * (b[0] - a[0]) / (b[1] - a[1]));
            }
        }
        cuts.sort_by(f32::total_cmp);
        for pair in cuts.chunks_exact(2) {
            let left = ((pair[0] + pad) * RESOLUTION - 0.5).ceil().max(0.) as usize;
            let right = ((pair[1] + pad) * RESOLUTION - 0.5).ceil().max(0.) as usize;
            for x in left.min(w)..right.min(w) {
                mask[y * w + x] = true;
            }
        }
    }
    let tree = OutlineTree::new(points);
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let p = [
                (x as f32 + 0.5) / RESOLUTION - pad,
                (y as f32 + 0.5) / RESOLUTION - pad,
            ];
            let (distance, normal) = tree.sample(p);
            let d = if mask[y * w + x] { -distance } else { distance };
            for v in [d, normal[0], normal[1], 1.] {
                data.push(half::f16::from_f32(v).to_bits());
            }
        }
    }
    // Solve only in canonical shape coordinates, independently of shadow padding.
    // Coverage distances and exterior normals remain exact and unchanged.
    let iw = (width * RESOLUTION).ceil() as usize;
    let ih = (height * RESOLUTION).ceil() as usize;
    let offset = (pad * RESOLUTION) as usize;
    let mut directions = Vec::with_capacity(iw * ih);
    let mut free = Vec::with_capacity(iw * ih);
    for y in 0..ih {
        for x in 0..iw {
            let i = ((y + offset) * w + x + offset) * 4;
            directions.push([
                half::f16::from_bits(data[i + 1]).to_f32(),
                half::f16::from_bits(data[i + 2]).to_f32(),
            ]);
            free.push(
                x > 0
                    && y > 0
                    && x + 1 < iw
                    && y + 1 < ih
                    && half::f16::from_bits(data[i]).to_f32() < -0.75,
            );
        }
    }
    extend_directions(iw, ih, &free, &mut directions);
    for y in 0..ih {
        for x in 0..iw {
            let i = ((y + offset) * w + x + offset) * 4;
            data[i + 1] = half::f16::from_f32(directions[y * iw + x][0]).to_bits();
            data[i + 2] = half::f16::from_f32(directions[y * iw + x][1]).to_bits();
        }
    }
    DistanceAtlas {
        width: w as u32,
        height: h as u32,
        data,
    }
}

// Coarse-to-fine Dirichlet extension. The boundary direction is fixed; each free
// interior value approximates the discrete Laplace equation. Unlike nearest-edge
// normals this field has no medial-axis discontinuity. This runs on geometry
// changes only and adds no texture reads or per-frame passes to the renderer.
fn extend_directions(w: usize, h: usize, free: &[bool], values: &mut [[f32; 2]]) {
    if w > 8 && h > 8 {
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let mut coarse = vec![[0.; 2]; cw * ch];
        let mut active = vec![true; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                for dy in 0..2 {
                    for dx in 0..2 {
                        let i = (2 * y + dy).min(h - 1) * w + (2 * x + dx).min(w - 1);
                        active[y * cw + x] &= free[i];
                        for c in 0..2 {
                            coarse[y * cw + x][c] += values[i][c] * 0.25;
                        }
                    }
                }
            }
        }
        extend_directions(cw, ch, &active, &mut coarse);
        for y in 0..h {
            for x in 0..w {
                if !free[y * w + x] {
                    continue;
                }
                let fx = (x as f32 * 0.5 - 0.25).max(0.);
                let fy = (y as f32 * 0.5 - 0.25).max(0.);
                let ix = fx as usize;
                let iy = fy as usize;
                let tx = fx - ix as f32;
                let ty = fy - iy as f32;
                for c in 0..2 {
                    let a = coarse[iy * cw + ix][c] * (1. - tx)
                        + coarse[iy * cw + (ix + 1).min(cw - 1)][c] * tx;
                    let b = coarse[(iy + 1).min(ch - 1) * cw + ix][c] * (1. - tx)
                        + coarse[(iy + 1).min(ch - 1) * cw + (ix + 1).min(cw - 1)][c] * tx;
                    values[y * w + x][c] = a * (1. - ty) + b * ty;
                }
            }
        }
    }
    for _ in 0..32 {
        for parity in 0..2 {
            for y in 1..h.saturating_sub(1) {
                for x in 1..w.saturating_sub(1) {
                    let i = y * w + x;
                    if (x + y) % 2 != parity || !free[i] {
                        continue;
                    }
                    values[i] = [0, 1].map(|c| {
                        (values[i - 1][c] + values[i + 1][c] + values[i - w][c] + values[i + w][c])
                            * 0.25
                    });
                }
            }
        }
    }
}

#[derive(Clone)]
struct Segment {
    a: [f32; 2],
    b: [f32; 2],
    n0: [f32; 2],
    n1: [f32; 2],
}
struct Node {
    lo: [f32; 2],
    hi: [f32; 2],
    start: usize,
    end: usize,
    children: Option<(usize, usize)>,
}
struct OutlineTree {
    segments: Vec<Segment>,
    nodes: Vec<Node>,
}
fn norm(v: [f32; 2]) -> [f32; 2] {
    let l = v[0].hypot(v[1]).max(1e-12);
    [v[0] / l, v[1] / l]
}
fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
fn segment_sample(s: &Segment, p: [f32; 2]) -> (f32, f32) {
    let d = [s.b[0] - s.a[0], s.b[1] - s.a[1]];
    let q = [p[0] - s.a[0], p[1] - s.a[1]];
    let t = (dot(q, d) / dot(d, d).max(1e-12)).clamp(0., 1.);
    ((q[0] - t * d[0]).powi(2) + (q[1] - t * d[1]).powi(2), t)
}
impl OutlineTree {
    fn new(points: &[[f32; 2]]) -> Self {
        let mut points = points.to_vec();
        points.dedup_by(|a, b| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 0.0001);
        if points.len() > 1
            && (points[0][0] - points[points.len() - 1][0]).abs()
                + (points[0][1] - points[points.len() - 1][1]).abs()
                < 0.0001
        {
            points.pop();
        }
        let n = points.len();
        let area = (0..n)
            .map(|i| {
                let a = points[i];
                let b = points[(i + 1) % n];
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f32>()
            .signum();
        let normals: Vec<_> = (0..n)
            .map(|i| {
                let a = points[i];
                let b = points[(i + 1) % n];
                norm([(b[1] - a[1]) * area, (a[0] - b[0]) * area])
            })
            .collect();
        let mut segments = Vec::new();
        for i in 0..n {
            let normal = normals[i];
            // Smooth only tangent-continuous curve samples. Preserve genuinely sharp corners.
            let smooth = |other: [f32; 2]| {
                if dot(normal, other) > 0.85 {
                    norm([normal[0] + other[0], normal[1] + other[1]])
                } else {
                    normal
                }
            };
            segments.push(Segment {
                a: points[i],
                b: points[(i + 1) % n],
                n0: smooth(normals[(i + n - 1) % n]),
                n1: smooth(normals[(i + 1) % n]),
            });
        }
        let mut tree = Self {
            segments,
            nodes: Vec::new(),
        };
        tree.build(0, n);
        tree
    }
    fn build(&mut self, start: usize, end: usize) -> usize {
        let mut lo = [f32::INFINITY; 2];
        let mut hi = [f32::NEG_INFINITY; 2];
        for s in &self.segments[start..end] {
            for k in 0..2 {
                lo[k] = lo[k].min(s.a[k]).min(s.b[k]);
                hi[k] = hi[k].max(s.a[k]).max(s.b[k]);
            }
        }
        let index = self.nodes.len();
        self.nodes.push(Node {
            lo,
            hi,
            start,
            end,
            children: None,
        });
        if end - start > 6 {
            let axis = usize::from(hi[1] - lo[1] > hi[0] - lo[0]);
            self.segments[start..end]
                .sort_by(|a, b| (a.a[axis] + a.b[axis]).total_cmp(&(b.a[axis] + b.b[axis])));
            let mid = (start + end) / 2;
            let left = self.build(start, mid);
            let right = self.build(mid, end);
            self.nodes[index].children = Some((left, right));
        }
        index
    }
    fn bound_distance(&self, index: usize, p: [f32; 2]) -> f32 {
        let n = &self.nodes[index];
        let x = (n.lo[0] - p[0]).max(0.).max(p[0] - n.hi[0]);
        let y = (n.lo[1] - p[1]).max(0.).max(p[1] - n.hi[1]);
        x * x + y * y
    }
    fn nearest(&self, index: usize, p: [f32; 2], best: &mut (f32, usize, f32)) {
        if self.bound_distance(index, p) > best.0 {
            return;
        }
        let node = &self.nodes[index];
        if let Some((mut a, mut b)) = node.children {
            if self.bound_distance(a, p) > self.bound_distance(b, p) {
                std::mem::swap(&mut a, &mut b);
            }
            self.nearest(a, p, best);
            self.nearest(b, p, best);
        } else {
            for i in node.start..node.end {
                let (distance, t) = segment_sample(&self.segments[i], p);
                if distance < best.0 {
                    *best = (distance, i, t);
                }
            }
        }
    }
    fn sample(&self, p: [f32; 2]) -> (f32, [f32; 2]) {
        let mut best = (f32::INFINITY, 0, 0.);
        self.nearest(0, p, &mut best);
        let s = &self.segments[best.1];
        let t = best.2;
        (
            best.0.sqrt(),
            norm([
                s.n0[0] * (1. - t) + s.n1[0] * t,
                s.n0[1] * (1. - t) + s.n1[1] * t,
            ]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(a: &DistanceAtlas, x: usize, y: usize, c: usize) -> f32 {
        half::f16::from_bits(a.data[(y * a.width as usize + x) * 4 + c]).to_f32()
    }
    #[test]
    fn accelerated_distance_matches_brute_force() {
        let tree = OutlineTree::new(&outline(Shape::Flower, 120., 80., 0.));
        for y in (0..90).step_by(3) {
            for x in (0..130).step_by(3) {
                let p = [x as f32, y as f32];
                let actual = tree.sample(p).0;
                let expected = tree
                    .segments
                    .iter()
                    .map(|s| segment_sample(s, p).0)
                    .fold(f32::INFINITY, f32::min)
                    .sqrt();
                assert!((actual - expected).abs() < 0.0001);
            }
        }
    }
    #[test]
    fn capsule_normals_follow_the_analytic_circle() {
        let tree = OutlineTree::new(&outline(Shape::Capsule, 120., 60., 0.));
        for i in 1..90 {
            let a = (i as f32).to_radians();
            let p = [90. + 29. * a.cos(), 30. + 29. * a.sin()];
            let (_, n) = tree.sample(p);
            assert!(dot(n, [a.cos(), a.sin()]) > 0.999);
        }
    }
    #[test]
    fn every_shape_has_finite_signed_distance_and_outward_normals() {
        for shape in std::iter::once(Shape::Capsule).chain(Shape::ALL) {
            let a = distance_atlas(&outline(shape, 120., 60., 0.7), 120., 60.);
            assert!(sample(&a, 126, 66, 0) < -10.);
            assert!(sample(&a, 0, 0, 0) > 0.);
            assert!(a.data.iter().all(|&v| half::f16::from_bits(v).is_finite()));
        }
        let a = distance_atlas(&outline(Shape::Rounded, 120., 60., 0.7), 120., 60.);
        assert!(sample(&a, 126, 8, 2) < -0.9);
        assert!(sample(&a, 126, 124, 2) > 0.9);
    }
    #[test]
    fn interior_directions_cross_medial_axes_continuously() {
        let a = distance_atlas(&outline(Shape::Rectangle, 40., 40., 0.), 40., 40.);
        // x=y is where the old nearest-side normal jumped from left to top.
        for i in 16..64 {
            for c in 1..=2 {
                assert!((sample(&a, i - 1, i, c) - sample(&a, i + 1, i, c)).abs() < 0.15);
            }
        }
        for (w, h) in [(1., 30.), (30., 1.), (3., 3.), (128., 10.)] {
            let thin = distance_atlas(&outline(Shape::Rectangle, w, h, 0.), w, h);
            assert!(
                thin.data
                    .iter()
                    .all(|&v| half::f16::from_bits(v).is_finite())
            );
        }
    }
    #[test]
    fn smoothing_changes_corner_contour() {
        assert_ne!(
            outline(Shape::Rounded, 120., 60., 0.),
            outline(Shape::Rounded, 120., 60., 1.)
        );
    }
    #[test]
    fn exterior_padding_preserves_the_shape_and_extends_shadow_distances() {
        let outline = outline(Shape::Flower, 120., 60., 0.7);
        let standard = distance_atlas(&outline, 120., 60.);
        let extended = distance_atlas_padded(&outline, 120., 60., 19.);
        let offset = ((19. - PAD) * RESOLUTION) as usize;
        for y in 0..standard.height as usize {
            for x in 0..standard.width as usize {
                for channel in 0..4 {
                    assert_eq!(
                        sample(&standard, x, y, channel),
                        sample(&extended, x + offset, y + offset, channel)
                    );
                }
            }
        }
        assert!(sample(&extended, 0, extended.height as usize / 2, 0) > 15.);
        assert_eq!(
            distance_atlas_padded(&outline, 120., 60., PAD).data,
            standard.data
        );
    }
}
