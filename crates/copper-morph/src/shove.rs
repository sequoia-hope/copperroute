//! Moving parts on a routed board without ripping its copper: traces of other nets in the way are
//! shoved aside by the router's push-and-shove engine, and traces attached to the moving pins are
//! dragged along. This is what the settle engine's copper pass does with free trace nodes, done
//! here by the same machinery the router uses to make room for a forced pad.

use std::collections::BTreeSet;

use copper_board::structure::{AngleRestriction, ShapeEntrySide};
use copper_board::{Board, ItemId};
use copper_geometry::{IntVector, TileShape, Vector};
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
        *board = trial;
        true
    }
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
