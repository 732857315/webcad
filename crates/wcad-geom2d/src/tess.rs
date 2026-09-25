//! Fill tessellation (lyon) for solid hatches, filled regions and glyph outlines.
//!
//! lyon works in `f32`; inputs are translated to their bounding-box center first so large drawing
//! coordinates keep their precision, and results are returned in `f64` drawing coordinates.

use lyon_path::Path;
use lyon_path::math::point;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use wcad_math::{BBox2, DVec2};

use crate::curve::Curve;
use crate::curves::Curve2;
use crate::text::PathCmd;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FillRule {
    #[default]
    EvenOdd,
    NonZero,
}

impl FillRule {
    fn lyon(self) -> lyon_tessellation::FillRule {
        match self {
            FillRule::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
            FillRule::NonZero => lyon_tessellation::FillRule::NonZero,
        }
    }
}

/// Triangle mesh: vertices plus indices (3 per triangle).
pub type Mesh = (Vec<DVec2>, Vec<u32>);

/// Triangulate closed polygons (closing edge implied) with the given fill rule.
pub fn fill_polygons(loops: &[Vec<DVec2>], rule: FillRule) -> Mesh {
    let bb = BBox2::from_points(loops.iter().flatten().copied().filter(|p| p.is_finite()));
    if bb.is_empty() || !(bb.size().max_element() < 1e30) {
        return (Vec::new(), Vec::new());
    }
    let o = bb.center();
    let mut b = Path::builder();
    let mut any = false;
    for l in loops {
        let pts: Vec<DVec2> = l.iter().copied().filter(|p| p.is_finite()).collect();
        if pts.len() < 3 {
            continue;
        }
        let rel = |p: DVec2| point((p.x - o.x) as f32, (p.y - o.y) as f32);
        b.begin(rel(pts[0]));
        for p in &pts[1..] {
            b.line_to(rel(*p));
        }
        b.end(true);
        any = true;
    }
    if !any {
        return (Vec::new(), Vec::new());
    }
    let tol = (bb.size().length() * 1e-4).max(1e-6) as f32;
    run(&b.build(), rule, tol, o)
}

/// Triangulate outline paths (e.g. glyphs from [`crate::text::layout`], non-zero rule) with curve
/// flattening tolerance `tolerance` (drawing units).
pub fn fill_paths(paths: &[Vec<PathCmd>], rule: FillRule, tolerance: f64) -> Mesh {
    let mut bb = BBox2::EMPTY;
    for c in paths.iter().flatten() {
        match *c {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => bb.include(p),
            PathCmd::QuadTo(a, p) => {
                bb.include(a);
                bb.include(p);
            }
            PathCmd::CubicTo(a, b, p) => {
                bb.include(a);
                bb.include(b);
                bb.include(p);
            }
            PathCmd::Close => {}
        }
    }
    if bb.is_empty() || !(bb.size().max_element() < 1e30) {
        return (Vec::new(), Vec::new());
    }
    let o = bb.center();
    let rel = |p: DVec2| point((p.x - o.x) as f32, (p.y - o.y) as f32);
    let mut b = Path::builder();
    let mut open = false;
    let mut any = false;
    for path in paths {
        for c in path {
            match *c {
                PathCmd::MoveTo(p) => {
                    if open {
                        b.end(true);
                    }
                    b.begin(rel(p));
                    open = true;
                    any = true;
                }
                PathCmd::LineTo(p) => {
                    if open {
                        b.line_to(rel(p));
                    } else {
                        b.begin(rel(p));
                        open = true;
                        any = true;
                    }
                }
                PathCmd::QuadTo(a, p) => {
                    if open {
                        b.quadratic_bezier_to(rel(a), rel(p));
                    } else {
                        b.begin(rel(p));
                        open = true;
                        any = true;
                    }
                }
                PathCmd::CubicTo(a, c2, p) => {
                    if open {
                        b.cubic_bezier_to(rel(a), rel(c2), rel(p));
                    } else {
                        b.begin(rel(p));
                        open = true;
                        any = true;
                    }
                }
                PathCmd::Close => {
                    if open {
                        b.end(true);
                        open = false;
                    }
                }
            }
        }
    }
    if open {
        b.end(true);
    }
    if !any {
        return (Vec::new(), Vec::new());
    }
    let tol = if tolerance > 0.0 && tolerance.is_finite() {
        tolerance
    } else {
        bb.size().length() * 1e-3
    };
    run(&b.build(), rule, (tol as f32).max(1e-6), o)
}

