//! Opt-in, disk-bounded SVG capture for inspecting the maze router.
//!
//! Rendering is deliberately isolated here. The routing algorithm only calls the cheap capture
//! hooks below; when no recorder is active they return after one relaxed atomic load.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use copper_board::{Board, Item};
use copper_geometry::{FloatPoint, TileShape};

use crate::autoroute::expansion::RoomRef;
use crate::autoroute::maze::{AutorouteEngine, MazeListElement};

thread_local! {
    static RECORDER: RefCell<Option<SvgRecorder>> = const { RefCell::new(None) };
}
static ACTIVE_RECORDERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
pub struct RoutingVisualizationOptions {
    pub output_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Capture one frame for every N observed maze steps. Route commits are always retained.
    pub every: u64,
    /// Hard disk-space guard. Routing continues after this many frames.
    pub max_frames: u64,
}

impl RoutingVisualizationOptions {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            width: 1280,
            height: 720,
            every: 1,
            max_frames: 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingVisualizationSummary {
    pub output_dir: PathBuf,
    pub observed_steps: u64,
    pub frames_written: u64,
    pub reached_frame_limit: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct RoutingVisualizationGuard {
    active: bool,
    output_dir: PathBuf,
    _not_send: PhantomData<Rc<()>>,
}

impl RoutingVisualizationGuard {
    pub fn finish(mut self) -> RoutingVisualizationSummary {
        self.active = false;
        finish_recorder(&self.output_dir)
    }
}

impl Drop for RoutingVisualizationGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = finish_recorder(&self.output_dir);
        }
    }
}

/// Starts visualization on the current routing thread.
///
/// The destination must be absent or empty. Existing frames are never overwritten.
pub fn start_routing_visualization(
    options: RoutingVisualizationOptions,
) -> io::Result<RoutingVisualizationGuard> {
    let already_active = RECORDER.with(|slot| slot.borrow().is_some());
    if already_active {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a routing visualization is already active on this thread",
        ));
    }
    validate_options(&options)?;
    fs::create_dir_all(&options.output_dir)?;
    if fs::read_dir(&options.output_dir)?
        .next()
        .transpose()?
        .is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "visualization directory '{}' is not empty",
                options.output_dir.display()
            ),
        ));
    }

    let index = File::create_new(options.output_dir.join("frames.jsonl"))?;
    let output_dir = options.output_dir.clone();
    let recorder = SvgRecorder {
        options,
        index: BufWriter::new(index),
        observed_steps: 0,
        frames_written: 0,
        error: None,
        phase: "autorouter",
        pass: 0,
    };
    RECORDER.with(|slot| *slot.borrow_mut() = Some(recorder));
    ACTIVE_RECORDERS.fetch_add(1, Ordering::Relaxed);
    Ok(RoutingVisualizationGuard {
        active: true,
        output_dir,
        _not_send: PhantomData,
    })
}

fn validate_options(options: &RoutingVisualizationOptions) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if options.width < 64 || options.height < 64 {
        return Err(invalid(
            "visualization width and height must both be at least 64 pixels",
        ));
    }
    if options.every == 0 || options.max_frames == 0 {
        return Err(invalid(
            "visualization every and max-frames must both be greater than zero",
        ));
    }
    Ok(())
}

fn finish_recorder(output_dir: &Path) -> RoutingVisualizationSummary {
    RECORDER.with(|slot| {
        let Some(mut recorder) = slot.borrow_mut().take() else {
            return RoutingVisualizationSummary {
                output_dir: output_dir.to_path_buf(),
                observed_steps: 0,
                frames_written: 0,
                reached_frame_limit: false,
                error: Some("visualization recorder was not active".to_string()),
            };
        };
        ACTIVE_RECORDERS.fetch_sub(1, Ordering::Relaxed);
        if let Err(error) = recorder.index.flush() {
            recorder.note_error(error);
        }
        if recorder.error.is_none()
            && let Err(error) = write_viewer(&recorder.options.output_dir, recorder.frames_written)
        {
            recorder.note_error(error);
        }
        RoutingVisualizationSummary {
            output_dir: recorder.options.output_dir,
            observed_steps: recorder.observed_steps,
            frames_written: recorder.frames_written,
            reached_frame_limit: recorder.frames_written >= recorder.options.max_frames,
            error: recorder.error,
        }
    })
}

pub(crate) fn capture_maze_step(
    board: &Board,
    engine: &AutorouteEngine,
    element: &MazeListElement,
    queue_len: usize,
) {
    crate::live::maze_step(board, engine, element, queue_len);
    if ACTIVE_RECORDERS.load(Ordering::Relaxed) == 0 {
        return;
    }
    capture(
        FrameContext {
            board,
            engine,
            event: "maze-pop",
            current_room: element.next_room,
            entry: Some((element.shape_entry.a, element.shape_entry.b)),
            queue_len,
            expansion_cost: Some(element.expansion_value),
            sorting_cost: Some(element.sorting_value),
        },
        false,
    );
}

