//! Changing the board's shape: one step moves the outline from one polygon to another and carries
//! everything on the board with it. Parts move rigidly (turned and shifted as the caller says);
//! one harmonic field, pinned to the outline's points and to every pad, carries the rest: trace
//! corners, vias, and the small keepouts and pours, which move rigidly with the field at their
//! centre. Trace corners on a pin or via centre land exactly on its new centre, so contacts hold.

use std::collections::BTreeMap;

use copper_board::items::Item;
use copper_board::structure::{FixedState, Unit};
use copper_board::{Board, ItemId};
use copper_geometry::{
    FloatPoint, IntBox, IntPoint, Point, PolygonShape, Polyline, PolylineShapeRef, TileShape, Vector,
};

use crate::frames;
use crate::warp::WarpField;

/// A part's rigid move for one step: turned about its location, then shifted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PartMove {
    pub by: (f64, f64),
    pub turn_degree: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReshapeReport {
    pub traces: usize,
    pub vias: usize,
    pub areas: usize,
    pub parts: usize,
}

const MARGIN_MM: f64 = 3.0;
const SPLIT_MM: f64 = 0.5;
/// Areas larger than this bend with the field, corner by corner; smaller ones move rigidly.
const LARGE_AREA_MM: f64 = 3.0;

/// One step: the outline goes from `from` to `to` (corresponding points, board units) and each part
/// in `moves` moves rigidly. `cell_mm` is the field's grid. The two nets of each pair in
/// `pairs` move together: a corner takes the field at the midpoint to its partner's nearest
/// corner, so the pair's gap survives a field that squeezes across it.
pub fn reshape_step(
    board: &mut Board,
    from: &[FloatPoint],
    to: &[FloatPoint],
    moves: &BTreeMap<i32, PartMove>,
    cell_mm: f64,
    pairs: &[(i32, i32)],
) -> ReshapeReport {
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let mut report = ReshapeReport::default();

    let pin_moves = pin_moves(board, moves);
    let mut snaps: BTreeMap<(i32, i32), IntPoint> = pin_moves
        .iter()
        .map(|(_, old, new)| ((old.x, old.y), *new))
        .collect();

    let bounds = box_around(from.iter().chain(to).copied(), &board.bounding_box);
    let mut field = WarpField::new(&bounds, cell_mm * per_mm, MARGIN_MM * per_mm);
    let pin = (0.5 * cell_mm * per_mm).round() as i32;
    for (old, new) in from.iter().zip(to) {
        let at = IntPoint::new(old.x.round() as i32, old.y.round() as i32);
        field.add_inclusion(&[square(at, pin)], (new.x - old.x, new.y - old.y), false);
    }
    for (id, old, new) in &pin_moves {
        let count = board
            .get_item(*id)
            .map_or(0, |item| item.tile_shape_count(&board.ctx()));
        let mut shapes: Vec<TileShape> = (0..count)
            .filter_map(|i| board.item_tile_shape_ref(*id, i).map(|s| s.into_owned()))
            .collect();
        if shapes.is_empty() {
            shapes.push(square(*old, pin));
        }
        let vector = (f64::from(new.x - old.x), f64::from(new.y - old.y));
        field.add_inclusion(&shapes, vector, false);
    }
    field.solve();

    let lifted = lift_traces(board, per_mm);
    report.traces = lifted.len();

    for (&component, part) in moves {
        let Some(Point::Int(location)) = board
            .components
            .get_all()
            .find(|c| c.id == component)
            .and_then(|c| c.get_location().cloned())
        else {
            continue;
        };
        let items = board.get_component_items(component);
        if part.turn_degree != 0.0 {
            board.components.rotate(component, part.turn_degree, &location);
            for &id in &items {
                board.rotate_item_approx(id, part.turn_degree, &location.to_float());
            }
        }
        let by = Vector::Int(copper_geometry::IntVector::new(
            part.by.0.round() as i32,
            part.by.1.round() as i32,
        ));
        board.components.move_component(component, &by);
        for &id in &items {
            let _ = board.move_item_by(id, &by);
        }
        report.parts += 1;
    }
    // Rotation rounds differently from any prediction, and contacts need identical centres:
    // snap to where each pin really is now.
    for (id, old, _) in &pin_moves {
        if let Some(Point::Int(actual)) = board.drill_center(*id) {
            snaps.insert((old.x, old.y), actual);
        }
    }

    let vias: Vec<ItemId> = board
        .get_items()
        .filter(|item| matches!(item, Item::Via(_)))
        .map(Item::id)
        .collect();
    for id in vias {
        let Some(Point::Int(old)) = board.drill_center(id) else {
            continue;
        };
        let new = snaps.get(&(old.x, old.y)).copied().unwrap_or_else(|| {
            let (dx, dy) = field.displacement(&old.to_float());
            IntPoint::new(old.x + dx.round() as i32, old.y + dy.round() as i32)
        });
        let by = Vector::Int(copper_geometry::IntVector::new(new.x - old.x, new.y - old.y));
        if matches!(board.move_item_by(id, &by), Ok(true)) {
            snaps.insert((old.x, old.y), new);
            report.vias += 1;
        }
    }

    let areas: Vec<ItemId> = board
        .get_items()
        .filter(|item| {
            item.component_id() <= 0
                && matches!(
                    item,
                    Item::ObstacleArea(_) | Item::ViaObstacleArea(_) | Item::ConductionArea(_)
                )
        })
        .map(Item::id)
        .collect();
    for id in areas {
        let Some(bounds) = item_box(board, id) else {
            continue;
        };
        if (bounds.x1 - bounds.x0).max(bounds.y1 - bounds.y0) > 10.0 * per_mm {
            tracing::debug!(
                "large area {:?}: {:?} {:.1} x {:.1} mm",
                id,
                board.get_item(id).map(|i| (i.kind(), i.first_layer(&board.ctx()), i.get_fixed_state())),
                (bounds.x1 - bounds.x0) / per_mm,
                (bounds.y1 - bounds.y0) / per_mm
            );
        }
        if (bounds.x1 - bounds.x0).max(bounds.y1 - bounds.y0) > LARGE_AREA_MM * per_mm
            && warp_area(board, id, &field, per_mm)
        {
            report.areas += 1;
            continue;
        }
        let centre = bounds.centre();
        let (dx, dy) = field.displacement(&centre);
        let by = Vector::Int(copper_geometry::IntVector::new(dx.round() as i32, dy.round() as i32));
        if matches!(board.move_item_by(id, &by), Ok(true)) {
            report.areas += 1;
        }
    }


    let partner: BTreeMap<i32, i32> = pairs
        .iter()
        .flat_map(|&(p, n)| [(p, n), (n, p)])
        .collect();
    let mut corners_of: BTreeMap<(i32, usize), Vec<FloatPoint>> = BTreeMap::new();
    for trace in &lifted {
        for net in &trace.nets {
            if partner.contains_key(net) {
                corners_of.entry((*net, trace.layer)).or_default().extend(&trace.corners);
            }
        }
    }
    let reach = 1.0 * per_mm;
    let pair_point = |trace: &LiftedTrace, c: &FloatPoint| -> FloatPoint {
        let Some(other) = trace.nets.iter().find_map(|n| partner.get(n)) else {
            return *c;
        };
        let nearest = corners_of
            .get(&(*other, trace.layer))
            .into_iter()
            .flatten()
            .min_by(|a, b| {
                (a.x - c.x).hypot(a.y - c.y).total_cmp(&(b.x - c.x).hypot(b.y - c.y))
            });
        match nearest {
            Some(q) if (q.x - c.x).hypot(q.y - c.y) <= reach => {
                FloatPoint::new((c.x + q.x) / 2.0, (c.y + q.y) / 2.0)
            }
            _ => *c,
        }
    };

    for trace in lifted {
        let mut points: Vec<Point> = Vec::with_capacity(trace.corners.len());
        for c in &trace.corners {
            let key = (c.x.round() as i32, c.y.round() as i32);
            let p = snaps.get(&key).copied().unwrap_or_else(|| {
                let (dx, dy) = field.displacement(&pair_point(&trace, c));
                IntPoint::new((c.x + dx).round() as i32, (c.y + dy).round() as i32)
            });
            if points.last() != Some(&Point::Int(p)) {
                points.push(Point::Int(p));
            }
        }
        if points.len() < 2 {
            continue;
        }
        board.insert_trace_without_cleaning(
            Polyline::from_points(&points),
            trace.layer,
            trace.half_width,
            trace.nets,
            trace.clearance_class,
            trace.fixed_state,
        );
    }

    replace_outline(board, to);
    frames::committed(board, &moves.keys().copied().collect::<Vec<_>>(), &copper_geometry::IntVector::ZERO);
    report
}

