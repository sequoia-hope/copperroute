use std::collections::{BTreeMap, BTreeSet};

use copper_board::board::StopConnectionOption;
use copper_board::items::Item;
use copper_board::{Board, DrcConstraints, ItemId};
use copper_geometry::{IntVector, ShapeOps, TileShape, Vector};

/// A component item and what it would collide with. An item paired with itself means it would
/// leave the board.
pub type Conflict = (ItemId, ItemId);

const BISECTIONS: usize = 6;
/// Margin given to a pair that can never be satisfied: leaving the board, or dragging fixed
/// copper.
const HOPELESS: f64 = -1.0e9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveOracle {
    /// Extra distance kept beyond the clearance rule, in board units.
    pub guard: f64,
    /// When set, copper the router may rip up does not block: it will be rerouted anyway.
    pub routable_copper_is_soft: bool,
}

impl MoveOracle {
    /// Pairs closer than the referee allows if the component moved by `by`.
    pub fn conflicts(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
    ) -> BTreeSet<Conflict> {
        self.shortfalls(board, &[component_id], by)
            .into_keys()
            .collect()
    }

    /// Each pair short of its clearance, with its margin (gap minus required, negative).
    ///
    /// The required clearance is the stricter of the router's clearance matrix and the DRC's
    /// rule, which judges a pad by its net class as KiCad does; the two differ on boards whose
    /// pins carry a smaller item clearance class than their nets.
    fn shortfalls(
        &self,
        board: &mut Board,
        components: &[i32],
        by: &IntVector,
    ) -> BTreeMap<Conflict, f64> {
        let constraints = copper_drc::constraints::resolve(board);
        let reach = f64::from(
            constraints
                .netclass_clearance
                .values()
                .copied()
                .chain(constraints.min_clearance)
                .max()
                .unwrap_or(0),
        ) + self.guard;
        let tolerance = f64::from(constraints.epsilon);
        let members = group_items(board, components);
        let vector = Vector::Int(*by);
        let mut found: BTreeMap<Conflict, f64> = BTreeMap::new();
        let mut note = |pair: Conflict, margin: f64| {
            let entry = found.entry(pair).or_insert(margin);
            *entry = entry.min(margin);
        };
        if *by != IntVector::ZERO {
            for (pin, copper) in attached_copper(board, &members) {
                let routable = board.get_item(copper).is_some_and(Item::is_routable);
                if !(routable && self.routable_copper_is_soft) {
                    note((pin, copper), HOPELESS);
                }
            }
        }
        for &id in &members {
            let Some((nets, clearance_class, layers)) = item_query_parameters(board, id) else {
                continue;
            };
            for (index, layer) in layers.into_iter().enumerate() {
                let Some(moved) = board
                    .item_tile_shape(id, index)
                    .map(|s| s.translate_by(&vector))
                else {
                    continue;
                };
                if !moved
                    .enlarge(self.guard)
                    .is_contained_in(&board.bounding_box)
                {
                    note((id, id), HOPELESS);
                }
                let candidates = board.overlapping_items_with_clearance(
                    &moved.enlarge(reach),
                    Some(layer),
                    &nets,
                    clearance_class,
                );
                for other_id in candidates {
                    if members.contains(&other_id) {
                        continue;
                    }
                    let Some(required) =
                        self.required_clearance(board, &constraints, id, other_id, layer)
                    else {
                        continue;
                    };
                    let gap = shape_gap(board, &moved, other_id, layer, required);
                    if gap < required - tolerance {
                        note((id, other_id), gap - required);
                    }
                }
            }
        }
        found
    }

