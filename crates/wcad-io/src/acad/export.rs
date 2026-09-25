//! `wcad_doc::Document` → `acadrust::CadDocument`.

use std::collections::HashMap;
use std::f64::consts::TAU;

use acadrust::entities::dimension::{
    Dimension as ADim, DimensionAligned, DimensionAngular3Pt, DimensionBase, DimensionDiameter, DimensionLinear,
    DimensionOrdinate, DimensionRadius,
};
use acadrust::entities::hatch::{
    BoundaryEdge, BoundaryPath, CircularArcEdge, EllipticArcEdge, HatchPattern, HatchPatternLine, LineEdge,
    PolylineEdge, SplineEdge,
};
use acadrust::entities::mtext::AttachmentPoint;
use acadrust::entities::text::{TextHorizontalAlignment, TextVerticalAlignment};
use acadrust::entities::{
    Arc as AArc, Circle as ACircle, Ellipse as AEllipse, EntityCommon, EntityType, Hatch as AHatch, Insert as AInsert,
    Line as ALine, LwPolyline, MText as AMText, Point as APoint, Solid as ASolid, Spline as ASpline, Text as AText,
};
use acadrust::tables::linetype::LineTypeElement;
use acadrust::tables::{BlockRecord, DimStyle as ADimStyle, Layer as ALayer, LineType, TextStyle as ATextStyle};
use acadrust::types::{DxfVersion as AVersion, Handle, Vector3};
use acadrust::{CadDocument, TableEntry};
use wcad_doc::{
    Color, DimKind, Dimension, Document, Drawing, Entity, EntityKind, HAlign, Hatch, LineWeight, LinetypeRef, VAlign,
};
use wcad_geom2d::{Curve2, Nurbs2};
use wcad_math::{DVec2, normalize_0_2pi};

use super::{Flavor, a2, a3, color_to, ellipse_param_to_angle, lineweight_to, units_to};
use crate::{DxfVersion, Error, Result, dimgeom, pattern};

pub(crate) fn acad_version(v: DxfVersion) -> AVersion {
    match v {
        DxfVersion::R2000 => AVersion::AC1015,
        DxfVersion::R2004 => AVersion::AC1018,
        DxfVersion::R2007 => AVersion::AC1021,
        DxfVersion::R2010 => AVersion::AC1024,
        DxfVersion::R2013 => AVersion::AC1027,
        DxfVersion::R2018 => AVersion::AC1032,
    }
}

struct Names<'a> {
    drawing: &'a Drawing,
    flavor: Flavor,
    /// Encode non-ASCII text as escapes (pre-2007 releases use a single-byte code page).
    escape: bool,
    blocks: HashMap<wcad_doc::BlockId, String>,
}

fn enc_if(escape: bool, s: &str) -> String {
    if escape { crate::mtext::encode_unicode_escapes(s) } else { s.to_string() }
}

