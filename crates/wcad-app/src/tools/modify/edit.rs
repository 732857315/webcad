//! Curve editing tools. Geometry is computed before opening the single undo transaction.

use std::f64::consts::TAU;

use wcad_doc::{BlockId, Color, Drawing, Entity, EntityId, EntityKind, LineWeight, LinetypeRef};
use wcad_geom2d::{Curve, Curve2, PolySegment, edit as geom, offset};
use wcad_math::{DAffine2, DVec2};

use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::i18n::Lang;
use crate::tools::{Accept, Preview, Tool, ToolCx, ToolFlow, ToolInput, dedup_ids};

const JOIN_TOL: f64 = 1e-9;
const MAX_BLOCK_DEPTH: usize = 16;
const MAX_PARTS: usize = 4096;

pub fn register(r: &mut CommandRegistry) {
    let specs: [CommandSpec; 8] = [
        spec(
            "OFFSET",
            &["O"],
            |l| l.pick("偏移", "Offset"),
            "||",
            || Box::new(EditTool::new(Op::Offset)),
        ),
        spec(
            "TRIM",
            &["TR"],
            |l| l.pick("修剪", "Trim"),
            "-/-",
            || Box::new(EditTool::new(Op::Trim)),
        ),
        spec(
            "EXTEND",
            &["EX"],
            |l| l.pick("延伸", "Extend"),
            "-->",
            || Box::new(EditTool::new(Op::Extend)),
        ),
        spec(
            "FILLET",
            &["F"],
            |l| l.pick("圆角", "Fillet"),
            ")",
            || Box::new(EditTool::new(Op::Fillet)),
        ),
        spec(
            "CHAMFER",
            &["CHA"],
            |l| l.pick("倒角", "Chamfer"),
            "/",
            || Box::new(EditTool::new(Op::Chamfer)),
        ),
        spec(
            "BREAK",
            &["BR"],
            |l| l.pick("打断", "Break"),
            "- -",
            || Box::new(EditTool::new(Op::Break)),
        ),
        spec(
            "EXPLODE",
            &["X"],
            |l| l.pick("分解", "Explode"),
            "*",
            || Box::new(EditTool::new(Op::Explode)),
        ),
        spec(
            "JOIN",
            &["J"],
            |l| l.pick("合并", "Join"),
            "+",
            || Box::new(EditTool::new(Op::Join)),
        ),
    ];
    for s in specs {
        r.add(s);
    }
}

