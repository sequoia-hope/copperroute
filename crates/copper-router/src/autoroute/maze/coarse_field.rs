//! An obstacle-aware admissible heuristic: a backward Dijkstra from the destination over a coarse
//! grid of the board, in which a cell is blocked only when one fixed obstacle covers it entirely.
//!
//! Ripup-able copper never blocks a cell, because the search may pass through it at a cost, so the
//! field stays a lower bound on the true cost-to-go. Off unless `COPPERROUTE_OBSTACLE_HEURISTIC` is
//! set; `COPPERROUTE_HEURISTIC_STATS=<file>` appends one line per search comparing the coarse
//! bound with the box distance at the start.
//!
//! A cell counts as covered only when a square one unit larger on every side is inside the shape,
//! so a centreline running exactly along a cell edge at zero clearance is never cut off.
//!
//! Why the bound is admissible: a path that needs `n` steps through the 8-connected cell graph on
//! one layer is at least `(n - 2) * cell / 2` long, since every second step moves it at least one
//! cell further from where it was. Each step is therefore charged `cell / 2` at the layer's
//! cheapest trace cost, and the two-step slack is taken back once per layer segment: from the via
//! edge and once more at the end.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::io::Write;
use std::rc::Rc;
use std::sync::LazyLock;

use copper_board::{Board, Item, TreeId};
use copper_geometry::{FloatPoint, IntBox, IntPoint, Point};

use super::control::AutorouteControl;

const MAX_CELLS_PER_SIDE: i64 = 384;
const FREE: i32 = 0;
const BLOCKED_FOR_ALL: i32 = -1;
const NO_NET: i32 = i32::MIN;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("COPPERROUTE_OBSTACLE_HEURISTIC").is_some_and(|v| v != "0"));
static STATS: LazyLock<Option<std::sync::Mutex<std::fs::File>>> = LazyLock::new(|| {
    let path = std::env::var_os("COPPERROUTE_HEURISTIC_STATS")?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(std::sync::Mutex::new)
});

thread_local! {
    static GRIDS: RefCell<BTreeMap<TreeId, Rc<ObstacleGrid>>> = const { RefCell::new(BTreeMap::new()) };
}

pub fn enabled() -> bool {
    *ENABLED
}

pub fn reset() {
    GRIDS.with(|grids| grids.borrow_mut().clear());
}

/// Per cell: `FREE`, `BLOCKED_FOR_ALL`, or a positive net number the cell is blocked for every
/// net but.
struct ObstacleGrid {
    origin_x: i64,
    origin_y: i64,
    cell: i64,
    width: usize,
    height: usize,
    layers: usize,
    cells: Vec<i32>,
}

impl ObstacleGrid {
    fn build(board: &mut Board, tree: TreeId, layers: usize) -> ObstacleGrid {
        let bounds = board.bounding_box;
        let (origin_x, origin_y) = (i64::from(bounds.ll.x), i64::from(bounds.ll.y));
        let span = (i64::from(bounds.ur.x) - origin_x).max(i64::from(bounds.ur.y) - origin_y);
        let cell = (span / MAX_CELLS_PER_SIDE + 1).max(1);
        let width = ((i64::from(bounds.ur.x) - origin_x) / cell + 1) as usize;
        let height = ((i64::from(bounds.ur.y) - origin_y) / cell + 1) as usize;
        let mut grid = ObstacleGrid {
            origin_x,
            origin_y,
            cell,
            width,
            height,
            layers,
            cells: vec![FREE; width * height * layers],
        };
        for id in board.items_in_board_order() {
            let Some(exempt_net) = board.get_item(id).and_then(blocking_exemption) else {
                continue;
            };
            for i in 0..board.item_tree_shape_count(id, tree) {
                let (Some(shape), Some(layer)) = (
                    board.item_tree_shape(id, tree, i),
                    board.item_shape_layer(id, i),
                ) else {
                    continue;
                };
                if layer < layers {
                    grid.block_cells_inside(&shape, layer, exempt_net);
                }
            }
        }
        grid
    }