pub(crate) fn from_document(doc: &Document, version: DxfVersion, flavor: Flavor) -> Result<CadDocument> {
    let d = &doc.drawing;
    let escape = matches!(version, DxfVersion::R2000 | DxfVersion::R2004);
    let enc = |s: &str| enc_if(escape, s);
    let mut cad = CadDocument::with_version(acad_version(version));
    cad.header.insertion_units = units_to(doc.meta.units);
    let lts = d.tables.settings.ltscale;
    cad.header.linetype_scale = if lts.is_finite() && lts > 0.0 { lts } else { 1.0 };

    // Linetypes.
    for lt in d.tables.linetypes.values() {
        let upper = lt.name.to_ascii_uppercase();
        if upper == "BYLAYER" || upper == "BYBLOCK" || upper == "CONTINUOUS" || lt.name.is_empty() {
            continue;
        }
        let mut a = LineType::new(enc(&lt.name));
        a.description = enc(&lt.description);
        for &len in &lt.pattern {
            if len.is_finite() {
                a.add_element(LineTypeElement { length: len, complex: None });
            }
        }
        a.pattern_length = lt.pattern_length();
        a.set_handle(cad.allocate_handle());
        cad.line_types.add_or_replace(a);
    }

    // Layers.
    for l in d.tables.layers.values() {
        let linetype = d.tables.linetypes.get(&l.linetype).map(|t| enc(&t.name)).unwrap_or_else(|| "Continuous".into());
        let color = match l.color {
            Color::ByLayer | Color::ByBlock => color_to(Color::WHITE),
            c => color_to(c),
        };
        let lineweight = match l.lineweight {
            LineWeight::ByLayer | LineWeight::ByBlock => LineWeight::Default,
            w => w,
        };
        let apply = |a: &mut ALayer| {
            a.color = color;
            a.line_type = linetype.clone();
            a.line_weight = lineweight_to(lineweight);
            a.flags.off = !l.visible;
            a.flags.frozen = l.frozen;
            a.flags.locked = l.locked;
            a.is_plottable = l.plot;
        };
        let name = enc(&l.name);
        if let Some(existing) = cad.layers.get_mut(&name) {
            apply(existing);
        } else {
            let mut a = ALayer::new(name);
            apply(&mut a);
            a.set_handle(cad.allocate_handle());
            cad.layers.add_or_replace(a);
        }
    }

    // Text styles.
    for s in d.tables.text_styles.values() {
        let oblique = match flavor {
            Flavor::Dxf => s.oblique.to_degrees(),
            Flavor::Dwg => s.oblique,
        };
        let apply = |a: &mut ATextStyle| {
            a.height = if s.height.is_finite() && s.height > 0.0 { s.height } else { 0.0 };
            a.width_factor = if s.width_factor.is_finite() && s.width_factor > 0.0 { s.width_factor } else { 1.0 };
            a.oblique_angle = if oblique.is_finite() { oblique } else { 0.0 };
            if s.font.eq_ignore_ascii_case("default") || s.font.is_empty() {
                // AutoCAD's default shape font plus the Simplified Chinese big font.
                a.font_file = "txt".into();
                a.big_font_file = "gbcbig.shx".into();
                a.true_type_font.clear();
            } else {
                a.font_file = s.font.clone();
            }
        };
        let name = enc(&s.name);
        if let Some(existing) = cad.text_styles.get_mut(&name) {
            apply(existing);
        } else {
            let mut a = ATextStyle::new(name);
            apply(&mut a);
            a.set_handle(cad.allocate_handle());
            cad.text_styles.add_or_replace(a);
        }
    }

    // Dimension styles.
    for s in d.tables.dim_styles.values() {
        let text_style =
            d.tables.text_styles.get(&s.text_style).map(|t| enc(&t.name)).unwrap_or_else(|| "Standard".into());
        let text_style_handle = cad.text_styles.get(&text_style).map(|t| t.handle()).unwrap_or(Handle::NULL);
        let apply = |a: &mut ADimStyle| {
            a.dimtxt = s.text_height;
            a.dimasz = s.arrow_size;
            a.dimexo = s.ext_offset;
            a.dimexe = s.ext_extend;
            a.dimgap = s.text_gap;
            a.dimscale = s.scale;
            a.dimdec = s.decimals as i16;
            a.dimadec = s.angle_decimals as i16;
            a.dimpost = if s.prefix.is_empty() && s.suffix.is_empty() {
                String::new()
            } else {
                enc(&format!("{}<>{}", s.prefix, s.suffix))
            };
            a.dimtxsty = text_style.clone();
            if !text_style_handle.is_null() {
                a.dimtxsty_handle = text_style_handle;
            }
        };
        let name = enc(&s.name);
        if let Some(existing) = cad.dim_styles.get_mut(&name) {
            apply(existing);
        } else {
            let mut a = ADimStyle::new(name);
            apply(&mut a);
            a.set_handle(cad.allocate_handle());
            cad.dim_styles.add_or_replace(a);
        }
    }

    // Blocks. Base points are folded into the geometry (acadrust's DXF writer drops them).
    let mut names = Names { drawing: d, flavor, escape, blocks: HashMap::new() };
    let mut records = Vec::new();
    for (&id, b) in &d.blocks {
        let upper = b.name.to_ascii_uppercase();
        if b.name.is_empty() || upper.starts_with("*MODEL_SPACE") || upper.starts_with("*PAPER_SPACE") {
            continue;
        }
        let name = enc(&b.name);
        if cad.block_records.get(&name).is_some() {
            continue; // duplicate name
        }
        let mut br = BlockRecord::new(name.clone());
        let h = cad.allocate_handle();
        br.set_handle(h);
        br.block_entity_handle = cad.allocate_handle();
        br.block_end_handle = cad.allocate_handle();
        cad.block_records.add(br).map_err(|m| Error::Write { format: "block table", message: m })?;
        names.blocks.insert(id, name);
        records.push((h, b));
    }
    let mut dim_counter = 0usize;
    for (h, b) in records {
        for e in b.entities.values() {
            let mut e = e.clone();
            if b.base != DVec2::ZERO {
                translate(&mut e.kind, -b.base);
            }
            add_entity(&mut cad, &names, &e, Some(h), &mut dim_counter)?;
        }
    }
    for e in d.entities.values() {
        add_entity(&mut cad, &names, e, None, &mut dim_counter)?;
    }
    Ok(cad)
}

