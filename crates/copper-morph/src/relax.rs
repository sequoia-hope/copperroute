//! Pulling copper apart after a warp: every trace corner not holding a connection is pushed away
//! from other nets' copper that is closer than the clearance, a little at a time, so squeezed
//! bundles spread into free space nearby. Pins and vias stay where they are. Copper that crosses
//! other copper is left for rip-up: no push can tell which side it belongs on.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use copper_board::items::Item;
use copper_board::structure::{FixedState, Unit};
use copper_board::{Board, ItemId};
use copper_geometry::{FloatPoint, IntPoint, Point, Polyline};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RelaxReport {
    pub traces_moved: usize,
    pub overlaps_before: usize,
    pub overlaps_after: usize,
    /// The net pairs found too close before relaxing, the edge as net -1, with the smallest
    /// distance between them.
    pub close_pairs: BTreeMap<NetPair, f64>,
}

pub type NetPair = (Vec<i32>, Vec<i32>);

fn net_pair(a: &[i32], b: &[i32]) -> NetPair {
    if a <= b { (a.to_vec(), b.to_vec()) } else { (b.to_vec(), a.to_vec()) }
}

const CELL_MM: f64 = 1.0;
const MAX_STEP_MM: f64 = 0.05;
const SMOOTHING: f64 = 0.1;
const SLACK_MM: f64 = 0.005;

struct Wire {
    id: ItemId,
    layer: usize,
    half: f64,
    nets: Vec<i32>,
    class: usize,
    fixed: FixedState,
    points: Vec<FloatPoint>,
    movable: Vec<bool>,
    /// Corners shared with other traces move as one.
    joint: Vec<Option<usize>>,
}

struct Blob {
    layer: usize,
    nets: Vec<i32>,
    class: usize,
    polygon: Vec<FloatPoint>,
    centre: FloatPoint,
}

