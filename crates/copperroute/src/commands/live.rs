use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use copper_board::board::watch::{BoardWatcher, WatchGuard};
use copper_board::structure::Unit;
use copper_board::{Board, Item};
use copper_geometry::{FloatPoint, Shape, ShapeOps};
use copper_router::{MazeProbe, MazeWatchGuard, MazeWatcher};
use serde_json::{Value, json};

const PROBES_PER_FRAME: usize = 48;
const AIRLINE_INTERVAL: Duration = Duration::from_millis(400);

/// Newline-delimited JSON frames of the board as it changes: a `board` header, then `frame`s
/// holding only what was added, removed or moved since the last one, then `done`.
pub struct LiveStream {
    started: Instant,
    interval_ns: u64,
    /// No frames until the header is out: the board is built item by item while it loads.
    next_frame_ns: AtomicU64,
    inner: Mutex<Inner>,
}

struct Inner {
    out: Box<dyn Write + Send>,
    per_mm: f64,
    seq: u64,
    sent: HashMap<u32, u64>,
    parts: HashMap<i32, (i64, i64)>,
    probes: Vec<Value>,
    events: Vec<Value>,
    last_airlines: Option<Instant>,
    broken: bool,
}

pub struct Watching {
    pub stream: Arc<LiveStream>,
    _board: WatchGuard,
    _maze: MazeWatchGuard,
}

pub fn start(out: Box<dyn Write + Send>, fps: f64) -> Watching {
    let stream = Arc::new(LiveStream {
        started: Instant::now(),
        interval_ns: (1e9 / fps.max(1.0)) as u64,
        next_frame_ns: AtomicU64::new(u64::MAX),
        inner: Mutex::new(Inner {
            out,
            per_mm: 1.0,
            seq: 0,
            sent: HashMap::new(),
            parts: HashMap::new(),
            probes: Vec::new(),
            events: Vec::new(),
            last_airlines: None,
            broken: false,
        }),
    });
    Watching {
        _board: copper_board::board::watch::watch(stream.clone()),
        _maze: copper_router::watch_maze(stream.clone()),
        stream,
    }
}

impl LiveStream {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn due(&self) -> bool {
        self.now_ns() >= self.next_frame_ns.load(Ordering::Relaxed)
    }

    pub fn header(&self, board: &Board) {
        let mut inner = self.lock();
        inner.per_mm = board.communication.get_resolution(Unit::Mm);
        let per_mm = inner.per_mm;
        let layers: Vec<Value> = board
            .layer_structure()
            .layers
            .iter()
            .map(|l| json!({"name": l.name, "signal": l.is_signal}))
            .collect();
        let nets: BTreeSet<i32> = board.get_items().flat_map(|i| i.net_nos().iter().copied()).collect();
        let nets: BTreeMap<String, String> = nets
            .into_iter()
            .filter_map(|n| board.rules.nets.get(n).map(|net| (n.to_string(), net.name.clone())))
            .collect();
        let bbox = board.get_bounding_box();
        let line = json!({
            "t": "board",
            "layers": layers,
            "nets": nets,
            "bbox": [mm(bbox.ll.x, per_mm), mm(bbox.ll.y, per_mm), mm(bbox.ur.x, per_mm), mm(bbox.ur.y, per_mm)],
        });
        inner.write(&line);
        drop(inner);
        self.frame(board, true);
    }

    pub fn event(&self, event: Value) {
        self.lock().events.push(event);
    }