fn add_entity(
    cad: &mut CadDocument,
    names: &Names<'_>,
    e: &Entity,
    owner: Option<Handle>,
    dims: &mut usize,
) -> Result<()> {
    let Some(mut ae) = names.entity(e) else { return Ok(()) };
    if let (EntityKind::Dimension(d), EntityType::Dimension(ad)) = (&e.kind, &mut ae)
        && let Some(style) = names.drawing.tables.dim_styles.get(&d.style)
        && let Some(name) = dim_block(cad, names, d, style, dims)?
    {
        ad.base_mut().block_name = name;
    }
    if let Some(h) = owner {
        ae.common_mut().owner_handle = h;
    }
    cad.add_entity(ae).map_err(|err| Error::Write { format: "entity", message: err.to_string() })?;
    Ok(())
}

/// Anonymous `*D<n>` block with the dimension's graphics, so viewers that do not regenerate
/// dimensions (and AutoCAD, which expects the block) show them.
fn dim_block(
    cad: &mut CadDocument,
    names: &Names<'_>,
    d: &Dimension,
    style: &wcad_doc::DimStyle,
    counter: &mut usize,
) -> Result<Option<String>> {
    let Some(g) = dimgeom::build(d, style) else { return Ok(None) };
    let name = loop {
        *counter += 1;
        let n = format!("*D{counter}");
        if cad.block_records.get(&n).is_none() {
            break n;
        }
    };
    let mut br = BlockRecord::new(name.clone());
    br.flags.anonymous = true;
    let h = cad.allocate_handle();
    br.set_handle(h);
    br.block_entity_handle = cad.allocate_handle();
    br.block_end_handle = cad.allocate_handle();
    cad.block_records.add(br).map_err(|m| Error::Write { format: "dimension block", message: m })?;
    let mut parts: Vec<EntityType> = Vec::new();
    for (a, b) in &g.lines {
        parts.push(EntityType::Line(ALine::from_coords(a.x, a.y, 0.0, b.x, b.y, 0.0)));
    }
    for arc in &g.arcs {
        parts.push(EntityType::Arc(AArc::from_coords(
            arc.c.x,
            arc.c.y,
            0.0,
            arc.r,
            normalize_0_2pi(arc.start),
            normalize_0_2pi(arc.end),
        )));
    }
    for t in &g.arrows {
        parts.push(EntityType::Solid(ASolid::new(a3(t[0]), a3(t[1]), a3(t[2]), a3(t[2]))));
    }
    if !g.text.is_empty() {
        let mut m = AMText::with_value(names.enc(&g.text), a3(g.text_pos));
        m.height = g.text_height;
        m.rotation = g.text_rotation;
        m.style = names.style_name(style.text_style);
        let row = match g.valign {
            VAlign::Top => 0,
            VAlign::Middle => 1,
            VAlign::Bottom | VAlign::Baseline => 2,
        };
        let col = match g.halign {
            HAlign::Left => 1,
            HAlign::Center => 2,
            HAlign::Right => 3,
        };
        m.attachment_point = attachment(row * 3 + col);
        parts.push(EntityType::MText(m));
    }
    for mut p in parts {
        let c = p.common_mut();
        c.owner_handle = h;
        c.layer = "0".into();
        c.color = acadrust::types::Color::ByBlock;
        c.line_weight = acadrust::types::LineWeight::ByBlock;
        c.linetype = "ByBlock".into();
        cad.add_entity(p).map_err(|err| Error::Write { format: "dimension block", message: err.to_string() })?;
    }
    Ok(Some(name))
}