    fn block_cells_inside(
        &mut self,
        shape: &copper_geometry::TileShape,
        layer: usize,
        exempt_net: i32,
    ) {
        let bounds = shape.bounding_box();
        let (x0, y0) = self.cell_of(i64::from(bounds.ll.x), i64::from(bounds.ll.y));
        let (x1, y1) = self.cell_of(i64::from(bounds.ur.x), i64::from(bounds.ur.y));
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let left = self.origin_x + cx as i64 * self.cell;
                let bottom = self.origin_y + cy as i64 * self.cell;
                let covered = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|(dx, dy)| {
                    let x = left + dx * (self.cell + 2) - 1;
                    let y = bottom + dy * (self.cell + 2) - 1;
                    i32::try_from(x).is_ok_and(|x| {
                        i32::try_from(y)
                            .is_ok_and(|y| shape.contains(&Point::Int(IntPoint::new(x, y))))
                    })
                });
                if !covered {
                    continue;
                }
                let index = self.index(layer, cx, cy);
                self.cells[index] = match self.cells[index] {
                    FREE if exempt_net != NO_NET => exempt_net,
                    current if current == exempt_net => current,
                    _ => BLOCKED_FOR_ALL,
                };
            }
        }
    }

    /// `COPPERROUTE_OBSTACLE_GRID_DUMP=<dir>` writes `tree<id>.grid`: a text header line
    /// `origin_x origin_y cell width height layers`, then one cell value per line.
    fn dump_if_requested(&self, tree: TreeId) {
        let Some(dir) = std::env::var_os("COPPERROUTE_OBSTACLE_GRID_DUMP") else {
            return;
        };
        let path = std::path::Path::new(&dir).join(format!("tree{}.grid", tree.0));
        let Ok(mut file) = std::fs::File::create(path) else {
            return;
        };
        let mut text = format!(
            "{} {} {} {} {} {}\n",
            self.origin_x, self.origin_y, self.cell, self.width, self.height, self.layers
        );
        for value in &self.cells {
            text.push_str(&value.to_string());
            text.push('\n');
        }
        let _ = file.write_all(text.as_bytes());
    }

    fn cell_of(&self, x: i64, y: i64) -> (usize, usize) {
        let cx = ((x - self.origin_x) / self.cell).clamp(0, self.width as i64 - 1);
        let cy = ((y - self.origin_y) / self.cell).clamp(0, self.height as i64 - 1);
        (cx as usize, cy as usize)
    }

    fn index(&self, layer: usize, cx: usize, cy: usize) -> usize {
        (layer * self.height + cy) * self.width + cx
    }

    fn passable(&self, index: usize, net: i32) -> bool {
        match self.cells[index] {
            FREE => true,
            BLOCKED_FOR_ALL => false,
            exempt => exempt == net,
        }
    }
}

/// `None` when the item never blocks a trace centreline for good; otherwise the one net it does
/// not block (`NO_NET` when it blocks every net).
fn blocking_exemption(item: &Item) -> Option<i32> {
    let copper_that_stays = match item {
        Item::Pin(_) | Item::ObstacleArea(_) | Item::ConductionArea(_) => true,
        Item::Trace(_) | Item::Via(_) => !item.is_routable(),
        _ => false,
    };
    if !copper_that_stays || !item.is_trace_obstacle(NO_NET) {
        return None;
    }
    match item.net_count() {
        0 => Some(NO_NET),
        1 => Some(item.get_net_number(0)),
        _ => None,
    }
}

impl std::fmt::Debug for CoarseField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoarseField")
            .field("cell", &self.grid.cell)
            .field(
                "size",
                &(self.grid.width, self.grid.height, self.grid.layers),
            )
            .finish()
    }
}

pub struct CoarseField {
    grid: Rc<ObstacleGrid>,
    cost_to_go: Vec<f64>,
    final_slack: f64,
}

