use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use super::Board;

/// Told of every change to any board, clones and trial boards included, on the thread that made
/// it. It decides for itself how often to look.
pub trait BoardWatcher: Send + Sync {
    fn changed(&self, board: &Board);
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static WATCHER: RwLock<Option<Arc<dyn BoardWatcher>>> = RwLock::new(None);

/// Watches every board until the guard drops. A second watcher replaces the first.
pub fn watch(watcher: Arc<dyn BoardWatcher>) -> WatchGuard {
    *WATCHER.write().unwrap_or_else(|e| e.into_inner()) = Some(watcher);
    ACTIVE.store(true, Ordering::Release);
    WatchGuard(())
}

pub struct WatchGuard(());

impl Drop for WatchGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
        *WATCHER.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[inline]
pub(crate) fn notify(board: &Board) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let watcher = WATCHER.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(watcher) = watcher {
        watcher.changed(board);
    }
}