impl Names<'_> {
    fn common(&self, e: &Entity) -> EntityCommon {
        let t = &self.drawing.tables;
        let mut c = EntityCommon::new();
        c.layer = t.layers.get(&e.layer).map(|l| self.enc(&l.name)).unwrap_or_else(|| "0".into());
        c.color = color_to(e.color);
        c.linetype = match e.linetype {
            LinetypeRef::ByLayer => "ByLayer".into(),
            LinetypeRef::ByBlock => "ByBlock".into(),
            LinetypeRef::Id(id) => t.linetypes.get(&id).map(|l| self.enc(&l.name)).unwrap_or_else(|| "ByLayer".into()),
        };
        c.linetype_scale = if e.linetype_scale.is_finite() && e.linetype_scale > 0.0 { e.linetype_scale } else { 1.0 };
        c.line_weight = lineweight_to(e.lineweight);
        c
    }

    fn enc(&self, s: &str) -> String {
        enc_if(self.escape, s)
    }

    fn style_name(&self, id: wcad_doc::TextStyleId) -> String {
        self.drawing.tables.text_styles.get(&id).map(|s| self.enc(&s.name)).unwrap_or_else(|| "Standard".into())
    }

    fn entity(&self, e: &Entity) -> Option<EntityType> {
        let common = self.common(e);
        let mut out = match &e.kind {
            EntityKind::Point { p } => EntityType::Point(APoint::from_coords(p.x, p.y, 0.0)),
            EntityKind::Line(l) => EntityType::Line(ALine::from_coords(l.a.x, l.a.y, 0.0, l.b.x, l.b.y, 0.0)),
            EntityKind::Circle(c) => EntityType::Circle(ACircle::from_coords(c.c.x, c.c.y, 0.0, c.r)),
            EntityKind::Arc(a) => EntityType::Arc(AArc::from_coords(
                a.c.x,
                a.c.y,
                0.0,
                a.r,
                normalize_0_2pi(a.start),
                normalize_0_2pi(a.end),
            )),
            EntityKind::Ellipse(el) => {
                let mut a = AEllipse::from_center_axes(a3(el.c), a3(el.major), el.ratio.clamp(1e-6, 1.0));
                if el.is_full() {
                    a.start_parameter = 0.0;
                    a.end_parameter = TAU;
                } else {
                    a.start_parameter = normalize_0_2pi(el.start);
                    a.end_parameter = normalize_0_2pi(el.end);
                }
                EntityType::Ellipse(a)
            }
            EntityKind::Polyline(p) => {
                let mut lw = LwPolyline::new();
                for v in &p.verts {
                    lw.add_point_with_bulge(a2(v.p), v.bulge);
                }
                lw.is_closed = p.closed;
                EntityType::LwPolyline(lw)
            }
            EntityKind::Spline(s) => EntityType::Spline(spline(s)?),
            EntityKind::Text(t) => {
                let mut a = AText::with_value(self.enc(&t.text), a3(t.pos));
                a.height = t.height;
                a.rotation = t.rotation;
                a.width_factor = t.width_factor;
                a.oblique_angle = match self.flavor {
                    Flavor::Dxf => t.oblique.to_degrees(),
                    Flavor::Dwg => t.oblique,
                };
                a.style = self.style_name(t.style);
                a.horizontal_alignment = match t.halign {
                    HAlign::Left => TextHorizontalAlignment::Left,
                    HAlign::Center => TextHorizontalAlignment::Center,
                    HAlign::Right => TextHorizontalAlignment::Right,
                };
                a.vertical_alignment = match t.valign {
                    VAlign::Baseline => TextVerticalAlignment::Baseline,
                    VAlign::Bottom => TextVerticalAlignment::Bottom,
                    VAlign::Middle => TextVerticalAlignment::Middle,
                    VAlign::Top => TextVerticalAlignment::Top,
                };
                if !(t.halign == HAlign::Left && t.valign == VAlign::Baseline) {
                    a.alignment_point = Some(a3(t.pos));
                }
                EntityType::Text(a)
            }
            EntityKind::MText(m) => {
                let mut a = AMText::with_value(self.enc(&m.text), a3(m.pos));
                a.height = m.height;
                a.rectangle_width = m.width.max(0.0);
                a.rotation = m.rotation;
                a.style = self.style_name(m.style);
                a.line_spacing_factor = if m.line_spacing > 0.0 { m.line_spacing } else { 1.0 };
                a.attachment_point = attachment(m.attachment);
                EntityType::MText(a)
            }
            EntityKind::Dimension(d) => EntityType::Dimension(self.dimension(d)),
            EntityKind::Hatch(h) => EntityType::Hatch(self.hatch(h)?),
            EntityKind::Insert(i) => {
                let name = self.blocks.get(&i.block)?;
                let mut a = AInsert::new(name.clone(), a3(i.pos));
                a.set_x_scale(if i.scale.x.is_finite() && i.scale.x != 0.0 { i.scale.x } else { 1.0 });
                a.set_y_scale(if i.scale.y.is_finite() && i.scale.y != 0.0 { i.scale.y } else { 1.0 });
                a.rotation = i.rotation;
                EntityType::Insert(a)
            }
        };
        let c = out.common_mut();
        c.layer = common.layer;
        c.color = common.color;
        c.linetype = common.linetype;
        c.linetype_scale = common.linetype_scale;
        c.line_weight = common.line_weight;
        Some(out)
    }

    fn dimension(&self, d: &Dimension) -> ADim {
        let style = self.drawing.tables.dim_styles.get(&d.style);
        let style_name = style.map(|s| self.enc(&s.name)).unwrap_or_else(|| "Standard".into());
        let measurement = dimgeom::measure(&d.kind);
        let mut dim = match d.kind {
            DimKind::Linear { p1, p2, line_point, rotation } => {
                let mut x = DimensionLinear::rotated(a3(p1), a3(p2), rotation);
                x.definition_point = a3(line_point);
                ADim::Linear(x)
            }
            DimKind::Aligned { p1, p2, line_point } => {
                let mut x = DimensionAligned::new(a3(p1), a3(p2));
                x.definition_point = a3(line_point);
                ADim::Aligned(x)
            }
            DimKind::Radius { center, point } => ADim::Radius(DimensionRadius::new(a3(center), a3(point))),
            DimKind::Diameter { center, point } => {
                ADim::Diameter(DimensionDiameter::new(a3(point), a3(center * 2.0 - point)))
            }
            DimKind::Angular { vertex, p1, p2, arc_point } => {
                let mut x = DimensionAngular3Pt::new(a3(vertex), a3(p1), a3(p2));
                x.definition_point = a3(arc_point);
                ADim::Angular3Pt(x)
            }
            DimKind::Ordinate { origin, point, leader_end, x_axis } => {
                let mut x = DimensionOrdinate::new(a3(point), a3(leader_end), x_axis);
                x.definition_point = a3(origin);
                x.refresh_measurement();
                ADim::Ordinate(x)
            }
        };
        // Keep the base definition point in sync with the subtype (the DWG writer reads either).
        let def = match &dim {
            ADim::Radius(r) => r.angle_vertex,
            other => other_definition_point(other),
        };
        let base: &mut DimensionBase = dim.base_mut();
        base.definition_point = def;
        base.style_name = style_name;
        base.actual_measurement = match d.kind {
            DimKind::Angular { .. } => measurement.to_degrees(),
            _ => measurement,
        };
        base.set_text_override(d.text_override.as_deref().filter(|t| !t.is_empty()).map(|t| self.enc(t)));
        let text_mid = match (d.text_pos, style) {
            (Some(p), _) => p,
            (None, Some(s)) => dimgeom::default_text_pos(d, s),
            (None, None) => DVec2::ZERO,
        };
        base.text_middle_point = a3(text_mid);
        base.text_user_positioned = d.text_pos.is_some();
        dim
    }

    fn hatch(&self, h: &Hatch) -> Option<AHatch> {
        let mut a = if h.is_solid() {
            AHatch::solid()
        } else {
            let mut pat = HatchPattern::new(h.pattern.name.clone());
            let scale = if h.pattern.scale.is_finite() && h.pattern.scale > 0.0 { h.pattern.scale } else { 1.0 };
            for f in pattern::families(&pattern::lines_or_default(&h.pattern.name), h.pattern.angle, scale) {
                pat.add_line(HatchPatternLine {
                    angle: f.dir.y.atan2(f.dir.x),
                    base_point: a2(f.base),
                    offset: a2(f.offset),
                    dash_lengths: f.dashes.clone(),
                });
            }
            let mut a = AHatch::with_pattern(pat);
            a.pattern_angle = h.pattern.angle;
            a.pattern_scale = scale;
            a
        };
        for lp in &h.loops {
            let path = self.boundary(&lp.curves);
            if !path.edges.is_empty() {
                a.add_path(path);
            }
        }
        if a.paths.is_empty() {
            return None;
        }
        Some(a)
    }

    fn boundary(&self, curves: &[Curve2]) -> BoundaryPath {
        let mut path = BoundaryPath::new();
        // A single closed polyline is written as a polyline path.
        if let [Curve2::Polyline(p)] = curves
            && p.verts.len() >= 2
        {
            let verts = p.verts.iter().map(|v| Vector3::new(v.p.x, v.p.y, v.bulge)).collect();
            path.add_edge(BoundaryEdge::Polyline(PolylineEdge { vertices: verts, is_closed: true }));
            return path;
        }
        // Otherwise: an edge path, keeping the chain head-to-tail.
        let mut pieces: Vec<Curve2> = Vec::new();
        for c in curves {
            match c {
                Curve2::Polyline(p) => pieces.extend(explode_polyline(p)),
                other => pieces.push(other.clone()),
            }
        }
        let mut cur: Option<DVec2> = None;
        for c in &pieces {
            let Some((s, e)) = endpoints(c) else { continue };
            let reversed = cur.is_some_and(|p| p.distance_squared(e) < p.distance_squared(s));
            cur = Some(if reversed { s } else { e });
            match c {
                Curve2::Line(l) => {
                    let (a, b) = if reversed { (l.b, l.a) } else { (l.a, l.b) };
                    path.add_edge(BoundaryEdge::Line(LineEdge { start: a2(a), end: a2(b) }));
                }
                Curve2::Arc(arc) => {
                    let (start, end) = (arc.start, arc.start + arc.sweep());
                    path.add_edge(BoundaryEdge::CircularArc(if reversed {
                        CircularArcEdge {
                            center: a2(arc.c),
                            radius: arc.r,
                            start_angle: -end,
                            end_angle: -start,
                            counter_clockwise: false,
                        }
                    } else {
                        CircularArcEdge {
                            center: a2(arc.c),
                            radius: arc.r,
                            start_angle: start,
                            end_angle: end,
                            counter_clockwise: true,
                        }
                    }));
                }
                Curve2::Circle(ci) => path.add_edge(BoundaryEdge::CircularArc(CircularArcEdge {
                    center: a2(ci.c),
                    radius: ci.r,
                    start_angle: 0.0,
                    end_angle: TAU,
                    counter_clockwise: true,
                })),
                Curve2::Ellipse(el) => {
                    let full = el.is_full();
                    let (ps, pe) =
                        if full { (0.0, TAU) } else { (el.start, el.start + wcad_math::ccw_sweep(el.start, el.end)) };
                    let (mut s, mut e) = if full {
                        (0.0, TAU)
                    } else {
                        let s = ellipse_param_to_angle(el.ratio, ps);
                        let mut e = ellipse_param_to_angle(el.ratio, pe);
                        while e <= s {
                            e += TAU;
                        }
                        (s, e)
                    };
                    let ccw = !reversed;
                    if reversed {
                        (s, e) = (-e, -s);
                    }
                    let (s, e) = match self.flavor {
                        Flavor::Dxf => (s.to_degrees(), e.to_degrees()),
                        Flavor::Dwg => (s, e),
                    };
                    path.add_edge(BoundaryEdge::EllipticArc(EllipticArcEdge {
                        center: a2(el.c),
                        major_axis_endpoint: a2(el.major),
                        minor_axis_ratio: el.ratio,
                        start_angle: s,
                        end_angle: e,
                        counter_clockwise: ccw,
                    }));
                }
                Curve2::Spline(sp) => {
                    let rational = !sp.weights.is_empty() && sp.weights.len() == sp.ctrl.len();
                    let knots = if sp.knots.len() == sp.ctrl.len() + sp.degree as usize + 1 {
                        sp.knots.clone()
                    } else {
                        clamped_knots(sp.degree as usize, sp.ctrl.len())
                    };
                    path.add_edge(BoundaryEdge::Spline(SplineEdge {
                        degree: sp.degree as i32,
                        rational,
                        periodic: false,
                        knots,
                        control_points: sp
                            .ctrl
                            .iter()
                            .enumerate()
                            .map(|(i, p)| Vector3::new(p.x, p.y, if rational { sp.weights[i] } else { 1.0 }))
                            .collect(),
                        fit_points: sp.fit_points.iter().map(|p| a2(*p)).collect(),
                        start_tangent: acadrust::types::Vector2::new(0.0, 0.0),
                        end_tangent: acadrust::types::Vector2::new(0.0, 0.0),
                    }));
                }
                Curve2::Polyline(_) => {}
            }
        }
        path
    }
}

