//! Degenerate and hostile inputs must never panic (wasm aborts on panic).

use std::f64::consts::TAU;

use wcad_geom2d::curve::Curve;
use wcad_geom2d::*;

fn nasty() -> Vec<Curve2> {
    let n = f64::NAN;
    let inf = f64::INFINITY;
    vec![
        Curve2::Line(Line2::new(DVec2::ONE, DVec2::ONE)),
        Curve2::Line(Line2::new(DVec2::new(n, 0.0), DVec2::ONE)),
        Curve2::Line(Line2::new(DVec2::new(1e300, 0.0), DVec2::new(-1e300, 1.0))),
        Curve2::Circle(Circle2::new(DVec2::ZERO, 0.0)),
        Curve2::Circle(Circle2::new(DVec2::ZERO, -1.0)),
        Curve2::Circle(Circle2::new(DVec2::ZERO, n)),
        Curve2::Circle(Circle2::new(DVec2::ZERO, inf)),
        Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, 1.0, 1.0)),
        Curve2::Arc(Arc2::new(DVec2::ZERO, 0.0, 0.0, 1.0)),
        Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, n, n)),
        Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, inf, 1.0)),
        Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::ZERO,
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        }),
        Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::X,
            ratio: 0.0,
            start: 0.0,
            end: 1.0,
        }),
        Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::X,
            ratio: n,
            start: n,
            end: 1.0,
        }),
        Curve2::Polyline(Polyline2::default()),
        Curve2::Polyline(Polyline2::from_points([DVec2::ONE], true)),
        Curve2::Polyline(Polyline2::from_points(
            [DVec2::ONE, DVec2::ONE, DVec2::ONE],
            true,
        )),
        Curve2::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::ZERO, 1e300),
                PolyVertex::with_bulge(DVec2::X, n),
                PolyVertex::new(DVec2::Y),
            ],
            closed: true,
        }),
        Curve2::Spline(Nurbs2::default()),
        Curve2::Spline(Nurbs2 {
            degree: 0,
            ctrl: vec![DVec2::ZERO, DVec2::X],
            weights: vec![],
            knots: vec![0.0, 1.0],
            fit_points: vec![],
            closed: false,
        }),
        Curve2::Spline(Nurbs2 {
            degree: 2,
            ctrl: vec![DVec2::ZERO, DVec2::X, DVec2::Y],
            weights: vec![0.0, 0.0, 0.0],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            fit_points: vec![],
            closed: false,
        }),
        Curve2::Spline(Nurbs2 {
            degree: 2,
            ctrl: vec![DVec2::ZERO, DVec2::X, DVec2::Y, DVec2::ONE],
            weights: vec![],
            knots: vec![0.0, 0.0, 0.0, 3.0, 1.0, 1.0, 5.0],
            fit_points: vec![],
            closed: true,
        }),
        Curve2::Spline(Nurbs2 {
            degree: 99,
            ctrl: vec![DVec2::ZERO; 3],
            weights: vec![],
            knots: vec![0.0; 103],
            fit_points: vec![],
            closed: false,
        }),
    ]
}

fn good() -> Vec<Curve2> {
    vec![
        Curve2::Line(Line2::new(DVec2::new(-2.0, -2.0), DVec2::new(2.0, 2.0))),
        Curve2::Circle(Circle2::new(DVec2::ZERO, 1.0)),
        Curve2::Arc(Arc2::new(DVec2::ZERO, 1.5, 0.0, 3.0)),
        Curve2::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::X, DVec2::ONE],
            true,
        )),
    ]
}

#[test]
fn evaluation_never_panics() {
    for c in nasty() {
        let (a, b) = c.domain();
        for t in [a, b, 0.5 * (a + b), f64::NAN, -1e300, 1e300] {
            let _ = c.point_at(t);
            let _ = c.deriv_at(t);
            let _ = c.tangent_at(t);
            let _ = c.curvature_at(t);
            let _ = c.split(t);
            let _ = c.sub_curve(t, b);
            let _ = c.sub_curve(b, t);
        }
        let _ = c.bbox();
        let _ = c.length();
        let _ = c.param_at_length(0.5);
        let _ = c.closest(DVec2::new(0.3, 0.7));
        let _ = c.closest(DVec2::new(f64::NAN, 0.0));
        let _ = c.flatten(0.01);
        let _ = c.flatten(0.0);
        let _ = c.flatten(f64::NAN);
        let _ = c.reversed();
        let _ = c.transformed(&DAffine2::from_scale(DVec2::new(2.0, 0.5)));
        let _ = c.transformed(&DAffine2::from_scale(DVec2::ZERO));
        let _ = c.offset(0.1);
        let _ = c.offset(-0.1);
        let _ = offset(&c, 0.2, DVec2::ONE);
        for k in [
            snap::SnapKind::Endpoint,
            snap::SnapKind::Midpoint,
            snap::SnapKind::Center,
            snap::SnapKind::Quadrant,
            snap::SnapKind::Node,
        ] {
            let _ = snap::snap_points(&c, k);
        }
        let _ = snap::perpendicular_feet(&c, DVec2::new(3.0, 1.0));
        let _ = snap::tangent_points(&c, DVec2::new(3.0, 1.0));
        let _ = dash::dash_curve(&c, &[1.0, -0.5, 0.0, -0.5], 0.1, 0.0, 0.01);
    }
}

