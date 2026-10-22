//! Moving parts on a routed board without ripping its copper: traces of other nets in the way are
//! shoved aside by the router's push-and-shove engine, and traces attached to the moving pins are
//! dragged along. This is what the settle engine's copper pass does with free trace nodes, done
//! here by the same machinery the router uses to make room for a forced pad.

use std::collections::BTreeSet;

use copper_board::board::StopConnectionOption;
use copper_board::items::Item;
use copper_board::structure::{AngleRestriction, ShapeEntrySide};
use copper_board::{Board, ItemId};
use copper_geometry::{IntPoint, IntVector, Point, TileShape, Vector};
use copper_router::board_ext::ForcedPadRouter;

use crate::oracle::MoveOracle;

const MAX_RECURSION: i32 = 20;
const MAX_VIA_RECURSION: i32 = 8;

impl MoveOracle {
    /// Moves the components by `by`, shoving other nets' traces out of their pins' way. Fixed
    /// obstacles are judged by the oracle first. All or nothing: on failure the board is
    /// unchanged and `false` is returned.
    pub fn translate_shoving(&self, board: &mut Board, components: &[i32], by: &IntVector) -> bool {
        if *by == IntVector::ZERO {
            return true;
        }
        let fixed_only = MoveOracle {
            routable_copper_is_soft: true,
            ..*self
        };
        let (fraction, _) = fixed_only.line_search_group(board, components, by);
        if fraction < 1.0 {
            return false;
        }
        let mut trial = board.clone();
        if !shove_out_of_the_way(&mut trial, components, by) {
            return false;
        }
        let vector = Vector::Int(*by);
        for &component in components {
            trial.components.move_component(component, &vector);
        }
        let members: Vec<ItemId> = components
            .iter()
            .flat_map(|c| trial.get_component_items(*c))
            .collect();
        for id in members {
            if !matches!(trial.move_item_by(id, &vector), Ok(true)) {
                return false;
            }
        }
        if !self.settle_dragged_copper(board, &mut trial) {
            return false;
        }
        *board = trial;
        true
    }

    /// Places the component with its location at `target`, shoving copper out of its footprint:
    /// the settle engine's ghost, born full size because the shove engine moves copper
    /// recursively instead of in small steps.
    pub fn place_shoving(&self, board: &mut Board, component_id: i32, target: &IntPoint) -> bool {
        let Some(Point::Int(location)) = board
            .components
            .get_all()
            .find(|c| c.id == component_id)
            .and_then(|c| c.get_location().cloned())
        else {
            return false;
        };
        let by = IntVector::new(target.x - location.x, target.y - location.y);
        self.translate_shoving(board, &[component_id], &by)
    }

    /// Dragged traces run straight from a pin's old place to its new one and may now crowd other
    /// copper; the DRC is the judge. Routable copper behind a new violation is ripped for the
    /// router when the oracle allows it; any violation left refuses the move.
    fn settle_dragged_copper(&self, before: &mut Board, after: &mut Board) -> bool {
        let known = violation_pairs(before);
        let fresh: Vec<(ItemId, Option<ItemId>)> = violation_pairs(after)
            .into_iter()
            .filter(|pair| !known.contains(pair))
            .collect();
        if fresh.is_empty() {
            return true;
        }
        if !self.routable_copper_is_soft {
            return false;
        }
        let mut ripped = BTreeSet::new();
        for (a, b) in fresh {
            for id in std::iter::once(a).chain(b) {
                if after.get_item(id).is_some_and(Item::is_routable) {
                    ripped.extend(after.connection_items(id, StopConnectionOption::None));
                }
            }
        }
        ripped.retain(|id| after.get_item(*id).is_some_and(Item::is_routable));
        after.remove_items(ripped);
        violation_pairs(after)
            .iter()
            .all(|pair| known.contains(pair))
    }
}

fn violation_pairs(board: &mut Board) -> BTreeSet<(ItemId, Option<ItemId>)> {
    copper_drc::DesignRulesChecker::new(board)
        .get_all_violations()
        .into_iter()
        .map(|v| (v.first_item, v.second_item))
        .collect()
}

