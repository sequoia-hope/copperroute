use std::collections::BTreeSet;

use copper_board::board::StopConnectionOption;
use copper_board::items::Item;
use copper_board::{Board, ItemId};
use copper_geometry::{IntVector, ShapeOps, Vector};

/// A component item and what it would collide with. An item paired with itself means it would
/// leave the board.
pub type Conflict = (ItemId, ItemId);

const BISECTIONS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveOracle {
    /// Extra distance kept beyond the clearance rule, in board units.
    pub guard: f64,
    /// When set, copper the router may rip up does not block: it will be rerouted anyway.
    pub routable_copper_is_soft: bool,
}

impl MoveOracle {
    pub fn conflicts(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
    ) -> BTreeSet<Conflict> {
        let members: BTreeSet<ItemId> = board
            .get_component_items(component_id)
            .into_iter()
            .collect();
        let vector = Vector::Int(*by);
        let mut found = BTreeSet::new();
        if *by != IntVector::ZERO {
            for (pin, copper) in attached_copper(board, &members) {
                let routable = board.get_item(copper).is_some_and(|c| c.is_routable());
                if !(routable && self.routable_copper_is_soft) {
                    found.insert((pin, copper));
                }
            }
        }
        for &id in &members {
            let Some((nets, clearance_class, layers)) = item_query_parameters(board, id) else {
                continue;
            };
            for (index, layer) in layers.into_iter().enumerate() {
                let Some(shape) = board.item_tile_shape(id, index) else {
                    continue;
                };
                let mut moved = shape.translate_by(&vector);
                if self.guard > 0.0 {
                    moved = moved.enlarge(self.guard);
                }
                if !moved.is_contained_in(&board.bounding_box) {
                    found.insert((id, id));
                }
                let candidates = board.overlapping_items_with_clearance(
                    &moved,
                    Some(layer),
                    &nets,
                    clearance_class,
                );
                let ctx = board.ctx();
                let Some(item) = board.get_item(id) else {
                    continue;
                };
                for other_id in candidates {
                    if members.contains(&other_id) {
                        continue;
                    }
                    let Some(other) = board.get_item(other_id) else {
                        continue;
                    };
                    if self.routable_copper_is_soft && other.is_routable() {
                        continue;
                    }
                    if other.is_obstacle(item, &ctx) {
                        found.insert((id, other_id));
                    }
                }
            }
        }
        found
    }

    /// The largest fraction of `by` (full, else by bisection) whose conflicts are all
    /// grandfathered, with the conflicts the full move would have added.
    pub fn line_search(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
    ) -> (f64, BTreeSet<Conflict>) {
        let base = self.conflicts(board, component_id, &IntVector::ZERO);
        let added = |oracle: &Self, board: &mut Board, fraction: f64| -> BTreeSet<Conflict> {
            oracle
                .conflicts(board, component_id, &scale(by, fraction))
                .difference(&base)
                .copied()
                .collect()
        };
        let blockers = added(self, board, 1.0);
        if blockers.is_empty() {
            return (1.0, blockers);
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..BISECTIONS {
            let mid = (lo + hi) / 2.0;
            if added(self, board, mid).is_empty() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo, blockers)
    }

    /// Moves every item of the component and the component itself. Routable copper attached to
    /// its pins is ripped first, so nothing is dragged into places the oracle never checked; the
    /// router reroutes it. Refuses (returns false) when fixed copper is attached.
    pub fn translate(&self, board: &mut Board, component_id: i32, by: &IntVector) -> bool {
        if *by == IntVector::ZERO {
            return true;
        }
        let vector = Vector::Int(*by);
        let members: BTreeSet<ItemId> = board
            .get_component_items(component_id)
            .into_iter()
            .collect();
        let attached = attached_copper(board, &members);
        if attached.iter().any(|(_, c)| {
            !(self.routable_copper_is_soft && board.get_item(*c).is_some_and(|c| c.is_routable()))
        }) {
            return false;
        }
        let mut ripped = BTreeSet::new();
        for (_, copper) in attached {
            ripped.extend(board.connection_items(copper, StopConnectionOption::None));
        }
        ripped.retain(|id| board.get_item(*id).is_some_and(|item| item.is_routable()));
        board.remove_items(ripped);
        for id in members {
            if !matches!(board.move_item_by(id, &vector), Ok(true)) {
                return false;
            }
        }
        board.components.move_component(component_id, &vector);
        true
    }
}

pub fn scale(by: &IntVector, fraction: f64) -> IntVector {
    IntVector::new(
        (f64::from(by.x) * fraction).round() as i32,
        (f64::from(by.y) * fraction).round() as i32,
    )
}

/// (component item, attached copper of another component or none) pairs.
fn attached_copper(board: &Board, members: &BTreeSet<ItemId>) -> Vec<(ItemId, ItemId)> {
    let mut attached = Vec::new();
    for &id in members {
        for contact in board.normal_contacts(id) {
            if members.contains(&contact) {
                continue;
            }
            if board
                .get_item(contact)
                .is_some_and(|c| c.is_trace() || matches!(c, Item::Via(_)))
            {
                attached.push((id, contact));
            }
        }
    }
    attached
}

fn item_query_parameters(board: &Board, id: ItemId) -> Option<(Vec<i32>, usize, Vec<usize>)> {
    let ctx = board.ctx();
    let item = board.get_item(id)?;
    let layers = (0..item.tile_shape_count(&ctx))
        .map(|i| item.shape_layer(i, &ctx))
        .collect();
    Some((item.net_nos().to_vec(), item.clearance_class(), layers))
}
