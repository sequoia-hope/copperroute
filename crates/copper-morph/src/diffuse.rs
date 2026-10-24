//! Spreading crowded parts by diffusion: placement migration after Ren, Pan, Alpert and
//! Villarrubia (DAC 2005). Each side's demand (part footprints, the routing demand of pins, and
//! whatever demand the caller adds) is binned, and the density spreads as heat does, walled in by
//! the outline. Parts ride the flow, velocity −∇d/d over their footprint, so they leave crowded
//! bins in the order they sat; every round rebins from where the parts have got to. Bins below
//! the target density are raised to it, so only overfull regions push anything.

use std::collections::{BTreeMap, BTreeSet};

use copper_board::Board;
use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_geometry::{FloatPoint, IntVector};

use crate::frames;
use crate::oracle::{MoveOracle, scale};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffuseOptions {
    pub cell_mm: f64,
    /// Spreading stops once no bin is fuller than this (demand area over bin area).
    pub target_density: f64,
    /// Routing demand each pin adds to both sides, in mm².
    pub pin_demand_mm2: f64,
    pub rounds: usize,
    /// Diffusion steps between rebinnings.
    pub steps_per_round: usize,
    /// The diffusion time step in bins²; the explicit scheme is stable up to 0.25.
    pub dt: f64,
    pub max_move_mm: f64,
}