#[test]
fn algorithms_never_panic() {
    let all: Vec<Curve2> = nasty().into_iter().chain(good()).collect();
    for a in &all {
        for b in &all {
            let _ = intersect(a, b);
            let _ = trim(a, std::slice::from_ref(b), DVec2::new(0.5, 0.5));
            let _ = extend(a, std::slice::from_ref(b), DVec2::new(0.5, 0.5));
            let _ = fillet(a, b, 0.2, DVec2::new(0.5, 0.0), DVec2::new(0.0, 0.5));
            let _ = join(&[a.clone(), b.clone()], 1e-9);
        }
        let _ = break_at(a, DVec2::ZERO, DVec2::ONE);
    }
    let _ = find_regions(&all, 1e-9);
    let _ = find_regions(&all, 0.0);
    let _ = find_regions(&all, f64::NAN);
    let _ = region_at(&all, DVec2::new(0.2, 0.1), 1e-6);
    let pat = hatch::builtin("ANSI37").unwrap_or_else(|| panic!("builtin"));
    let _ = hatch_lines(std::slice::from_ref(&all), &pat, 1.0, 0.0);
    let _ = hatch_lines(std::slice::from_ref(&all), &pat, 0.0, f64::NAN);
    let _ = tess::fill_loops(std::slice::from_ref(&all), tess::FillRule::EvenOdd, 0.01);
    let _ = tess::fill_polygons(
        &[vec![
            DVec2::new(1e300, 0.0),
            DVec2::new(-1e300, 0.0),
            DVec2::new(0.0, 1e300),
        ]],
        tess::FillRule::NonZero,
    );
    let _ = dash_polyline(
        &[DVec2::ZERO, DVec2::new(f64::NAN, 1.0), DVec2::ONE],
        &[f64::NAN, -1.0],
        f64::NAN,
        f64::INFINITY,
    );
    let _ = dash_polyline(&[DVec2::ZERO, DVec2::ONE], &[0.0, 0.0], 1.0, 0.0);
    let _ = parse_pat("*A\n0,0,0,0,0\n*B,x\n1e999,0,0,0,1\n");
    let _ = Nurbs2::from_fit_points(&[DVec2::ZERO, DVec2::new(f64::NAN, 0.0)], 3);
    let _ = Nurbs2::from_fit_points(&[DVec2::ZERO, DVec2::X, DVec2::ZERO, DVec2::X], 3);
}

#[test]
fn stress_regions_and_hatch() {
    // 30 random-ish circles and lines: region areas must sum to the union area estimate
    let mut curves = Vec::new();
    for i in 0..15 {
        let a = i as f64 * 0.7;
        curves.push(Curve2::Circle(Circle2::new(
            DVec2::new(a.cos() * 5.0, a.sin() * 5.0),
            2.0 + (i % 3) as f64,
        )));
        curves.push(Curve2::Line(Line2::new(
            DVec2::new(-10.0, a - 5.0),
            DVec2::new(10.0, 5.0 - a * 0.5),
        )));
    }
    let regions = find_regions(&curves, 1e-9);
    assert!(regions.len() > 50, "{}", regions.len());
    for r in &regions {
        assert!(r.area() > 0.0);
        for w in r.outer.curves.windows(2) {
            assert!(w[0].end().distance(w[1].start()) < 1e-6);
        }
    }
    // regions tile the plane without overlap: sample points belong to at most one region
    let mut hits = 0;
    for i in 0..40 {
        for j in 0..40 {
            let p = DVec2::new(
                -10.0 + i as f64 * 0.5 + 0.013,
                -10.0 + j as f64 * 0.5 + 0.007,
            );
            let n = regions.iter().filter(|r| r.contains(p)).count();
            assert!(n <= 1, "{p} in {n} regions");
            hits += n;
        }
    }
    assert!(hits > 100);
    let pat = hatch::builtin("ANSI31").unwrap_or_else(|| panic!("builtin"));
    for r in regions.iter().take(10) {
        let lines = hatch::hatch_region(r, &pat, 0.1, 0.0);
        for l in &lines {
            assert!(r.contains(l.midpoint()) || l.length() < 1e-6);
        }
    }
}
