use std::sync::OnceLock;

use copper_geometry::{
    Area, FloatPoint, IntBox, IntPoint, Line, PolylineArea, PolylineShapeRef, TileShape, Vector,
};

use crate::ids::ItemId;
use crate::items::header::ItemHeader;
use crate::items::{BOARD_OUTLINE_HALF_WIDTH, ItemCtx};
use crate::structure::FixedState;

/// How far the bounding octagon of one piece of an outline edge's band may stray from the band.
/// Routing trees keep only bounding octagons, so a long edge off the 45-degree grid is cut into
/// pieces; as one piece its octagon reaches deep into the board and walls in pads near the edge.
const OUTLINE_PIECE_OVERSHOOT: f64 = 5.0 * BOARD_OUTLINE_HALF_WIDTH as f64;

#[derive(Debug, Clone)]
pub struct BoardOutline {
    pub hdr: ItemHeader,
    shapes: Vec<PolylineShapeRef>,
    keepout_area: OnceLock<Area>,
    keepout_lines: Option<Vec<TileShape>>,
    keepout_convex_pieces: OnceLock<Option<Vec<TileShape>>>,
    keepout_outside_outline: bool,
    piece_count: OnceLock<usize>,
}

impl PartialEq for BoardOutline {
    fn eq(&self, other: &BoardOutline) -> bool {
        self.hdr == other.hdr
            && self.shapes == other.shapes
            && self.keepout_outside_outline == other.keepout_outside_outline
            && self.keepout_area.get() == other.keepout_area.get()
            && self.keepout_lines == other.keepout_lines
    }
}

impl BoardOutline {
    pub fn new(hdr: ItemHeader, shapes: Vec<PolylineShapeRef>) -> BoardOutline {
        BoardOutline {
            hdr,
            shapes,
            keepout_area: OnceLock::new(),
            keepout_lines: None,
            keepout_convex_pieces: OnceLock::new(),
            keepout_outside_outline: false,
            piece_count: OnceLock::new(),
        }
    }

    pub fn copy(&self, new_id: ItemId) -> BoardOutline {
        BoardOutline::new(
            ItemHeader::new(
                new_id,
                Vec::new(),
                self.hdr.clearance_class(),
                0,
                FixedState::SystemFixed,
            ),
            self.shapes.clone(),
        )
    }

    pub fn get_half_width(&self) -> i32 {
        BOARD_OUTLINE_HALF_WIDTH
    }

    pub fn last_layer(&self, ctx: &ItemCtx<'_>) -> usize {
        ctx.rules.layer_structure().count().checked_sub(1).expect(
            "BoardOutline.lastLayer: the board has no layers — Java answers -1 here \
                 (BoardOutline.java:104)",
        )
    }

    pub fn tile_shape_count(&self, ctx: &ItemCtx<'_>) -> usize {
        let layer_count = ctx.rules.layer_structure().count();
        if self.keepout_outside_outline {
            self.keepout_convex_pieces(ctx)
                .map_or(0, |tiles| tiles.len() * layer_count)
        } else {
            self.piece_count() * layer_count
        }
    }

    pub fn shape_layer(&self, index: usize, ctx: &ItemCtx<'_>) -> usize {
        let layer_count = ctx.rules.layer_structure().count();
        let shape_count = self.tile_shape_count(ctx);
        if shape_count > 0 {
            index * layer_count / shape_count
        } else {
            0
        }
    }

    pub fn bounding_box(&self) -> IntBox {
        self.shapes.iter().fold(IntBox::EMPTY, |acc, shape| {
            acc.union(&shape.as_ops().bounding_box())
        })
    }

    pub fn translate_by(&mut self, vector: &Vector) {
        for shape in &mut self.shapes {
            *shape = shape.translate_by(vector);
        }
        if let Some(keepout_area) = self.keepout_area.get_mut() {
            *keepout_area = keepout_area.translate_by(vector);
        }
        self.keepout_lines = None;
        self.keepout_convex_pieces.take();
        self.piece_count.take();
    }

    pub fn turn_90_degree(&mut self, factor: i32, pole: &IntPoint) {
        for shape in &mut self.shapes {
            *shape = shape.turn_90_degree(factor, pole);
        }
        if let Some(keepout_area) = self.keepout_area.get_mut() {
            *keepout_area = keepout_area.turn_90_degree(factor, pole);
        }
        self.keepout_lines = None;
        self.keepout_convex_pieces.take();
        self.piece_count.take();
    }