fn other_definition_point(d: &ADim) -> Vector3 {
    match d {
        ADim::Aligned(x) => x.definition_point,
        ADim::Linear(x) => x.definition_point,
        ADim::Radius(x) => x.definition_point,
        ADim::Diameter(x) => x.definition_point,
        ADim::Angular2Ln(x) => x.definition_point,
        ADim::Angular3Pt(x) => x.definition_point,
        ADim::Ordinate(x) => x.definition_point,
        ADim::Arc(x) => x.definition_point,
        ADim::LargeRadial(x) => x.definition_point,
    }
}

fn attachment(a: u8) -> AttachmentPoint {
    match a {
        2 => AttachmentPoint::TopCenter,
        3 => AttachmentPoint::TopRight,
        4 => AttachmentPoint::MiddleLeft,
        5 => AttachmentPoint::MiddleCenter,
        6 => AttachmentPoint::MiddleRight,
        7 => AttachmentPoint::BottomLeft,
        8 => AttachmentPoint::BottomCenter,
        9 => AttachmentPoint::BottomRight,
        _ => AttachmentPoint::TopLeft,
    }
}

/// Clamped uniform knot vector with `n + p + 1` entries.
fn clamped_knots(p: usize, n: usize) -> Vec<f64> {
    let m = n + p + 1;
    let inner = n.saturating_sub(p);
    (0..m)
        .map(|i| {
            if i <= p {
                0.0
            } else if i >= n {
                1.0
            } else {
                (i - p) as f64 / inner.max(1) as f64
            }
        })
        .collect()
}

