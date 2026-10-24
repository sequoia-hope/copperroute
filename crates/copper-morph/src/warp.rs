//! A harmonic displacement field with rigid inclusions on a regular grid, ported from the rubber
//! and pcbmorph warp fields: inside an inclusion the field is that inclusion's vector, elsewhere it
//! is harmonic, and it decays to zero on the boundary of a domain that extends past the board.
//! Copper evaluated through it flows smoothly around moving parts instead of being dragged.

use copper_geometry::{FloatPoint, IntBox, TileShape};

const UNOWNED: i32 = -1;
const CG_TOLERANCE: f64 = 1e-6;
const CG_MAX_ITERATIONS: usize = 4000;

pub struct WarpField {
    x0: f64,
    y0: f64,
    h: f64,
    nx: usize,
    ny: usize,
    owner: Vec<i32>,
    vectors: Vec<(f64, f64)>,
    pending: Vec<(Vec<TileShape>, bool)>,
    u: Vec<(f64, f64)>,
}

impl WarpField {
    /// `bounds` in board units; `h` the cell size and `margin` how far the domain extends past
    /// `bounds`, both in board units.
    pub fn new(bounds: &IntBox, h: f64, margin: f64) -> WarpField {
        let x0 = f64::from(bounds.ll.x) - margin;
        let y0 = f64::from(bounds.ll.y) - margin;
        let nx = ((f64::from(bounds.ur.x) + margin - x0) / h).ceil() as usize + 1;
        let ny = ((f64::from(bounds.ur.y) + margin - y0) / h).ceil() as usize + 1;
        WarpField {
            x0,
            y0,
            h,
            nx,
            ny,
            owner: vec![UNOWNED; nx * ny],
            vectors: Vec::new(),
            pending: Vec::new(),
            u: vec![(0.0, 0.0); nx * ny],
        }
    }

    /// Adds an inclusion moving by `vector`: the cells under `shapes` and a one-cell halo, and
    /// when `with_hull` also the space their convex hull encloses. Territory is claimed in
    /// rubber's rounds when solving: every inclusion's own shapes first, hulls only after, so a
    /// part's hull never takes the pads of a part sitting inside it. Within a round, earlier
    /// inclusions win.
    pub fn add_inclusion(&mut self, shapes: &[TileShape], vector: (f64, f64), with_hull: bool) {
        self.vectors.push(vector);
        self.pending.push((shapes.to_vec(), with_hull));
    }

    fn claim_territory(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (index, (shapes, _)) in pending.iter().enumerate() {
            let mut cells = Vec::new();
            for shape in shapes {
                self.cells_inside(shape, 1, &mut cells);
            }
            self.claim(index, cells);
        }
        for (index, (shapes, with_hull)) in pending.iter().enumerate() {
            if !with_hull || shapes.len() < 2 {
                continue;
            }
            let hull = convex_hull(
                shapes
                    .iter()
                    .flat_map(TileShape::corner_approx_arr)
                    .collect(),
            );
            if hull.len() < 3 {
                continue;
            }
            let (lo, hi) = self.window(&bounding(&hull), 0);
            let mut cells = Vec::new();
            for j in lo.1..=hi.1 {
                for i in lo.0..=hi.0 {
                    if point_in_convex(&hull, &self.cell_center(i, j)) {
                        cells.push(j * self.nx + i);
                    }
                }
            }
            self.claim(index, cells);
        }
    }

    fn claim(&mut self, index: usize, cells: Vec<usize>) {
        let index = i32::try_from(index).expect("fewer inclusions than i32::MAX");
        for cell in cells {
            if self.owner[cell] == UNOWNED {
                self.owner[cell] = index;
            }
        }
    }

    /// Solves for the harmonic field: owned cells hold their inclusion's vector, the domain border
    /// holds zero, everything else is the discrete Laplace solution (conjugate gradients on the
    /// reduced, symmetric positive definite system).
    pub fn solve(&mut self) {
        self.claim_territory();
        for (cell, owner) in self.owner.iter().enumerate() {
            if *owner >= 0 {
                self.u[cell] = self.vectors[*owner as usize];
            }
        }
        for component in 0..2 {
            self.solve_component(component);
        }
    }