/// Relaxes for `iterations` rounds. `outline`, when given, pushes copper back inside it by the
/// edge clearance. Net pairs in `tolerated` may come as close as the distance given and no
/// closer: pushing copper off a pin it was always necked down onto only shoves it into its other
/// neighbours.
pub fn relax_clearance(
    board: &mut Board,
    iterations: usize,
    outline: Option<&[FloatPoint]>,
    tolerated: &BTreeMap<NetPair, f64>,
) -> RelaxReport {
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let cell = CELL_MM * per_mm;
    let max_step = MAX_STEP_MM * per_mm;
    let slack = SLACK_MM * per_mm;
    let (mut wires, blobs, joints) = collect(board);
    let edge_class = board.get_items().find_map(|item| match item {
        Item::BoardOutline(_) => Some(item.clearance_class()),
        _ => None,
    });
    let start: Vec<Vec<FloatPoint>> = wires.iter().map(|w| w.points.clone()).collect();

    let mut blob_cells: HashMap<(i64, i64, usize), Vec<usize>> = HashMap::new();
    for (b, blob) in blobs.iter().enumerate() {
        for key in cells_of(&blob.polygon, 0.0, cell, blob.layer) {
            blob_cells.entry(key).or_default().push(b);
        }
    }

    let mut report = RelaxReport::default();
    for round in 0..=iterations {
        let mut segment_cells: HashMap<(i64, i64, usize), Vec<(usize, usize)>> = HashMap::new();
        for (w, wire) in wires.iter().enumerate() {
            for s in 0..wire.points.len().saturating_sub(1) {
                let ends = [wire.points[s], wire.points[s + 1]];
                for key in cells_of(&ends, wire.half, cell, wire.layer) {
                    segment_cells.entry(key).or_default().push((w, s));
                }
            }
        }
        let mut push: Vec<Vec<(f64, f64)>> = wires.iter().map(|w| vec![(0.0, 0.0); w.points.len()]).collect();
        let mut overlaps = 0;
        for (w, wire) in wires.iter().enumerate() {
            for s in 0..wire.points.len().saturating_sub(1) {
                let (a0, a1) = (wire.points[s], wire.points[s + 1]);
                let mut near: Vec<(usize, usize)> = cells_of(&[a0, a1], wire.half, cell, wire.layer)
                    .filter_map(|key| segment_cells.get(&key))
                    .flatten()
                    .copied()
                    .filter(|&(v, t)| (v, t) > (w, s) && v != w)
                    .collect();
                near.sort_unstable();
                near.dedup();
                for (v, t) in near {
                    let other = &wires[v];
                    if other.nets.iter().any(|n| wire.nets.contains(n)) {
                        continue;
                    }
                    let need = wire.half
                        + other.half
                        + f64::from(board.clearance_value(wire.class, other.class, wire.layer))
                        + slack;
                    let (b0, b1) = (other.points[t], other.points[t + 1]);
                    let (d, ta, tb, pa, pb) = closest(a0, a1, b0, b1);
                    if d >= need {
                        continue;
                    }
                    let key = net_pair(&wire.nets, &other.nets);
                    if round == 0 {
                        note(&mut report.close_pairs, &key, d);
                    }
                    let need = tolerated.get(&key).map_or(need, |floor| need.min(*floor));
                    if d >= need {
                        continue;
                    }
                    overlaps += 1;
                    if d <= 0.0 {
                        continue;
                    }
                    let n = ((pa.x - pb.x) / d, (pa.y - pb.y) / d);
                    let share = (need - d) / 2.0;
                    add(&mut push[w], s, ta, n, share);
                    add(&mut push[v], t, tb, (-n.0, -n.1), share);
                }
                let blobs_near: BTreeSet<usize> = cells_of(&[a0, a1], wire.half, cell, wire.layer)
                    .filter_map(|key| blob_cells.get(&key))
                    .flatten()
                    .copied()
                    .collect();
                for b in blobs_near {
                    let blob = &blobs[b];
                    if blob.nets.iter().any(|n| wire.nets.contains(n)) {
                        continue;
                    }
                    let need = wire.half
                        + f64::from(board.clearance_value(wire.class, blob.class, wire.layer))
                        + slack;
                    let (d, ta, pa, pb) = to_polygon(a0, a1, &blob.polygon);
                    if d >= need {
                        continue;
                    }
                    let key = net_pair(&wire.nets, &blob.nets);
                    if round == 0 {
                        note(&mut report.close_pairs, &key, d);
                    }
                    let need = tolerated.get(&key).map_or(need, |floor| need.min(*floor));
                    if d >= need {
                        continue;
                    }
                    overlaps += 1;
                    let (n, depth) = if d > 0.0 {
                        (((pa.x - pb.x) / d, (pa.y - pb.y) / d), need - d)
                    } else {
                        let (dx, dy) = (pa.x - blob.centre.x, pa.y - blob.centre.y);
                        let len = dx.hypot(dy).max(1.0);
                        ((dx / len, dy / len), need)
                    };
                    add(&mut push[w], s, ta, n, depth);
                }
            }
        }
        if let (Some(outline), Some(edge_class)) = (outline, edge_class) {
            for (w, wire) in wires.iter().enumerate() {
                let need = wire.half + f64::from(board.clearance_value(wire.class, edge_class, wire.layer)) + slack;
                for (i, p) in wire.points.iter().enumerate() {
                    let q = nearest_on_polygon(outline, p);
                    let d = (p.x - q.x).hypot(p.y - q.y);
                    let inside = inside(outline, p);
                    if inside && d >= need {
                        continue;
                    }
                    let key = net_pair(&wire.nets, &[-1]);
                    let reach = if inside { d } else { -d };
                    if round == 0 {
                        note(&mut report.close_pairs, &key, reach);
                    }
                    if tolerated.get(&key).is_some_and(|floor| reach >= *floor) {
                        continue;
                    }
                    overlaps += 1;
                    if d <= 0.0 {
                        continue;
                    }
                    let sign = if inside { 1.0 } else { -1.0 };
                    let n = (sign * (p.x - q.x) / d, sign * (p.y - q.y) / d);
                    let depth = if inside { need - d } else { need + d };
                    push[w][i].0 += n.0 * depth;
                    push[w][i].1 += n.1 * depth;
                }
            }
        }
        if round == 0 {
            report.overlaps_before = overlaps;
        }
        report.overlaps_after = overlaps;
        if round == iterations || overlaps == 0 {
            break;
        }
        let mut joint_push = vec![(0.0, 0.0); joints];
        for (wire, push) in wires.iter().zip(&push) {
            for (joint, p) in wire.joint.iter().zip(push) {
                if let Some(j) = joint {
                    joint_push[*j].0 += p.0;
                    joint_push[*j].1 += p.1;
                }
            }
        }
        let cap = |(dx, dy): (f64, f64)| {
            let len = dx.hypot(dy);
            if len > max_step { (dx * max_step / len, dy * max_step / len) } else { (dx, dy) }
        };
        for (wire, push) in wires.iter_mut().zip(&push) {
            let before = wire.points.clone();
            for i in 0..wire.points.len() {
                if !wire.movable[i] {
                    continue;
                }
                let (dx, dy) = match wire.joint[i] {
                    Some(j) => cap(joint_push[j]),
                    None => {
                        let (dx, dy) = push[i];
                        if dx == 0.0 && dy == 0.0 {
                            continue;
                        }
                        let mid = FloatPoint::new((before[i - 1].x + before[i + 1].x) / 2.0, (before[i - 1].y + before[i + 1].y) / 2.0);
                        cap((dx + SMOOTHING * (mid.x - before[i].x), dy + SMOOTHING * (mid.y - before[i].y)))
                    }
                };
                wire.points[i] = FloatPoint::new(before[i].x + dx, before[i].y + dy);
            }
        }
    }

    for (wire, start) in wires.into_iter().zip(start) {
        let moved = wire
            .points
            .iter()
            .zip(&start)
            .any(|(p, q)| (p.x - q.x).hypot(p.y - q.y) >= 0.5);
        if !moved {
            continue;
        }
        let mut points: Vec<Point> = Vec::with_capacity(wire.points.len());
        for p in &wire.points {
            let q = Point::Int(IntPoint::new(p.x.round() as i32, p.y.round() as i32));
            if points.last() != Some(&q) {
                points.push(q);
            }
        }
        if points.len() < 2 {
            continue;
        }
        if let Some(item) = board.get_item_mut(wire.id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(wire.id);
        board.insert_trace_without_cleaning(
            Polyline::from_points(&points),
            wire.layer,
            wire.half as i32,
            wire.nets,
            wire.class,
            wire.fixed,
        );
        report.traces_moved += 1;
    }
    report
}

/// Drops trace corners that sit within `tolerance` (board units) of the line through their
/// neighbours, keeping every corner that holds a connection. A warped trace keeps a corner every
/// half millimetre, which makes the router's trace insertion and pull-tight crawl.
pub fn simplify_traces(board: &mut Board, tolerance: f64) -> usize {
    let (wires, _, _) = collect(board);
    let mut simplified = 0;
    for wire in wires {
        let mut keep = vec![false; wire.points.len()];
        for (i, movable) in wire.movable.iter().enumerate() {
            keep[i] = !movable || wire.joint[i].is_some();
        }
        let anchors: Vec<usize> = (0..keep.len()).filter(|i| keep[*i]).collect();
        for pair in anchors.windows(2) {
            douglas_peucker(&wire.points, pair[0], pair[1], tolerance, &mut keep);
        }
        if keep.iter().all(|k| *k) {
            continue;
        }
        let points: Vec<Point> = wire
            .points
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(p, _)| Point::Int(IntPoint::new(p.x.round() as i32, p.y.round() as i32)))
            .collect();
        if let Some(item) = board.get_item_mut(wire.id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(wire.id);
        board.insert_trace_without_cleaning(
            Polyline::from_points(&points),
            wire.layer,
            wire.half as i32,
            wire.nets,
            wire.class,
            wire.fixed,
        );
        simplified += 1;
    }
    simplified
}

fn douglas_peucker(points: &[FloatPoint], first: usize, last: usize, tolerance: f64, keep: &mut [bool]) {
    let (a, b) = (points[first], points[last]);
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    let mut worst = (0.0, first);
    for (i, p) in points.iter().enumerate().take(last).skip(first + 1) {
        let d = if len == 0.0 {
            (p.x - a.x).hypot(p.y - a.y)
        } else {
            ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
        };
        if d > worst.0 {
            worst = (d, i);
        }
    }
    if worst.0 > tolerance {
        keep[worst.1] = true;
        douglas_peucker(points, first, worst.1, tolerance, keep);
        douglas_peucker(points, worst.1, last, tolerance, keep);
    }
}

fn collect(board: &Board) -> (Vec<Wire>, Vec<Blob>, usize) {
    let ctx = board.ctx();
    let mut wires = Vec::new();
    let mut blobs = Vec::new();
    let mut held: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut ends: HashMap<(i32, i32, usize), usize> = HashMap::new();
    for item in board.get_items() {
        match item {
            Item::Trace(trace) => {
                let polyline = trace.polyline();
                let points: Vec<FloatPoint> = (0..polyline.corner_count())
                    .filter_map(|i| polyline.corner(i).map(|p| p.to_float()))
                    .collect();
                for end in [points.first(), points.last()].into_iter().flatten() {
                    let key = (end.x.round() as i32, end.y.round() as i32, trace.get_layer());
                    let next = ends.len();
                    ends.entry(key).or_insert(next);
                }
                wires.push(Wire {
                    id: item.id(),
                    layer: trace.get_layer(),
                    half: f64::from(trace.get_half_width()),
                    nets: item.net_nos().to_vec(),
                    class: item.clearance_class(),
                    fixed: item.get_fixed_state(),
                    movable: Vec::new(),
                    joint: Vec::new(),
                    points,
                });
            }
            _ if item.is_drill_item() => {
                if let Some(Point::Int(c)) = board.drill_center(item.id()) {
                    held.insert((c.x, c.y));
                }
                let first = item.first_layer(&ctx);
                for i in 0..item.tile_shape_count(&ctx) {
                    let Some(shape) = board.item_tile_shape_ref(item.id(), i) else {
                        continue;
                    };
                    let polygon = shape.corner_approx_arr();
                    if polygon.is_empty() {
                        continue;
                    }
                    let n = polygon.len() as f64;
                    let centre = FloatPoint::new(
                        polygon.iter().map(|p| p.x).sum::<f64>() / n,
                        polygon.iter().map(|p| p.y).sum::<f64>() / n,
                    );
                    blobs.push(Blob {
                        layer: first + i,
                        nets: item.net_nos().to_vec(),
                        class: item.clearance_class(),
                        polygon,
                        centre,
                    });
                }
            }
            _ => {}
        }
    }
    for wire in &mut wires {
        let keys: Vec<(i32, i32)> = wire.points.iter().map(|p| (p.x.round() as i32, p.y.round() as i32)).collect();
        wire.movable = keys.iter().map(|k| !held.contains(k)).collect();
        wire.joint = keys.iter().map(|&(x, y)| ends.get(&(x, y, wire.layer)).copied()).collect();
    }
    (wires, blobs, ends.len())
}

fn note(pairs: &mut BTreeMap<NetPair, f64>, key: &NetPair, distance: f64) {
    let entry = pairs.entry(key.clone()).or_insert(distance);
    *entry = entry.min(distance);
}

fn add(push: &mut [(f64, f64)], segment: usize, t: f64, n: (f64, f64), amount: f64) {
    for (i, weight) in [(segment, 1.0 - t), (segment + 1, t)] {
        push[i].0 += n.0 * amount * weight;
        push[i].1 += n.1 * amount * weight;
    }
}

fn cells_of(points: &[FloatPoint], pad: f64, cell: f64, layer: usize) -> impl Iterator<Item = (i64, i64, usize)> {
    let x0 = points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min) - pad - cell;
    let y0 = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min) - pad - cell;
    let x1 = points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max) + pad + cell;
    let y1 = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max) + pad + cell;
    let (i0, i1) = ((x0 / cell).floor() as i64, (x1 / cell).floor() as i64);
    let (j0, j1) = ((y0 / cell).floor() as i64, (y1 / cell).floor() as i64);
    (i0..=i1).flat_map(move |i| (j0..=j1).map(move |j| (i, j, layer)))
}

