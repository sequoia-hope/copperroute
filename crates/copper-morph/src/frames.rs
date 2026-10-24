//! Opt-in capture of every move committed to a board. Operators report only what they keep, so
//! trial boards that are thrown away are never shown.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use copper_board::Board;
use copper_geometry::IntVector;

#[derive(Debug, Clone, PartialEq)]
pub struct MoveFrame {
    pub operators: Vec<&'static str>,
    pub components: Vec<i32>,
    pub by: IntVector,
}

type Observer = Box<dyn FnMut(&Board, &MoveFrame)>;

thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
    static SCOPES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static PAUSED: RefCell<usize> = const { RefCell::new(0) };
}

#[derive(Debug)]
pub struct ObserverGuard(PhantomData<Rc<()>>);

impl Drop for ObserverGuard {
    fn drop(&mut self) {
        OBSERVER.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Shows every committed move on this thread to `observer` until the guard drops.
pub fn observe(observer: impl FnMut(&Board, &MoveFrame) + 'static) -> ObserverGuard {
    OBSERVER.with(|slot| *slot.borrow_mut() = Some(Box::new(observer)));
    ObserverGuard(PhantomData)
}

#[derive(Debug)]
pub struct ScopeGuard {
    pause: bool,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if self.pause {
            PAUSED.with(|p| *p.borrow_mut() -= 1);
        } else {
            SCOPES.with(|s| s.borrow_mut().pop());
        }
    }
}

/// Names the operator responsible for moves committed until the guard drops.
pub fn scope(operator: &'static str) -> ScopeGuard {
    SCOPES.with(|s| s.borrow_mut().push(operator));
    ScopeGuard {
        pause: false,
        _not_send: PhantomData,
    }
}

/// Hides moves until the guard drops: for work on a trial board that may be thrown away.
pub fn pause() -> ScopeGuard {
    PAUSED.with(|p| *p.borrow_mut() += 1);
    ScopeGuard {
        pause: true,
        _not_send: PhantomData,
    }
}

/// Reports a move kept on `board`, made by the operators in scope.
pub fn committed(board: &Board, components: &[i32], by: &IntVector) {
    if PAUSED.with(|p| *p.borrow() > 0) {
        return;
    }
    OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow_mut().as_mut() {
            let frame = MoveFrame {
                operators: SCOPES.with(|s| s.borrow().clone()),
                components: components.to_vec(),
                by: *by,
            };
            observer(board, &frame);
        }
    });
}