    /// The displacement at `point`, bilinear between the four surrounding cell centres.
    pub fn displacement(&self, point: &FloatPoint) -> (f64, f64) {
        let fx = ((point.x - self.x0) / self.h).clamp(0.0, (self.nx - 1) as f64);
        let fy = ((point.y - self.y0) / self.h).clamp(0.0, (self.ny - 1) as f64);
        let (i, j) = (
            (fx.floor() as usize).min(self.nx - 2),
            (fy.floor() as usize).min(self.ny - 2),
        );
        let (tx, ty) = (fx - i as f64, fy - j as f64);
        let at = |i: usize, j: usize| self.u[j * self.nx + i];
        let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
        let lerp = |p: f64, q: f64, r: f64, s: f64| {
            p * (1.0 - tx) * (1.0 - ty) + q * tx * (1.0 - ty) + r * (1.0 - tx) * ty + s * tx * ty
        };
        (lerp(a.0, b.0, c.0, d.0), lerp(a.1, b.1, c.1, d.1))
    }

    fn free(&self, i: usize, j: usize) -> bool {
        i > 0 && j > 0 && i + 1 < self.nx && j + 1 < self.ny && self.owner[j * self.nx + i] < 0
    }

    fn solve_component(&mut self, component: usize) {
        let (nx, ny) = (self.nx, self.ny);
        let value = |u: &(f64, f64)| if component == 0 { u.0 } else { u.1 };
        let free: Vec<usize> = (0..nx * ny)
            .filter(|&c| self.free(c % nx, c / nx))
            .collect();
        if free.is_empty() {
            return;
        }
        let mut slot = vec![usize::MAX; nx * ny];
        for (k, &c) in free.iter().enumerate() {
            slot[c] = k;
        }
        let neighbours = |c: usize| [c - 1, c + 1, c - nx, c + nx];
        // A x = b with A = 4I - (free neighbours), b = sum of fixed neighbours' values.
        let mut b = vec![0.0; free.len()];
        for (k, &c) in free.iter().enumerate() {
            for n in neighbours(c) {
                if slot[n] == usize::MAX {
                    b[k] += value(&self.u[n]);
                }
            }
        }
        let apply = |x: &[f64], out: &mut [f64]| {
            for (k, &c) in free.iter().enumerate() {
                let mut sum = 4.0 * x[k];
                for n in neighbours(c) {
                    if slot[n] != usize::MAX {
                        sum -= x[slot[n]];
                    }
                }
                out[k] = sum;
            }
        };
        let mut x = vec![0.0; free.len()];
        let mut r = b.clone();
        let mut p = r.clone();
        let mut ap = vec![0.0; free.len()];
        let mut rr: f64 = r.iter().map(|v| v * v).sum();
        let target = CG_TOLERANCE * CG_TOLERANCE * b.iter().map(|v| v * v).sum::<f64>().max(1e-30);
        for _ in 0..CG_MAX_ITERATIONS {
            if rr <= target {
                break;
            }
            apply(&p, &mut ap);
            let alpha = rr / p.iter().zip(&ap).map(|(a, b)| a * b).sum::<f64>();
            for k in 0..x.len() {
                x[k] += alpha * p[k];
                r[k] -= alpha * ap[k];
            }
            let next: f64 = r.iter().map(|v| v * v).sum();
            let beta = next / rr;
            rr = next;
            for k in 0..p.len() {
                p[k] = r[k] + beta * p[k];
            }
        }
        for (k, &c) in free.iter().enumerate() {
            if component == 0 {
                self.u[c].0 = x[k];
            } else {
                self.u[c].1 = x[k];
            }
        }
    }

    fn cell_center(&self, i: usize, j: usize) -> FloatPoint {
        FloatPoint::new(self.x0 + i as f64 * self.h, self.y0 + j as f64 * self.h)
    }

    fn window(
        &self,
        bounds: &(f64, f64, f64, f64),
        halo: usize,
    ) -> ((usize, usize), (usize, usize)) {
        let clamp_x = |v: f64| ((v - self.x0) / self.h).clamp(0.0, (self.nx - 1) as f64);
        let clamp_y = |v: f64| ((v - self.y0) / self.h).clamp(0.0, (self.ny - 1) as f64);
        let lo = (
            (clamp_x(bounds.0).floor() as usize).saturating_sub(halo),
            (clamp_y(bounds.1).floor() as usize).saturating_sub(halo),
        );
        let hi = (
            (clamp_x(bounds.2).ceil() as usize + halo).min(self.nx - 1),
            (clamp_y(bounds.3).ceil() as usize + halo).min(self.ny - 1),
        );
        (lo, hi)
    }