/// Distance between segments ab and cd, the parameters of the closest points on each, and the
/// points themselves.
fn closest(a: FloatPoint, b: FloatPoint, c: FloatPoint, d: FloatPoint) -> (f64, f64, f64, FloatPoint, FloatPoint) {
    let (ux, uy) = (b.x - a.x, b.y - a.y);
    let (vx, vy) = (d.x - c.x, d.y - c.y);
    let (wx, wy) = (a.x - c.x, a.y - c.y);
    let (aa, bb, cc) = (ux * ux + uy * uy, ux * vx + uy * vy, vx * vx + vy * vy);
    let (dd, ee) = (ux * wx + uy * wy, vx * wx + vy * wy);
    let denominator = aa * cc - bb * bb;
    let mut s = if denominator > 1e-12 { ((bb * ee - cc * dd) / denominator).clamp(0.0, 1.0) } else { 0.0 };
    let mut t = if cc > 1e-12 { ((bb * s + ee) / cc).clamp(0.0, 1.0) } else { 0.0 };
    if aa > 1e-12 {
        s = ((bb * t - dd) / aa).clamp(0.0, 1.0);
    }
    if cc > 1e-12 {
        t = ((bb * s + ee) / cc).clamp(0.0, 1.0);
    }
    let p = FloatPoint::new(a.x + ux * s, a.y + uy * s);
    let q = FloatPoint::new(c.x + vx * t, c.y + vy * t);
    ((p.x - q.x).hypot(p.y - q.y), s, t, p, q)
}

