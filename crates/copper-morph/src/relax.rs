//! Pulling copper apart after a warp, and pulling it taut: every trace corner not holding a connection is pushed away
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
    pub vias_moved: usize,
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
const SLACK_MM: f64 = 0.01;
/// Under tension, contact rounds per pull, so the clearances win.
const CONTACT_PASSES: usize = 3;
/// Contact-only rounds that end a pull.
const SETTLE_ROUNDS: usize = 30;

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
    /// The free via this shape belongs to, which moves it.
    via: Option<usize>,
    layer: usize,
    nets: Vec<i32>,
    class: usize,
    polygon: Vec<FloatPoint>,
    centre: FloatPoint,
}

struct ViaNode {
    id: ItemId,
    joint: usize,
}

/// Relaxes for `iterations` rounds. With `tension`, every trace is also a rubber band: each round
/// it first pulls taut (each segment draws its ends together by that fraction of its length) and
/// then the clearances push it back, so it ends hugging what it wraps around. `outline`, when given, pushes copper back inside it by the
/// edge clearance. Net pairs in `tolerated` may come as close as the distance given and no
/// closer: pushing copper off a pin it was always necked down onto only shoves it into its other
/// neighbours. With `free_vias`, vias move too, as nodes joining the traces that meet them.
pub fn relax_clearance(
    board: &mut Board,
    iterations: usize,
    outline: Option<&[FloatPoint]>,
    tolerated: &BTreeMap<NetPair, f64>,
    tension: f64,
    free_vias: bool,
) -> RelaxReport {
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let cell = CELL_MM * per_mm;
    let max_step = MAX_STEP_MM * per_mm;
    let slack = SLACK_MM * per_mm;
    let (mut wires, blobs, joints, vias) = collect(board, free_vias);
    let mut offset = vec![(0.0, 0.0); vias.len()];
    let edge_class = board.get_items().find_map(|item| match item {
        Item::BoardOutline(_) => Some(item.clearance_class()),
        _ => None,
    });
    let start: Vec<Vec<FloatPoint>> = wires.iter().map(|w| w.points.clone()).collect();

    let mut report = RelaxReport::default();
    let pulling = if tension > 0.0 { iterations * CONTACT_PASSES } else { 0 };
    let total = if tension > 0.0 { pulling + SETTLE_ROUNDS } else { iterations };
    for round in 0..=total {
        if tension > 0.0 && round > 0 && round <= pulling && round % CONTACT_PASSES == 0 {
            let mut pull: Vec<Vec<(f64, f64)>> = wires.iter().map(|w| vec![(0.0, 0.0); w.points.len()]).collect();
            for (wire, pull) in wires.iter().zip(&mut pull) {
                for s in 0..wire.points.len().saturating_sub(1) {
                    let (a, b) = (wire.points[s], wire.points[s + 1]);
                    let (dx, dy) = (tension * (b.x - a.x), tension * (b.y - a.y));
                    pull[s].0 += dx;
                    pull[s].1 += dy;
                    pull[s + 1].0 -= dx;
                    pull[s + 1].1 -= dy;
                }
            }
            let moved = apply(&mut wires, &pull, None, joints, max_step, 0.0);
            follow(&vias, &mut offset, &moved);
        }
        let polygons: Vec<Vec<FloatPoint>> = blobs
            .iter()
            .map(|blob| match blob.via {
                Some(v) => blob.polygon.iter().map(|p| FloatPoint::new(p.x + offset[v].0, p.y + offset[v].1)).collect(),
                None => Vec::new(),
            })
            .collect();
        let polygon = |b: usize| -> &[FloatPoint] {
            if blobs[b].via.is_some() { &polygons[b] } else { &blobs[b].polygon }
        };
        let mut blob_cells: HashMap<(i64, i64, usize), Vec<usize>> = HashMap::new();
        for (b, blob) in blobs.iter().enumerate() {
            for key in cells_of(polygon(b), 0.0, cell, blob.layer) {
                blob_cells.entry(key).or_default().push(b);
            }
        }
        let mut via_push = vec![(0.0, 0.0); joints];
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
                    let (d, ta, pa, pb) = to_polygon(a0, a1, polygon(b));
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
                    let centre = blob.via.map_or(blob.centre, |v| FloatPoint::new(blob.centre.x + offset[v].0, blob.centre.y + offset[v].1));
                    let (n, depth) = if d > 0.0 {
                        (((pa.x - pb.x) / d, (pa.y - pb.y) / d), need - d)
                    } else {
                        let (dx, dy) = (pa.x - centre.x, pa.y - centre.y);
                        let len = dx.hypot(dy).max(1.0);
                        ((dx / len, dy / len), need)
                    };
                    match blob.via {
                        Some(v) => {
                            add(&mut push[w], s, ta, n, depth / 2.0);
                            via_push[vias[v].joint].0 -= n.0 * depth / 2.0;
                            via_push[vias[v].joint].1 -= n.1 * depth / 2.0;
                        }
                        None => add(&mut push[w], s, ta, n, depth),
                    }
                }
            }
        }
        for (b, blob) in blobs.iter().enumerate() {
            let Some(v) = blob.via else {
                continue;
            };
            let mine = polygon(b);
            let near: BTreeSet<usize> = cells_of(mine, 0.0, cell, blob.layer)
                .filter_map(|key| blob_cells.get(&key))
                .flatten()
                .copied()
                .filter(|&o| o != b && !blobs[o].nets.iter().any(|n| blob.nets.contains(n)))
                .collect();
            for o in near {
                let other = &blobs[o];
                let need = f64::from(board.clearance_value(blob.class, other.class, blob.layer)) + slack;
                let theirs = polygon(o);
                let mut best = (f64::INFINITY, mine[0], mine[0]);
                for (i, a) in mine.iter().enumerate() {
                    let (d, _, pa, pb) = to_polygon(*a, mine[(i + 1) % mine.len()], theirs);
                    if d < best.0 {
                        best = (d, pa, pb);
                    }
                }
                let (d, pa, pb) = best;
                if d >= need {
                    continue;
                }
                let key = net_pair(&blob.nets, &other.nets);
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
                    let (dx, dy) = (blob.centre.x + offset[v].0 - other.centre.x, blob.centre.y + offset[v].1 - other.centre.y);
                    let len = dx.hypot(dy).max(1.0);
                    ((dx / len, dy / len), need)
                };
                let share = if other.via.is_some() { 0.5 } else { 1.0 };
                via_push[vias[v].joint].0 += n.0 * depth * share;
                via_push[vias[v].joint].1 += n.1 * depth * share;
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
        if round == total || (overlaps == 0 && round >= pulling) {
            break;
        }
        let moved = apply(&mut wires, &push, Some(&via_push), joints, max_step, SMOOTHING);
        follow(&vias, &mut offset, &moved);
    }

    write_back(board, wires, &start, &vias, &offset, &mut report);
    report
}