impl CoarseField {
    pub fn build(
        board: &mut Board,
        tree: TreeId,
        ctrl: &AutorouteControl,
        destinations: &[(IntBox, usize)],
    ) -> Option<CoarseField> {
        if destinations.is_empty() {
            return None;
        }
        let layers = ctrl.layer_count;
        let grid = GRIDS.with(|grids| {
            grids
                .borrow_mut()
                .entry(tree)
                .or_insert_with(|| {
                    let grid = ObstacleGrid::build(board, tree, layers);
                    grid.dump_if_requested(tree);
                    Rc::new(grid)
                })
                .clone()
        });
        if grid.layers != layers {
            return None;
        }

        let half_cell = grid.cell as f64 / 2.0;
        let step_cost: Vec<f64> = (0..layers)
            .map(|layer| {
                let costs = &ctrl.trace_costs[layer];
                costs.horizontal.min(costs.vertical).max(0.0) * half_cell
            })
            .collect();
        let segment_slack = 2.0
            * (0..layers)
                .filter(|layer| ctrl.layer_active[*layer])
                .map(|layer| step_cost[layer])
                .fold(0.0, f64::max);
        let via_cost = (ctrl.min_normal_via_cost - segment_slack).max(0.0);

        let net = ctrl.net_number;
        let plane = grid.width * grid.height;
        let mut cost_to_go = vec![f64::INFINITY; plane * layers];
        let mut heap = BinaryHeap::new();
        for (bounds, layer) in destinations {
            if *layer >= layers || !ctrl.layer_active[*layer] {
                continue;
            }
            let (x0, y0) = grid.cell_of(i64::from(bounds.ll.x), i64::from(bounds.ll.y));
            let (x1, y1) = grid.cell_of(i64::from(bounds.ur.x), i64::from(bounds.ur.y));
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    let index = grid.index(*layer, cx, cy);
                    cost_to_go[index] = 0.0;
                    heap.push(Reverse((Ordered(0.0), index)));
                }
            }
        }

        while let Some(Reverse((Ordered(cost), index))) = heap.pop() {
            if cost > cost_to_go[index] {
                continue;
            }
            let layer = index / plane;
            let (cy, cx) = ((index % plane) / grid.width, index % grid.width);
            let mut relax = |next: usize, step: f64| {
                let candidate = cost + step;
                if candidate < cost_to_go[next] && grid.passable(next, net) {
                    cost_to_go[next] = candidate;
                    heap.push(Reverse((Ordered(candidate), next)));
                }
            };
            for (dx, dy) in [
                (-1i64, -1i64),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                if nx < 0 || ny < 0 || nx >= grid.width as i64 || ny >= grid.height as i64 {
                    continue;
                }
                relax(
                    grid.index(layer, nx as usize, ny as usize),
                    step_cost[layer],
                );
            }
            if ctrl.vias_allowed {
                for other in (0..layers).filter(|l| *l != layer && ctrl.layer_active[*l]) {
                    relax(grid.index(other, cx, cy), via_cost);
                }
            }
        }

        Some(CoarseField {
            grid,
            cost_to_go,
            final_slack: segment_slack,
        })
    }

    /// `None` when the coarse graph cannot reach the destination from here, which happens only
    /// where cells are partially blocked in ways the grid cannot express; callers fall back.
    pub fn lower_bound(&self, point: &FloatPoint, layer: usize) -> Option<f64> {
        if layer >= self.grid.layers {
            return None;
        }
        let (cx, cy) = self.grid.cell_of(point.x as i64, point.y as i64);
        let cost = self.cost_to_go[self.grid.index(layer, cx, cy)];
        cost.is_finite().then(|| (cost - self.final_slack).max(0.0))
    }
}

pub fn record_start(
    box_distance: f64,
    coarse: Option<f64>,
    net: i32,
    point: &FloatPoint,
    layer: usize,
) {
    let Some(stats) = STATS.as_ref() else {
        return;
    };
    if let Ok(mut file) = stats.lock() {
        let coarse = coarse.map_or_else(|| "unreachable".to_string(), |c| format!("{c:.0}"));
        let _ = writeln!(
            file,
            "{box_distance:.0} {coarse} {net} {:.0} {:.0} {layer}",
            point.x, point.y
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Ordered(f64);

impl Eq for Ordered {}

impl PartialOrd for Ordered {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ordered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}