/// Distance from segment ab to a polygon's border (zero when it reaches inside), the parameter on
/// ab of the closest point, and the closest points.
fn to_polygon(a: FloatPoint, b: FloatPoint, polygon: &[FloatPoint]) -> (f64, f64, FloatPoint, FloatPoint) {
    let mut best = (f64::INFINITY, 0.0, a, a);
    for (i, p) in polygon.iter().enumerate() {
        let q = polygon[(i + 1) % polygon.len()];
        let (d, s, _, pa, pb) = closest(a, b, *p, q);
        if d < best.0 {
            best = (d, s, pa, pb);
        }
    }
    let mid = FloatPoint::new(a.x + (b.x - a.x) * best.1, a.y + (b.y - a.y) * best.1);
    if polygon.len() >= 3 && (inside(polygon, &a) || inside(polygon, &b) || inside(polygon, &mid)) {
        best.0 = 0.0;
    }
    best
}

pub(crate) fn inside(polygon: &[FloatPoint], p: &FloatPoint) -> bool {
    let mut inside = false;
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            inside = !inside;
        }
    }
    inside
}

pub(crate) fn nearest_on_polygon(polygon: &[FloatPoint], p: &FloatPoint) -> FloatPoint {
    let mut best = (f64::INFINITY, *p);
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        let (ex, ey) = (b.x - a.x, b.y - a.y);
        let len = ex * ex + ey * ey;
        let t = if len == 0.0 { 0.0 } else { (((p.x - a.x) * ex + (p.y - a.y) * ey) / len).clamp(0.0, 1.0) };
        let q = FloatPoint::new(a.x + t * ex, a.y + t * ey);
        let d = (q.x - p.x).hypot(q.y - p.y);
        if d < best.0 {
            best = (d, q);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> FloatPoint {
        FloatPoint::new(x, y)
    }

    #[test]
    fn closest_points_of_parallel_and_skew_segments() {
        let (d, s, t, _, _) = closest(p(0.0, 0.0), p(10.0, 0.0), p(4.0, 3.0), p(6.0, 3.0));
        assert!((d - 3.0).abs() < 1e-9);
        assert!((0.4..=0.6).contains(&s) && (0.0..=1.0).contains(&t));

        let (d, s, t, a, b) = closest(p(0.0, 0.0), p(10.0, 0.0), p(12.0, -1.0), p(12.0, 5.0));
        assert!((d - 2.0).abs() < 1e-9);
        assert_eq!((s, a.x, b.x), (1.0, 10.0, 12.0));
        assert!((t - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_segment_reaching_into_a_polygon_is_at_distance_zero() {
        let square = [p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(0.0, 4.0)];
        assert_eq!(to_polygon(p(2.0, 2.0), p(9.0, 2.0), &square).0, 0.0);
        let (d, _, _, on) = to_polygon(p(6.0, -1.0), p(6.0, 5.0), &square);
        assert!((d - 2.0).abs() < 1e-9);
        assert_eq!(on.x, 4.0);
    }
}
