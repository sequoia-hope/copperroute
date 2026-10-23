//! A conservative routing grid for one net: a cell centre is free for a trace of the net only if a
//! trace centred anywhere on a step from it to a neighbouring free centre keeps its clearance to
//! every other net's copper. So any path the grid accepts is legal on the board; the grid may
//! refuse legal paths, never the other way round.

use copper_board::DrcConstraints;
use copper_board::items::Item;
use copper_board::library::Padstack;
use copper_board::{Board, ItemId, PadstackId};
use copper_drc::constraints::{netclass_name, resolve};
use copper_geometry::{FloatPoint, IntBox, ShapeOps};

/// How far a step between neighbouring cell centres strays from the nearer centre, as a fraction
/// of the cell size: half the diagonal.
const STEP_REACH: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone, PartialEq)]
pub struct ViaChoice {
    pub padstack: PadstackId,
    pub clearance_class: usize,
    pub radius: f64,
    pub drill_radius: f64,
}

#[derive(Debug, Clone)]
pub struct RouteGrid {
    pub net: i32,
    pub x0: f64,
    pub y0: f64,
    pub h: f64,
    pub nx: usize,
    pub ny: usize,
    /// Board layer of each routing layer.
    pub layers: Vec<usize>,
    pub half_width: i32,
    pub trace_clearance_class: usize,
    pub via: Option<ViaChoice>,
    /// Per routing layer and cell: a trace of the net may not pass this centre.
    pub blocked: Vec<bool>,
    /// Per cell: a via of the net may not stand here.
    pub via_blocked: Vec<bool>,
    /// Per routing layer and cell: extra cost of entering it (negotiated congestion); empty
    /// for none.
    pub penalty: Vec<u32>,
}

impl RouteGrid {
    pub fn penalty_at(&self, index: usize) -> u32 {
        self.penalty.get(index).copied().unwrap_or(0)
    }
}

impl RouteGrid {
    pub fn cell_count(&self) -> usize {
        self.nx * self.ny
    }

    pub fn index(&self, layer: usize, i: usize, j: usize) -> usize {
        (layer * self.ny + j) * self.nx + i
    }

    pub fn centre(&self, i: usize, j: usize) -> FloatPoint {
        FloatPoint::new(
            self.x0 + (i as f64 + 0.5) * self.h,
            self.y0 + (j as f64 + 0.5) * self.h,
        )
    }

    pub fn build(board: &mut Board, net: i32, h: f64) -> RouteGrid {
        let profile = GridProfile::for_net(board, net);
        let mut map = ObstacleMap::new(board, profile, h);
        let ids: Vec<ItemId> = board.get_items().map(Item::id).collect();
        map.stamp(board, &ids);
        RouteGrid::view(&map, net)
    }

    /// The grid a net sees in a shared obstacle map: a cell is blocked unless nothing claims it
    /// or only this net does.
    pub fn view(map: &ObstacleMap, net: i32) -> RouteGrid {
        let free = |owner: i32| owner == FREE || owner == net;
        RouteGrid {
            net,
            x0: map.x0,
            y0: map.y0,
            h: map.h,
            nx: map.nx,
            ny: map.ny,
            layers: map.profile.layers.clone(),
            half_width: map.profile.half_width,
            trace_clearance_class: map.profile.trace_clearance_class,
            via: map.profile.via.clone(),
            blocked: map.owner.iter().map(|&o| !free(o)).collect(),
            via_blocked: map
                .via_owner
                .iter()
                .zip(&map.hole_owner)
                .map(|(&o, &h)| !free(o) || !free(h))
                .collect(),
            penalty: Vec::new(),
        }
    }
}

const FREE: i32 = 0;
const SEVERAL: i32 = -1;

/// What a grid depends on besides the board: the routing net's class.
#[derive(Debug, Clone, PartialEq)]
pub struct GridProfile {
    pub layers: Vec<usize>,
    pub half_width: i32,
    pub trace_clearance_class: usize,
    /// The DRC clearance of the routing net's class.
    pub own_rule: i32,
    pub via: Option<ViaChoice>,
}