/// Moves each movable corner by its push (shared joints by the sum over their corners and
/// `extra`), at most `max_step`; pushed corners off a joint also smooth toward their neighbours'
/// midpoint. Returns how far each joint moved.
fn apply(
    wires: &mut [Wire],
    push: &[Vec<(f64, f64)>],
    extra: Option<&[(f64, f64)]>,
    joints: usize,
    max_step: f64,
    smoothing: f64,
) -> Vec<(f64, f64)> {
    let mut joint_push = extra.map_or_else(|| vec![(0.0, 0.0); joints], <[_]>::to_vec);
    for (wire, push) in wires.iter().zip(push) {
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
    let moved: Vec<(f64, f64)> = joint_push.iter().map(|p| cap(*p)).collect();
    for (wire, push) in wires.iter_mut().zip(push) {
        let before = wire.points.clone();
        for i in 0..wire.points.len() {
            if !wire.movable[i] {
                continue;
            }
            let (dx, dy) = match wire.joint[i] {
                Some(j) => moved[j],
                None => {
                    let (dx, dy) = push[i];
                    if dx == 0.0 && dy == 0.0 {
                        continue;
                    }
                    let mid = FloatPoint::new((before[i - 1].x + before[i + 1].x) / 2.0, (before[i - 1].y + before[i + 1].y) / 2.0);
                    cap((dx + smoothing * (mid.x - before[i].x), dy + smoothing * (mid.y - before[i].y)))
                }
            };
            wire.points[i] = FloatPoint::new(before[i].x + dx, before[i].y + dy);
        }
    }
    moved
}

fn follow(vias: &[ViaNode], offset: &mut [(f64, f64)], moved: &[(f64, f64)]) {
    for (via, offset) in vias.iter().zip(offset) {
        offset.0 += moved[via.joint].0;
        offset.1 += moved[via.joint].1;
    }
}

/// Puts moved copper back. Traces come out before their vias move, since moving a via with
/// traces on it would draw new copper from its old centre to its new one.
fn write_back(
    board: &mut Board,
    wires: Vec<Wire>,
    start: &[Vec<FloatPoint>],
    vias: &[ViaNode],
    offset: &[(f64, f64)],
    report: &mut RelaxReport,
) {
    let shifts: Vec<(i32, i32)> = offset.iter().map(|(dx, dy)| (dx.round() as i32, dy.round() as i32)).collect();
    let moving: BTreeSet<usize> = vias.iter().zip(&shifts).filter(|(_, by)| **by != (0, 0)).map(|(v, _)| v.joint).collect();
    let mut lifted = Vec::new();
    for (wire, start) in wires.into_iter().zip(start) {
        let moved = wire.points.iter().zip(start).any(|(p, q)| (p.x - q.x).hypot(p.y - q.y) >= 0.5)
            || wire.joint.iter().flatten().any(|j| moving.contains(j));
        if !moved {
            continue;
        }
        if let Some(item) = board.get_item_mut(wire.id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(wire.id);
        lifted.push(wire);
    }
    let mut centres: HashMap<usize, FloatPoint> = HashMap::new();
    for (via, by) in vias.iter().zip(&shifts) {
        if *by == (0, 0) {
            continue;
        }
        let Some(Point::Int(old)) = board.drill_center(via.id) else {
            continue;
        };
        let vector = copper_geometry::Vector::Int(copper_geometry::IntVector::new(by.0, by.1));
        let centre = if matches!(board.move_item_by(via.id, &vector), Ok(true)) {
            report.vias_moved += 1;
            FloatPoint::new(f64::from(old.x + by.0), f64::from(old.y + by.1))
        } else {
            old.to_float()
        };
        centres.insert(via.joint, centre);
    }
    for mut wire in lifted {
        for (p, joint) in wire.points.iter_mut().zip(&wire.joint) {
            if let Some(centre) = joint.and_then(|j| centres.get(&j)) {
                *p = *centre;
            }
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
}

/// Splits trace segments longer than `max_len` (board units), so a band has corners to bend at.
pub fn subdivide_traces(board: &mut Board, max_len: f64) -> usize {
    let (wires, _, _, _) = collect(board, false);
    let mut split = 0;
    for wire in wires {
        let mut points: Vec<Point> = Vec::new();
        for pair in wire.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let pieces = ((a.x - b.x).hypot(a.y - b.y) / max_len).ceil().max(1.0) as usize;
            for k in 0..pieces {
                let t = k as f64 / pieces as f64;
                let q = Point::Int(IntPoint::new((a.x + (b.x - a.x) * t).round() as i32, (a.y + (b.y - a.y) * t).round() as i32));
                if points.last() != Some(&q) {
                    points.push(q);
                }
            }
        }
        if let Some(last) = wire.points.last() {
            points.push(Point::Int(IntPoint::new(last.x.round() as i32, last.y.round() as i32)));
        }
        if points.len() <= wire.points.len() {
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
        split += 1;
    }
    split
}

/// Drops trace corners that sit within `tolerance` (board units) of the line through their
/// neighbours, keeping every corner that holds a connection. A warped trace keeps a corner every
/// half millimetre, which makes the router's trace insertion and pull-tight crawl.
pub fn simplify_traces(board: &mut Board, tolerance: f64) -> usize {
    let (wires, _, _, _) = collect(board, false);
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

fn collect(board: &Board, free_vias: bool) -> (Vec<Wire>, Vec<Blob>, usize, Vec<ViaNode>) {
    let ctx = board.ctx();
    let mut wires = Vec::new();
    let mut blobs = Vec::new();
    let mut held: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut ends: HashMap<(i32, i32, usize), usize> = HashMap::new();
    let mut vias = Vec::new();
    let mut joints = 0;
    for item in board.get_items() {
        if !(free_vias && matches!(item, Item::Via(_)) && !item.is_user_fixed()) {
            continue;
        }
        let Some(Point::Int(c)) = board.drill_center(item.id()) else {
            continue;
        };
        let joint = joints;
        joints += 1;
        for layer in item.first_layer(&ctx)..=item.last_layer(&ctx) {
            ends.insert((c.x, c.y, layer), joint);
        }
        vias.push(ViaNode { id: item.id(), joint });
    }
    let free: HashMap<ItemId, usize> = vias.iter().enumerate().map(|(v, via)| (via.id, v)).collect();
    for item in board.get_items() {
        match item {
            Item::Trace(trace) => {
                let polyline = trace.polyline();
                let points: Vec<FloatPoint> = (0..polyline.corner_count())
                    .filter_map(|i| polyline.corner(i).map(|p| p.to_float()))
                    .collect();
                for end in [points.first(), points.last()].into_iter().flatten() {
                    let key = (end.x.round() as i32, end.y.round() as i32, trace.get_layer());
                    if !ends.contains_key(&key) {
                        ends.insert(key, joints);
                        joints += 1;
                    }
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
                let via = free.get(&item.id()).copied();
                if let (None, Some(Point::Int(c))) = (via, board.drill_center(item.id())) {
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
                        via,
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
    let mut meeting: HashMap<usize, usize> = HashMap::new();
    for wire in &mut wires {
        let keys: Vec<(i32, i32)> = wire.points.iter().map(|p| (p.x.round() as i32, p.y.round() as i32)).collect();
        wire.joint = keys.iter().map(|&(x, y)| ends.get(&(x, y, wire.layer)).copied()).collect();
        for j in wire.joint.iter().flatten() {
            *meeting.entry(*j).or_insert(0) += 1;
        }
        wire.movable = keys.iter().map(|k| !held.contains(k)).collect();
    }
    // An end that meets no other trace and no free via ends on a pad or a pour, whatever its
    // centre: it must stay put.
    let via_joints: BTreeSet<usize> = vias.iter().map(|v| v.joint).collect();
    for wire in &mut wires {
        let last = wire.points.len() - 1;
        for i in [0, last] {
            if let Some(j) = wire.joint[i]
                && meeting[&j] < 2
                && !via_joints.contains(&j)
            {
                wire.movable[i] = false;
            }
        }
    }
    (wires, blobs, joints, vias)
}

fn note(pairs: &mut BTreeMap<NetPair, f64>, key: &NetPair, distance: f64) {
    let entry = pairs.entry(key.clone()).or_insert(distance);
    *entry = entry.min(distance);
}

/// Spreads a push at parameter `t` of a segment over its ends, scaled so the point at `t` itself
/// moves the full `amount`.
fn add(push: &mut [(f64, f64)], segment: usize, t: f64, n: (f64, f64), amount: f64) {
    let amount = amount / ((1.0 - t).powi(2) + t.powi(2));
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