/// Flatten closed curve loops (e.g. hatch boundaries or regions) and triangulate them.
pub fn fill_loops<L: AsRef<[Curve2]>>(loops: &[L], rule: FillRule, tol: f64) -> Mesh {
    let polys: Vec<Vec<DVec2>> = loops
        .iter()
        .map(|l| {
            let mut pts: Vec<DVec2> = Vec::new();
            for c in l.as_ref() {
                let f = c.flatten(tol);
                let skip = usize::from(!pts.is_empty());
                pts.extend(f.into_iter().skip(skip));
            }
            pts
        })
        .collect();
    fill_polygons(&polys, rule)
}

fn run(path: &Path, rule: FillRule, tol: f32, o: DVec2) -> Mesh {
    let mut buf: VertexBuffers<DVec2, u32> = VertexBuffers::new();
    let opts = FillOptions::tolerance(tol).with_fill_rule(rule.lyon());
    let res = FillTessellator::new().tessellate_path(
        path,
        &opts,
        &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
            let p = v.position();
            DVec2::new(p.x as f64 + o.x, p.y as f64 + o.y)
        }),
    );
    if res.is_err() {
        return (Vec::new(), Vec::new());
    }
    (buf.vertices, buf.indices)
}

/// Total area of a triangle mesh (for tests and diagnostics).
pub fn mesh_area(m: &Mesh) -> f64 {
    m.1.as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, c) = (m.0[t[0] as usize], m.0[t[1] as usize], m.0[t[2] as usize]);
            0.5 * wcad_math::cross2(b - a, c - a).abs()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::{Circle2, Line2};

    #[test]
    fn square_with_hole() {
        let outer = vec![
            DVec2::ZERO,
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        let hole = vec![
            DVec2::new(3.0, 3.0),
            DVec2::new(3.0, 7.0),
            DVec2::new(7.0, 7.0),
            DVec2::new(7.0, 3.0),
        ];
        let m = fill_polygons(&[outer.clone(), hole], FillRule::EvenOdd);
        assert!((mesh_area(&m) - 84.0).abs() < 1e-6);
        // far from the origin
        let far: Vec<DVec2> = outer.iter().map(|p| *p + DVec2::splat(1e7)).collect();
        let m = fill_polygons(&[far], FillRule::NonZero);
        assert!((mesh_area(&m) - 100.0).abs() < 1e-6);
        assert!(m.0.iter().all(|p| p.x >= 1e7 - 1e-6));
        assert_eq!(fill_polygons(&[], FillRule::EvenOdd).1.len(), 0);
    }

    #[test]
    fn loops_and_paths() {
        let c = vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0))];
        let m = fill_loops(&[c], FillRule::EvenOdd, 1e-3);
        assert!((mesh_area(&m) - 25.0 * std::f64::consts::PI).abs() < 0.05);
        let sq: Vec<Curve2> = [
            (0.0, 0.0, 1.0, 0.0),
            (1.0, 0.0, 1.0, 1.0),
            (1.0, 1.0, 0.0, 1.0),
            (0.0, 1.0, 0.0, 0.0),
        ]
        .iter()
        .map(|&(a, b, c, d)| Curve2::Line(Line2::new(DVec2::new(a, b), DVec2::new(c, d))))
        .collect();
        assert!((mesh_area(&fill_loops(&[sq], FillRule::EvenOdd, 1e-3)) - 1.0).abs() < 1e-9);
        let p = vec![
            PathCmd::MoveTo(DVec2::ZERO),
            PathCmd::LineTo(DVec2::new(2.0, 0.0)),
            PathCmd::QuadTo(DVec2::new(3.0, 1.0), DVec2::new(2.0, 2.0)),
            PathCmd::CubicTo(
                DVec2::new(1.5, 2.5),
                DVec2::new(0.5, 2.5),
                DVec2::new(0.0, 2.0),
            ),
            PathCmd::Close,
        ];
        let m = fill_paths(&[p], FillRule::NonZero, 1e-3);
        assert!(mesh_area(&m) > 4.0 && mesh_area(&m) < 6.0);
        // malformed (LineTo first) does not panic
        let _ = fill_paths(
            &[vec![
                PathCmd::LineTo(DVec2::ONE),
                PathCmd::LineTo(DVec2::X),
                PathCmd::Close,
            ]],
            FillRule::NonZero,
            0.01,
        );
    }
}