impl GridProfile {
    pub fn for_net(board: &Board, net: i32) -> GridProfile {
        let net_class = board
            .rules
            .nets
            .get(net)
            .map(|n| board.rules.net_classes.get(n.net_class).clone());
        let layers: Vec<usize> = (0..board.get_layer_count())
            .filter(|&l| board.layer_structure().layers[l].is_signal)
            .filter(|&l| {
                net_class
                    .as_ref()
                    .is_none_or(|c| c.is_active_routing_layer(l))
            })
            .collect();
        let half_width = layers
            .first()
            .map_or(0, |&l| board.rules.get_trace_half_width(net, l));
        let constraints = resolve(board);
        let own_rule = netclass_name(board, net)
            .and_then(|name| constraints.netclass_clearance.get(&name).copied())
            .unwrap_or(0);
        let via = net_class
            .as_ref()
            .and_then(|c| c.get_via_rule())
            .and_then(|rule| {
                rule.iter().next().map(|info| {
                    let padstack = board.library.padstacks.get(info.get_padstack());
                    ViaChoice {
                        padstack: info.get_padstack(),
                        clearance_class: info.get_clearance_class_index(),
                        radius: padstack.map_or(0.0, padstack_radius),
                        drill_radius: padstack.map_or(0.0, Padstack::drill_radius),
                    }
                })
            });
        GridProfile {
            layers,
            half_width,
            trace_clearance_class: net_class
                .as_ref()
                .map_or(0, |c| c.get_trace_clearance_class()),
            own_rule,
            via,
        }
    }
}

impl GridProfile {
    /// The clearance between copper of the profile's `own_class` and an item of `other_class`
    /// whose net class has the DRC rule `other_rule`: the strictest of the matrix and the rules.
    pub fn clearance(
        &self,
        board: &Board,
        constraints: &DrcConstraints,
        own_class: usize,
        other_class: usize,
        layer: usize,
        other_rule: i32,
    ) -> f64 {
        let matrix = board
            .rules
            .clearance_matrix
            .get_value(own_class, other_class, layer, false);
        f64::from(
            matrix
                .max(self.own_rule)
                .max(other_rule)
                .max(constraints.min_clearance.unwrap_or(0)),
        )
    }
}

/// Who blocks each cell for a profile: nobody, one net (which may still use it), or several
/// nets or a netless obstacle. Built once, then stamped with each item added to the board.
#[derive(Debug, Clone)]
pub struct ObstacleMap {
    pub profile: GridProfile,
    x0: f64,
    y0: f64,
    h: f64,
    nx: usize,
    ny: usize,
    owner: Vec<i32>,
    via_owner: Vec<i32>,
    /// Per cell: whose via may be drilled here as far as holes go. A via's own centre stays its
    /// net's, so a later path of the net can change layers through it instead of stacking a
    /// second via on top.
    hole_owner: Vec<i32>,
}

impl ObstacleMap {
    pub fn new(board: &Board, profile: GridProfile, h: f64) -> ObstacleMap {
        let bounds = board.bounding_box;
        let x0 = f64::from(bounds.ll.x);
        let y0 = f64::from(bounds.ll.y);
        let nx = ((f64::from(bounds.ur.x) - x0) / h).floor().max(1.0) as usize;
        let ny = ((f64::from(bounds.ur.y) - y0) / h).floor().max(1.0) as usize;
        let layers = profile.layers.len();
        ObstacleMap {
            profile,
            x0,
            y0,
            h,
            nx,
            ny,
            owner: vec![FREE; layers * nx * ny],
            via_owner: vec![FREE; nx * ny],
            hole_owner: vec![FREE; nx * ny],
        }
    }

    /// Stamps `items` into the map.
    pub fn stamp(&mut self, board: &mut Board, items: &[ItemId]) {
        let constraints = resolve(board);
        let profile = self.profile.clone();
        for &id in items {
            self.stamp_hole(board, &constraints, id);
            let (blocks_traces, blocks_vias, class, owner, other_rule) = {
                let Some(item) = board.get_item(id) else {
                    continue;
                };
                let blocks_traces = item.is_trace_obstacle(i32::MIN);
                let blocks_vias = blocks_traces || matches!(item, Item::ViaObstacleArea(_));
                let owner = match item.net_count() {
                    0 => SEVERAL,
                    1 => item.get_net_number(0),
                    _ => SEVERAL,
                };
                let other_rule = (item.net_count() > 0)
                    .then(|| netclass_name(board, item.get_net_number(0)))
                    .flatten()
                    .and_then(|name| constraints.netclass_clearance.get(&name).copied())
                    .unwrap_or(0);
                (
                    blocks_traces,
                    blocks_vias,
                    item.clearance_class(),
                    owner,
                    other_rule,
                )
            };
            if !blocks_traces && !blocks_vias {
                continue;
            }
            let shape_layers: Vec<usize> = {
                let ctx = board.ctx();
                let item = board.get_item(id).expect("present");
                (0..item.tile_shape_count(&ctx))
                    .map(|i| item.shape_layer(i, &ctx))
                    .collect()
            };
            for (index, layer) in shape_layers.into_iter().enumerate() {
                let Some(shape) = board.item_tile_shape(id, index) else {
                    continue;
                };
                let rule = |own_class: usize| {
                    profile.clearance(board, &constraints, own_class, class, layer, other_rule)
                };
                if blocks_traces {
                    if let Some(routing_layer) =
                        self.profile.layers.iter().position(|&l| l == layer)
                    {
                        let reach = f64::from(self.profile.half_width)
                            + rule(self.profile.trace_clearance_class)
                            + STEP_REACH * self.h;
                        self.mark(&shape, reach, Some(routing_layer), owner);
                    }
                }
                if blocks_vias {
                    if let Some(via) = self.profile.via.clone() {
                        let reach = via.radius + rule(via.clearance_class);
                        self.mark(&shape, reach, None, owner);
                    }
                }
            }
        }
    }