    pub fn rotate_approx(&mut self, angle_in_degree: f64, pole: &FloatPoint) {
        let angle = angle_in_degree.to_radians();
        for shape in &mut self.shapes {
            *shape = shape.rotate_approx(angle, pole);
        }
        if let Some(keepout_area) = self.keepout_area.get_mut() {
            *keepout_area = keepout_area.rotate_approx(angle, pole);
        }
        self.keepout_lines = None;
        self.keepout_convex_pieces.take();
        self.piece_count.take();
    }

    pub fn change_placement_side(&mut self, pole: &IntPoint) {
        for shape in &mut self.shapes {
            *shape = shape.mirror_vertical(pole);
        }
        if let Some(keepout_area) = self.keepout_area.get_mut() {
            *keepout_area = keepout_area.mirror_vertical(pole);
        }
        self.keepout_lines = None;
        self.keepout_convex_pieces.take();
        self.piece_count.take();
    }

    pub fn shape_count(&self) -> usize {
        self.shapes.len()
    }

    pub fn get_shape(&self, index: usize) -> Option<&PolylineShapeRef> {
        self.shapes.get(index)
    }

    pub fn keepout_convex_pieces(&self, ctx: &ItemCtx<'_>) -> Option<&[TileShape]> {
        self.keepout_convex_pieces
            .get_or_init(|| self.get_keepout_area(ctx).split_to_convex())
            .as_deref()
    }

    pub fn get_keepout_area(&self, ctx: &ItemCtx<'_>) -> &Area {
        self.keepout_area.get_or_init(|| {
            Area::Polyline(PolylineArea::new(
                PolylineShapeRef::Tile(TileShape::Box(*ctx.bounding_box)),
                self.shapes.clone(),
            ))
        })
    }

    pub fn get_keepout_lines(&mut self) -> &[TileShape] {
        self.keepout_lines.get_or_insert_with(Vec::new)
    }

    pub fn keepout_outside_outline_generated(&self) -> bool {
        self.keepout_outside_outline
    }

    pub fn generate_keepout_outside(&mut self, value: bool) {
        if value == self.keepout_outside_outline {
            return;
        }
        self.keepout_outside_outline = value;
    }

    pub fn line_count(&self) -> usize {
        self.shapes
            .iter()
            .map(|shape| shape.as_ops().border_line_count())
            .sum()
    }

    fn piece_count(&self) -> usize {
        *self.piece_count.get_or_init(|| {
            (0..self.shapes.len())
                .map(|shape_no| {
                    (0..self.shapes[shape_no].as_ops().border_line_count())
                        .map(|line_no| self.piece_cuts(shape_no, line_no).len() + 1)
                        .sum::<usize>()
                })
                .sum()
        })
    }

    /// Lines square to border line `line_no` of shape `shape_no` that cut its band into pieces
    /// short enough for [`OUTLINE_PIECE_OVERSHOOT`], in order along the line.
    pub fn piece_cuts(&self, shape_no: usize, line_no: usize) -> Vec<Line> {
        let ops = self.shapes[shape_no].as_ops();
        let count = ops.border_line_count();
        let (Some(line), Some(from), Some(to)) = (
            ops.border_line(line_no),
            ops.corner_approx(line_no),
            ops.corner_approx((line_no + 1) % count),
        ) else {
            return Vec::new();
        };
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let eighth = std::f64::consts::FRAC_PI_4;
        let off_grid = dy.atan2(dx).rem_euclid(eighth);
        let off_grid = off_grid.min(eighth - off_grid);
        let pieces = (dx.hypot(dy) * off_grid.sin() / OUTLINE_PIECE_OVERSHOOT).ceil();
        if !(pieces > 1.0) {
            return Vec::new();
        }
        let square = line.direction().get_vector().turn_90_degree(1);
        (1..pieces as usize)
            .map(|k| {
                let t = k as f64 / pieces;
                let at = FloatPoint::new(from.x + t * dx, from.y + t * dy).round();
                Line::new(at, IntPoint::new(at.x + square.x, at.y + square.y))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_outline_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoardOutline>();
    }

    #[test]
    fn get_keepout_lines_is_always_empty() {
        let mut outline = BoardOutline::new(
            ItemHeader::new(ItemId(1), Vec::new(), 0, 0, FixedState::SystemFixed),
            Vec::new(),
        );
        assert!(outline.get_keepout_lines().is_empty());
        outline.translate_by(&Vector::new(1, 1));
        assert!(outline.get_keepout_lines().is_empty());
    }
}