fn shove_out_of_the_way(board: &mut Board, components: &[i32], by: &IntVector) -> bool {
    let members: BTreeSet<ItemId> = components
        .iter()
        .flat_map(|c| board.get_component_items(*c))
        .collect();
    let ignore: Vec<ItemId> = members.iter().copied().collect();
    let tree = board.trees.get_default_tree().id();
    let orthogonal = board.rules.trace_angle_restriction == AngleRestriction::NinetyDegree;
    let vector = Vector::Int(*by);
    for &id in &members {
        let Some(item) = board.get_item(id) else {
            continue;
        };
        if !item.is_drill_item() {
            continue;
        }
        let nets = item.net_nos().to_vec();
        let clearance_class = item.clearance_class();
        let (first_layer, last_layer) = {
            let ctx = board.ctx();
            (item.first_layer(&ctx), item.last_layer(&ctx))
        };
        let Some(center) = board.drill_center(id) else {
            return false;
        };
        for layer in first_layer..=last_layer {
            let Some(shape) = board.item_tree_shape(id, tree, layer - first_layer) else {
                continue;
            };
            let moved = shape.translate_by(&vector);
            let region = if orthogonal {
                TileShape::Box(moved.bounding_box())
            } else {
                match moved.bounding_octagon() {
                    Some(octagon) => TileShape::Octagon(octagon),
                    None => continue,
                }
            };
            let from_side = ShapeEntrySide::from_point(&center, &region);
            let shoved = ForcedPadRouter::forced_pad(
                board,
                &region,
                &from_side,
                layer,
                &nets,
                clearance_class,
                false,
                Some(&ignore),
                MAX_RECURSION,
                MAX_VIA_RECURSION,
                &|| false,
            );
            if !matches!(shoved, Ok(true)) {
                return false;
            }
        }
    }
    true
}

impl MoveOracle {
    /// The settle engine's ghost site search: the place where the part's pins centre on the pins
    /// it connects to, or the nearest admissible place to it within `radius`, scanned on a grid
    /// of `pitch` and tried nearest first with `place_shoving`. Returns the move made.
    pub fn place_near_partners(
        &self,
        board: &mut Board,
        component_id: i32,
        radius: i32,
        pitch: i32,
        attempts: usize,
    ) -> Option<IntVector> {
        let ideal = partner_offset(board, component_id)?;
        let fixed_only = MoveOracle {
            routable_copper_is_soft: true,
            ..*self
        };
        let base = fixed_only.conflicts(board, component_id, &IntVector::ZERO);
        let steps = (radius / pitch.max(1)).max(0);
        let mut candidates: Vec<IntVector> = Vec::new();
        for i in -steps..=steps {
            for j in -steps..=steps {
                let offset = IntVector::new(ideal.x + i * pitch, ideal.y + j * pitch);
                if f64::from(i * pitch).hypot(f64::from(j * pitch)) <= f64::from(radius) {
                    candidates.push(offset);
                }
            }
        }
        candidates.sort_by_key(|v| {
            let (dx, dy) = (i64::from(v.x - ideal.x), i64::from(v.y - ideal.y));
            dx * dx + dy * dy
        });
        let mut tried = 0;
        for by in candidates {
            if tried >= attempts {
                break;
            }
            if !fixed_only
                .conflicts(board, component_id, &by)
                .is_subset(&base)
            {
                continue;
            }
            tried += 1;
            if self.translate_shoving(board, &[component_id], &by) {
                return Some(by);
            }
        }
        None
    }
}

/// The move that would put the centroid of the part's pins on the centroid of the pins they
/// connect to on other parts.
fn partner_offset(board: &Board, component_id: i32) -> Option<IntVector> {
    let own: Vec<&Item> = board
        .get_items()
        .filter(|i| i.component_id() == component_id && i.is_drill_item())
        .collect();
    let nets: BTreeSet<i32> = own.iter().flat_map(|i| i.net_nos().to_vec()).collect();
    let centroid = |items: &mut dyn Iterator<Item = &Item>| -> Option<(f64, f64)> {
        let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
        for item in items {
            let Some(center) = board.drill_center(item.id()) else {
                continue;
            };
            let c = center.to_float();
            x += c.x;
            y += c.y;
            n += 1.0;
        }
        (n > 0.0).then(|| (x / n, y / n))
    };
    let mine = centroid(&mut own.iter().copied())?;
    let theirs = centroid(&mut board.get_items().filter(|i| {
        i.is_drill_item()
            && i.component_id() > 0
            && i.component_id() != component_id
            && i.net_nos().iter().any(|n| nets.contains(n))
    }))?;
    Some(IntVector::new(
        (theirs.0 - mine.0).round() as i32,
        (theirs.1 - mine.1).round() as i32,
    ))
}
