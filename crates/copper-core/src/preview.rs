use copper_board::{Board, Item};
use copper_geometry::{FloatPoint, Shape, ShapeOps};

pub fn layer_color(board: &Board, layer: usize) -> &'static str {
    let name = &board.layer_structure().layers[layer].name;
    if name == "F.Cu" || layer == 0 {
        "#f26d78"
    } else if name == "B.Cu" || layer + 1 == board.get_layer_count() {
        "#68b8ff"
    } else {
        ["#ce9fff", "#ffa657", "#83c5af", "#ff8fc8"][(layer - 1) % 4]
    }
}
fn points(ps: &[FloatPoint]) -> String {
    ps.iter()
        .map(|p| format!("{},{}", p.x, -p.y))
        .collect::<Vec<_>>()
        .join(" ")
}
fn shape_svg(shape: &Shape, fill: &str, opacity: f64) -> String {
    match shape {
        Shape::Circle(c) => format!(
            r#"<circle cx="{}" cy="{}" r="{}" fill="{fill}" opacity="{opacity}"/>"#,
            c.center.x,
            -f64::from(c.center.y),
            c.radius
        ),
        _ => format!(
            r#"<polygon points="{}" fill="{fill}" opacity="{opacity}"/>"#,
            points(&shape.corner_approx_arr())
        ),
    }
}
/// The board as SVG in board units with y flipped, the parts in `highlight` drawn white, and
/// `overlay` (SVG in the same coordinates) drawn over the copper.
pub fn board_svg(board: &Board, highlight: &[i32], overlay: &str) -> String {
    board_svg_in(board, highlight, overlay, None)
}

/// `board_svg` with the view fixed to `view` (x0, y0, x1, y1 in board units) instead of the
/// board's bounding box, so frames of a changing board line up.
pub fn board_svg_in(
    board: &Board,
    highlight: &[i32],
    overlay: &str,
    view: Option<[f64; 4]>,
) -> String {
    let bbox = board.get_bounding_box();
    let [x0, y0, x1, y1] = view.unwrap_or([
        f64::from(bbox.ll.x),
        f64::from(bbox.ll.y),
        f64::from(bbox.ur.x),
        f64::from(bbox.ur.y),
    ]);
    let margin = (x1 - x0).max(y1 - y0).max(1.0) * 0.03;
    let x = x0 - margin;
    let y = -y1 - margin;
    let w = (x1 - x0).max(1.0) + margin * 2.0;
    let h = (y1 - y0).max(1.0) + margin * 2.0;
    let mut out = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x} {y} {w} {h}" role="img" aria-label="DSN board preview"><rect x="{x}" y="{y}" width="{w}" height="{h}" fill="#101c25"/>"##
    );
    let ctx = board.ctx();
    for item in board.get_items() {
        match item {
            Item::BoardOutline(outline) => {
                for i in 0..outline.shape_count() {
                    if let Some(shape) = outline.get_shape(i) {
                        out.push_str(&format!(
                            r##"<polygon points="{}" fill="none" stroke="#83c5af" stroke-width="{}"/>"##,
                            points(&shape.as_ops().corner_approx_arr()), margin / 12.0
                        ));
                    }
                }
            }
            Item::Trace(trace) => {
                out.push_str(&format!(
                    r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round"/>"#,
                    points(&trace.polyline().corner_approx_arr()),
                    layer_color(board, trace.get_layer()), 2 * trace.get_half_width()
                ));
            }
            Item::Pin(pin) => {
                let fill = if highlight.contains(&item.component_id()) {
                    "#ffffff"
                } else {
                    "#edcf86"
                };
                for layer in pin.first_layer(&ctx)..=pin.last_layer(&ctx) {
                    if let Some(shape) = pin.get_shape_on_layer(layer, &ctx) {
                        out.push_str(&shape_svg(&shape, fill, 1.0));
                    }
                }
                if pin.first_layer(&ctx) != pin.last_layer(&ctx) {
                    if let Some(stack) = pin.get_padstack(&ctx) {
                        out.push_str(&drill_svg(
                            pin.get_center(&ctx).to_float(),
                            stack.drill_radius(),
                        ));
                    }
                }
            }
            Item::Via(via) => {
                if let Some(shape) = via.get_shape_on_layer(via.first_layer(&ctx), &ctx) {
                    out.push_str(&shape_svg(&shape, "#edcf86", 1.0));
                }
                if let Some(stack) = via.get_padstack(&ctx) {
                    out.push_str(&drill_svg(
                        via.get_center().to_float(),
                        stack.drill_radius(),
                    ));
                }
            }
            _ => {
                for i in 0..item.tile_shape_count(&ctx) {
                    if let Some(shape) = board.item_tile_shape_ref(item.id(), i) {
                        out.push_str(&format!(
                            r##"<polygon points="{}" fill="#83c5af" opacity="0.15"/>"##,
                            points(&shape.corner_approx_arr())
                        ));
                    }
                }
            }
        }
    }
    out.push_str(overlay);
    out.push_str(&airline_svg(
        &copper_drc::all_airlines(board),
        margin / 20.0,
    ));
    out.push_str("</svg>");
    out
}

fn airline_svg(airlines: &[copper_drc::AirLine], width: f64) -> String {
    if airlines.is_empty() {
        return String::new();
    }
    let d = airlines
        .iter()
        .map(|a| {
            format!(
                "M {} {} L {} {}",
                a.from_corner.x, -a.from_corner.y, a.to_corner.x, -a.to_corner.y
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r##"<g class="ratsnest" fill="none" stroke="#e8f0f2" stroke-width="{width}" stroke-dasharray="{} {}" opacity="0.4"><path d="{d}"/></g>"##,
        width * 6.0,
        width * 4.0
    )
}

fn drill_svg(center: FloatPoint, radius: f64) -> String {
    format!(
        r##"<circle cx="{}" cy="{}" r="{radius}" fill="#101c25"/>"##,
        center.x, -center.y
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use copper_board::ItemId;
    use copper_drc::AirLine;

    fn airline(from: (f64, f64), to: (f64, f64)) -> AirLine {
        AirLine::new(
            1,
            ItemId(0),
            FloatPoint {
                x: from.0,
                y: from.1,
            },
            ItemId(1),
            FloatPoint { x: to.0, y: to.1 },
        )
    }

    #[test]
    fn airlines_are_drawn_as_one_path_with_the_y_flip_the_rest_of_the_svg_uses() {
        let svg = airline_svg(&[airline((0.0, 5.0), (10.0, 20.0))], 1.0);
        assert!(svg.contains(r#"<path d="M 0 -5 L 10 -20"/>"#), "{svg}");
        assert!(svg.contains(r#"class="ratsnest""#), "{svg}");
    }

    #[test]
    fn several_airlines_share_the_one_path() {
        let svg = airline_svg(
            &[
                airline((0.0, 4.0), (1.0, 1.0)),
                airline((2.0, 2.0), (3.0, 3.0)),
            ],
            1.0,
        );
        assert!(svg.contains(r#"d="M 0 -4 L 1 -1 M 2 -2 L 3 -3""#), "{svg}");
    }

    #[test]
    fn a_fully_routed_board_draws_no_ratsnest_group_at_all() {
        assert_eq!(airline_svg(&[], 1.0), "");
    }
}