    /// Blocks drilling within the hole-to-hole distance of `id`'s hole, for every net: holes
    /// conflict whatever their nets.
    fn stamp_hole(&mut self, board: &Board, constraints: &DrcConstraints, id: ItemId) {
        let Some(via) = &self.profile.via else {
            return;
        };
        let Some(hole) = copper_drc::checks::geometry::hole_of(board, id) else {
            return;
        };
        let reuse_net = match board.get_item(id) {
            Some(item @ Item::Via(_)) if item.net_count() == 1 => item.get_net_number(0),
            _ => SEVERAL,
        };
        let reach = hole.radius
            + via.drill_radius
            + f64::from(constraints.hole_to_hole.unwrap_or(0).max(0));
        let lo_i = ((hole.center.x - reach - self.x0) / self.h)
            .floor()
            .max(0.0) as usize;
        let lo_j = ((hole.center.y - reach - self.y0) / self.h)
            .floor()
            .max(0.0) as usize;
        let hi_i = (((hole.center.x + reach - self.x0) / self.h).ceil() as usize).min(self.nx - 1);
        let hi_j = (((hole.center.y + reach - self.y0) / self.h).ceil() as usize).min(self.ny - 1);
        for j in lo_j..=hi_j {
            for i in lo_i..=hi_i {
                let centre = FloatPoint::new(
                    self.x0 + (i as f64 + 0.5) * self.h,
                    self.y0 + (j as f64 + 0.5) * self.h,
                );
                let distance = centre.distance(&hole.center);
                if distance >= reach {
                    continue;
                }
                let owner = if distance < 1.0 { reuse_net } else { SEVERAL };
                let slot = &mut self.hole_owner[j * self.nx + i];
                *slot = match *slot {
                    FREE => owner,
                    current if current == owner => current,
                    _ => SEVERAL,
                };
            }
        }
    }

    fn mark(
        &mut self,
        shape: &copper_geometry::TileShape,
        reach: f64,
        routing_layer: Option<usize>,
        owner: i32,
    ) {
        let bb: IntBox = shape.bounding_box();
        let lo_i = (((f64::from(bb.ll.x) - reach - self.x0) / self.h)
            .floor()
            .max(0.0)) as usize;
        let lo_j = (((f64::from(bb.ll.y) - reach - self.y0) / self.h)
            .floor()
            .max(0.0)) as usize;
        let hi_i =
            ((((f64::from(bb.ur.x) + reach - self.x0) / self.h).ceil()) as usize).min(self.nx - 1);
        let hi_j =
            ((((f64::from(bb.ur.y) + reach - self.y0) / self.h).ceil()) as usize).min(self.ny - 1);
        for j in lo_j..=hi_j {
            for i in lo_i..=hi_i {
                let centre = FloatPoint::new(
                    self.x0 + (i as f64 + 0.5) * self.h,
                    self.y0 + (j as f64 + 0.5) * self.h,
                );
                if shape.distance(&centre) >= reach {
                    continue;
                }
                let slot = match routing_layer {
                    Some(layer) => &mut self.owner[(layer * self.ny + j) * self.nx + i],
                    None => &mut self.via_owner[j * self.nx + i],
                };
                *slot = match *slot {
                    FREE => owner,
                    current if current == owner => current,
                    _ => SEVERAL,
                };
            }
        }
    }
}

fn padstack_radius(padstack: &Padstack) -> f64 {
    (padstack.from_layer()..=padstack.to_layer())
        .filter_map(|layer| padstack.get_shape(layer))
        .map(|shape| {
            let bb = shape.bounding_box();
            f64::from((bb.ur.x - bb.ll.x).max(bb.ur.y - bb.ll.y)) / 2.0
        })
        .fold(0.0, f64::max)
}
