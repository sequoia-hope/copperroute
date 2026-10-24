//! A localized push field ("make room here, this way"): every free part near the centre gets a
//! settle target along the field's vector, falling off with distance as
//! `(1 + d²/radius²)^(-decay/2)`, measured from the centroid of the part's pins.

use std::collections::BTreeMap;

use copper_board::Board;
use copper_geometry::{FloatPoint, IntVector};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dipole {
    pub center: FloatPoint,
    pub vector: (f64, f64),
    pub radius: f64,
    pub decay: f64,
}

impl Dipole {
    pub fn displacement(&self, at: &FloatPoint) -> (f64, f64) {
        let d2 = (at.x - self.center.x).powi(2) + (at.y - self.center.y).powi(2);
        let w = (1.0 + d2 / (self.radius * self.radius).max(1e-6)).powf(-0.5 * self.decay);
        (self.vector.0 * w, self.vector.1 * w)
    }
}

/// The summed displacement of every dipole at each free part, for parts it moves by at least
/// `smallest`.
pub fn dipole_targets(
    board: &Board,
    dipoles: &[Dipole],
    smallest: f64,
) -> BTreeMap<i32, IntVector> {
    let mut targets = BTreeMap::new();
    for component in board.components.get_all().filter(|c| !c.position_fixed) {
        let Some(at) = pin_centroid(board, component.id) else {
            continue;
        };
        let (mut dx, mut dy) = (0.0, 0.0);
        for dipole in dipoles {
            let (x, y) = dipole.displacement(&at);
            dx += x;
            dy += y;
        }
        if dx.hypot(dy) >= smallest {
            targets.insert(
                component.id,
                IntVector::new(dx.round() as i32, dy.round() as i32),
            );
        }
    }
    targets
}

fn pin_centroid(board: &Board, component_id: i32) -> Option<FloatPoint> {
    let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
    for item in board
        .get_items()
        .filter(|i| i.component_id() == component_id && i.is_drill_item())
    {
        if let Some(center) = board.drill_center(item.id()) {
            let c = center.to_float();
            x += c.x;
            y += c.y;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| FloatPoint { x: x / n, y: y / n })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dipole() -> Dipole {
        Dipole {
            center: FloatPoint { x: 0.0, y: 0.0 },
            vector: (0.0, -4.0),
            radius: 10.0,
            decay: 2.0,
        }
    }

    #[test]
    fn the_centre_gets_the_whole_push_and_one_radius_out_half_of_it() {
        assert_eq!(dipole().displacement(&FloatPoint { x: 0.0, y: 0.0 }), (0.0, -4.0));
        let (dx, dy) = dipole().displacement(&FloatPoint { x: 10.0, y: 0.0 });
        assert_eq!(dx, 0.0);
        assert!((dy + 2.0).abs() < 1e-12, "{dy}");
    }
}