fn spline(s: &Nurbs2) -> Option<ASpline> {
    let p = s.degree.clamp(1, 32) as usize;
    let mut a = ASpline::new();
    a.degree = p as i32;
    a.flags.planar = true;
    a.flags.closed = s.closed;
    a.normal = Vector3::new(0.0, 0.0, 1.0);
    a.fit_points = s.fit_points.iter().map(|q| a3(*q)).collect();
    if s.ctrl.len() > p {
        a.control_points = s.ctrl.iter().map(|q| a3(*q)).collect();
        a.knots = if s.knots.len() == s.ctrl.len() + p + 1 { s.knots.clone() } else { clamped_knots(p, s.ctrl.len()) };
        if !s.weights.is_empty() && s.weights.len() == s.ctrl.len() {
            a.weights = s.weights.clone();
            a.flags.rational = true;
        }
    } else if a.fit_points.len() < 2 {
        return None;
    }
    Some(a)
}

fn endpoints(c: &Curve2) -> Option<(DVec2, DVec2)> {
    let (s, segs) = crate::geom::curve_segs(c)?;
    Some((s, crate::geom::segs_end(s, &segs)))
}

/// Split a polyline into line and arc curves (edge paths cannot contain polylines).
fn explode_polyline(p: &wcad_geom2d::Polyline2) -> Vec<Curve2> {
    let mut out = Vec::new();
    let n = p.verts.len();
    for i in 0..p.segment_count() {
        let a = p.verts[i];
        let b = p.verts[(i + 1) % n];
        match crate::geom::bulge_arc(a.p, b.p, a.bulge) {
            Some(crate::geom::Seg::Arc { c, u, t0, dt, .. }) => {
                let r = u.length();
                let arc = if dt >= 0.0 {
                    wcad_geom2d::Arc2::new(c, r, t0, t0 + dt)
                } else {
                    // Clockwise segment: same arc, CCW from its end.
                    wcad_geom2d::Arc2::new(c, r, t0 + dt, t0)
                };
                out.push(Curve2::Arc(arc));
            }
            _ => out.push(Curve2::Line(wcad_geom2d::Line2::new(a.p, b.p))),
        }
    }
    out
}