#[derive(Clone, Copy)]
pub(crate) enum RoutingPhase {
    Fanout,
    Autorouter,
    Optimizer,
}

pub(crate) fn set_route_context(phase: RoutingPhase, pass: i32) {
    if ACTIVE_RECORDERS.load(Ordering::Relaxed) == 0 {
        return;
    }
    RECORDER.with(|slot| {
        if let Some(recorder) = slot.borrow_mut().as_mut() {
            recorder.phase = match phase {
                RoutingPhase::Fanout => "fanout",
                RoutingPhase::Autorouter => "autorouter",
                RoutingPhase::Optimizer => "optimizer",
            };
            recorder.pass = pass;
        }
    });
}

pub(crate) fn capture_route_committed(board: &Board, engine: &AutorouteEngine) {
    if ACTIVE_RECORDERS.load(Ordering::Relaxed) == 0 {
        return;
    }
    capture(
        FrameContext {
            board,
            engine,
            event: "route-committed",
            current_room: None,
            entry: None,
            queue_len: 0,
            expansion_cost: None,
            sorting_cost: None,
        },
        true,
    );
}

fn capture(context: FrameContext<'_>, force: bool) {
    RECORDER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(recorder) = slot.as_mut() else {
            return;
        };
        recorder.observed_steps = recorder.observed_steps.saturating_add(1);
        if recorder.error.is_some()
            || (!force && (recorder.observed_steps - 1) % recorder.options.every != 0)
        {
            return;
        }
        if recorder.frames_written >= recorder.options.max_frames {
            return;
        }
        if let Err(error) = recorder.write_frame(context) {
            recorder.note_error(error);
        }
    });
}

struct SvgRecorder {
    options: RoutingVisualizationOptions,
    index: BufWriter<File>,
    observed_steps: u64,
    frames_written: u64,
    error: Option<String>,
    phase: &'static str,
    pass: i32,
}

impl SvgRecorder {
    fn write_frame(&mut self, context: FrameContext<'_>) -> io::Result<()> {
        let frame_no = self.frames_written + 1;
        let filename = format!("frame-{frame_no:08}.svg");
        let svg = render_svg(
            &context,
            self.phase,
            self.pass,
            self.options.width,
            self.options.height,
        );
        let mut file = File::create_new(self.options.output_dir.join(&filename))?;
        file.write_all(svg.as_bytes())?;
        writeln!(
            self.index,
            "{{\"frame\":{frame_no},\"step\":{},\"phase\":\"{}\",\"pass\":{},\"event\":\"{}\",\"net\":{},\"layer\":{},\"queue\":{},\"file\":\"{}\"}}",
            self.observed_steps,
            self.phase,
            self.pass,
            context.event,
            context.engine.get_net_number(),
            context.current_layer().map_or(-1, |layer| layer as i64),
            context.queue_len,
            filename
        )?;
        self.frames_written = frame_no;
        Ok(())
    }

    fn note_error(&mut self, error: io::Error) {
        if self.error.is_none() {
            self.error = Some(error.to_string());
        }
    }
}

struct FrameContext<'a> {
    board: &'a Board,
    engine: &'a AutorouteEngine,
    event: &'static str,
    current_room: Option<RoomRef>,
    entry: Option<(FloatPoint, FloatPoint)>,
    queue_len: usize,
    expansion_cost: Option<f64>,
    sorting_cost: Option<f64>,
}

impl FrameContext<'_> {
    fn current_layer(&self) -> Option<usize> {
        self.current_room
            .and_then(|room| self.engine.rooms.room_layer(self.board, room))
    }

    fn render_room(
        &self,
        svg: &mut String,
        room: RoomRef,
        shape: Option<&TileShape>,
        styles: RoomStyles,
        view: Viewport,
    ) {
        let Some(shape) = shape else { return };
        let style = if self.current_room == Some(room) {
            styles.1
        } else {
            styles.0
        };
        render_shape(svg, shape, view, style);
    }
}

#[derive(Clone, Copy)]
struct Viewport {
    min_x: f64,
    min_y: f64,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    height: f64,
}

impl Viewport {
    fn new(board: &Board, width: u32, height: u32) -> Self {
        let bounds = board.get_bounding_box();
        let min_x = f64::from(bounds.ll.x);
        let min_y = f64::from(bounds.ll.y);
        let board_width = f64::from(bounds.ur.x - bounds.ll.x).abs().max(1.0);
        let board_height = f64::from(bounds.ur.y - bounds.ll.y).abs().max(1.0);
        let drawable_width = (f64::from(width) - 48.0).max(1.0);
        let drawable_height = (f64::from(height) - 48.0).max(1.0);
        let scale = (drawable_width / board_width).min(drawable_height / board_height);
        Self {
            min_x,
            min_y,
            scale,
            offset_x: (f64::from(width) - board_width * scale) / 2.0,
            offset_y: (f64::from(height) - board_height * scale) / 2.0,
            height: f64::from(height),
        }
    }

