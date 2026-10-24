//! The shortest path through a channel of gates (the funnel algorithm), and the bends that keep
//! it clear of the corners it wraps.

use copper_geometry::FloatPoint;

/// The stretch of a triangle edge a wire may cross, seen walking forward: `left` on the walker's
/// left. An end shrunk back from an obstacle's sample remembers it, as `(sample, reach)`.
#[derive(Debug, Clone, Copy)]
pub struct Gate {
    pub left: FloatPoint,
    pub right: FloatPoint,
    pub left_wrap: Option<(FloatPoint, f64)>,
    pub right_wrap: Option<(FloatPoint, f64)>,
}

impl Gate {
    pub fn nearest(&self, p: &FloatPoint) -> FloatPoint {
        let (dx, dy) = (self.right.x - self.left.x, self.right.y - self.left.y);
        let len2 = dx * dx + dy * dy;
        if len2 == 0.0 {
            return self.left;
        }
        let t = (((p.x - self.left.x) * dx + (p.y - self.left.y) * dy) / len2).clamp(0.0, 1.0);
        FloatPoint::new(self.left.x + t * dx, self.left.y + t * dy)
    }
}

pub struct Bend {
    pub at: FloatPoint,
    wrap: Option<(FloatPoint, f64)>,
    prev: FloatPoint,
    next: FloatPoint,
}

const ARC_STEP: f64 = std::f64::consts::PI / 6.0;
/// An arc of chords this far out keeps every chord clear of the wrapped circle.
const ARC_SLACK: f64 = 1.04;

impl Bend {
    /// The bend as corners: a straight corner, or an arc round the sample it wraps, from the
    /// incoming direction's tangent to the outgoing one's.
    pub fn points(&self) -> Vec<FloatPoint> {
        let Some((u, reach)) = self.wrap else {
            return vec![self.at];
        };
        let away = (self.at.x - u.x, self.at.y - u.y);
        let normal = |d: (f64, f64)| {
            let len = d.0.hypot(d.1);
            if len == 0.0 {
                return None;
            }
            let n = (-d.1 / len, d.0 / len);
            Some(if n.0 * away.0 + n.1 * away.1 >= 0.0 { n } else { (-n.0, -n.1) })
        };
        let (Some(a), Some(b)) = (
            normal((self.at.x - self.prev.x, self.at.y - self.prev.y)),
            normal((self.next.x - self.at.x, self.next.y - self.at.y)),
        ) else {
            return vec![self.at];
        };
        let start = a.1.atan2(a.0);
        let mut sweep = b.1.atan2(b.0) - start;
        while sweep > std::f64::consts::PI {
            sweep -= 2.0 * std::f64::consts::PI;
        }
        while sweep < -std::f64::consts::PI {
            sweep += 2.0 * std::f64::consts::PI;
        }
        let steps = (sweep.abs() / ARC_STEP).ceil().max(1.0) as usize;
        let radius = reach * ARC_SLACK;
        (0..=steps)
            .map(|k| {
                let angle = start + sweep * k as f64 / steps as f64;
                FloatPoint::new(u.x + radius * angle.cos(), u.y + radius * angle.sin())
            })
            .collect()
    }
}

fn cross(a: &FloatPoint, b: &FloatPoint, c: &FloatPoint) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn same(a: &FloatPoint, b: &FloatPoint) -> bool {
    a.x == b.x && a.y == b.y
}

/// The shortest path from `from` through every gate to `to`, as bends.
pub fn pull(from: &FloatPoint, gates: &[Gate], to: &FloatPoint) -> Vec<Bend> {
    let mut portals: Vec<Gate> = Vec::with_capacity(gates.len() + 2);
    let point = |p: &FloatPoint| Gate { left: *p, right: *p, left_wrap: None, right_wrap: None };
    portals.push(point(from));
    portals.extend_from_slice(gates);
    portals.push(point(to));

    let mut corners: Vec<(FloatPoint, Option<(FloatPoint, f64)>)> = vec![(*from, None)];
    let (mut apex, mut left, mut right) = (*from, *from, *from);
    let (mut left_i, mut right_i) = (0, 0);
    let mut i = 1;
    while i < portals.len() {
        let gate = portals[i];
        if cross(&apex, &right, &gate.right) >= 0.0 {
            if same(&apex, &right) || cross(&apex, &left, &gate.right) < 0.0 {
                right = gate.right;
                right_i = i;
            } else {
                corners.push((left, portals[left_i].left_wrap));
                apex = left;
                right = apex;
                right_i = left_i;
                i = left_i + 1;
                continue;
            }
        }
        if cross(&apex, &left, &gate.left) <= 0.0 {
            if same(&apex, &left) || cross(&apex, &right, &gate.left) > 0.0 {
                left = gate.left;
                left_i = i;
            } else {
                corners.push((right, portals[right_i].right_wrap));
                apex = right;
                left = apex;
                left_i = right_i;
                i = right_i + 1;
                continue;
            }
        }
        i += 1;
    }
    corners.push((*to, None));
    corners.dedup_by(|b, a| same(&a.0, &b.0));
    (0..corners.len())
        .map(|k| Bend {
            at: corners[k].0,
            wrap: corners[k].1,
            prev: corners[k.saturating_sub(1)].0,
            next: corners[(k + 1).min(corners.len() - 1)].0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> FloatPoint {
        FloatPoint::new(x, y)
    }

    fn gate(left: FloatPoint, right: FloatPoint) -> Gate {
        Gate { left, right, left_wrap: None, right_wrap: None }
    }

    #[test]
    fn an_open_channel_is_one_straight_line() {
        let gates = [gate(p(1.0, 1.0), p(1.0, -1.0)), gate(p(2.0, 1.0), p(2.0, -1.0))];
        let bends = pull(&p(0.0, 0.0), &gates, &p(3.0, 0.0));
        let at: Vec<(f64, f64)> = bends.iter().map(|b| (b.at.x, b.at.y)).collect();
        assert_eq!(at, [(0.0, 0.0), (3.0, 0.0)]);
    }

    #[test]
    fn a_narrowed_gate_bends_the_path_round_its_end() {
        let gates = [gate(p(1.0, 2.0), p(1.0, 1.0))];
        let bends = pull(&p(0.0, 0.0), &gates, &p(2.0, 0.0));
        let at: Vec<(f64, f64)> = bends.iter().map(|b| (b.at.x, b.at.y)).collect();
        assert_eq!(at, [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]);
    }

    #[test]
    fn a_wrapped_bend_keeps_nearly_its_reach_from_the_sample() {
        let wrap = Some((p(1.0, 0.0), 0.5));
        let gates = [Gate { left: p(1.0, 2.0), right: p(1.0, 0.5), left_wrap: None, right_wrap: wrap }];
        let bends = pull(&p(0.0, -1.0), &gates, &p(2.0, -1.0));
        let points: Vec<FloatPoint> = bends.iter().flat_map(Bend::points).collect();
        assert!(points.len() > 3, "{points:?}");
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let t = (((1.0 - a.x) * dx + (0.0 - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let d = (a.x + t * dx - 1.0).hypot(a.y + t * dy);
            assert!(d >= 0.45, "{a:?} {b:?} at {d}");
        }
    }
}