fn spec(
    name: &'static str,
    aliases: &'static [&'static str],
    label: fn(Lang) -> &'static str,
    icon: &'static str,
    ctor: fn() -> Box<dyn Tool>,
) -> CommandSpec {
    CommandSpec {
        name,
        aliases,
        label,
        icon,
        tab: Some(RibbonTab::Modify),
        group: "curve_edit",
        kind: CommandKind::Tool(ctor),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Offset,
    Trim,
    Extend,
    Fillet,
    Chamfer,
    Break,
    Explode,
    Join,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Value1,
    Value2,
    Boundaries,
    Selection,
    Object,
    Second,
    Side,
    Break1,
    Break2,
}

#[derive(Debug)]
enum EditError {
    Pick,
    Invalid,
    Unsupported,
    Locked,
    Changed,
    NoSolution,
    SameObject,
    Boundary,
    JoinProperties,
    BlockLimit,
    BlockContent,
    MissingBlock,
}

type EditResult<T> = Result<T, EditError>;

impl EditError {
    fn report(&self, cx: &mut ToolCx<'_>) {
        let (zh, en) = match self {
            Self::Pick => ("未拾取到对象。", "No object at the pick point."),
            Self::Invalid => (
                "输入或几何无效、退化或超出数值范围。",
                "Invalid, degenerate or out-of-range input/geometry.",
            ),
            Self::Unsupported => (
                "此命令不支持所选对象类型。",
                "This command does not support that entity type.",
            ),
            Self::Locked => (
                "对象或结果所在图层已锁定、隐藏或冻结；未修改。",
                "A source or result layer is locked, hidden or frozen; nothing changed.",
            ),
            Self::Changed => (
                "所选对象已变化，请重新启动命令。",
                "The picked object changed; restart the command.",
            ),
            Self::NoSolution => (
                "无有效交点或编辑解；原对象保持不变。",
                "No intersection or valid edit solution; original objects retained.",
            ),
            Self::SameObject => (
                "请选择不同的第二个对象。",
                "Pick a different second object.",
            ),
            Self::Boundary => (
                "请选择非边界对象进行编辑。",
                "Pick an object that is not one of the boundaries.",
            ),
            Self::JoinProperties => (
                "合并要求至少两个端点相接且属性相同的直线、圆弧或开放多段线。",
                "Join needs at least two endpoint-connected lines, arcs or open polylines with identical properties.",
            ),
            Self::BlockLimit => (
                "块循环引用或分解超限（16层、4096个对象/段）。",
                "Cyclic block or explode limit exceeded (16 levels, 4096 objects/segments).",
            ),
            Self::BlockContent => (
                "仅分解曲线和点组成的块；文字、标注、填充及不可靠的变换不支持。",
                "Only curve/point blocks can be exploded; text, dimensions, hatches and unsafe transforms are unsupported.",
            ),
            Self::MissingBlock => (
                "块或图层/线型定义缺失，无法安全分解。",
                "Missing block or layer/linetype definition; cannot safely explode.",
            ),
        };
        cx.error(cx.lang().pick(zh, en));
    }
}

// Reject invalid stored data before calling kernels that have fallback representations.
fn valid_curve(c: &Curve2) -> bool {
    let raw = match c {
        Curve2::Line(l) => l.a.is_finite() && l.b.is_finite(),
        Curve2::Circle(c) => c.c.is_finite() && c.r.is_finite() && c.r > 0.0,
        Curve2::Arc(a) => {
            a.c.is_finite()
                && a.r.is_finite()
                && a.r > 0.0
                && a.start.is_finite()
                && a.end.is_finite()
                && a.start != a.end
                && (a.end - a.start).is_finite()
        }
        Curve2::Ellipse(e) => {
            e.c.is_finite()
                && e.major.is_finite()
                && e.major.length() > 0.0
                && e.ratio.is_finite()
                && e.ratio > 0.0
                && e.ratio <= 1.0
                && e.start.is_finite()
                && e.end.is_finite()
                && (e.end - e.start).is_finite()
        }
        Curve2::Polyline(p) => {
            p.segment_count() > 0
                && p.verts
                    .iter()
                    .all(|v| v.p.is_finite() && v.bulge.is_finite())
                && (0..p.segment_count()).all(|i| {
                    let a = p.verts[i];
                    let b = p.verts[(i + 1) % p.verts.len()];
                    a.p.distance(b.p) > 0.0
                        && (a.bulge.abs() < wcad_geom2d::bulge::BULGE_EPS
                            || wcad_geom2d::bulge_to_arc(a.p, b.p, a.bulge).is_some())
                })
        }
        Curve2::Spline(s) => {
            s.validate().is_ok()
                && s.knots.iter().all(|t| t.is_finite())
                && s.fit_points.iter().all(|p| p.is_finite())
        }
    };
    if !raw {
        return false;
    }
    let bb = c.bbox();
    let length = c.length();
    bb.min.is_finite()
        && bb.max.is_finite()
        && bb.size().length().is_finite()
        && c.start().is_finite()
        && c.end().is_finite()
        && length.is_finite()
        && length > 0.0
}

fn curve(e: &Entity) -> EditResult<Curve2> {
    let c = e.kind.as_curve().ok_or(EditError::Unsupported)?;
    if !valid_curve(&c) || !e.linetype_scale.is_finite() {
        return Err(EditError::Invalid);
    }
    Ok(c)
}

fn supports(op: Op, c: &Curve2) -> bool {
    match op {
        Op::Fillet => matches!(c, Curve2::Line(_) | Curve2::Arc(_) | Curve2::Circle(_)),
        Op::Chamfer => matches!(c, Curve2::Line(_)),
        Op::Extend => !c.is_closed() && !matches!(c, Curve2::Spline(_)),
        Op::Join => {
            matches!(c, Curve2::Line(_) | Curve2::Arc(_))
                || matches!(c, Curve2::Polyline(p) if !p.closed)
        }
        _ => true,
    }
}

fn pick(cx: &ToolCx<'_>, p: DVec2, op: Op) -> EditResult<Entity> {
    if !p.is_finite() {
        return Err(EditError::Invalid);
    }
    let id = cx.pick(p).ok_or(EditError::Pick)?;
    let e = cx.entity(id).ok_or(EditError::Pick)?;
    if !cx.drawing().is_layer_editable(e.layer) {
        return Err(EditError::Locked);
    }
    let c = curve(e)?;
    if !supports(op, &c) {
        return Err(EditError::Unsupported);
    }
    Ok(e.clone())
}

fn pieces(e: &Entity, curves: Vec<Curve2>) -> Vec<Entity> {
    curves
        .into_iter()
        .enumerate()
        .map(|(i, c)| Entity {
            id: if i == 0 { e.id } else { EntityId(0) },
            kind: EntityKind::from_curve(c),
            ..e.clone()
        })
        .collect()
}

struct EditPlan {
    originals: Vec<Entity>,
    result: Vec<Entity>,
}

impl EditPlan {
    fn validate(&self, d: &Drawing) -> EditResult<()> {
        if self.originals.is_empty() || self.result.len() > MAX_PARTS {
            return Err(EditError::Invalid);
        }
        for e in &self.originals {
            if d.entities.get(&e.id) != Some(e) {
                return Err(EditError::Changed);
            }
        }
        for e in self.originals.iter().chain(&self.result) {
            if !d.is_layer_editable(e.layer) {
                return Err(EditError::Locked);
            }
        }
        for e in &self.result {
            let valid = match &e.kind {
                EntityKind::Point { p } => p.is_finite(),
                _ => curve(e).is_ok(),
            };
            if !valid || !e.linetype_scale.is_finite() {
                return Err(EditError::Invalid);
            }
        }
        if self.originals == self.result {
            return Err(EditError::NoSolution);
        }
        Ok(())
    }

    fn apply(self, cx: &mut ToolCx<'_>, label: &'static str) -> EditResult<()> {
        self.validate(cx.drawing())?;
        cx.transact(label, |tx| {
            for e in &self.originals {
                if !self.result.iter().any(|r| r.id == e.id) {
                    tx.remove(e.id);
                }
            }
            for e in self.result {
                tx.insert(e);
            }
        });
        cx.selection_mut().clear();
        Ok(())
    }

    fn preview(self, cx: &ToolCx<'_>, out: &mut Preview) {
        if self.validate(cx.drawing()).is_ok() {
            out.ghosts.extend(self.result);
        }
    }
}

fn corner_plan(
    op: Op,
    a: &Entity,
    b: &Entity,
    d1: f64,
    d2: f64,
    pa: DVec2,
    pb: DVec2,
) -> EditResult<EditPlan> {
    if a.id == b.id {
        return Err(EditError::SameObject);
    }
    if !pa.is_finite()
        || !pb.is_finite()
        || !d1.is_finite()
        || d1 < 0.0
        || !d2.is_finite()
        || d2 < 0.0
    {
        return Err(EditError::Invalid);
    }
    let (ca, cb) = (curve(a)?, curve(b)?);
    if !supports(op, &ca) || !supports(op, &cb) {
        return Err(EditError::Unsupported);
    }
    let (ra, rb, connector) = if op == Op::Chamfer {
        let (Curve2::Line(la), Curve2::Line(lb)) = (&ca, &cb) else {
            return Err(EditError::Unsupported);
        };
        let r = geom::chamfer(la, lb, d1, d2, pa, pb).ok_or(EditError::NoSolution)?;
        // An excessive distance must not reverse a retained line past its far endpoint.
        if r.a.is_some_and(|l| (l.b - l.a).dot(la.b - la.a) <= 0.0)
            || r.b.is_some_and(|l| (l.b - l.a).dot(lb.b - lb.a) <= 0.0)
        {
            return Err(EditError::NoSolution);
        }
        (
            r.a.map(Curve2::Line),
            r.b.map(Curve2::Line),
            r.line.map(Curve2::Line),
        )
    } else {
        let r = geom::fillet(&ca, &cb, d1, pa, pb).ok_or(EditError::NoSolution)?;
        if let Some(arc) = r.arc {
            // Some carrier solutions (notably enclosing-circle offsets) are not real
            // tangencies. Validate the returned arc rather than drawing a false fillet.
            let tol = 1e-8 * ca.length().max(cb.length()).max(d1).max(1e-12);
            let tangent = |c: &Curve2, p: DVec2| match c {
                Curve2::Line(l) => {
                    (p - l.a).perp_dot(l.dir()).abs() <= tol
                        && (p - arc.c).dot(l.dir()).abs() <= tol
                }
                Curve2::Arc(a) => {
                    (p.distance(a.c) - a.r).abs() <= tol
                        && (p - a.c)
                            .normalize_or_zero()
                            .perp_dot((p - arc.c).normalize_or_zero())
                            .abs()
                            <= 1e-8
                }
                Curve2::Circle(c) => {
                    (p.distance(c.c) - c.r).abs() <= tol
                        && (p - c.c)
                            .normalize_or_zero()
                            .perp_dot((p - arc.c).normalize_or_zero())
                            .abs()
                            <= 1e-8
                }
                _ => false,
            };
            let (s, e) = (arc.start_point(), arc.end_point());
            if arc.sweep() >= TAU - 1e-12
                || !valid_curve(&Curve2::Arc(arc))
                || !((tangent(&ca, s) && tangent(&cb, e)) || (tangent(&ca, e) && tangent(&cb, s)))
            {
                return Err(EditError::NoSolution);
            }
            let joins = |c: &Option<Curve2>| {
                c.as_ref().is_none_or(|c| {
                    if matches!(c, Curve2::Circle(_)) {
                        return true;
                    }
                    let (a, b) = c.domain();
                    [
                        (s, arc.tangent_at(arc.domain().0)),
                        (e, -arc.tangent_at(arc.domain().1)),
                    ]
                    .into_iter()
                    .any(|(p, inward)| {
                        (c.start().distance(p) <= tol && c.tangent_at(a).dot(inward) < -1.0 + 1e-8)
                            || (c.end().distance(p) <= tol
                                && (-c.tangent_at(b)).dot(inward) < -1.0 + 1e-8)
                    })
                })
            };
            // Oversized radii can make trim_to keep the wrong side of a tangent.
            // The retained curve must meet the arc smoothly, not double back in a cusp.
            if !joins(&r.a) || !joins(&r.b) {
                return Err(EditError::NoSolution);
            }
        }
        (r.a, r.b, r.arc.map(Curve2::Arc))
    };
    let mut result = pieces(a, ra.into_iter().collect());
    result.extend(pieces(b, rb.into_iter().collect()));
    if let Some(c) = connector {
        result.push(Entity {
            id: EntityId(0),
            kind: EntityKind::from_curve(c),
            ..a.clone()
        });
    }
    if result.is_empty() {
        return Err(EditError::NoSolution);
    }
    Ok(EditPlan {
        originals: vec![a.clone(), b.clone()],
        result,
    })
}

struct EditTool {
    op: Op,
    step: Step,
    d1: f64,
    d2: f64,
    first: Option<(Entity, DVec2)>,
    break1: Option<DVec2>,
    boundaries: Vec<EntityId>,
}

impl EditTool {
    fn new(op: Op) -> Self {
        let step = match op {
            Op::Offset | Op::Fillet | Op::Chamfer => Step::Value1,
            Op::Trim | Op::Extend => Step::Boundaries,
            Op::Explode | Op::Join => Step::Selection,
            Op::Break => Step::Object,
        };
        Self {
            op,
            step,
            d1: 1.0,
            d2: 1.0,
            first: None,
            break1: None,
            boundaries: Vec::new(),
        }
    }

    fn plan_at(&self, cx: &ToolCx<'_>, p: DVec2) -> EditResult<EditPlan> {
        if !p.is_finite() {
            return Err(EditError::Invalid);
        }
        match self.step {
            Step::Side => {
                let (e, _) = self.first.as_ref().ok_or(EditError::Pick)?;
                let c = curve(e)?;
                if !self.d1.is_finite() || self.d1 <= 0.0 {
                    return Err(EditError::Invalid);
                }
                let curves = offset(&c, self.d1, p);
                if curves.is_empty() {
                    return Err(EditError::NoSolution);
                }
                let mut result = vec![e.clone()];
                result.extend(pieces(e, curves).into_iter().map(|mut e| {
                    e.id = EntityId(0);
                    e
                }));
                Ok(EditPlan {
                    originals: vec![e.clone()],
                    result,
                })
            }
            Step::Second => {
                let (a, pa) = self.first.as_ref().ok_or(EditError::Pick)?;
                let b = pick(cx, p, self.op)?;
                corner_plan(self.op, a, &b, self.d1, self.d2, *pa, p)
            }
            Step::Break2 => {
                let (e, _) = self.first.as_ref().ok_or(EditError::Pick)?;
                let a = self.break1.ok_or(EditError::Invalid)?;
                let c = curve(e)?;
                let curves = geom::break_at(&c, a, p);
                if curves.is_empty() {
                    let (lo, hi) = c.domain();
                    let (t1, t2) = (c.closest(a).0, c.closest(p).0);
                    // Only an explicit end-to-end break of an open curve may erase it.
                    // A failed/tiny sub-curve extraction must not become a deletion.
                    if c.is_closed() || t1.min(t2) != lo || t1.max(t2) != hi {
                        return Err(EditError::NoSolution);
                    }
                }
                let result = pieces(e, curves);
                Ok(EditPlan {
                    originals: vec![e.clone()],
                    result,
                })
            }
            Step::Object if matches!(self.op, Op::Trim | Op::Extend) => {
                let e = pick(cx, p, self.op)?;
                if self.boundaries.contains(&e.id) {
                    return Err(EditError::Boundary);
                }
                let boundaries = self
                    .boundaries
                    .iter()
                    .map(|id| curve(cx.entity(*id).ok_or(EditError::Changed)?))
                    .collect::<EditResult<Vec<_>>>()?;
                let c = curve(&e)?;
                let curves = if self.op == Op::Trim {
                    geom::trim(&c, &boundaries, p)
                } else {
                    geom::extend(&c, &boundaries, p).map(|c| vec![c])
                }
                .ok_or(EditError::NoSolution)?;
                // TRIM cannot legitimately remove everything: that indicates an unusable
                // sub-curve result, not permission to delete the source.
                if curves.is_empty() {
                    return Err(EditError::NoSolution);
                }
                Ok(EditPlan {
                    result: pieces(&e, curves),
                    originals: vec![e],
                })
            }
            _ => Err(EditError::NoSolution),
        }
    }

    fn selection_plan(&self, cx: &ToolCx<'_>, ids: &[EntityId]) -> EditResult<EditPlan> {
        let originals = dedup_ids(ids.iter().copied())
            .into_iter()
            .map(|id| {
                let e = cx.entity(id).ok_or(EditError::Changed)?;
                if !cx.drawing().is_layer_editable(e.layer) {
                    return Err(EditError::Locked);
                }
                Ok(e.clone())
            })
            .collect::<EditResult<Vec<_>>>()?;
        let result = if self.op == Op::Explode {
            let mut ex = Explosion {
                drawing: cx.drawing(),
                path: Vec::new(),
                result: Vec::new(),
                visits: 0,
            };
            for e in &originals {
                if !matches!(e.kind, EntityKind::Polyline(_) | EntityKind::Insert(_)) {
                    return Err(EditError::Unsupported);
                }
                ex.expand(e, DAffine2::IDENTITY, None, 1.0)?;
            }
            if ex.result.is_empty() {
                return Err(EditError::NoSolution);
            }
            ex.result
        } else {
            let Some(first) = originals.first() else {
                return Err(EditError::JoinProperties);
            };
            if originals.len() < 2 || originals.iter().any(|e| !same_properties(first, e)) {
                return Err(EditError::JoinProperties);
            }
            let curves = originals
                .iter()
                .map(|e| {
                    let c = curve(e)?;
                    if supports(Op::Join, &c) {
                        Ok(c)
                    } else {
                        Err(EditError::Unsupported)
                    }
                })
                .collect::<EditResult<Vec<_>>>()?;
            let c = geom::join(&curves, JOIN_TOL).ok_or(EditError::NoSolution)?;
            pieces(first, vec![c])
        };
        Ok(EditPlan { originals, result })
    }
}

fn same_properties(a: &Entity, b: &Entity) -> bool {
    a.layer == b.layer
        && a.color == b.color
        && a.linetype == b.linetype
        && a.linetype_scale == b.linetype_scale
        && a.lineweight == b.lineweight
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        match self.op {
            Op::Offset => "OFFSET",
            Op::Trim => "TRIM",
            Op::Extend => "EXTEND",
            Op::Fillet => "FILLET",
            Op::Chamfer => "CHAMFER",
            Op::Break => "BREAK",
            Op::Explode => "EXPLODE",
            Op::Join => "JOIN",
        }
    }

    fn prompt(&self, l: Lang) -> String {
        match self.step {
            Step::Value1 => match self.op {
                Op::Offset => l.pick("输入偏移距离 (>0) <1>", "Offset distance (>0) <1>"),
                Op::Fillet => l.pick("输入圆角半径 (>=0) <1>", "Fillet radius (>=0) <1>"),
                _ => l.pick("输入第一倒角距离 (>=0) <1>", "First chamfer distance (>=0) <1>"),
            }.into(),
            Step::Value2 => format!("{} <{}>", l.pick("输入第二倒角距离 (>=0)", "Second chamfer distance (>=0)"), self.d1),
            Step::Boundaries => l.pick("选择边界曲线，Enter 确认", "Select boundary curves, Enter to accept").into(),
            Step::Selection if self.op == Op::Join => l.pick("选择属性相同的直线/圆弧/开放多段线，Enter 合并 (容差 1e-9)", "Select same-property lines/arcs/open polylines, Enter to join (tolerance 1e-9)").into(),
            Step::Selection => l.pick("选择多段线或纯曲线/点块，Enter 分解", "Select polylines or curve/point blocks, Enter to explode").into(),
            Step::Object => match self.op {
                Op::Offset => l.pick("拾取偏移曲线，Enter 结束", "Pick curve to offset, Enter to finish"),
                Op::Trim => l.pick("拾取要修剪的部分，可重复；Enter 结束", "Pick part to trim (repeat), Enter to finish"),
                Op::Extend => l.pick("拾取直线/圆弧/椭圆弧/开放多段线的延伸端，Enter 结束", "Pick end of line/arc/elliptical arc/open polyline to extend, Enter to finish"),
                Op::Fillet => l.pick("拾取第一条直线/圆弧/圆（保留侧决定选解）", "Pick first line/arc/circle (keep-side pick chooses solution)"),
                Op::Chamfer => l.pick("拾取第一条直线的保留侧", "Pick first line on the side to keep"),
                _ => l.pick("拾取要打断的曲线", "Pick curve to break"),
            }.into(),
            Step::Second => l.pick("拾取第二对象的保留侧", "Pick second object on the side to keep").into(),
            Step::Side => l.pick("指定偏移侧的点", "Specify a point on the offset side").into(),
            Step::Break1 => l.pick("指定第一断点（投影到曲线）", "First break point (projected onto curve)").into(),
            Step::Break2 => l.pick("指定第二断点；闭合曲线沿曲线方向删除两点间部分", "Second break point; closed curves remove first-to-second in curve direction").into(),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Value1 | Step::Value2 => Accept::VALUE,
            Step::Boundaries | Step::Selection => Accept::SELECTION,
            _ => Accept::PICK,
        }
    }

    fn start(&mut self, cx: &mut ToolCx<'_>) -> ToolFlow {
        if !self.accepts().selection {
            cx.selection_mut().clear();
        }
        ToolFlow::Continue
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        if matches!(&input, ToolInput::Point(p) | ToolInput::Hover(p) if !p.is_finite())
            || matches!(&input, ToolInput::Value(v) if !v.is_finite())
        {
            EditError::Invalid.report(cx);
            return ToolFlow::Continue;
        }
        match input {
            ToolInput::Escape => {
                cx.selection_mut().clear();
                return ToolFlow::Cancel;
            }
            ToolInput::Value(_) | ToolInput::Enter
                if matches!(self.step, Step::Value1 | Step::Value2) =>
            {
                let v = if let ToolInput::Value(v) = input {
                    v
                } else if self.step == Step::Value2 {
                    self.d1
                } else {
                    1.0
                };
                if v < 0.0 || (self.op == Op::Offset && v == 0.0) {
                    EditError::Invalid.report(cx);
                } else if self.step == Step::Value2 {
                    self.d2 = v;
                    self.step = Step::Object;
                } else {
                    self.d1 = v;
                    self.step = if self.op == Op::Chamfer {
                        Step::Value2
                    } else {
                        Step::Object
                    };
                }
            }
            ToolInput::Selection(ids)
                if matches!(self.step, Step::Boundaries | Step::Selection) =>
            {
                if ids.is_empty() {
                    return ToolFlow::Done;
                }
                if self.step == Step::Boundaries {
                    let ids = dedup_ids(ids);
                    let valid = ids.iter().try_for_each(|id| {
                        curve(cx.entity(*id).ok_or(EditError::Changed)?).map(|_| ())
                    });
                    match valid {
                        Ok(()) => {
                            self.boundaries = ids;
                            self.step = Step::Object;
                            cx.selection_mut().clear();
                        }
                        Err(e) => e.report(cx),
                    }
                } else {
                    match self
                        .selection_plan(cx, &ids)
                        .and_then(|p| p.apply(cx, self.name()))
                    {
                        Ok(()) => return ToolFlow::Done,
                        Err(e) => e.report(cx),
                    }
                }
            }
            ToolInput::Point(p)
                if self.step == Step::Object && !matches!(self.op, Op::Trim | Op::Extend) =>
            {
                match pick(cx, p, self.op) {
                    Ok(e) => {
                        if self.op == Op::Offset
                            && matches!(e.kind, EntityKind::Ellipse(_) | EntityKind::Spline(_))
                        {
                            cx.message(cx.lang().pick(
                                "椭圆/样条偏移使用近似多段线结果。",
                                "Ellipse/spline offsets produce approximate polylines.",
                            ));
                        }
                        cx.selection_mut().set([e.id]);
                        self.first = Some((e, p));
                        self.step = match self.op {
                            Op::Offset => Step::Side,
                            Op::Break => Step::Break1,
                            _ => Step::Second,
                        };
                    }
                    Err(e) => e.report(cx),
                }
            }
            ToolInput::Point(p) if self.step == Step::Break1 => {
                self.break1 = Some(p);
                self.step = Step::Break2;
            }
            ToolInput::Point(p)
                if matches!(
                    self.step,
                    Step::Object | Step::Side | Step::Second | Step::Break2
                ) =>
            {
                match self.plan_at(cx, p).and_then(|p| p.apply(cx, self.name())) {
                    Ok(()) if matches!(self.op, Op::Offset | Op::Trim | Op::Extend) => {
                        self.first = None;
                        self.step = Step::Object;
                    }
                    Ok(()) => return ToolFlow::Done,
                    Err(e) => e.report(cx),
                }
            }
            ToolInput::Enter => {
                cx.selection_mut().clear();
                return ToolFlow::Done;
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if self.step == Step::Selection {
            if let Ok(plan) = self.selection_plan(cx, &cx.selected_ids()) {
                plan.preview(cx, out);
            }
            return;
        }
        let Some(p) = cx.cursor().filter(|p| p.is_finite()) else {
            return;
        };
        if let Ok(plan) = self.plan_at(cx, p) {
            plan.preview(cx, out);
        }
        if matches!(self.step, Step::Break1 | Step::Break2)
            && let Some((e, _)) = &self.first
            && let Ok(c) = curve(e)
        {
            if let Some(a) = self.break1 {
                out.points.push(c.closest(a).1);
            }
            out.points.push(c.closest(p).1);
        }
    }
}

struct Explosion<'a> {
    drawing: &'a Drawing,
    path: Vec<BlockId>,
    result: Vec<Entity>,
    visits: usize,
}

impl Explosion<'_> {
    fn expand(
        &mut self,
        source: &Entity,
        m: DAffine2,
        parent: Option<&Entity>,
        scale: f64,
    ) -> EditResult<()> {
        self.visits += 1;
        if self.visits > MAX_PARTS || self.result.len() >= MAX_PARTS {
            return Err(EditError::BlockLimit);
        }
        let mut e = source.clone();
        e.id = EntityId(0);
        if let Some(parent) = parent {
            if Some(e.layer) == self.drawing.layer_by_name("0") {
                e.layer = parent.layer;
            }
            let layer = self
                .drawing
                .layer(parent.layer)
                .ok_or(EditError::MissingBlock)?;
            if e.color == Color::ByBlock {
                e.color = match parent.color {
                    Color::ByLayer => layer.color,
                    c => c,
                };
            }
            if e.linetype == LinetypeRef::ByBlock {
                e.linetype = match parent.linetype {
                    LinetypeRef::ByLayer => LinetypeRef::Id(layer.linetype),
                    t => t,
                };
            }
            if e.lineweight == LineWeight::ByBlock {
                e.lineweight = match parent.lineweight {
                    LineWeight::ByLayer => layer.lineweight,
                    w => w,
                };
            }
        } else if matches!(e.kind, EntityKind::Insert(_)) {
            if e.color == Color::ByBlock {
                e.color = Color::WHITE;
            }
            if e.lineweight == LineWeight::ByBlock {
                e.lineweight = LineWeight::Default;
            }
            if e.linetype == LinetypeRef::ByBlock {
                e.linetype = LinetypeRef::Id(
                    self.drawing
                        .linetype_by_name("CONTINUOUS")
                        .ok_or(EditError::MissingBlock)?,
                );
            }
        }
        if !self.drawing.is_layer_editable(e.layer) {
            return Err(EditError::Locked);
        }
        if !e.linetype_scale.is_finite() || !scale.is_finite() || scale <= 0.0 {
            return Err(EditError::Invalid);
        }
        match &source.kind {
            EntityKind::Insert(ins) => {
                if self.path.len() >= MAX_BLOCK_DEPTH || self.path.contains(&ins.block) {
                    return Err(EditError::BlockLimit);
                }
                let block = self
                    .drawing
                    .blocks
                    .get(&ins.block)
                    .ok_or(EditError::MissingBlock)?;
                if block.entities.is_empty()
                    || !block.base.is_finite()
                    || !ins.pos.is_finite()
                    || !ins.scale.is_finite()
                    || !ins.rotation.is_finite()
                {
                    return Err(EditError::BlockContent);
                }
                let world = m * crate::select::insert_transform(ins, block.base);
                let x = world.matrix2.x_axis;
                let y = world.matrix2.y_axis;
                let det = world.matrix2.determinant();
                if !world.translation.is_finite()
                    || !x.is_finite()
                    || !y.is_finite()
                    || !det.is_finite()
                    || det.abs() <= 1e-12 * x.length() * y.length()
                    || x.length().min(y.length()) <= 1e-12 * x.length().max(y.length())
                {
                    return Err(EditError::BlockContent);
                }
                let child_scale = scale * (ins.scale.x * ins.scale.y).abs().sqrt();
                self.path.push(ins.block);
                for child in block.entities.values() {
                    self.expand(child, world, Some(&e), child_scale)?;
                }
                self.path.pop();
            }
            EntityKind::Point { p } => {
                e.kind = EntityKind::Point {
                    p: m.transform_point2(*p),
                };
                self.result.push(e);
            }
            _ => {
                let c = curve(source).map_err(|err| match err {
                    EditError::Unsupported => EditError::BlockContent,
                    err => err,
                })?;
                if let Curve2::Polyline(p) = &c
                    && self.result.len() + p.segment_count() > MAX_PARTS
                {
                    return Err(EditError::BlockLimit);
                }
                let curves = match c {
                    Curve2::Polyline(p) => p
                        .segments()
                        .map(|s| match s {
                            PolySegment::Line(l) => Curve2::Line(l),
                            PolySegment::Arc(a) => Curve2::Arc(a.to_arc()),
                        })
                        .collect::<Vec<_>>(),
                    c => vec![c],
                };
                if self.result.len() + curves.len() > MAX_PARTS {
                    return Err(EditError::BlockLimit);
                }
                // Split bulges before transforming: anisotropic/sheared arcs become
                // exact ellipse arcs, never a reinterpreted circular bulge.
                for c in curves {
                    let c = if m == DAffine2::IDENTITY {
                        c
                    } else {
                        c.transformed(&m)
                    };
                    if matches!(&c, Curve2::Ellipse(e) if e.ratio <= 1e-12)
                        && m != DAffine2::IDENTITY
                    {
                        // The geometry library clamps transformed ellipse ratios here.
                        // Refuse the near-singular image instead of silently changing it.
                        return Err(EditError::BlockContent);
                    }
                    if !valid_curve(&c) {
                        return Err(EditError::Invalid);
                    }
                    let mut part = Entity {
                        kind: EntityKind::from_curve(c),
                        ..e.clone()
                    };
                    if parent.is_some() {
                        part.linetype_scale = part.linetype_scale.abs().max(1e-12) * scale;
                    }
                    self.result.push(part);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::f64::consts::{FRAC_PI_2, PI};
    use std::sync::Arc;
    use wcad_doc::{Block, Insert};
    use wcad_geom2d::{Arc2, Circle2, EllipseArc2, Line2, Nurbs2, PolyVertex, Polyline2};

    use crate::editor::Editor;
    use crate::testing::Harness;

    fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> EntityKind {
        EntityKind::Line(Line2::new(DVec2::new(x0, y0), DVec2::new(x1, y1)))
    }

    fn harness(kinds: Vec<EntityKind>) -> (Harness, Vec<EntityId>) {
        // Register locally as well as testing register itself, independent of other features.
        let mut registry = CommandRegistry::new();
        crate::tools::core::register(&mut registry);
        register(&mut registry);
        let mut h = Harness {
            ed: Editor::new(Arc::new(registry)),
        };
        h.ed.lang = Lang::En;
        h.ed.units_per_px = 0.01;
        let lt = h.ed.doc.drawing.linetype_by_name("DASHED").unwrap();
        let ids = h.ed.doc.transact("fixture", |tx| {
            kinds
                .into_iter()
                .map(|kind| {
                    let id = tx.add(kind);
                    tx.modify(id, |e| {
                        e.color = Color::Rgb(23, 67, 121);
                        e.linetype = LinetypeRef::Id(lt);
                        e.linetype_scale = 2.5;
                        e.lineweight = LineWeight::Mm100(35);
                    });
                    id
                })
                .collect()
        });
        h.ed.pump();
        h.ed.doc.clear_history();
        (h, ids)
    }

    fn entities(h: &Harness) -> BTreeMap<EntityId, Entity> {
        h.ed.doc.drawing.entities.clone()
    }

    fn round_trip(h: &mut Harness, before: BTreeMap<EntityId, Entity>, name: &str) {
        let after = entities(h);
        assert_ne!(before, after);
        assert_eq!(h.ed.doc.undo().as_deref(), Some(name));
        assert_eq!(entities(h), before);
        assert!(
            !h.ed.doc.can_undo(),
            "one edit must be exactly one transaction"
        );
        assert_eq!(h.ed.doc.redo().as_deref(), Some(name));
        assert_eq!(entities(h), after);
        h.ed.pump();
    }

    fn raw(tool: &mut EditTool, h: &mut Harness, input: ToolInput) -> ToolFlow {
        let ed = &mut h.ed;
        let mut cx = ToolCx {
            doc: &mut ed.doc,
            selection: &mut ed.selection,
            draft: &ed.draft,
            index: &ed.index,
            log: &mut ed.log,
            requests: &mut ed.requests,
            lang: ed.lang,
            cursor: ed.cursor.map(|c| c.point),
            last_point: ed.last_point,
            units_per_px: ed.units_per_px,
        };
        tool.on_input(input, &mut cx)
    }

    fn polyline() -> EntityKind {
        EntityKind::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::new(-2.0, 0.0), 1.0),
                PolyVertex::with_bulge(DVec2::new(2.0, 0.0), -0.5),
                PolyVertex::new(DVec2::new(4.0, 2.0)),
                PolyVertex::new(DVec2::new(6.0, 2.0)),
            ],
            closed: false,
        })
    }

    #[test]
    fn registration_aliases_labels_icons_and_modify_tab() {
        let mut r = CommandRegistry::new();
        register(&mut r);
        for (name, alias) in [
            ("OFFSET", "O"),
            ("TRIM", "TR"),
            ("EXTEND", "EX"),
            ("FILLET", "F"),
            ("CHAMFER", "CHA"),
            ("BREAK", "BR"),
            ("EXPLODE", "X"),
            ("JOIN", "J"),
        ] {
            let s = r.find(alias).unwrap();
            assert_eq!(s.name, name);
            assert_eq!(s.tab, Some(RibbonTab::Modify));
            assert!(s.icon.is_ascii());
            assert_ne!((s.label)(Lang::Zh), (s.label)(Lang::En));
            let CommandKind::Tool(ctor) = s.kind else {
                panic!()
            };
            assert_eq!(ctor().name(), name);
        }
    }

    #[test]
    fn offset_raw_side_preview_attributes_and_undo_redo() {
        let (mut h, ids) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
        let before = entities(&h);
        h.ed.draft.osnap_on = true;
        h.ed.draft.ortho = true;
        h.cmd("O").cmd("2").click(9.95, 0.02).hover(5.0, -0.02);
        assert_eq!(h.ed.tool_accepts(), Accept::PICK);
        assert_eq!(h.ed.cursor.unwrap().point, DVec2::new(5.0, -0.02));
        assert!(h.ed.cursor.unwrap().snap.is_none());
        assert_eq!(h.ed.preview.ghosts.len(), 2);
        assert_eq!(h.ed.preview.ghosts[1].kind, line(0.0, -2.0, 10.0, -2.0));
        h.click(5.0, -0.02).enter();
        assert_eq!(h.count("LINE"), 2);
        assert_eq!(h.ed.doc.drawing.entities[&ids[0]], before[&ids[0]]);
        assert!(
            h.ed.doc
                .drawing
                .entities
                .values()
                .all(|e| same_properties(e, &before[&ids[0]]))
        );
        round_trip(&mut h, before, "OFFSET");
    }

    #[test]
    fn offset_collapse_is_retryable_and_zero_distance_is_rejected() {
        let (mut h, _) = harness(vec![EntityKind::Circle(Circle2::new(DVec2::ZERO, 2.0))]);
        let before = entities(&h);
        h.cmd("O").cmd("0");
        assert!(h.ed.tool_accepts().value);
        h.cmd("2").click(2.0, 0.0).click(0.0, 0.0);
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
        h.hover(4.0, 0.0);
        assert_eq!(
            h.ed.preview.ghosts[1].kind,
            EntityKind::Circle(Circle2::new(DVec2::ZERO, 4.0))
        );
        h.click(4.0, 0.0).enter();
        round_trip(&mut h, before, "OFFSET");
    }

    #[test]
    fn ellipse_offset_is_explicitly_approximate() {
        let (mut h, _) = harness(vec![EntityKind::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(5.0, 0.0),
            ratio: 0.6,
            start: 0.0,
            end: TAU,
        })]);
        h.cmd("O").cmd("0.5").click(5.0, 0.0);
        assert!(h.last_message().contains("approximate polylines"));
        h.click(10.0, 0.0).enter();
        assert_eq!(h.count("ELLIPSE"), 1);
        assert_eq!(h.count("LWPOLYLINE"), 1);
    }

    #[test]
    fn trim_middle_keeps_two_pieces_attributes_and_one_transaction() {
        let (mut h, ids) = harness(vec![
            line(0.0, 0.0, 10.0, 0.0),
            line(3.0, -2.0, 3.0, 2.0),
            line(7.0, -2.0, 7.0, 2.0),
        ]);
        let before = entities(&h);
        h.cmd("TR")
            .click(3.0, 1.0)
            .click(7.0, 1.0)
            .enter()
            .hover(5.0, 0.02);
        assert!(h.ed.tool_accepts().pick);
        assert_eq!(h.ed.preview.ghosts.len(), 2);
        h.click(5.0, 0.02).enter();
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[0]].kind,
            line(0.0, 0.0, 3.0, 0.0)
        );
        assert!(
            h.ed.doc
                .drawing
                .entities
                .values()
                .any(|e| e.kind == line(7.0, 0.0, 10.0, 0.0))
        );
        assert!(
            h.ed.doc
                .drawing
                .entities
                .values()
                .all(|e| same_properties(e, &before[&ids[0]]))
        );
        round_trip(&mut h, before, "TRIM");
    }

    #[test]
    fn trim_circle_retains_opposite_arc() {
        let (mut h, ids) = harness(vec![
            EntityKind::Circle(Circle2::new(DVec2::ZERO, 5.0)),
            line(-10.0, 3.0, 10.0, 3.0),
        ]);
        h.ed.selection.set([ids[1]]);
        h.cmd("TR").click(0.0, 5.0).enter();
        let EntityKind::Arc(a) = h.ed.doc.drawing.entities[&ids[0]].kind else {
            panic!()
        };
        assert!(a.mid_point().distance(DVec2::new(0.0, -5.0)) < 1e-9);
    }

    #[test]
    fn extend_repeats_and_each_edit_is_individually_undoable() {
        let (mut h, ids) = harness(vec![
            line(0.0, 0.0, 5.0, 0.0),
            line(0.0, 2.0, 5.0, 2.0),
            line(8.0, -5.0, 8.0, 5.0),
        ]);
        let before = entities(&h);
        h.cmd("EX").click(8.0, 3.0).enter().hover(4.98, 0.01);
        assert_eq!(h.ed.preview.ghosts[0].kind, line(0.0, 0.0, 8.0, 0.0));
        h.click(4.98, 0.01).click(4.98, 2.01).enter();
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[1]].kind,
            line(0.0, 2.0, 8.0, 2.0)
        );
        assert_eq!(h.ed.doc.undo().as_deref(), Some("EXTEND"));
        assert_eq!(h.ed.doc.drawing.entities[&ids[1]], before[&ids[1]]);
        assert_eq!(h.ed.doc.undo().as_deref(), Some("EXTEND"));
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
        assert_eq!(h.ed.doc.redo().as_deref(), Some("EXTEND"));
        assert_eq!(h.ed.doc.redo().as_deref(), Some("EXTEND"));
    }

    #[test]
    fn trim_and_extend_no_hit_leave_sources_and_redo_history_intact() {
        for cmd in ["TR", "EX"] {
            let (mut h, ids) = harness(vec![line(0.0, 0.0, 5.0, 0.0), line(0.0, 3.0, 5.0, 3.0)]);
            h.ed.doc.transact("marker", |tx| {
                tx.add(line(20.0, 0.0, 21.0, 0.0));
            });
            h.ed.doc.undo();
            h.ed.pump();
            let before = entities(&h);
            let rev = h.ed.doc.revision();
            h.ed.selection.set([ids[1]]);
            h.cmd(cmd).click(4.0, 0.01).enter();
            assert_eq!(entities(&h), before);
            assert_eq!(h.ed.doc.revision(), rev);
            assert_eq!(h.ed.doc.redo_label(), Some("marker"));
            assert!(!h.ed.doc.can_undo());
        }
    }

    #[test]
    fn fillet_radius_pick_solution_and_distinct_properties_survive() {
        let (mut h, ids) = harness(vec![
            line(0.0, 0.0, 10.0, 0.0),
            line(10.0, -2.0, 10.0, 10.0),
        ]);
        h.ed.doc.transact("fixture style", |tx| {
            tx.modify(ids[1], |e| e.color = Color::Aci(1));
        });
        h.ed.doc.clear_history();
        let before = entities(&h);
        h.cmd("F").cmd("2").click(3.0, 0.01).hover(10.01, 5.0);
        assert_eq!(h.ed.preview.ghosts.len(), 3);
        h.click(10.01, 5.0);
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[0]].kind,
            line(0.0, 0.0, 8.0, 0.0)
        );
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[1]].kind,
            line(10.0, 2.0, 10.0, 10.0)
        );
        assert!(same_properties(
            &h.ed.doc.drawing.entities[&ids[1]],
            &before[&ids[1]]
        ));
        let (arc_id, EntityKind::Arc(a)) = h.of_type("ARC")[0] else {
            panic!()
        };
        assert!(a.c.distance(DVec2::new(8.0, 2.0)) < 1e-9);
        assert!((a.r - 2.0).abs() < 1e-9 && (a.sweep() - FRAC_PI_2).abs() < 1e-9);
        assert!(same_properties(
            &h.ed.doc.drawing.entities[&arc_id],
            &before[&ids[0]]
        ));
        round_trip(&mut h, before, "FILLET");
    }

    #[test]
    fn zero_fillet_and_zero_chamfer_extend_to_real_corner() {
        for cmd in ["F", "CHA"] {
            let (mut h, ids) =
                harness(vec![line(0.0, 0.0, 10.0, 0.0), line(12.0, 3.0, 12.0, 10.0)]);
            let before = entities(&h);
            h.cmd(cmd).cmd("0");
            if cmd == "CHA" {
                h.cmd("0");
            }
            h.click(3.0, 0.0).click(12.0, 5.0);
            assert_eq!(h.count("ARC"), 0);
            assert_eq!(h.count("LINE"), 2);
            assert_eq!(
                h.ed.doc.drawing.entities[&ids[0]].kind,
                line(0.0, 0.0, 12.0, 0.0)
            );
            assert_eq!(
                h.ed.doc.drawing.entities[&ids[1]].kind,
                line(12.0, 0.0, 12.0, 10.0)
            );
            round_trip(
                &mut h,
                before,
                if cmd == "F" { "FILLET" } else { "CHAMFER" },
            );
        }
    }

    #[test]
    fn fillet_line_arc_and_line_circle_are_true_tangencies() {
        for other in [
            EntityKind::Arc(Arc2::new(DVec2::new(0.0, 5.0), 3.0, PI, TAU)),
            EntityKind::Circle(Circle2::new(DVec2::new(0.0, 5.0), 3.0)),
        ] {
            let (mut h, _) = harness(vec![line(-10.0, 0.0, 10.0, 0.0), other]);
            let before = entities(&h);
            h.cmd("F").cmd("2").click(-6.0, 0.0).click(-3.0, 5.0);
            assert!(!h.ed.has_tool());
            let a = h
                .of_type("ARC")
                .into_iter()
                .find_map(|(_, k)| match k {
                    EntityKind::Arc(a) if (a.r - 2.0).abs() < 1e-9 => Some(a),
                    _ => None,
                })
                .unwrap();
            assert!((a.c.y - 2.0).abs() < 1e-9);
            assert!((a.c.distance(DVec2::new(0.0, 5.0)) - 5.0).abs() < 1e-9);
            assert!(a.c.x < 0.0);
            round_trip(&mut h, before, "FILLET");
        }
    }

    #[test]
    fn corner_parallel_same_object_and_unsupported_type_are_atomic() {
        for cmd in ["F", "CHA"] {
            let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0), line(0.0, 3.0, 10.0, 3.0)]);
            let before = entities(&h);
            h.cmd(cmd).cmd("1");
            if cmd == "CHA" {
                h.cmd("1");
            }
            h.click(3.0, 0.0).click(4.0, 0.0).click(3.0, 3.0);
            assert_eq!(entities(&h), before);
            assert!(!h.ed.doc.can_undo());
            h.esc();
        }
        let (mut h, _) = harness(vec![polyline()]);
        h.cmd("F").cmd("1").click(5.0, 2.0);
        assert!(h.last_message().contains("does not support"));
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn chamfer_two_distances_and_limit() {
        let (mut h, ids) = harness(vec![line(0.0, 0.0, 10.0, 0.0), line(10.0, 0.0, 10.0, 10.0)]);
        let before = entities(&h);
        h.cmd("CHA")
            .cmd("2")
            .cmd("3")
            .click(1.0, 0.0)
            .hover(10.0, 9.0);
        assert_eq!(h.ed.preview.ghosts.len(), 3);
        h.click(10.0, 9.0);
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[0]].kind,
            line(0.0, 0.0, 8.0, 0.0)
        );
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[1]].kind,
            line(10.0, 3.0, 10.0, 10.0)
        );
        assert!(
            h.ed.doc
                .drawing
                .entities
                .values()
                .any(|e| e.kind == line(8.0, 0.0, 10.0, 3.0))
        );
        round_trip(&mut h, before, "CHAMFER");

        for d in [10.0, 11.0] {
            let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0), line(10.0, 0.0, 10.0, 10.0)]);
            let before = entities(&h);
            h.cmd("CHA")
                .cmd(&d.to_string())
                .cmd(&d.to_string())
                .click(1.0, 0.0)
                .click(10.0, 9.0);
            if d == 10.0 {
                assert_eq!(h.count("LINE"), 1);
                round_trip(&mut h, before, "CHAMFER");
            } else {
                assert_eq!(entities(&h), before);
                assert!(!h.ed.doc.can_undo());
            }
        }
    }

    #[test]
    fn break_two_projected_points_and_coincident_split() {
        for (a, b) in [(3.0, 6.0), (4.0, 4.0)] {
            let (mut h, ids) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
            let before = entities(&h);
            h.cmd("BR").click(1.0, 0.0).click(a, 0.02).hover(b, -0.02);
            assert_eq!(h.ed.preview.ghosts.len(), 2);
            h.click(b, -0.02);
            assert_eq!(
                h.ed.doc.drawing.entities[&ids[0]].kind,
                line(0.0, 0.0, a, 0.0)
            );
            assert!(
                h.ed.doc
                    .drawing
                    .entities
                    .values()
                    .any(|e| e.kind == line(b, 0.0, 10.0, 0.0))
            );
            assert!(
                h.ed.doc
                    .drawing
                    .entities
                    .values()
                    .all(|e| same_properties(e, &before[&ids[0]]))
            );
            round_trip(&mut h, before, "BREAK");
        }
    }

    #[test]
    fn break_circle_direction_coincident_noop_and_complete_open_removal() {
        let (mut h, ids) = harness(vec![EntityKind::Circle(Circle2::new(DVec2::ZERO, 1.0))]);
        let before = entities(&h);
        h.cmd("BR").click(1.0, 0.0).click(1.0, 0.0).click(1.0, 0.0);
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
        h.click(0.0, 1.0);
        let EntityKind::Arc(a) = h.ed.doc.drawing.entities[&ids[0]].kind else {
            panic!()
        };
        assert!((a.sweep() - 1.5 * PI).abs() < 1e-9);
        round_trip(&mut h, before, "BREAK");

        let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
        let before = entities(&h);
        h.cmd("BR").click(5.0, 0.0).click(0.0, 0.0).click(10.0, 0.0);
        assert_eq!(h.count("LINE"), 0);
        round_trip(&mut h, before, "BREAK");
    }

    #[test]
    fn explode_preserves_positive_and_negative_bulges_and_all_properties() {
        let (mut h, ids) = harness(vec![polyline()]);
        let before = entities(&h);
        let EntityKind::Polyline(p) = &before[&ids[0]].kind else {
            panic!()
        };
        let expected: Vec<_> = p
            .segments()
            .map(|s| match s {
                PolySegment::Line(l) => EntityKind::Line(l),
                PolySegment::Arc(a) => EntityKind::Arc(a.to_arc()),
            })
            .collect();
        h.cmd("X");
        h.ed.selection.set(ids);
        h.ed.refresh_preview();
        assert_eq!(h.ed.preview.ghosts.len(), 3);
        h.enter();
        assert_eq!(h.count("LWPOLYLINE"), 0);
        assert_eq!(h.count("ARC"), 2);
        assert_eq!(h.count("LINE"), 1);
        let original = before.values().next().unwrap();
        for (e, kind) in h.ed.doc.drawing.entities.values().zip(expected) {
            assert_eq!(e.kind, kind);
            assert!(same_properties(e, original));
        }
        round_trip(&mut h, before, "EXPLODE");
    }

    fn block_entity(tx: &mut wcad_doc::Tx<'_>, kind: EntityKind) -> Entity {
        Entity {
            id: tx.ids().entity(),
            layer: tx.drawing().layer_by_name("0").unwrap(),
            color: Color::ByBlock,
            linetype: LinetypeRef::ByBlock,
            linetype_scale: 2.0,
            lineweight: LineWeight::ByBlock,
            kind,
        }
    }

    fn add_block(tx: &mut wcad_doc::Tx<'_>, base: DVec2, children: Vec<Entity>) -> BlockId {
        let id = tx.ids().block();
        tx.blocks_mut().insert(
            id,
            Block {
                name: format!("block{}", id.0),
                base,
                entities: children.into_iter().map(|e| (e.id, e)).collect(),
            },
        );
        id
    }

    #[test]
    fn explode_nested_mirrored_nonuniform_insert_keeps_world_geometry_and_inheritance() {
        let (mut h, _) = harness(Vec::new());
        let pl = Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::new(1.0, 2.0), 1.0),
                PolyVertex::new(DVec2::new(3.0, 2.0)),
                PolyVertex::new(DVec2::new(3.0, 4.0)),
            ],
            closed: false,
        };
        let (root_id, expected, scale) = h.ed.doc.transact("fixture", |tx| {
            let leaf = block_entity(tx, EntityKind::Polyline(pl.clone()));
            let point = block_entity(
                tx,
                EntityKind::Point {
                    p: DVec2::new(6.0, 1.0),
                },
            );
            let inner_base = DVec2::new(1.0, 2.0);
            let inner = add_block(tx, inner_base, vec![leaf, point]);
            let inner_ins = Insert {
                block: inner,
                pos: DVec2::new(4.0, 5.0),
                scale: DVec2::new(2.0, 1.0),
                rotation: 0.3,
            };
            let nested = block_entity(tx, EntityKind::Insert(inner_ins.clone()));
            let outer_base = DVec2::new(-1.0, 3.0);
            let outer = add_block(tx, outer_base, vec![nested]);
            let outer_ins = Insert {
                block: outer,
                pos: DVec2::new(10.0, 20.0),
                scale: DVec2::new(-1.5, 0.75),
                rotation: -0.4,
            };
            let id = tx.add(EntityKind::Insert(outer_ins.clone()));
            let layer = tx.ids().layer();
            let mut definition = tx.drawing().tables.layers.values().next().unwrap().clone();
            definition.name = "insert layer".into();
            tx.tables_mut().layers.insert(layer, definition);
            let lt = tx.drawing().linetype_by_name("DASHED").unwrap();
            tx.modify(id, |e| {
                e.layer = layer;
                e.color = Color::Aci(3);
                e.linetype = LinetypeRef::Id(lt);
                e.lineweight = LineWeight::Mm100(50);
            });
            (
                id,
                crate::select::insert_transform(&outer_ins, outer_base)
                    * crate::select::insert_transform(&inner_ins, inner_base),
                (outer_ins.scale.x * outer_ins.scale.y * inner_ins.scale.x * inner_ins.scale.y)
                    .abs()
                    .sqrt(),
            )
        });
        h.ed.pump();
        h.ed.doc.clear_history();
        let before = entities(&h);
        let blocks = h.ed.doc.drawing.blocks.clone();
        h.ed.selection.set([root_id]);
        h.cmd("X");
        assert_eq!(h.count("INSERT"), 0);
        assert_eq!(h.count("ELLIPSE"), 1);
        assert_eq!(h.count("LINE"), 1);
        assert_eq!(h.count("POINT"), 1);
        let source = &before[&root_id];
        for e in h.ed.doc.drawing.entities.values() {
            assert_eq!(e.layer, source.layer);
            assert_eq!(e.color, source.color);
            assert_eq!(e.linetype, source.linetype);
            assert_eq!(e.lineweight, source.lineweight);
            if let Some(c) = e.kind.as_curve() {
                assert!((e.linetype_scale - 2.0 * scale).abs() < 1e-12);
                let segment = if matches!(e.kind, EntityKind::Ellipse(_)) {
                    pl.segment(0).unwrap()
                } else {
                    pl.segment(1).unwrap()
                };
                for i in 0..=16 {
                    let p = expected.transform_point2(segment.point_at(i as f64 / 16.0));
                    assert!(c.closest(p).1.distance(p) < 1e-8);
                }
            } else if let EntityKind::Point { p } = e.kind {
                assert!(p.distance(expected.transform_point2(DVec2::new(6.0, 1.0))) < 1e-9);
            }
        }
        assert_eq!(h.ed.doc.drawing.blocks, blocks);
        round_trip(&mut h, before, "EXPLODE");
    }

    #[test]
    fn explode_cycle_missing_block_degenerate_transform_and_unsupported_content_are_atomic() {
        for case in 0..4 {
            let (mut h, _) = harness(Vec::new());
            let id = h.ed.doc.transact("fixture", |tx| {
                let child = block_entity(tx, line(0.0, 0.0, 10.0, 0.0));
                let block = add_block(tx, DVec2::ZERO, vec![child]);
                let mut ins = Insert {
                    block,
                    pos: DVec2::ZERO,
                    scale: DVec2::ONE,
                    rotation: 0.0,
                };
                match case {
                    0 => {
                        let child = block_entity(tx, EntityKind::Insert(ins.clone()));
                        tx.blocks_mut()
                            .get_mut(&block)
                            .unwrap()
                            .entities
                            .insert(child.id, child);
                    }
                    1 => ins.block = BlockId(u32::MAX),
                    2 => ins.scale.x = 0.0,
                    _ => {
                        let child = block_entity(
                            tx,
                            EntityKind::Hatch(wcad_doc::Hatch {
                                loops: Vec::new(),
                                pattern: wcad_doc::HatchPatternRef {
                                    name: "SOLID".into(),
                                    angle: 0.0,
                                    scale: 1.0,
                                },
                            }),
                        );
                        tx.blocks_mut()
                            .get_mut(&block)
                            .unwrap()
                            .entities
                            .insert(child.id, child);
                    }
                }
                tx.add(EntityKind::Insert(ins))
            });
            h.ed.pump();
            h.ed.doc.clear_history();
            let before = entities(&h);
            h.ed.selection.set([id]);
            h.cmd("X");
            assert_eq!(entities(&h), before, "case {case}");
            assert!(!h.ed.doc.can_undo());
            assert!(h.ed.preview.ghosts.is_empty());
        }
    }

    #[test]
    fn explode_depth_and_output_limits_are_checked_before_mutation() {
        for depth in [MAX_BLOCK_DEPTH, MAX_BLOCK_DEPTH + 1] {
            let (mut h, _) = harness(Vec::new());
            let id = h.ed.doc.transact("fixture", |tx| {
                let mut kind = line(0.0, 0.0, 1.0, 0.0);
                for _ in 0..depth {
                    let child = block_entity(tx, kind);
                    let block = add_block(tx, DVec2::ZERO, vec![child]);
                    kind = EntityKind::Insert(Insert {
                        block,
                        pos: DVec2::X,
                        scale: DVec2::ONE,
                        rotation: 0.0,
                    });
                }
                tx.add(kind)
            });
            h.ed.pump();
            h.ed.doc.clear_history();
            let before = entities(&h);
            h.ed.selection.set([id]);
            h.cmd("X");
            if depth == MAX_BLOCK_DEPTH {
                assert_eq!(
                    h.of_type("LINE")[0].1,
                    line(depth as f64, 0.0, depth as f64 + 1.0, 0.0)
                );
                round_trip(&mut h, before, "EXPLODE");
            } else {
                assert_eq!(entities(&h), before);
                assert!(!h.ed.doc.can_undo());
            }
        }
        let (mut h, ids) = harness(vec![EntityKind::Polyline(Polyline2::from_points(
            (0..MAX_PARTS + 2).map(|i| DVec2::new(i as f64, 0.0)),
            false,
        ))]);
        let before = entities(&h);
        h.ed.selection.set(ids);
        h.cmd("X");
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn join_reversed_lines_and_arc_chain_keep_properties_and_undo_redo() {
        for arc in [false, true] {
            let second = if arc {
                EntityKind::Arc(Arc2::new(DVec2::new(2.0, 1.0), 1.0, -FRAC_PI_2, FRAC_PI_2))
            } else {
                line(5.0, 0.0, 2.0, 0.0)
            };
            let (mut h, ids) = harness(vec![line(0.0, 0.0, 2.0, 0.0), second]);
            let before = entities(&h);
            h.cmd("J");
            h.ed.selection.set(ids.clone());
            h.ed.refresh_preview();
            assert_eq!(h.ed.preview.ghosts.len(), 1);
            h.enter();
            assert_eq!(h.ed.doc.drawing.entities.len(), 1);
            let result = &h.ed.doc.drawing.entities[&ids[0]];
            assert!(same_properties(result, &before[&ids[0]]));
            if arc {
                let EntityKind::Polyline(p) = &result.kind else {
                    panic!()
                };
                assert_eq!(p.segment_count(), 2);
                assert!((p.verts[1].bulge - 1.0).abs() < 1e-9);
            } else {
                assert_eq!(result.kind, line(0.0, 0.0, 5.0, 0.0));
            }
            round_trip(&mut h, before, "JOIN");
        }
    }

    #[test]
    fn join_disconnected_mixed_properties_and_unsupported_curves_are_not_deleted() {
        for case in 0..3 {
            let other = if case == 2 {
                EntityKind::Circle(Circle2::new(DVec2::new(2.0, 1.0), 1.0))
            } else {
                line(if case == 0 { 3.0 } else { 2.0 }, 0.0, 5.0, 0.0)
            };
            let (mut h, ids) = harness(vec![line(0.0, 0.0, 2.0, 0.0), other]);
            if case == 1 {
                h.ed.doc.transact("fixture", |tx| {
                    tx.modify(ids[1], |e| e.color = Color::Aci(1));
                });
                h.ed.doc.clear_history();
            }
            let before = entities(&h);
            h.ed.selection.set(ids);
            h.cmd("J");
            assert_eq!(entities(&h), before);
            assert!(!h.ed.doc.can_undo());
        }
    }

    #[test]
    fn all_commands_protect_locked_layers() {
        for op in [
            Op::Offset,
            Op::Trim,
            Op::Extend,
            Op::Fillet,
            Op::Chamfer,
            Op::Break,
            Op::Explode,
            Op::Join,
        ] {
            let (mut h, ids) = harness(vec![
                line(0.0, 0.0, 5.0, 0.0),
                line(8.0, -5.0, 8.0, 5.0),
                polyline(),
            ]);
            let layer = h.ed.doc.drawing.tables.current_layer;
            h.ed.doc.transact("lock", |tx| {
                tx.tables_mut().layers.get_mut(&layer).unwrap().locked = true;
            });
            h.ed.doc.clear_history();
            h.ed.pump();
            let before = entities(&h);
            h.ed.start_tool(Box::new(EditTool::new(op)));
            match op {
                Op::Offset => {
                    h.cmd("1").click(4.0, 0.0).click(4.0, 2.0);
                }
                Op::Trim | Op::Extend => {
                    h.ed.feed(ToolInput::Selection(vec![ids[1]]));
                    h.click(4.0, 0.0);
                }
                Op::Fillet | Op::Chamfer => {
                    h.cmd("1");
                    if op == Op::Chamfer {
                        h.cmd("1");
                    }
                    h.click(4.0, 0.0).click(8.0, 3.0);
                }
                Op::Break => {
                    h.click(4.0, 0.0).click(3.0, 0.0).click(2.0, 0.0);
                }
                Op::Explode => h.ed.feed(ToolInput::Selection(vec![ids[2]])),
                Op::Join => h.ed.feed(ToolInput::Selection(ids[..2].to_vec())),
            }
            assert_eq!(entities(&h), before);
            assert!(!h.ed.doc.can_undo());
            h.esc();
        }
    }

    #[test]
    fn offset_rechecks_lock_after_pick_and_locked_boundaries_remain_usable() {
        let (mut h, ids) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
        h.cmd("O").cmd("2").click(3.0, 0.0);
        let layer = h.ed.doc.drawing.tables.current_layer;
        h.ed.doc.transact("lock", |tx| {
            tx.tables_mut().layers.get_mut(&layer).unwrap().locked = true;
        });
        h.ed.doc.clear_history();
        let before = entities(&h);
        h.click(3.0, 2.0);
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
        assert!(h.ed.preview.ghosts.is_empty());
        h.esc();
        h.ed.doc.transact("fixture", |tx| {
            let new_layer = tx.ids().layer();
            let mut layer = tx.drawing().tables.layers.values().next().unwrap().clone();
            layer.name = "editable".into();
            layer.locked = false;
            tx.tables_mut().layers.insert(new_layer, layer);
            tx.add(line(4.0, -2.0, 4.0, 2.0));
            tx.modify(ids[0], |e| e.layer = new_layer);
        });
        h.ed.doc.clear_history();
        h.ed.pump();
        let boundary = *h.ed.doc.drawing.entities.keys().last().unwrap();
        h.ed.selection.set([boundary]);
        h.cmd("TR").click(7.0, 0.0).enter();
        assert_eq!(
            h.ed.doc.drawing.entities[&ids[0]].kind,
            line(0.0, 0.0, 4.0, 0.0)
        );
    }

    #[test]
    fn cancellation_and_preview_never_create_history() {
        for op in [
            Op::Offset,
            Op::Trim,
            Op::Extend,
            Op::Fillet,
            Op::Chamfer,
            Op::Break,
            Op::Explode,
            Op::Join,
        ] {
            let (mut h, ids) = harness(vec![
                line(0.0, 0.0, 10.0, 0.0),
                line(10.0, -2.0, 10.0, 10.0),
                polyline(),
            ]);
            let before = entities(&h);
            let rev = h.ed.doc.revision();
            h.ed.start_tool(Box::new(EditTool::new(op)));
            match op {
                Op::Offset => {
                    h.cmd("1").click(8.0, 0.0).hover(8.0, 2.0);
                }
                Op::Trim | Op::Extend => {
                    h.ed.feed(ToolInput::Selection(vec![ids[1]]));
                    h.hover(8.0, 0.0);
                }
                Op::Fillet | Op::Chamfer => {
                    h.cmd("1");
                    if op == Op::Chamfer {
                        h.cmd("2");
                    }
                    h.click(8.0, 0.0).hover(10.0, 8.0);
                }
                Op::Break => {
                    h.click(8.0, 0.0).click(3.0, 0.02).hover(7.0, 0.02);
                }
                Op::Explode => h.ed.selection.set([ids[2]]),
                Op::Join => h.ed.selection.set(ids[..2].iter().copied()),
            }
            h.ed.refresh_preview();
            h.esc();
            assert_eq!(entities(&h), before);
            assert_eq!(h.ed.doc.revision(), rev);
            assert!(!h.ed.doc.can_undo());
            assert!(h.ed.preview.is_empty());
        }
    }

    #[test]
    fn direct_nonfinite_inputs_do_not_advance_any_tool() {
        for op in [
            Op::Offset,
            Op::Trim,
            Op::Extend,
            Op::Fillet,
            Op::Chamfer,
            Op::Break,
            Op::Explode,
            Op::Join,
        ] {
            let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
            let mut t = EditTool::new(op);
            let step = t.step;
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                for input in [
                    ToolInput::Value(v),
                    ToolInput::Point(DVec2::new(v, 0.0)),
                    ToolInput::Hover(DVec2::new(0.0, v)),
                ] {
                    assert_eq!(raw(&mut t, &mut h, input), ToolFlow::Continue);
                    assert!(t.step == step);
                    assert!(t.first.is_none());
                    assert!(!h.ed.doc.can_undo());
                }
            }
        }
    }

    #[test]
    fn invalid_stored_curves_and_extreme_values_are_rejected() {
        for c in [
            Curve2::Line(Line2::new(DVec2::ZERO, DVec2::ZERO)),
            Curve2::Line(Line2::new(DVec2::splat(-f64::MAX), DVec2::splat(f64::MAX))),
            Curve2::Circle(Circle2::new(DVec2::ZERO, f64::INFINITY)),
            Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, f64::NAN, PI)),
            Curve2::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::ZERO, f64::MAX),
                    PolyVertex::new(DVec2::X),
                ],
                closed: false,
            }),
            Curve2::Spline(Nurbs2::default()),
        ] {
            assert!(!valid_curve(&c));
        }
        let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0), line(10.0, 0.0, 10.0, 10.0)]);
        let before = entities(&h);
        h.cmd("F")
            .cmd(&f64::MAX.to_string())
            .click(3.0, 0.0)
            .click(10.0, 5.0);
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn fillet_pick_quadrant_and_oversized_radius_validation() {
        for sign in [-1.0, 1.0] {
            let (mut h, _) = harness(vec![
                line(-10.0, 0.0, 10.0, 0.0),
                line(0.0, -10.0, 0.0, 10.0),
            ]);
            h.cmd("F")
                .cmd("2")
                .click(sign * 5.0, 0.0)
                .click(0.0, -sign * 5.0);
            let EntityKind::Arc(a) = h.of_type("ARC")[0].1 else {
                panic!()
            };
            assert!(a.c.distance(DVec2::new(sign * 2.0, -sign * 2.0)) < 1e-9);
        }
        for radius in [8.0, 10.0, 11.0] {
            let (mut h, _) = harness(vec![line(0.0, 0.0, 10.0, 0.0), line(10.0, 0.0, 10.0, 10.0)]);
            let before = entities(&h);
            h.cmd("F")
                .cmd(&radius.to_string())
                .click(1.0, 0.0)
                .click(10.0, 9.0);
            if radius == 8.0 {
                assert_eq!(h.count("ARC"), 1);
                assert_eq!(h.count("LINE"), 2);
                round_trip(&mut h, before, "FILLET");
            } else {
                assert_eq!(entities(&h), before);
                assert!(!h.ed.doc.can_undo());
            }
        }
    }

    #[test]
    fn extend_arc_and_bulge_polyline_keep_their_carriers() {
        let arc = Arc2::new(DVec2::ZERO, 5.0, 0.0, PI / 4.0);
        for kind in [
            EntityKind::Arc(arc),
            EntityKind::Polyline(arc.to_polyline(true)),
        ] {
            let (mut h, ids) = harness(vec![kind, line(-10.0, 4.0, 10.0, 4.0)]);
            let before = entities(&h);
            h.ed.selection.set([ids[1]]);
            let p = arc.end_point();
            h.cmd("EX").click(p.x, p.y).enter();
            let c = h.ed.doc.drawing.entities[&ids[0]].kind.as_curve().unwrap();
            assert!((c.end().y - 4.0).abs() < 1e-9);
            assert!((c.end().length() - 5.0).abs() < 1e-9);
            assert!(c.length() > arc.length());
            round_trip(&mut h, before, "EXTEND");
        }
    }

    #[test]
    fn break_spline_is_exact_and_extend_spline_is_explicitly_unsupported() {
        let spline = Nurbs2::from_fit_points(
            &[DVec2::ZERO, DVec2::new(2.0, 1.0), DVec2::new(4.0, 0.0)],
            3,
        )
        .unwrap();
        let c = Curve2::Spline(spline.clone());
        let (lo, hi) = c.domain();
        let p1 = c.point_at(lo + (hi - lo) * 0.25);
        let p2 = c.point_at(lo + (hi - lo) * 0.75);
        let (mut h, _) = harness(vec![EntityKind::Spline(spline.clone())]);
        let before = entities(&h);
        h.cmd("BR")
            .click(0.0, 0.0)
            .click(p1.x, p1.y)
            .click(p2.x, p2.y);
        assert_eq!(h.count("SPLINE"), 2);
        let parts: Vec<_> =
            h.ed.doc
                .drawing
                .entities
                .values()
                .map(|e| e.kind.as_curve().unwrap())
                .collect();
        assert!(parts[0].end().distance(p1) < 1e-8);
        assert!(parts[1].start().distance(p2) < 1e-8);
        round_trip(&mut h, before, "BREAK");
        let (mut h, ids) = harness(vec![EntityKind::Spline(spline), line(6.0, -2.0, 6.0, 2.0)]);
        h.ed.selection.set([ids[1]]);
        h.cmd("EX").click(4.0, 0.0);
        assert!(h.last_message().contains("does not support"));
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn explode_cannot_create_entities_on_a_locked_child_layer() {
        let (mut h, _) = harness(Vec::new());
        let id = h.ed.doc.transact("fixture", |tx| {
            let layer_id = tx.ids().layer();
            let mut layer = tx.drawing().tables.layers.values().next().unwrap().clone();
            layer.name = "locked child".into();
            layer.locked = true;
            tx.tables_mut().layers.insert(layer_id, layer);
            let good = block_entity(tx, line(0.0, 0.0, 1.0, 0.0));
            let mut locked = block_entity(tx, line(0.0, 1.0, 1.0, 1.0));
            locked.layer = layer_id;
            let block = add_block(tx, DVec2::ZERO, vec![good, locked]);
            tx.add(EntityKind::Insert(Insert {
                block,
                pos: DVec2::ZERO,
                scale: DVec2::ONE,
                rotation: 0.0,
            }))
        });
        h.ed.pump();
        h.ed.doc.clear_history();
        let before = entities(&h);
        h.ed.selection.set([id]);
        h.cmd("X");
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
    }

    #[test]
    fn stale_pick_cannot_overwrite_a_later_change() {
        let (mut h, ids) = harness(vec![line(0.0, 0.0, 10.0, 0.0)]);
        h.cmd("BR").click(1.0, 0.0).click(3.0, 0.0);
        h.ed.doc.transact("external change", |tx| {
            tx.modify(ids[0], |e| e.color = Color::Aci(1));
        });
        h.ed.doc.clear_history();
        let before = entities(&h);
        h.click(6.0, 0.0);
        assert_eq!(entities(&h), before);
        assert!(!h.ed.doc.can_undo());
        assert!(h.last_message().contains("changed"));
    }
}
