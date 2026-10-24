use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use copper_board::Board;
use copper_geometry::FloatPoint;

use crate::autoroute::maze::{AutorouteEngine, MazeListElement};

/// One room the maze search has just reached, for showing the search as it runs.
pub struct MazeProbe<'a> {
    pub net: i32,
    pub layer: Option<usize>,
    pub room: Vec<FloatPoint>,
    pub door: (FloatPoint, FloatPoint),
    pub queue: usize,
    pub board: &'a Board,
}

pub trait MazeWatcher: Send + Sync {
    /// Asked before every step; the probe is built only when this says yes.
    fn wants_probe(&self) -> bool;
    fn probe(&self, probe: MazeProbe<'_>);
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static WATCHER: RwLock<Option<Arc<dyn MazeWatcher>>> = RwLock::new(None);

pub fn watch_maze(watcher: Arc<dyn MazeWatcher>) -> MazeWatchGuard {
    *WATCHER.write().unwrap_or_else(|e| e.into_inner()) = Some(watcher);
    ACTIVE.store(true, Ordering::Release);
    MazeWatchGuard(())
}

pub struct MazeWatchGuard(());

impl Drop for MazeWatchGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
        *WATCHER.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(crate) fn maze_step(board: &Board, engine: &AutorouteEngine, element: &MazeListElement, queue: usize) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let watcher = WATCHER.read().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(watcher) = watcher else { return };
    if !watcher.wants_probe() {
        return;
    }
    let room = element
        .next_room
        .and_then(|room| engine.rooms.room_shape(room))
        .map(|shape| shape.corner_approx_arr())
        .unwrap_or_default();
    watcher.probe(MazeProbe {
        net: engine.get_net_number(),
        layer: element.next_room.and_then(|room| engine.rooms.room_layer(board, room)),
        room,
        door: (element.shape_entry.a, element.shape_entry.b),
        queue,
        board,
    });
}