/// Each pin of every part: its id, centre now, and centre after the part's rigid move (none for
/// parts not in `moves`, which the field must hold still).
fn pin_moves(board: &Board, moves: &BTreeMap<i32, PartMove>) -> Vec<(ItemId, IntPoint, IntPoint)> {
    let mut out = Vec::new();
    let still = PartMove::default();
    let components: Vec<i32> = board.components.get_all().map(|c| c.id).collect();
    for component in components {
        let part = moves.get(&component).unwrap_or(&still);
        let Some(location) = board
            .components
            .get_all()
            .find(|c| c.id == component)
            .and_then(|c| c.get_location().map(Point::to_float))
        else {
            continue;
        };
        let (s, c) = part.turn_degree.to_radians().sin_cos();
        for id in board.get_component_items(component) {
            if !board.get_item(id).is_some_and(Item::is_drill_item) {
                continue;
            }
            let Some(Point::Int(old)) = board.drill_center(id) else {
                continue;
            };
            let (rx, ry) = (f64::from(old.x) - location.x, f64::from(old.y) - location.y);
            let new = IntPoint::new(
                (location.x + rx * c - ry * s + part.by.0).round() as i32,
                (location.y + rx * s + ry * c + part.by.1).round() as i32,
            );
            out.push((id, old, new));
        }
    }
    out
}