    fn point(self, point: FloatPoint) -> (f64, f64) {
        let x = self.offset_x + (point.x - self.min_x) * self.scale;
        let from_bottom = self.offset_y + (point.y - self.min_y) * self.scale;
        (x, self.height - from_bottom)
    }
}

#[derive(Clone, Copy)]
struct Style(&'static str, &'static str, f64, f64);
type RoomStyles = (Style, Style);

const ACTIVE_ROOM: Style = Style("#fff176", "#fffde7", 0.30, 3.0);
const COMPLETE_ROOM: RoomStyles = (Style("#35d0ba", "#73f5df", 0.10, 1.2), ACTIVE_ROOM);
const OBSTACLE_ROOM: RoomStyles = (
    Style("#ff5964", "#ff8b93", 0.11, 1.0),
    Style("#fff176", "#fffde7", 0.34, 3.0),
);
const INCOMPLETE_ROOM: RoomStyles = (
    Style("none", "#d3baff", 0.72, 1.0),
    Style("none", "#fff176", 1.0, 3.0),
);

fn render_svg(
    context: &FrameContext<'_>,
    phase: &str,
    pass: i32,
    width: u32,
    height: u32,
) -> String {
    let view = Viewport::new(context.board, width, height);
    let layer = context.current_layer();
    let mut svg = String::with_capacity(64 * 1024);
    let _ = writeln!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"##
    );
    svg.push_str(
        r##"<rect width="100%" height="100%" fill="#10151d"/>
<g stroke-linejoin="round" stroke-linecap="round">"##,
    );

    render_board_items(&mut svg, context.board, context.engine, layer, view);
    render_rooms(&mut svg, context, layer, view);

    if let Some((a, b)) = context.entry {
        let (ax, ay) = view.point(a);
        let (bx, by) = view.point(b);
        let _ = writeln!(
            svg,
            r##"<line x1="{ax:.2}" y1="{ay:.2}" x2="{bx:.2}" y2="{by:.2}" stroke="#ff5af7" stroke-width="4" vector-effect="non-scaling-stroke"/>"##
        );
    }
    svg.push_str("</g>\n");

    let expansion = context
        .expansion_cost
        .map_or_else(|| "-".to_string(), |value| format!("{value:.2}"));
    let sorting = context
        .sorting_cost
        .map_or_else(|| "-".to_string(), |value| format!("{value:.2}"));
    let layer_text = layer.map_or_else(|| "-".to_string(), |value| value.to_string());
    let _ = writeln!(
        svg,
        r##"<rect x="12" y="12" width="450" height="60" rx="6" fill="#080b10" fill-opacity=".88"/>
<text x="24" y="36" fill="#ffffff" font-family="ui-monospace,monospace" font-size="16">stage: {} · pass: {} · {} · net {} · layer {} · queue {}</text>
<text x="24" y="59" fill="#aab8ca" font-family="ui-monospace,monospace" font-size="14">expansion {} · priority {}</text>
</svg>"##,
        phase,
        pass,
        context.event,
        context.engine.get_net_number(),
        layer_text,
        context.queue_len,
        expansion,
        sorting
    );
    svg
}

fn render_board_items(
    svg: &mut String,
    board: &Board,
    engine: &AutorouteEngine,
    layer: Option<usize>,
    view: Viewport,
) {
    for id in board.items_in_board_order().into_iter().rev() {
        let Some(item) = board.get_item(id) else {
            continue;
        };
        let style = match item {
            Item::Trace(_) => Style("#48b8ff", "#8bd3ff", 0.78, 1.0),
            Item::Via(_) => Style("#ffd166", "#fff0a8", 0.92, 1.0),
            Item::Pin(_) => Style("#ff9f43", "#ffd0a1", 0.82, 1.0),
            Item::ConductionArea(_) => Style("#375d49", "#68a77e", 0.40, 1.0),
            Item::BoardOutline(_) => Style("none", "#e8eef7", 0.95, 1.0),
            Item::ObstacleArea(_)
            | Item::ViaObstacleArea(_)
            | Item::ComponentObstacleArea(_)
            | Item::ComponentOutline(_) => Style("#566171", "#8290a3", 0.48, 1.0),
        };
        let shape_count = item.tile_shape_count(&board.ctx());
        for index in 0..shape_count {
            if layer.is_some_and(|wanted| {
                board
                    .item_shape_layer(id, index)
                    .is_some_and(|actual| actual != wanted)
            }) {
                continue;
            }
            let Some(shape) = board.item_tree_shape_ref(id, engine.tree, index) else {
                continue;
            };
            render_shape(svg, &shape, view, style);
        }
    }
}