/// Translate an entity kind by `d` (block base normalization).
fn translate(kind: &mut EntityKind, d: DVec2) {
    match kind {
        EntityKind::Point { p } => *p += d,
        EntityKind::Text(t) => t.pos += d,
        EntityKind::MText(t) => t.pos += d,
        EntityKind::Insert(i) => i.pos += d,
        EntityKind::Dimension(dim) => {
            match &mut dim.kind {
                DimKind::Linear { p1, p2, line_point, .. } | DimKind::Aligned { p1, p2, line_point } => {
                    *p1 += d;
                    *p2 += d;
                    *line_point += d;
                }
                DimKind::Radius { center, point } | DimKind::Diameter { center, point } => {
                    *center += d;
                    *point += d;
                }
                DimKind::Angular { vertex, p1, p2, arc_point } => {
                    *vertex += d;
                    *p1 += d;
                    *p2 += d;
                    *arc_point += d;
                }
                DimKind::Ordinate { origin, point, leader_end, .. } => {
                    *origin += d;
                    *point += d;
                    *leader_end += d;
                }
            }
            if let Some(p) = &mut dim.text_pos {
                *p += d;
            }
        }
        EntityKind::Hatch(h) => {
            for lp in &mut h.loops {
                for c in &mut lp.curves {
                    *c = translate_curve(c, d);
                }
            }
        }
        other => {
            if let Some(c) = other.as_curve() {
                *other = EntityKind::from_curve(translate_curve(&c, d));
            }
        }
    }
}

fn translate_curve(c: &Curve2, d: DVec2) -> Curve2 {
    let mut c = c.clone();
    match &mut c {
        Curve2::Line(l) => {
            l.a += d;
            l.b += d;
        }
        Curve2::Circle(x) => x.c += d,
        Curve2::Arc(x) => x.c += d,
        Curve2::Ellipse(x) => x.c += d,
        Curve2::Polyline(p) => {
            for v in &mut p.verts {
                v.p += d;
            }
        }
        Curve2::Spline(s) => {
            for p in &mut s.ctrl {
                *p += d;
            }
            for p in &mut s.fit_points {
                *p += d;
            }
        }
    }
    c
}