    fn cells_inside(&self, shape: &TileShape, halo: usize, out: &mut Vec<usize>) {
        let bb = shape.bounding_box();
        let bounds = (
            f64::from(bb.ll.x),
            f64::from(bb.ll.y),
            f64::from(bb.ur.x),
            f64::from(bb.ur.y),
        );
        let (lo, hi) = self.window(&bounds, halo + 1);
        let reach = self.h * (halo as f64 + 0.5);
        for j in lo.1..=hi.1 {
            for i in lo.0..=hi.0 {
                if shape.distance(&self.cell_center(i, j)) <= reach {
                    out.push(j * self.nx + i);
                }
            }
        }
    }
}

fn bounding(points: &[FloatPoint]) -> (f64, f64, f64, f64) {
    points.iter().fold(
        (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        |(a, b, c, d), p| (a.min(p.x), b.min(p.y), c.max(p.x), d.max(p.y)),
    )
}

fn convex_hull(mut points: Vec<FloatPoint>) -> Vec<FloatPoint> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if points.len() < 3 {
        return points;
    }
    let cross = |o: &FloatPoint, a: &FloatPoint, b: &FloatPoint| {
        (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
    };
    let mut hull: Vec<FloatPoint> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &FloatPoint>> = if pass == 0 {
            Box::new(points.iter())
        } else {
            Box::new(points.iter().rev())
        };
        for p in iter {
            while hull.len() >= start + 2
                && cross(&hull[hull.len() - 2], &hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(*p);
        }
        hull.pop();
    }
    hull
}

fn point_in_convex(hull: &[FloatPoint], p: &FloatPoint) -> bool {
    (0..hull.len()).all(|i| {
        let (a, b) = (&hull[i], &hull[(i + 1) % hull.len()]);
        (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) >= 0.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use copper_geometry::{IntBox, IntPoint};

    fn square(x: i32, y: i32, r: i32) -> TileShape {
        TileShape::Box(IntBox::new(
            IntPoint::new(x - r, y - r),
            IntPoint::new(x + r, y + r),
        ))
    }

    #[test]
    fn inclusions_move_rigidly_and_the_field_fades_between_them() {
        let bounds = IntBox::new(IntPoint::new(0, 0), IntPoint::new(1000, 1000));
        let mut field = WarpField::new(&bounds, 10.0, 100.0);
        field.add_inclusion(&[square(300, 500, 40)], (20.0, 0.0), false);
        field.add_inclusion(&[square(700, 500, 40)], (0.0, 0.0), false);
        field.solve();
        let at = |x: f64, y: f64| field.displacement(&FloatPoint::new(x, y));
        assert!(
            (at(300.0, 500.0).0 - 20.0).abs() < 1e-9,
            "moving inclusion is rigid"
        );
        assert!(at(700.0, 500.0).0.abs() < 1e-9, "pinned inclusion stays");
        let mid = at(500.0, 500.0).0;
        assert!(
            mid > 1.0 && mid < 19.0,
            "the gap between them stretches smoothly: {mid}"
        );
        assert!(
            at(380.0, 500.0).0 > at(600.0, 500.0).0,
            "monotone between them"
        );
        assert!(
            at(-90.0, 500.0).0.abs() < 1.0,
            "fades to zero at the domain border"
        );
    }
}

use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_board::{Board, FixedState, ItemId};
use copper_geometry::{IntPoint, IntVector, Point, Polyline, Vector};

use crate::frames;
use crate::oracle::MoveOracle;

/// Field cell size and how far its domain extends past the board, in millimetres (rubber's H
/// and MARGIN).
const CELL_MM: f64 = 0.15;
const MARGIN_MM: f64 = 3.0;
/// Segments longer than this are split before warping, so their chords follow the curved field.
const SPLIT_MM: f64 = 0.5;
/// Traces displaced less than this (board units) everywhere are left untouched.
const STILL: f64 = 1.0;
/// Fixed copper is bent only where it would move by more than this, in millimetres.
const FIXED_STILL_MM: f64 = 0.005;

struct LiftedTrace {
    corners: Vec<FloatPoint>,
    /// Corners shared with copper that stays put: trace contacts need identical end points.
    anchored: Vec<bool>,
    layer: usize,
    half_width: i32,
    nets: Vec<i32>,
    clearance_class: usize,
    fixed_state: FixedState,
}

impl MoveOracle {
    /// Moves the components by `by` and lets nearby routable traces flow with a harmonic warp
    /// field instead of being dragged: rigid on the moving parts, pinned at static pads, vias and
    /// keepouts, smooth in between. The DRC and the net-length caps judge the result as for
    /// `translate_shoving`. All or nothing.
    pub fn translate_warping(&self, board: &mut Board, components: &[i32], by: &IntVector) -> bool {
        if *by == IntVector::ZERO {
            return true;
        }
        let fixed_only = MoveOracle {
            routable_copper_is_soft: true,
            ..*self
        };
        let (fraction, blockers) = fixed_only.line_search_bending(board, components, by, true);
        if fraction < 1.0 {
            let names: Vec<String> = blockers
                .iter()
                .map(|(a, b)| format!("{} / {}", describe(board, *a), describe(board, *b)))
                .collect();
            tracing::debug!("warp refused: blocked at {fraction:.3} of the move by {names:?}");
            return false;
        }
        let _scope = frames::scope("warp");
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let riders = self.vias_in_moving_pads(board, components);
        let fields: Vec<WarpField> = (0..board.get_layer_count())
            .map(|layer| layer_field(board, components, &riders, by, layer, per_mm))
            .collect();
        let mut snaps = BTreeMap::new();
        for &id in components
            .iter()
            .flat_map(|c| board.get_component_items(*c))
            .chain(riders.iter().copied())
            .collect::<Vec<_>>()
            .iter()
        {
            if let Some(Point::Int(c)) = board.drill_center(id) {
                snaps.insert((c.x, c.y), IntPoint::new(c.x + by.x, c.y + by.y));
            }
        }

        let mut trial = board.clone();
        let lifted = lift_moving_traces(
            &mut trial,
            &fields,
            per_mm,
            self.fixed_copper_bends,
            &riders,
        );
        let vector = Vector::Int(*by);
        for &component in components {
            trial.components.move_component(component, &vector);
        }
        let members: Vec<ItemId> = components
            .iter()
            .flat_map(|c| trial.get_component_items(*c))
            .collect();
        for id in members.into_iter().chain(riders.iter().copied()) {
            if !matches!(trial.move_item_by(id, &vector), Ok(true)) {
                return false;
            }
        }
        for trace in lifted {
            let field = &fields[trace.layer];
            let points: Vec<Point> = trace
                .corners
                .iter()
                .zip(&trace.anchored)
                .map(|(c, anchored)| {
                    let at = (c.x.round() as i32, c.y.round() as i32);
                    if let Some(snapped) = snaps.get(&at) {
                        return Point::Int(*snapped);
                    }
                    let (dx, dy) = if *anchored {
                        (0.0, 0.0)
                    } else {
                        field.displacement(c)
                    };
                    Point::Int(IntPoint::new(
                        (c.x + dx).round() as i32,
                        (c.y + dy).round() as i32,
                    ))
                })
                .collect();
            trial.insert_trace_without_cleaning(
                Polyline::from_points(&points),
                trace.layer,
                trace.half_width,
                trace.nets,
                trace.clearance_class,
                trace.fixed_state,
            );
        }
        if !self.settle_dragged_copper(board, &mut trial) {
            tracing::debug!("warp refused: the warped copper breaks a rule or a connection");
            return false;
        }
        if !crate::shove::within_length_caps(board, &trial, components) {
            tracing::debug!("warp refused: a net grows past its length cap");
            return false;
        }
        *board = trial;
        frames::committed(board, components, by);
        true
    }
}

fn describe(board: &Board, id: ItemId) -> String {
    let Some(item) = board.get_item(id) else {
        return format!("{id:?}");
    };
    let nets: Vec<String> = item
        .net_nos()
        .iter()
        .map(|n| board.rules.nets.get(*n).map_or_else(|| n.to_string(), |net| net.name.clone()))
        .collect();
    format!(
        "{:?} {:?} nets {nets:?} component {} {:?}",
        id,
        item.kind(),
        item.component_id(),
        item.get_fixed_state()
    )
}

impl MoveOracle {
    /// Vias whose drill centre is a moving pin's: contacts need identical centres, so they ride
    /// with the part, when the oracle may move them.
    fn vias_in_moving_pads(&self, board: &Board, components: &[i32]) -> Vec<ItemId> {
        let centres: BTreeSet<(i32, i32)> = components
            .iter()
            .flat_map(|c| board.get_component_items(*c))
            .filter_map(|id| match board.drill_center(id) {
                Some(Point::Int(c)) => Some((c.x, c.y)),
                _ => None,
            })
            .collect();
        board
            .get_items()
            .filter(|item| matches!(item, Item::Via(_)))
            .filter(|item| item.is_routable() || self.fixed_copper_bends)
            .filter(|item| {
                matches!(board.drill_center(item.id()), Some(Point::Int(c)) if centres.contains(&(c.x, c.y)))
            })
            .map(Item::id)
            .collect()
    }
}

fn layer_field(
    board: &mut Board,
    components: &[i32],
    riders: &[ItemId],
    by: &IntVector,
    layer: usize,
    per_mm: f64,
) -> WarpField {
    let mut field = WarpField::new(&board.bounding_box, CELL_MM * per_mm, MARGIN_MM * per_mm);
    let moving: BTreeSet<ItemId> = components
        .iter()
        .flat_map(|c| board.get_component_items(*c))
        .collect();
    for &component in components {
        let shapes = shapes_on_layer(board, &board.get_component_items(component), layer);
        if !shapes.is_empty() {
            field.add_inclusion(&shapes, (f64::from(by.x), f64::from(by.y)), true);
        }
    }
    let shapes = shapes_on_layer(board, riders, layer);
    if !shapes.is_empty() {
        field.add_inclusion(&shapes, (f64::from(by.x), f64::from(by.y)), false);
    }
    let anchors: Vec<ItemId> = board
        .get_items()
        .filter(|item| {
            !moving.contains(&item.id())
                && !riders.contains(&item.id())
                && matches!(item, Item::Pin(_) | Item::Via(_) | Item::ObstacleArea(_))
        })
        .map(Item::id)
        .collect();
    for id in anchors {
        let shapes = shapes_on_layer(board, &[id], layer);
        if !shapes.is_empty() {
            field.add_inclusion(&shapes, (0.0, 0.0), false);
        }
    }
    field.solve();
    field
}

fn shapes_on_layer(board: &mut Board, items: &[ItemId], layer: usize) -> Vec<TileShape> {
    let mut shapes = Vec::new();
    for &id in items {
        let layers: Vec<usize> = {
            let ctx = board.ctx();
            let Some(item) = board.get_item(id) else {
                continue;
            };
            (0..item.tile_shape_count(&ctx))
                .map(|i| item.shape_layer(i, &ctx))
                .collect()
        };
        for (i, shape_layer) in layers.into_iter().enumerate() {
            if shape_layer == layer {
                if let Some(shape) = board.item_tile_shape(id, i) {
                    shapes.push(shape);
                }
            }
        }
    }
    shapes
}

/// Trace ends and via centres per layer: where another piece of copper may join a trace partway
/// along a segment. A warped trace is split there so both sides of the junction move together.
fn junction_points(board: &Board) -> std::collections::BTreeMap<usize, Vec<FloatPoint>> {
    let mut points: std::collections::BTreeMap<usize, Vec<FloatPoint>> =
        std::collections::BTreeMap::new();
    let ctx = board.ctx();
    for item in board.get_items() {
        match item {
            Item::Trace(trace) => {
                for end in [trace.first_corner(), trace.last_corner()]
                    .into_iter()
                    .flatten()
                {
                    points
                        .entry(trace.get_layer())
                        .or_default()
                        .push(end.to_float());
                }
            }
            Item::Via(_) => {
                if let Some(center) = board.drill_center(item.id()) {
                    for layer in item.first_layer(&ctx)..=item.last_layer(&ctx) {
                        points.entry(layer).or_default().push(center.to_float());
                    }
                }
            }
            _ => {}
        }
    }
    points
}

/// Where `p` lies strictly inside segment `a`-`b` (within a board unit of it), as a fraction.
fn parameter_on_segment(a: &FloatPoint, b: &FloatPoint, p: &FloatPoint) -> Option<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared < 1.0 {
        return None;
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length_squared;
    if t <= 0.0 || t >= 1.0 {
        return None;
    }
    let (qx, qy) = (a.x + dx * t, a.y + dy * t);
    ((qx - p.x).hypot(qy - p.y) <= 1.0).then_some(t)
}

/// Removes the routable traces the fields would move, returning them with their corners split
/// finely enough to follow the field.
fn lift_moving_traces(
    board: &mut Board,
    fields: &[WarpField],
    per_mm: f64,
    fixed_bends: bool,
    riders: &[ItemId],
) -> Vec<LiftedTrace> {
    let split = SPLIT_MM * per_mm;
    let junctions = junction_points(board);
    let mut lifted = Vec::new();
    let mut ids = Vec::new();
    for item in board.get_items() {
        let Item::Trace(trace) = item else {
            continue;
        };
        if !item.is_routable() && !(fixed_bends && crate::oracle::bendable(board, item.id())) {
            continue;
        }
        let layer = trace.get_layer();
        let polyline = trace.polyline();
        let corners: Vec<FloatPoint> = (0..polyline.corner_count())
            .filter_map(|i| polyline.corner(i).map(|p| p.to_float()))
            .collect();
        let mut fine = Vec::new();
        let on_layer = junctions.get(&layer).map_or(&[][..], Vec::as_slice);
        for pair in corners.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let pieces = ((a.x - b.x).hypot(a.y - b.y) / split).ceil().max(1.0) as usize;
            let mut stops: Vec<f64> = (0..pieces).map(|k| k as f64 / pieces as f64).collect();
            stops.extend(
                on_layer
                    .iter()
                    .filter_map(|p| parameter_on_segment(&a, &b, p)),
            );
            stops.sort_by(f64::total_cmp);
            stops.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
            for t in stops {
                fine.push(FloatPoint::new(
                    a.x + (b.x - a.x) * t,
                    a.y + (b.y - a.y) * t,
                ));
            }
        }
        if let Some(last) = corners.last() {
            fine.push(*last);
        }
        let still = if item.is_routable() {
            STILL
        } else {
            FIXED_STILL_MM * per_mm
        };
        let moves = fine.iter().any(|c| {
            let (dx, dy) = fields[layer].displacement(c);
            dx.hypot(dy) > still
        });
        if moves {
            ids.push(item.id());
            lifted.push(LiftedTrace {
                anchored: Vec::new(),
                corners: fine,
                layer,
                half_width: trace.get_half_width(),
                nets: item.net_nos().to_vec(),
                clearance_class: item.clearance_class(),
                fixed_state: item.get_fixed_state(),
            });
        }
    }
    // The board refuses to delete fixed items, which bending lifts too; each goes back in with
    // its fixed state.
    for id in ids {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    let staying: BTreeSet<(usize, i64, i64)> = board
        .get_items()
        .filter_map(|item| match item {
            Item::Trace(trace) => Some((
                trace.get_layer(),
                [trace.first_corner(), trace.last_corner()],
            )),
            _ => None,
        })
        .flat_map(|(layer, ends)| ends.into_iter().flatten().map(move |p| (layer, p)))
        .map(|(layer, p)| {
            let p = p.to_float();
            (layer, p.x.round() as i64, p.y.round() as i64)
        })
        .collect();
    let ctx = board.ctx();
    let vias: Vec<(usize, i64, i64)> = board
        .get_items()
        .filter(|item| matches!(item, Item::Via(_)) && !riders.contains(&item.id()))
        .filter_map(|item| {
            let c = board.drill_center(item.id())?.to_float();
            Some((item.first_layer(&ctx)..=item.last_layer(&ctx)).map(move |layer| {
                (layer, c.x.round() as i64, c.y.round() as i64)
            }))
        })
        .flatten()
        .collect();
    let staying: BTreeSet<_> = staying.into_iter().chain(vias).collect();
    for trace in &mut lifted {
        let layer = trace.layer;
        trace.anchored = trace
            .corners
            .iter()
            .map(|c| staying.contains(&(layer, c.x.round() as i64, c.y.round() as i64)))
            .collect();
    }
    lifted
}