struct LiftedTrace {
    corners: Vec<FloatPoint>,
    layer: usize,
    half_width: i32,
    nets: Vec<i32>,
    clearance_class: usize,
    fixed_state: FixedState,
}

/// Removes every trace, returning it with its corners split finely enough to follow the field.
fn lift_traces(board: &mut Board, per_mm: f64) -> Vec<LiftedTrace> {
    let split = SPLIT_MM * per_mm;
    let mut lifted = Vec::new();
    let mut ids = Vec::new();
    for item in board.get_items() {
        let Item::Trace(trace) = item else {
            continue;
        };
        let polyline = trace.polyline();
        let corners: Vec<FloatPoint> = (0..polyline.corner_count())
            .filter_map(|i| polyline.corner(i).map(|p| p.to_float()))
            .collect();
        let mut fine = Vec::new();
        for pair in corners.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let pieces = ((a.x - b.x).hypot(a.y - b.y) / split).ceil().max(1.0) as usize;
            for k in 0..pieces {
                let t = k as f64 / pieces as f64;
                fine.push(FloatPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
            }
        }
        if let Some(last) = corners.last() {
            fine.push(*last);
        }
        ids.push(item.id());
        lifted.push(LiftedTrace {
            corners: fine,
            layer: trace.get_layer(),
            half_width: trace.get_half_width(),
            nets: item.net_nos().to_vec(),
            clearance_class: item.clearance_class(),
            fixed_state: item.get_fixed_state(),
        });
    }
    // The board refuses to delete fixed items; each goes back in with its fixed state.
    for id in ids {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    lifted
}

/// Re-inserts an obstacle area with its border and holes carried by the field (edges split so
/// they can bend). False when the item is not an obstacle area this can rebuild.
fn warp_area(board: &mut Board, id: ItemId, field: &WarpField, per_mm: f64) -> bool {
    let (area, layer, class, fixed) = {
        let ctx = board.ctx();
        match board.get_item(id) {
            Some(item @ Item::ObstacleArea(o)) => (
                o.get_area(&ctx).clone(),
                item.first_layer(&ctx),
                item.clearance_class(),
                item.get_fixed_state(),
            ),
            _ => return false,
        }
    };
    let bend = |shape: &PolylineShapeRef| -> Option<PolylineShapeRef> {
        let corners = shape.as_ops().corner_approx_arr();
        if corners.len() < 3 {
            return None;
        }
        let mut points: Vec<Point> = Vec::new();
        for (i, a) in corners.iter().enumerate() {
            let b = corners[(i + 1) % corners.len()];
            let pieces = ((a.x - b.x).hypot(a.y - b.y) / (1.0 * per_mm)).ceil().max(1.0) as usize;
            for k in 0..pieces {
                let t = k as f64 / pieces as f64;
                let p = FloatPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                let (dx, dy) = field.displacement(&p);
                let q = Point::Int(IntPoint::new((p.x + dx).round() as i32, (p.y + dy).round() as i32));
                if points.last() != Some(&q) {
                    points.push(q);
                }
            }
        }
        (points.len() >= 3).then(|| PolylineShapeRef::Polygon(PolygonShape::from_points(&points)))
    };
    let warped = match &area {
        copper_geometry::Area::Polyline(pa) => {
            let Some(border) = bend(pa.get_border()) else {
                return false;
            };
            let holes = pa.get_holes().iter().filter_map(bend).collect();
            copper_geometry::Area::Polyline(copper_geometry::PolylineArea::new(border, holes))
        }
        copper_geometry::Area::Shape(copper_geometry::Shape::Polygon(p)) => {
            match bend(&PolylineShapeRef::Polygon(p.clone())) {
                Some(PolylineShapeRef::Polygon(q)) => copper_geometry::Area::Shape(copper_geometry::Shape::Polygon(q)),
                _ => return false,
            }
        }
        copper_geometry::Area::Shape(copper_geometry::Shape::Tile(t)) => {
            match bend(&PolylineShapeRef::Tile(t.clone())) {
                Some(PolylineShapeRef::Polygon(q)) => copper_geometry::Area::Shape(copper_geometry::Shape::Polygon(q)),
                _ => return false,
            }
        }
        copper_geometry::Area::Shape(copper_geometry::Shape::Circle(_)) => return false,
    };
    if let Some(item) = board.get_item_mut(id) {
        item.set_fixed_state(FixedState::Unfixed);
    }
    board.remove_item(id);
    board.insert_obstacle(warped, layer, class, fixed);
    true
}

fn replace_outline(board: &mut Board, to: &[FloatPoint]) {
    let Some((id, class, keepout)) = board.get_items().find_map(|item| match item {
        Item::BoardOutline(outline) => Some((
            item.id(),
            item.clearance_class(),
            outline.keepout_outside_outline_generated(),
        )),
        _ => None,
    }) else {
        return;
    };
    if let Some(item) = board.get_item_mut(id) {
        item.set_fixed_state(FixedState::Unfixed);
    }
    board.remove_item(id);
    let corners: Vec<Point> = to
        .iter()
        .map(|p| Point::Int(IntPoint::new(p.x.round() as i32, p.y.round() as i32)))
        .collect();
    let new = board.insert_outline(
        vec![PolylineShapeRef::Polygon(PolygonShape::from_points(&corners))],
        class,
    );
    board.generate_keepout_outside(new, keepout);
    board.bounding_box = box_around(to.iter().copied(), &IntBox::EMPTY);
}

fn item_box(board: &Board, id: ItemId) -> Option<FloatBox> {
    let ctx = board.ctx();
    let item = board.get_item(id)?;
    let mut out: Option<FloatBox> = None;
    for i in 0..item.tile_shape_count(&ctx) {
        let Some(shape) = board.item_tile_shape_ref(id, i) else {
            continue;
        };
        let b = shape.bounding_box();
        let fb = FloatBox {
            x0: f64::from(b.ll.x),
            y0: f64::from(b.ll.y),
            x1: f64::from(b.ur.x),
            y1: f64::from(b.ur.y),
        };
        out = Some(match out {
            None => fb,
            Some(o) => FloatBox {
                x0: o.x0.min(fb.x0),
                y0: o.y0.min(fb.y0),
                x1: o.x1.max(fb.x1),
                y1: o.y1.max(fb.y1),
            },
        });
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct FloatBox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl FloatBox {
    fn centre(&self) -> FloatPoint {
        FloatPoint::new((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
}

fn square(at: IntPoint, half: i32) -> TileShape {
    TileShape::Box(IntBox::from_coords(at.x - half, at.y - half, at.x + half, at.y + half))
}

fn box_around(points: impl Iterator<Item = FloatPoint>, with: &IntBox) -> IntBox {
    let (mut x0, mut y0, mut x1, mut y1) = if with.is_empty() {
        (i32::MAX, i32::MAX, i32::MIN, i32::MIN)
    } else {
        (with.ll.x, with.ll.y, with.ur.x, with.ur.y)
    };
    for p in points {
        x0 = x0.min(p.x.floor() as i32);
        y0 = y0.min(p.y.floor() as i32);
        x1 = x1.max(p.x.ceil() as i32);
        y1 = y1.max(p.y.ceil() as i32);
    }
    IntBox::from_coords(x0, y0, x1, y1)
}
