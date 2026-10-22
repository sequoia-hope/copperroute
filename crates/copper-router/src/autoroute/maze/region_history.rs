//! Negotiated-congestion history over space: every ripup marks the coarse cells under the ripped
//! copper, and moving through a marked cell costs more for every net afterwards. Nets that have
//! another way around drift away from contested regions and leave them to the nets that need them.
//!
//! Off unless `COPPERROUTE_REGION_HISTORY` is a positive factor; a movement's cost is multiplied
//! by `1 + factor * ripups_recorded_in_the_cell_at_its_midpoint`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use copper_geometry::{FloatPoint, IntBox};

const CELLS_ON_LONG_SIDE: i64 = 128;

static FACTOR: LazyLock<f64> = LazyLock::new(|| {
    std::env::var("COPPERROUTE_REGION_HISTORY")
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|factor| factor.is_finite() && *factor > 0.0)
        .unwrap_or(0.0)
});

struct History {
    origin_x: i64,
    origin_y: i64,
    cell: i64,
    width: usize,
    height: usize,
    layers: BTreeMap<usize, Vec<u32>>,
}

thread_local! {
    static HISTORY: RefCell<Option<History>> = const { RefCell::new(None) };
}

pub fn enabled() -> bool {
    *FACTOR > 0.0
}

pub fn reset(board_box: &IntBox) {
    if !enabled() {
        return;
    }
    let (origin_x, origin_y) = (i64::from(board_box.ll.x), i64::from(board_box.ll.y));
    let span_x = (i64::from(board_box.ur.x) - origin_x).max(1);
    let span_y = (i64::from(board_box.ur.y) - origin_y).max(1);
    let cell = (span_x.max(span_y) / CELLS_ON_LONG_SIDE + 1).max(1);
    HISTORY.with(|history| {
        *history.borrow_mut() = Some(History {
            origin_x,
            origin_y,
            cell,
            width: (span_x / cell + 1) as usize,
            height: (span_y / cell + 1) as usize,
            layers: BTreeMap::new(),
        });
    });
}

pub fn record(footprint: &IntBox, first_layer: usize, last_layer: usize) {
    if !enabled() {
        return;
    }
    HISTORY.with(|history| {
        let mut history = history.borrow_mut();
        let Some(history) = history.as_mut() else {
            return;
        };
        let (x0, y0) = history.cell_of(i64::from(footprint.ll.x), i64::from(footprint.ll.y));
        let (x1, y1) = history.cell_of(i64::from(footprint.ur.x), i64::from(footprint.ur.y));
        let size = history.width * history.height;
        let width = history.width;
        for layer in first_layer..=last_layer {
            let cells = history.layers.entry(layer).or_insert_with(|| vec![0; size]);
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    cells[cy * width + cx] += 1;
                }
            }
        }
    });
}

pub fn cost_multiplier(point: &FloatPoint, layer: usize) -> f64 {
    if !enabled() {
        return 1.0;
    }
    HISTORY.with(|history| {
        let history = history.borrow();
        let Some(history) = history.as_ref() else {
            return 1.0;
        };
        let Some(cells) = history.layers.get(&layer) else {
            return 1.0;
        };
        let (cx, cy) = history.cell_of(point.x as i64, point.y as i64);
        1.0 + *FACTOR * f64::from(cells[cy * history.width + cx])
    })
}

impl History {
    fn cell_of(&self, x: i64, y: i64) -> (usize, usize) {
        let cx = ((x - self.origin_x) / self.cell).clamp(0, self.width as i64 - 1);
        let cy = ((y - self.origin_y) / self.cell).clamp(0, self.height as i64 - 1);
        (cx as usize, cy as usize)
    }
}