impl Default for DiffuseOptions {
    fn default() -> Self {
        DiffuseOptions {
            cell_mm: 1.0,
            target_density: 0.8,
            pin_demand_mm2: 0.25,
            rounds: 40,
            steps_per_round: 5,
            dt: 0.2,
            max_move_mm: 2.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiffuseReport {
    /// Each moving part's displacement, in board units.
    pub moves: BTreeMap<i32, (f64, f64)>,
    pub peak_before: f64,
    pub peak_after: f64,
    pub rounds: usize,
}

struct Part {
    id: i32,
    front: bool,
    /// x0, y0, x1, y1 where the part starts.
    footprint: [f64; 4],
    pins: Vec<FloatPoint>,
}

struct Grid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    inside: Vec<bool>,
}

impl Grid {
    fn index(&self, i: usize, j: usize) -> usize {
        j * self.nx + i
    }

    fn bin_of(&self, p: &FloatPoint) -> Option<usize> {
        let i = ((p.x - self.x0) / self.cell).floor();
        let j = ((p.y - self.y0) / self.cell).floor();
        (i >= 0.0 && j >= 0.0 && (i as usize) < self.nx && (j as usize) < self.ny)
            .then(|| self.index(i as usize, j as usize))
    }

    fn add_box(&self, density: &mut [f64], b: [f64; 4]) {
        let area = self.cell * self.cell;
        let i0 = ((b[0] - self.x0) / self.cell).floor().max(0.0) as usize;
        let j0 = ((b[1] - self.y0) / self.cell).floor().max(0.0) as usize;
        let i1 = (((b[2] - self.x0) / self.cell).floor() as usize).min(self.nx - 1);
        let j1 = (((b[3] - self.y0) / self.cell).floor() as usize).min(self.ny - 1);
        for j in j0..=j1 {
            for i in i0..=i1 {
                let (bx, by) = (self.x0 + i as f64 * self.cell, self.y0 + j as f64 * self.cell);
                let w = (b[2].min(bx + self.cell) - b[0].max(bx)).max(0.0);
                let h = (b[3].min(by + self.cell) - b[1].max(by)).max(0.0);
                density[self.index(i, j)] += w * h / area;
            }
        }
    }

    /// The walls, outside bins and the grid's edge, pass no flux.
    fn diffuse(&self, d: &[f64], dt: f64) -> Vec<f64> {
        let mut out = d.to_vec();
        for j in 0..self.ny {
            for i in 0..self.nx {
                let k = self.index(i, j);
                if !self.inside[k] {
                    continue;
                }
                let mut flux = 0.0;
                for (di, dj) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                    if let Some(n) = self.neighbour(i, j, di, dj) {
                        flux += d[n] - d[k];
                    }
                }
                out[k] = d[k] + dt * flux;
            }
        }
        out
    }

    fn neighbour(&self, i: usize, j: usize, di: i64, dj: i64) -> Option<usize> {
        let (ni, nj) = (i as i64 + di, j as i64 + dj);
        if ni < 0 || nj < 0 || ni >= self.nx as i64 || nj >= self.ny as i64 {
            return None;
        }
        let n = self.index(ni as usize, nj as usize);
        self.inside[n].then_some(n)
    }

    /// In bins per unit time.
    fn velocity(&self, d: &[f64], p: &FloatPoint) -> (f64, f64) {
        let Some(k) = self.bin_of(p) else {
            return (0.0, 0.0);
        };
        if !self.inside[k] || d[k] <= 0.0 {
            return (0.0, 0.0);
        }
        let (i, j) = (k % self.nx, k / self.nx);
        let slope = |minus: Option<usize>, plus: Option<usize>| match (minus, plus) {
            (Some(a), Some(b)) => (d[b] - d[a]) / 2.0,
            (None, Some(b)) => d[b] - d[k],
            (Some(a), None) => d[k] - d[a],
            (None, None) => 0.0,
        };
        let gx = slope(self.neighbour(i, j, -1, 0), self.neighbour(i, j, 1, 0));
        let gy = slope(self.neighbour(i, j, 0, -1), self.neighbour(i, j, 0, 1));
        (-gx / d[k], -gy / d[k])
    }
}

/// How each part should move to bring every bin down to the target density. Parts in one of
/// `groups` move together; parts in `locked`, position-fixed parts and any group holding one stay,
/// though their demand still pushes. `demand` adds routing demand to both sides: points and
/// areas, in board units.
pub fn diffusion_moves(
    board: &Board,
    options: &DiffuseOptions,
    groups: &[Vec<i32>],
    locked: &BTreeSet<i32>,
    demand: &[(FloatPoint, f64)],
) -> DiffuseReport {
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let parts = parts(board);
    let Some(grid) = grid(board, options.cell_mm * per_mm) else {
        return DiffuseReport::default();
    };
    let held: BTreeSet<i32> = board
        .components
        .get_all()
        .filter(|c| c.position_fixed || locked.contains(&c.id))
        .map(|c| c.id)
        .collect();
    let index: BTreeMap<i32, usize> = parts.iter().enumerate().map(|(k, p)| (p.id, k)).collect();
    let mut bodies: Vec<Vec<usize>> = Vec::new();
    let mut grouped = BTreeSet::new();
    for group in groups {
        let members: Vec<usize> = group.iter().filter_map(|id| index.get(id).copied()).collect();
        grouped.extend(members.iter().copied());
        if !members.is_empty() {
            bodies.push(members);
        }
    }
    bodies.extend((0..parts.len()).filter(|k| !grouped.contains(k)).map(|k| vec![k]));
    bodies.retain(|body| !body.iter().any(|&k| held.contains(&parts[k].id)));

    let pin_demand = options.pin_demand_mm2 * per_mm * per_mm / (grid.cell * grid.cell);
    let extra: Vec<(usize, f64)> =
        demand.iter().filter_map(|(p, a)| grid.bin_of(p).map(|k| (k, a / (grid.cell * grid.cell)))).collect();
    let mut offset = vec![(0.0, 0.0); parts.len()];
    let bin = |offset: &[(f64, f64)]| -> [Vec<f64>; 2] {
        let mut sides = [vec![0.0; grid.inside.len()], vec![0.0; grid.inside.len()]];
        for (part, (dx, dy)) in parts.iter().zip(offset) {
            let f = part.footprint;
            grid.add_box(&mut sides[usize::from(!part.front)], [f[0] + dx, f[1] + dy, f[2] + dx, f[3] + dy]);
            for pin in &part.pins {
                if let Some(k) = grid.bin_of(&FloatPoint::new(pin.x + dx, pin.y + dy)) {
                    sides[0][k] += pin_demand;
                    sides[1][k] += pin_demand;
                }
            }
        }
        for &(k, a) in &extra {
            sides[0][k] += a;
            sides[1][k] += a;
        }
        sides
    };
    let peak = |sides: &[Vec<f64>; 2]| {
        sides
            .iter()
            .flat_map(|d| d.iter().zip(&grid.inside).filter(|(_, inside)| **inside).map(|(v, _)| *v))
            .fold(0.0, f64::max)
    };
    let mut report = DiffuseReport { peak_before: peak(&bin(&offset)), ..DiffuseReport::default() };
    let limit = options.max_move_mm * per_mm;
    for round in 0..options.rounds {
        let mut fields = bin(&offset);
        if peak(&fields) <= options.target_density {
            break;
        }
        report.rounds = round + 1;
        for field in &mut fields {
            for v in field.iter_mut() {
                *v = v.max(options.target_density);
            }
        }
        for _ in 0..options.steps_per_round {
            for body in &bodies {
                let mut v = (0.0, 0.0);
                let mut samples = 0.0;
                for &k in body {
                    let (part, (dx, dy)) = (&parts[k], offset[k]);
                    let f = part.footprint;
                    let (cx, cy) = ((f[0] + f[2]) / 2.0 + dx, (f[1] + f[3]) / 2.0 + dy);
                    for (x, y) in [(cx, cy), (f[0] + dx, f[1] + dy), (f[2] + dx, f[1] + dy), (f[0] + dx, f[3] + dy), (f[2] + dx, f[3] + dy)] {
                        let w = grid.velocity(&fields[usize::from(!part.front)], &FloatPoint::new(x, y));
                        v = (v.0 + w.0, v.1 + w.1);
                        samples += 1.0;
                    }
                }
                let step = (v.0 / samples * options.dt * grid.cell, v.1 / samples * options.dt * grid.cell);
                for &k in body {
                    let next = (offset[k].0 + step.0, offset[k].1 + step.1);
                    let len = next.0.hypot(next.1);
                    offset[k] = if len > limit { (next.0 * limit / len, next.1 * limit / len) } else { next };
                }
            }
            for field in &mut fields {
                *field = grid.diffuse(field, options.dt);
            }
        }
    }
    report.peak_after = peak(&bin(&offset));
    report.moves = parts
        .iter()
        .zip(&offset)
        .filter(|(_, (dx, dy))| dx.hypot(*dy) >= 1.0)
        .map(|(p, o)| (p.id, *o))
        .collect();
    report
}

/// Makes as much of each move as the oracle admits: all of it, else the largest clean fraction,
/// bending fixed copper when the oracle allows that. Parts in one of `groups` move together. Moves
/// that were cut short are tried again for what is left, `sweeps` times, since a neighbour may
/// have moved out of the way. Returns each part's displacement as made.
pub fn apply_moves(
    board: &mut Board,
    oracle: &MoveOracle,
    moves: &BTreeMap<i32, (f64, f64)>,
    groups: &[Vec<i32>],
    sweeps: usize,
) -> BTreeMap<i32, IntVector> {
    let _scope = frames::scope("diffuse");
    let mut bodies: Vec<Vec<i32>> = groups
        .iter()
        .map(|g| g.iter().copied().filter(|id| moves.contains_key(id)).collect::<Vec<_>>())
        .filter(|g| !g.is_empty())
        .collect();
    let grouped: BTreeSet<i32> = bodies.iter().flatten().copied().collect();
    bodies.extend(moves.keys().filter(|id| !grouped.contains(id)).map(|&id| vec![id]));
    let want: Vec<IntVector> = bodies
        .iter()
        .map(|body| {
            let (x, y) = moves[&body[0]];
            IntVector::new(x.round() as i32, y.round() as i32)
        })
        .collect();
    let mut made = vec![IntVector::ZERO; bodies.len()];
    for _ in 0..sweeps.max(1) {
        let mut progressed = false;
        for (k, body) in bodies.iter().enumerate() {
            let left = IntVector::new(want[k].x - made[k].x, want[k].y - made[k].y);
            if left == IntVector::ZERO {
                continue;
            }
            if oracle.fixed_copper_bends && oracle.translate_warping(board, body, &left) {
                made[k] = want[k];
                progressed = true;
                continue;
            }
            let (fraction, _) = oracle.line_search_group(board, body, &left);
            let step = scale(&left, fraction);
            if step != IntVector::ZERO && oracle.translate_group(board, body, &step) {
                made[k] = IntVector::new(made[k].x + step.x, made[k].y + step.y);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    bodies
        .iter()
        .zip(made)
        .filter(|(_, m)| *m != IntVector::ZERO)
        .flat_map(|(body, m)| body.iter().map(move |&id| (id, m)))
        .collect()
}

/// Routing demand along each open connection's straight line, as a wire `width_mm` wide, in
/// samples `sample_mm` apart.
pub fn airline_demand(board: &Board, width_mm: f64, sample_mm: f64) -> Vec<(FloatPoint, f64)> {
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let spacing = sample_mm * per_mm;
    let area = width_mm * per_mm * spacing;
    let mut out = Vec::new();
    for line in copper_drc::all_airlines(board) {
        let (a, b) = (line.from_corner, line.to_corner);
        let n = ((a.x - b.x).hypot(a.y - b.y) / spacing).ceil().max(1.0) as usize;
        for k in 0..=n {
            let t = k as f64 / n as f64;
            out.push((FloatPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t), area));
        }
    }
    out
}

fn parts(board: &Board) -> Vec<Part> {
    let ctx = board.ctx();
    let mut out = Vec::new();
    for component in board.components.get_all().filter(|c| c.is_placed()) {
        let mut footprint: Option<[f64; 4]> = None;
        let mut pins = Vec::new();
        for id in board.get_component_items(component.id) {
            let Some(item) = board.get_item(id) else {
                continue;
            };
            if !matches!(item, Item::Pin(_) | Item::ComponentOutline(_)) {
                continue;
            }
            if matches!(item, Item::Pin(_))
                && let Some(c) = board.drill_center(id)
            {
                pins.push(c.to_float());
            }
            let b = item.bounding_box(&ctx);
            let b = [f64::from(b.ll.x), f64::from(b.ll.y), f64::from(b.ur.x), f64::from(b.ur.y)];
            footprint = Some(footprint.map_or(b, |f| [f[0].min(b[0]), f[1].min(b[1]), f[2].max(b[2]), f[3].max(b[3])]));
        }
        if let Some(footprint) = footprint {
            out.push(Part { id: component.id, front: component.placed_on_front(), footprint, pins });
        }
    }
    out
}

fn grid(board: &Board, cell: f64) -> Option<Grid> {
    let b = board.bounding_box;
    if b.ur.x <= b.ll.x || b.ur.y <= b.ll.y || cell <= 0.0 {
        return None;
    }
    let (x0, y0) = (f64::from(b.ll.x), f64::from(b.ll.y));
    let nx = ((f64::from(b.ur.x) - x0) / cell).ceil().max(1.0) as usize;
    let ny = ((f64::from(b.ur.y) - y0) / cell).ceil().max(1.0) as usize;
    let outlines: Vec<_> = board
        .get_items()
        .filter_map(|item| match item {
            Item::BoardOutline(outline) => Some(outline),
            _ => None,
        })
        .flat_map(|outline| (0..outline.shape_count()).filter_map(|k| outline.get_shape(k)))
        .collect();
    let mut inside = vec![true; nx * ny];
    if !outlines.is_empty() {
        for j in 0..ny {
            for i in 0..nx {
                let c = FloatPoint::new(x0 + (i as f64 + 0.5) * cell, y0 + (j as f64 + 0.5) * cell);
                inside[j * nx + i] = outlines.iter().any(|shape| shape.contains_float(&c));
            }
        }
    }
    Some(Grid { x0, y0, cell, nx, ny, inside })
}