fn render_rooms(
    svg: &mut String,
    context: &FrameContext<'_>,
    layer: Option<usize>,
    view: Viewport,
) {
    for (id, room) in context.engine.rooms.complete_rooms.iter() {
        let room_ref = RoomRef::Complete(copper_board::RoomId(id));
        if layer.is_some_and(|wanted| room.get_layer() != wanted) {
            continue;
        }
        context.render_room(svg, room_ref, room.get_shape(), COMPLETE_ROOM, view);
    }
    for (id, room) in context.engine.rooms.obstacle_rooms.iter() {
        let room_ref = RoomRef::Obstacle(copper_board::ObstacleRoomId(id));
        if layer.is_some_and(|wanted| room.get_layer(context.board) != Some(wanted)) {
            continue;
        }
        context.render_room(svg, room_ref, room.get_shape(), OBSTACLE_ROOM, view);
    }
    for (id, room) in context.engine.rooms.incomplete_rooms.iter() {
        let room_ref = RoomRef::Incomplete(crate::IncompleteRoomId(id));
        if layer.is_some_and(|wanted| room.get_layer() != wanted) {
            continue;
        }
        context.render_room(svg, room_ref, room.get_shape(), INCOMPLETE_ROOM, view);
    }
}

fn render_shape(svg: &mut String, shape: &TileShape, view: Viewport, style: Style) {
    let corners = shape.corner_approx_arr();
    if corners.len() < 2 {
        return;
    }
    svg.push_str("<polygon points=\"");
    for corner in corners {
        let (x, y) = view.point(corner);
        let _ = write!(svg, "{x:.2},{y:.2} ");
    }
    let Style(fill, stroke, opacity, width) = style;
    let _ = writeln!(
        svg,
        r##"" fill="{}" stroke="{}" fill-opacity="{:.3}" stroke-opacity="{:.3}" stroke-width="{:.2}" vector-effect="non-scaling-stroke"/>"##,
        fill,
        stroke,
        opacity,
        opacity.max(0.55),
        width
    );
}

fn write_viewer(directory: &Path, frame_count: u64) -> io::Result<()> {
    let html = format!(
        r##"<!doctype html>
<meta charset="utf-8">
<title>Copperroute maze visualization</title>
<style>
html,body{{margin:0;background:#080b10;color:#e8eef7;font:14px system-ui;height:100%}}
body{{display:grid;grid-template-rows:auto 1fr}} header{{padding:10px;display:flex;gap:12px;align-items:center}}
img{{width:100%;height:100%;object-fit:contain;min-height:0}} input[type=range]{{flex:1}}
</style>
<header><button id="play">Play</button><input id="seek" type="range" min="0" max="{}" value="0"><span id="label"></span></header>
<img id="frame" alt="routing frame">
<script>
const frameCount={}, image=document.querySelector('#frame'), seek=document.querySelector('#seek'), label=document.querySelector('#label'), play=document.querySelector('#play');
let current=0,timer=null; function show(n){{if(!frameCount)return;current=Math.max(0,Math.min(frameCount-1,n));seek.value=current;image.src=`frame-${{String(current+1).padStart(8,'0')}}.svg`;label.textContent=`${{current+1}} / ${{frameCount}}`;}}
function stop(){{clearInterval(timer);timer=null;play.textContent='Play';}} play.onclick=()=>{{if(timer){{stop();return}}play.textContent='Pause';timer=setInterval(()=>{{if(current+1>=frameCount)stop();else show(current+1)}},83)}};
seek.oninput=()=>show(+seek.value); addEventListener('keydown',e=>{{if(e.key==='ArrowRight')show(current+1);if(e.key==='ArrowLeft')show(current-1);if(e.key===' '){{e.preventDefault();play.click()}}}}); show(0);
</script>
"##,
        frame_count.saturating_sub(1),
        frame_count
    );
    fs::write(directory.join("viewer.html"), html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bound_disk_usage() {
        let options = RoutingVisualizationOptions::new(PathBuf::from("frames"));
        assert_eq!(options.every, 1);
        assert_eq!(options.max_frames, 1_000);
        assert_eq!((options.width, options.height), (1280, 720));
    }

    #[test]
    fn viewer_generates_fixed_width_frame_names() {
        let directory = std::env::temp_dir().join(format!(
            "copper-router-visualization-viewer-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        write_viewer(&directory, 2).unwrap();
        let viewer = fs::read_to_string(directory.join("viewer.html")).unwrap();
        assert!(viewer.contains("const frameCount=2"));
        assert!(viewer.contains("padStart(8,'0')"));
        fs::remove_dir_all(directory).unwrap();
    }
}