    /// `None` when `other` does not constrain `id` at all.
    fn required_clearance(
        &self,
        board: &Board,
        constraints: &DrcConstraints,
        id: ItemId,
        other_id: ItemId,
        layer: usize,
    ) -> Option<f64> {
        let item = board.get_item(id)?;
        let other = board.get_item(other_id)?;
        if self.routable_copper_is_soft && other.is_routable() {
            return None;
        }
        if !other.is_obstacle(item, &board.ctx()) {
            return None;
        }
        let matrix = board.rules.clearance_matrix.get_value(
            item.clearance_class(),
            other.clearance_class(),
            layer,
            false,
        );
        let drc =
            copper_drc::constraints::pair_clearance(board, constraints, item, other).unwrap_or(0);
        Some(f64::from(matrix.max(drc)) + self.guard)
    }

    /// The largest fraction of `by` (full, else by bisection) that creates no shortfall and lets
    /// no grandfathered pair get closer (the settle engine's non-worsening rule), with the pairs
    /// that fail at the smallest failing fraction found: those are what stops the move, while the
    /// full move may have jumped clean past them.
    pub fn line_search(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
    ) -> (f64, BTreeSet<Conflict>) {
        self.line_search_group(board, &[component_id], by)
    }

    /// `line_search` for parts moving together: pairs inside the group never conflict.
    pub fn line_search_group(
        &self,
        board: &mut Board,
        components: &[i32],
        by: &IntVector,
    ) -> (f64, BTreeSet<Conflict>) {
        let base = self.shortfalls(board, components, &IntVector::ZERO);
        let failing = |oracle: &Self, board: &mut Board, fraction: f64| -> BTreeSet<Conflict> {
            oracle
                .shortfalls(board, components, &scale(by, fraction))
                .into_iter()
                .filter(|(pair, margin)| base.get(pair).is_none_or(|before| *margin < before - 1.0))
                .map(|(pair, _)| pair)
                .collect()
        };
        let mut blockers = failing(self, board, 1.0);
        if blockers.is_empty() {
            return (1.0, blockers);
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..BISECTIONS {
            let mid = (lo + hi) / 2.0;
            let at_mid = failing(self, board, mid);
            if at_mid.is_empty() {
                lo = mid;
            } else {
                hi = mid;
                blockers = at_mid;
            }
        }
        (lo, blockers)
    }

    /// Moves every item of the component and the component itself. Routable copper attached to
    /// its pins is ripped first, so nothing is dragged into places the oracle never checked; the
    /// router reroutes it. Refuses (returns false) when fixed copper is attached.
    pub fn translate(&self, board: &mut Board, component_id: i32, by: &IntVector) -> bool {
        self.translate_group(board, &[component_id], by)
    }

    pub fn translate_group(&self, board: &mut Board, components: &[i32], by: &IntVector) -> bool {
        if *by == IntVector::ZERO {
            return true;
        }
        let vector = Vector::Int(*by);
        let members = group_items(board, components);
        let attached = attached_copper(board, &members);
        if attached.iter().any(|(_, c)| {
            !(self.routable_copper_is_soft && board.get_item(*c).is_some_and(Item::is_routable))
        }) {
            return false;
        }
        let mut ripped = BTreeSet::new();
        for (_, copper) in attached {
            ripped.extend(board.connection_items(copper, StopConnectionOption::None));
        }
        ripped.retain(|id| board.get_item(*id).is_some_and(Item::is_routable));
        board.remove_items(ripped);
        // Pin shapes derive from the component's location, so it must move before the items are
        // re-entered into the search trees.
        for &component_id in components {
            board.components.move_component(component_id, &vector);
        }
        for id in members {
            if !matches!(board.move_item_by(id, &vector), Ok(true)) {
                return false;
            }
        }
        true
    }

    /// Moves the component as far toward `by` as admissible, first pushing aside other free
    /// components that block it (each by its own line search, up to `depth` levels deep): the
    /// settle engine's pressure, where a blocked mover deposits demand on its blockers. Returns
    /// every move made, the requested component's last.
    pub fn push(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
        depth: usize,
    ) -> Vec<(i32, IntVector)> {
        let mut moves = Vec::new();
        self.push_into(
            board,
            component_id,
            by,
            depth,
            &mut BTreeSet::from([component_id]),
            &mut moves,
        );
        moves
    }

    fn push_into(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
        depth: usize,
        busy: &mut BTreeSet<i32>,
        moves: &mut Vec<(i32, IntVector)>,
    ) {
        let (fraction, blockers) = self.line_search(board, component_id, by);
        if fraction < 1.0 && depth > 0 {
            let mut pushed_any = false;
            for blocker in blocking_components(board, &blockers) {
                if busy.insert(blocker) {
                    let before = moves.len();
                    self.push_into(board, blocker, by, depth - 1, busy, moves);
                    pushed_any |= moves.len() > before;
                }
            }
            if pushed_any {
                let (retry, _) = self.line_search(board, component_id, by);
                self.commit(board, component_id, &scale(by, retry), moves);
                return;
            }
        }
        self.commit(board, component_id, &scale(by, fraction), moves);
    }

    fn commit(
        &self,
        board: &mut Board,
        component_id: i32,
        step: &IntVector,
        moves: &mut Vec<(i32, IntVector)>,
    ) {
        if *step != IntVector::ZERO && self.translate(board, component_id, step) {
            moves.push((component_id, *step));
        }
    }
}

fn group_items(board: &Board, components: &[i32]) -> BTreeSet<ItemId> {
    components
        .iter()
        .flat_map(|c| board.get_component_items(*c))
        .collect()
}

impl MoveOracle {
    /// The settle engine's group step: the part plus the free parts that wedge it (transitively,
    /// up to `max_members`) move together as far as admissible. Returns the group and the step.
    pub fn group_step(
        &self,
        board: &mut Board,
        component_id: i32,
        by: &IntVector,
        max_members: usize,
    ) -> (Vec<i32>, IntVector) {
        let mut group = vec![component_id];
        let mut frontier = vec![component_id];
        while let Some(current) = frontier.pop() {
            let (fraction, blockers) = self.line_search_group(board, &[current], by);
            if fraction >= 1.0 {
                continue;
            }
            for blocker in blocking_components(board, &blockers) {
                if group.len() < max_members && !group.contains(&blocker) {
                    group.push(blocker);
                    frontier.push(blocker);
                }
            }
        }
        let (fraction, _) = self.line_search_group(board, &group, by);
        let step = scale(by, fraction);
        if step != IntVector::ZERO && self.translate_group(board, &group, &step) {
            (group, step)
        } else {
            (group, IntVector::ZERO)
        }
    }
}

/// The gap between `moved` and `other`'s shapes on `layer`, as the DRC measures it, up to `cap`.
fn shape_gap(board: &mut Board, moved: &TileShape, other: ItemId, layer: usize, cap: f64) -> f64 {
    let Some((_, _, layers)) = item_query_parameters(board, other) else {
        return cap;
    };
    let mut gap = cap;
    for (j, other_layer) in layers.into_iter().enumerate() {
        if other_layer != layer {
            continue;
        }
        if let Some(shape) = board.item_tile_shape(other, j) {
            gap = gap.min(Board::calculate_clearance_between_two_shapes(
                moved, &shape, cap, 0, 0,
            ));
        }
    }
    gap
}

/// Free components owning the obstacles in `blockers`.
fn blocking_components(board: &Board, blockers: &BTreeSet<Conflict>) -> Vec<i32> {
    let mut found = BTreeSet::new();
    for (_, obstacle) in blockers {
        let Some(component) = board.get_item(*obstacle).map(Item::component_id) else {
            continue;
        };
        let free = component > 0
            && board
                .components
                .get_all()
                .any(|c| c.id == component && !c.position_fixed);
        if free {
            found.insert(component);
        }
    }
    found.into_iter().collect()
}

pub fn scale(by: &IntVector, fraction: f64) -> IntVector {
    IntVector::new(
        (f64::from(by.x) * fraction).round() as i32,
        (f64::from(by.y) * fraction).round() as i32,
    )
}

/// (component item, attached trace or via) pairs.
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