    pub fn done(&self, board: &Board, summary: Value) {
        self.frame(board, true);
        let mut inner = self.lock();
        let mut line = json!({"t": "done", "open": copper_drc::all_airlines(board).len()});
        if let (Some(line), Value::Object(summary)) = (line.as_object_mut(), summary) {
            line.extend(summary);
        }
        inner.write(&line);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn frame(&self, board: &Board, force: bool) {
        let mut inner = if force {
            self.lock()
        } else {
            match self.inner.try_lock() {
                Ok(inner) => inner,
                Err(_) => return,
            }
        };
        if inner.broken {
            return;
        }
        let now = self.now_ns();
        self.next_frame_ns.store(now + self.interval_ns, Ordering::Relaxed);
        let per_mm = inner.per_mm;
        let ctx = board.ctx();

        let mut add = Vec::new();
        let mut seen = HashMap::with_capacity(inner.sent.len());
        for item in board.get_items() {
            let id = item.id().0;
            let print = fingerprint(item, &ctx);
            if inner.sent.get(&id) != Some(&print) {
                add.push(describe(board, item, per_mm));
            }
            seen.insert(id, print);
        }
        let del: Vec<u32> = inner.sent.keys().filter(|id| !seen.contains_key(id)).copied().collect();
        inner.sent = seen;

        let mut parts = Vec::new();
        for component in board.components.get_all() {
            let Some(at) = component.get_location() else { continue };
            let at = at.to_float();
            let at = (at.x.round() as i64, at.y.round() as i64);
            if inner.parts.get(&component.id) != Some(&at) {
                parts.push(json!({
                    "id": component.id,
                    "name": component.name,
                    "at": [round(at.0 as f64 / per_mm), round(at.1 as f64 / per_mm)],
                    "fixed": component.position_fixed,
                }));
                inner.parts.insert(component.id, at);
            }
        }

        let airlines_due = force || inner.last_airlines.is_none_or(|t| t.elapsed() >= AIRLINE_INTERVAL);
        let airlines = airlines_due.then(|| {
            inner.last_airlines = Some(Instant::now());
            copper_drc::all_airlines(board)
                .iter()
                .map(|a| {
                    json!([
                        round(a.from_corner.x / per_mm),
                        round(a.from_corner.y / per_mm),
                        round(a.to_corner.x / per_mm),
                        round(a.to_corner.y / per_mm),
                        a.net_number
                    ])
                })
                .collect::<Vec<_>>()
        });

        if add.is_empty() && del.is_empty() && parts.is_empty() && inner.probes.is_empty() && inner.events.is_empty() && airlines.is_none() {
            return;
        }
        inner.seq += 1;
        let mut line = json!({
            "t": "frame",
            "seq": inner.seq,
            "ms": now / 1_000_000,
            "add": add,
            "del": del,
            "parts": parts,
            "probes": std::mem::take(&mut inner.probes),
            "events": std::mem::take(&mut inner.events),
        });
        if let Some(airlines) = airlines {
            line["open"] = json!(airlines.len());
            line["air"] = Value::Array(airlines);
        }
        inner.write(&line);
    }
}

impl Inner {
    fn write(&mut self, line: &Value) {
        let written = serde_json::to_writer(&mut self.out, line)
            .map_err(std::io::Error::other)
            .and_then(|()| self.out.write_all(b"\n"))
            .and_then(|()| self.out.flush());
        if let Err(error) = written {
            tracing::warn!("live stream stopped: {error}");
            self.broken = true;
        }
    }
}

impl BoardWatcher for LiveStream {
    fn changed(&self, board: &Board) {
        if self.due() {
            self.frame(board, false);
        }
    }
}

impl MazeWatcher for LiveStream {
    fn wants_probe(&self) -> bool {
        self.inner.try_lock().is_ok_and(|inner| inner.probes.len() < PROBES_PER_FRAME)
    }

