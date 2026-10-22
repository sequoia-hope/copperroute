//! A conservative routing grid for one net: a cell centre is free for a trace of the net only if a
//! trace centred anywhere on a step from it to a neighbouring free centre keeps its clearance to
//! every other net's copper. So any path the grid accepts is legal on the board; the grid may
//! refuse legal paths, never the other way round.

use copper_board::items::Item;
use copper_board::library::Padstack;
use copper_board::{Board, ItemId, PadstackId};
use copper_drc::constraints::{netclass_name, resolve};
use copper_geometry::{FloatPoint, IntBox, ShapeOps};

/// How far a step between neighbouring cell centres strays from the nearer centre, as a fraction
/// of the cell size: half the diagonal.
const STEP_REACH: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone)]
pub struct ViaChoice {
    pub padstack: PadstackId,
    pub clearance_class: usize,
    pub radius: f64,
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
}

impl RouteGrid {
    pub fn cell_count(&self) -> usize {
        self.nx * self.ny
    }

    pub fn index(&self, layer: usize, i: usize, j: usize) -> usize {
        (layer * self.ny + j) * self.nx + i
    }

    pub fn centre(&self, i: usize, j: usize) -> FloatPoint {
        FloatPoint::new(self.x0 + (i as f64 + 0.5) * self.h, self.y0 + (j as f64 + 0.5) * self.h)
    }

    pub fn build(board: &mut Board, net: i32, h: f64) -> RouteGrid {
        let bounds = board.bounding_box;
        let x0 = f64::from(bounds.ll.x);
        let y0 = f64::from(bounds.ll.y);
        let nx = ((f64::from(bounds.ur.x) - x0) / h).floor().max(1.0) as usize;
        let ny = ((f64::from(bounds.ur.y) - y0) / h).floor().max(1.0) as usize;

        let class_index = board.rules.nets.get(net).map(|n| n.net_class);
        let net_class = class_index.map(|c| board.rules.net_classes.get(c).clone());
        let layers: Vec<usize> = (0..board.get_layer_count())
            .filter(|&l| board.layer_structure().layers[l].is_signal)
            .filter(|&l| net_class.as_ref().is_none_or(|c| c.is_active_routing_layer(l)))
            .collect();
        let half_width = layers.first().map_or(0, |&l| board.rules.get_trace_half_width(net, l));
        let trace_clearance_class = net_class.as_ref().map_or(0, |c| c.get_trace_clearance_class());
        let via = net_class.as_ref().and_then(|c| c.get_via_rule()).and_then(|rule| {
            rule.iter().next().map(|info| ViaChoice {
                padstack: info.get_padstack(),
                clearance_class: info.get_clearance_class_index(),
                radius: board
                    .library
                    .padstacks
                    .get(info.get_padstack())
                    .map_or(0.0, padstack_radius),
            })
        });

        let mut grid = RouteGrid {
            net,
            x0,
            y0,
            h,
            nx,
            ny,
            half_width,
            trace_clearance_class,
            via,
            blocked: vec![false; layers.len() * nx * ny],
            via_blocked: vec![false; nx * ny],
            layers,
        };
        grid.rasterize_obstacles(board);
        grid
    }

    fn rasterize_obstacles(&mut self, board: &mut Board) {
        let constraints = resolve(board);
        let own_rule = netclass_name(board, self.net)
            .and_then(|name| constraints.netclass_clearance.get(&name).copied())
            .unwrap_or(0);
        let ids: Vec<ItemId> = board.get_items().map(Item::id).collect();
        for id in ids {
            let (blocks_traces, blocks_vias, class, other_rule) = {
                let Some(item) = board.get_item(id) else {
                    continue;
                };
                if item.contains_net(self.net) {
                    continue;
                }
                let blocks_traces = item.is_trace_obstacle(self.net);
                let blocks_vias = blocks_traces || matches!(item, Item::ViaObstacleArea(_));
                let other_rule = (item.net_count() > 0)
                    .then(|| netclass_name(board, item.get_net_number(0)))
                    .flatten()
                    .and_then(|name| constraints.netclass_clearance.get(&name).copied())
                    .unwrap_or(0);
                (blocks_traces, blocks_vias, item.clearance_class(), other_rule)
            };
            if !blocks_traces && !blocks_vias {
                continue;
            }
            let shape_layers: Vec<usize> = {
                let ctx = board.ctx();
                let item = board.get_item(id).expect("present");
                (0..item.tile_shape_count(&ctx)).map(|i| item.shape_layer(i, &ctx)).collect()
            };
            for (index, layer) in shape_layers.into_iter().enumerate() {
                let Some(shape) = board.item_tile_shape(id, index) else {
                    continue;
                };
                let rule = |own_class: usize| {
                    let matrix = board.rules.clearance_matrix.get_value(own_class, class, layer, false);
                    f64::from(matrix.max(own_rule).max(other_rule).max(constraints.min_clearance.unwrap_or(0)))
                };
                if blocks_traces {
                    if let Some(routing_layer) = self.layers.iter().position(|&l| l == layer) {
                        let reach = f64::from(self.half_width) + rule(self.trace_clearance_class) + STEP_REACH * self.h;
                        self.mark(&shape, reach, Some(routing_layer));
                    }
                }
                if blocks_vias {
                    if let Some(via) = self.via.clone() {
                        let reach = via.radius + rule(via.clearance_class);
                        self.mark(&shape, reach, None);
                    }
                }
            }
        }
    }

    /// Marks the centres within `reach` of `shape`: on one routing layer, or the via mask.
    fn mark(&mut self, shape: &copper_geometry::TileShape, reach: f64, routing_layer: Option<usize>) {
        let bb: IntBox = shape.bounding_box();
        let lo_i = (((f64::from(bb.ll.x) - reach - self.x0) / self.h).floor().max(0.0)) as usize;
        let lo_j = (((f64::from(bb.ll.y) - reach - self.y0) / self.h).floor().max(0.0)) as usize;
        let hi_i = ((((f64::from(bb.ur.x) + reach - self.x0) / self.h).ceil()) as usize).min(self.nx - 1);
        let hi_j = ((((f64::from(bb.ur.y) + reach - self.y0) / self.h).ceil()) as usize).min(self.ny - 1);
        for j in lo_j..=hi_j {
            for i in lo_i..=hi_i {
                if shape.distance(&self.centre(i, j)) < reach {
                    match routing_layer {
                        Some(layer) => {
                            let index = self.index(layer, i, j);
                            self.blocked[index] = true;
                        }
                        None => self.via_blocked[j * self.nx + i] = true,
                    }
                }
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