    fn probe(&self, probe: MazeProbe<'_>) {
        if let Ok(mut inner) = self.inner.try_lock() {
            let per_mm = inner.per_mm;
            inner.probes.push(json!({
                "net": probe.net,
                "layer": probe.layer,
                "queue": probe.queue,
                "room": flat(&probe.room, per_mm),
                "door": flat(&[probe.door.0, probe.door.1], per_mm),
            }));
        }
        if self.due() {
            self.frame(probe.board, false);
        }
    }
}

fn round(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

fn mm(v: i32, per_mm: f64) -> f64 {
    round(f64::from(v) / per_mm)
}

fn flat(points: &[FloatPoint], per_mm: f64) -> Vec<f64> {
    points.iter().flat_map(|p| [round(p.x / per_mm), round(p.y / per_mm)]).collect()
}

fn fingerprint(item: &Item, ctx: &copper_board::ItemCtx<'_>) -> u64 {
    let mut h = DefaultHasher::new();
    std::mem::discriminant(item).hash(&mut h);
    item.net_nos().hash(&mut h);
    match item {
        Item::Trace(trace) => {
            trace.get_layer().hash(&mut h);
            trace.get_half_width().hash(&mut h);
            for p in trace.polyline().corner_approx_arr() {
                p.x.to_bits().hash(&mut h);
                p.y.to_bits().hash(&mut h);
            }
        }
        Item::Via(via) => {
            let c = via.get_center().to_float();
            (c.x.to_bits(), c.y.to_bits()).hash(&mut h);
        }
        Item::Pin(pin) => {
            let c = pin.get_center(ctx).to_float();
            (c.x.to_bits(), c.y.to_bits()).hash(&mut h);
        }
        _ => {
            let b = item.bounding_box(ctx);
            (b.ll.x, b.ll.y, b.ur.x, b.ur.y).hash(&mut h);
        }
    }
    h.finish()
}

fn shape(shape: &Shape, per_mm: f64) -> Value {
    match shape {
        Shape::Circle(c) => json!({"c": [mm(c.center.x, per_mm), mm(c.center.y, per_mm), round(f64::from(c.radius) / per_mm)]}),
        _ => json!({"g": flat(&shape.corner_approx_arr(), per_mm)}),
    }
}

fn describe(board: &Board, item: &Item, per_mm: f64) -> Value {
    let ctx = board.ctx();
    let id = item.id().0;
    let net = item.net_nos().first().copied().unwrap_or(0);
    let fixed = item.get_fixed_state() >= copper_board::structure::FixedState::SystemFixed;
    match item {
        Item::Trace(trace) => json!({
            "id": id, "k": "trace", "net": net, "fixed": fixed,
            "layer": trace.get_layer(),
            "w": round(f64::from(2 * trace.get_half_width()) / per_mm),
            "p": flat(&trace.polyline().corner_approx_arr(), per_mm),
        }),
        Item::Via(via) => {
            let c = via.get_center().to_float();
            json!({
                "id": id, "k": "via", "net": net, "fixed": fixed,
                "at": [round(c.x / per_mm), round(c.y / per_mm)],
                "layers": [via.first_layer(&ctx), via.last_layer(&ctx)],
                "shape": via.get_shape_on_layer(via.first_layer(&ctx), &ctx).map(|s| shape(&s, per_mm)),
                "drill": via.get_padstack(&ctx).map(|s| round(s.drill_radius() / per_mm)),
            })
        }
        Item::Pin(pin) => {
            let c = pin.get_center(&ctx).to_float();
            let pads: Vec<Value> = (pin.first_layer(&ctx)..=pin.last_layer(&ctx))
                .filter_map(|layer| pin.get_shape_on_layer(layer, &ctx).map(|s| json!({"layer": layer, "shape": shape(&s, per_mm)})))
                .collect();
            json!({
                "id": id, "k": "pin", "net": net, "part": item.component_id(),
                "at": [round(c.x / per_mm), round(c.y / per_mm)],
                "pads": pads,
                "drill": (pin.first_layer(&ctx) != pin.last_layer(&ctx))
                    .then(|| pin.get_padstack(&ctx).map(|s| round(s.drill_radius() / per_mm)))
                    .flatten(),
            })
        }
        Item::BoardOutline(outline) => {
            let polys: Vec<Value> = (0..outline.shape_count())
                .filter_map(|i| outline.get_shape(i).map(|s| json!(flat(&s.as_ops().corner_approx_arr(), per_mm))))
                .collect();
            json!({"id": id, "k": "outline", "polys": polys})
        }
        _ => {
            let kind = match item {
                Item::ConductionArea(_) => "pour",
                Item::ComponentOutline(_) => "body",
                Item::ViaObstacleArea(_) => "via-keepout",
                _ => "keepout",
            };
            let tiles: Vec<Value> = (0..item.tile_shape_count(&ctx))
                .filter_map(|i| {
                    board.item_tile_shape_ref(item.id(), i).map(|s| {
                        json!({"layer": item.shape_layer(i, &ctx), "g": flat(&s.corner_approx_arr(), per_mm)})
                    })
                })
                .collect();
            json!({"id": id, "k": kind, "net": net, "part": item.component_id(), "tiles": tiles})
        }
    }
}
