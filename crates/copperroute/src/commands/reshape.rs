use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use copper_board::{Board, ItemId};
use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_geometry::{FloatPoint, IntVector};
use copper_morph::{PartMove, reshape_step};
use serde::Deserialize;

use crate::ExitCode;
use crate::cli::{Cli, ReshapeArgs};
use crate::ops::fixed_wiring;
use crate::ops::load::{BoardSource, LoadRequest, Loaded, load};

#[derive(Deserialize)]
struct Plan {
    board_outline: Vec<[f64; 2]>,
    view: Option<[f64; 4]>,
    steps: Vec<PlanStep>,
    /// Parts that move as one rigid body.
    #[serde(default)]
    bodies: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct PlanStep {
    label: String,
    outline: Vec<[f64; 2]>,
    /// Part reference to [dx, dy, turn]: millimetres and degrees from where the plan starts.
    parts: BTreeMap<String, [f64; 3]>,
}

pub fn run(cli: &Cli, args: &ReshapeArgs) -> ExitCode {
    let plan: Plan = match std::fs::read_to_string(&args.plan)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(plan) => plan,
        Err(error) => {
            tracing::error!("{}: {error}", args.plan.display());
            return ExitCode::UsageError;
        }
    };
    let Some(backend) = copper_core::backend(&args.repair_backend) else {
        tracing::error!("--repair-backend {}: no such backend", args.repair_backend);
        return ExitCode::UsageError;
    };
    let last = args.to.unwrap_or(plan.steps.len().saturating_sub(1));
    if plan.steps.is_empty() || last >= plan.steps.len() {
        tracing::error!("{}: no step {last}", args.plan.display());
        return ExitCode::UsageError;
    }

    let (mut loaded, original) = match load_routed(cli, &args.input, &args.ses, args.kicad_project.clone(), &args.set) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let board = &mut loaded.board;

    let per_mm = board.communication.get_resolution(Unit::Mm);
    let scale = loaded.transform.scale_factor();
    let (base_x, base_y) = (loaded.transform.base_x(), loaded.transform.base_y());
    let to_board = |p: &[f64; 2]| FloatPoint::new(p[0] * per_mm - base_x * scale, p[1] * per_mm - base_y * scale);
    let ids: BTreeMap<String, i32> = board
        .components
        .get_all()
        .map(|c| (c.name.clone(), c.id))
        .collect();
    let names: BTreeMap<i32, String> = ids.iter().map(|(name, id)| (*id, name.clone())).collect();
    let locked: BTreeSet<i32> = args.lock.iter().filter_map(|name| ids.get(name).copied()).collect();
    if let Some(name) = args.lock.iter().find(|name| !ids.contains_key(*name)) {
        tracing::error!("--lock {name}: no such part");
        return ExitCode::UsageError;
    }
    let mut bodies: Vec<Vec<i32>> = plan
        .bodies
        .iter()
        .map(|body| body.iter().filter_map(|name| ids.get(name).copied()).collect())
        .collect();
    let mut body_of: BTreeMap<i32, usize> = BTreeMap::new();
    for (b, body) in bodies.iter().enumerate() {
        for part in body {
            body_of.insert(*part, b);
        }
    }
    for id in ids.values() {
        body_of.entry(*id).or_insert_with(|| {
            bodies.push(vec![*id]);
            bodies.len() - 1
        });
    }

    let mut recording = None;
    if let Some(dir) = &args.frames {
        match super::frames::record(dir, plan.steps.len() + 1, String::new()) {
            Ok(r) => recording = Some(r),
            Err(error) => {
                tracing::error!("Couldn't record frames in {}: {error}", dir.display());
                return ExitCode::Failure;
            }
        }
    }
    if let (Some(r), Some(v)) = (&recording, plan.view) {
        let (a, b) = (to_board(&[v[0], v[1]]), to_board(&[v[2], v[3]]));
        r.writer.borrow_mut().view = Some([a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)]);
    }

    let baseline = violation_counts(board);
    let start_open = copper_drc::all_airlines(board).len();
    let mut allowed: BTreeMap<ViolationKey, usize> = BTreeMap::new();
    for found in violation_keys(board) {
        *allowed.entry(found.key).or_insert(0) += 1;
    }
    let pairs = diff_pairs(board);
    let protected: BTreeSet<i32> = pairs
        .iter()
        .flat_map(|(p, n)| [p, n])
        .flat_map(|name| board.rules.nets.get_by_name(name).into_iter().map(|net| net.net_number).collect::<Vec<_>>())
        .collect();
    tracing::info!("differential pairs, never ripped: {pairs:?}");
    let net_of = |name: &str| board.rules.nets.get_by_name(name).first().map(|n| n.net_number);
    let pair_nets: Vec<(i32, i32)> = pairs
        .iter()
        .filter_map(|(p, n)| Some((net_of(p)?, net_of(n)?)))
        .collect();
    if let Some(r) = &recording {
        let mut stats = step_stats("start", board, &baseline, start_open, 0.0);
        stats["errors"] = error_marks(board, &allowed);
        stats["pairs"] = pair_metrics(board, &pairs, per_mm);
        r.writer.borrow_mut().write_with(board, &["start"], &[], &IntVector::ZERO, stats);
    }

    let mut outline: Vec<FloatPoint> = plan.board_outline.iter().map(to_board).collect();
    let mut applied: BTreeMap<i32, [f64; 3]> = BTreeMap::new();
    let pause = copper_morph::frames::pause();
    let mut reached = 0;
    let mut open_nets: BTreeSet<(i32, usize)> = BTreeSet::new();
    for (k, step) in plan.steps.iter().enumerate().take(last + 1) {
        let started = std::time::Instant::now();
        let target: Vec<FloatPoint> = step.outline.iter().map(to_board).collect();
        if target.len() != outline.len() {
            tracing::error!("step {k}: {} outline points, expected {}", target.len(), outline.len());
            return ExitCode::Failure;
        }
        let mut moves = BTreeMap::new();
        for (name, [dx, dy, turn]) in &step.parts {
            let Some(&id) = ids.get(name) else {
                continue;
            };
            let done = applied.entry(id).or_insert([0.0; 3]);
            let part = PartMove {
                by: ((dx - done[0]) * per_mm, (dy - done[1]) * per_mm),
                turn_degree: turn - done[2],
            };
            *done = [*dx, *dy, *turn];
            if part.by.0.hypot(part.by.1) >= 1.0 || part.turn_degree.abs() > 1e-6 {
                moves.insert(id, part);
            }
        }
        let report = reshape_step(board, &outline, &target, &moves, args.cell_mm, &pair_nets);
        outline = target;
        let pressed = relax(board, args, &outline, k);
        if args.nudge_mm > 0.0 {
            let mut pushes: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
            for (part, (x, y)) in pressed {
                let p = pushes.entry(body_of[&part]).or_default();
                p.0 += x;
                p.1 += y;
            }
            let mut nudges = BTreeMap::new();
            for (body, (x, y)) in pushes {
                let parts = &bodies[body];
                let len = x.hypot(y);
                if parts.iter().any(|p| locked.contains(p)) || len < NUDGE_MIN_MM * per_mm {
                    continue;
                }
                let scale = len.min(args.nudge_mm * per_mm) / len;
                for &part in parts {
                    nudges.insert(part, PartMove { by: (x * scale, y * scale), turn_degree: 0.0 });
                }
            }
            if !nudges.is_empty() {
                let names: Vec<&str> = nudges.keys().filter_map(|id| names.get(id).map(String::as_str)).collect();
                tracing::info!("step {k}: nudging {names:?}");
                copper_morph::nudge_parts(board, &outline, &nudges, args.cell_mm, &pair_nets);
                relax(board, args, &outline, k);
            }
        }
        let due = if args.repair_at_keyframes {
            !step.label.contains('→')
        } else {
            args.repair_every > 0 && (k % args.repair_every == args.repair_every - 1 || k == last)
        };
        let ripped = if due {
            if args.reroute {
                reroute(board, &loaded.settings, args.repair_ms, backend, args.rip_design_wiring)
            } else {
                repair(board, &allowed, &protected, &loaded.settings, args.repair_ms, backend)
            }
        } else {
            0
        };
        let seconds = started.elapsed().as_secs_f64();
        let airlines = copper_drc::all_airlines(board);
        let open = airlines.len();
        if tracing::enabled!(tracing::Level::DEBUG) {
            let nets: BTreeSet<i32> = airlines.iter().map(|a| a.net_number).collect();
            for a in airlines.iter().filter(|a| !open_nets.contains(&(a.net_number, open_count(&airlines, a.net_number)))) {
                tracing::debug!(
                    "step {k}: open {} {:?} at ({:.0}, {:.0}) - {:?} at ({:.0}, {:.0})",
                    board.rules.nets.get(a.net_number).map_or(String::new(), |n| n.name.clone()),
                    board.get_item(a.from_item).map(Item::kind),
                    a.from_corner.x,
                    a.from_corner.y,
                    board.get_item(a.to_item).map(Item::kind),
                    a.to_corner.x,
                    a.to_corner.y
                );
            }
            open_nets = nets.iter().map(|n| (*n, open_count(&airlines, *n))).collect();
        }
        tracing::info!(
            "step {k} ({}): {} parts, {} traces, {} vias, {} areas; {ripped} ripped; {open} open; {seconds:.1} s",
            step.label,
            report.parts,
            report.traces,
            report.vias,
            report.areas
        );
        let mut stats = step_stats(&step.label, board, &baseline, open, seconds);
        stats["errors"] = error_marks(board, &allowed);
        stats["ripped"] = serde_json::json!(ripped);
        stats["pairs"] = pair_metrics(board, &pairs, per_mm);
        let new = stats["new violations"].as_i64().unwrap_or(0);
        if let Some(r) = &recording {
            r.writer.borrow_mut().write_with(board, &["reshape", &step.label], &[], &IntVector::ZERO, stats);
        }
        reached = k;
        if args.stop_at.is_some_and(|limit| new > limit) {
            tracing::warn!("step {k} ({}): {new} new violations, past --stop-at; stopping here", step.label);
            for id in new_violation_items(board, &allowed).into_iter().take(STOP_REPORT) {
                let at = board.get_bounding_box_of_items([id]);
                let item = board.get_item(id);
                tracing::warn!(
                    "  {:?} of {:?} at ({:.2}, {:.2}) mm",
                    item.map(Item::kind),
                    item.map(|i| i.net_nos().iter().filter_map(|n| board.rules.nets.get(*n).map(|net| net.name.clone())).collect::<Vec<_>>()),
                    (f64::from(at.ll.x + at.ur.x) / 2.0 / per_mm + base_x * scale / per_mm),
                    (f64::from(at.ll.y + at.ur.y) / 2.0 / per_mm + base_y * scale / per_mm),
                );
            }
            break;
        }
    }
    drop(pause);

    let outline_mm: Vec<[f64; 2]> = plan.steps[reached].outline.clone();
    let text = serde_json::json!({ "step": reached, "label": plan.steps[reached].label, "outline_mm": outline_mm });
    if let Err(error) = write_routed(&mut loaded, &original, &args.output)
        .and_then(|()| std::fs::write(args.output.with_extension("outline.json"), text.to_string()).map_err(|e| e.to_string()))
    {
        tracing::error!("Couldn't write {}: {error}", args.output.display());
        return ExitCode::Failure;
    }
    ExitCode::Ok
}

/// The trace relaxation and then the via relaxation, as asked; returns what the copper still
/// presses on the pads after the last of them.
fn relax(board: &mut Board, args: &ReshapeArgs, outline: &[FloatPoint], k: usize) -> BTreeMap<i32, (f64, f64)> {
    let mut pressed = BTreeMap::new();
    if args.relax > 0 {
        let r = copper_morph::relax_clearance(board, args.relax, Some(outline), &BTreeMap::new(), args.tension, false);
        tracing::info!(
            "step {k}: relaxed {} traces, overlaps {} -> {}",
            r.traces_moved,
            r.overlaps_before,
            r.overlaps_after
        );
        pressed = r.part_push;
    }
    if args.via_relax > 0 {
        let r = copper_morph::relax_clearance(board, args.via_relax, Some(outline), &BTreeMap::new(), args.tension, true);
        tracing::info!("step {k}: {} vias moved, overlaps {} -> {}", r.vias_moved, r.overlaps_before, r.overlaps_after);
        pressed = r.part_push;
    }
    pressed
}

/// Loads a board with a routed session on it, the session's routing unfixed so it can move.
/// Also returns the design's own fixed wiring, which `write_routed` needs.
pub(super) fn load_routed(
    cli: &Cli,
    input: &Path,
    ses: &Path,
    kicad_project: Option<PathBuf>,
    set: &[String],
) -> Result<(Loaded, BTreeSet<fixed_wiring::Wiring>), ExitCode> {
    let mut request = LoadRequest::for_board(BoardSource::Path(input.to_path_buf()));
    request.discover_adjacent_rules = true;
    request.kicad_project = kicad_project;
    request.settings = super::overrides(cli, set);
    let mut loaded = load(&request).map_err(|error| {
        tracing::error!("{error}");
        ExitCode::Failure
    })?;
    let board = &mut loaded.board;
    let original = fixed_wiring::system_fixed_wiring(board);
    let designed: BTreeSet<ItemId> = board
        .get_items()
        .filter(|i| i.is_trace() || matches!(i, Item::Via(_)))
        .map(Item::id)
        .collect();
    fixed_wiring::apply_sidecar(ses, board).map_err(|error| {
        tracing::error!("{error}");
        ExitCode::Failure
    })?;
    let session = std::fs::read(ses).map_err(|error| {
        tracing::error!("Couldn't read {}: {error}", ses.display());
        ExitCode::Failure
    })?;
    copper_dsn::ses_reader::read(&session[..], board, &loaded.transform).map_err(|error| {
        tracing::error!("Couldn't import {}: {error}", ses.display());
        ExitCode::Failure
    })?;
    // The session reader fixes what it imports; its routing is the router's to redo, unlike
    // wiring the design itself fixed.
    let routed: Vec<ItemId> = board
        .get_items()
        .filter(|i| (i.is_trace() || matches!(i, Item::Via(_))) && !designed.contains(&i.id()))
        .map(Item::id)
        .collect();
    for id in routed {
        if let Some(item) = board.get_item_mut(id) {
            item.unfix();
        }
    }
    Ok((loaded, original))
}

/// Writes the session and its sidecar naming the design wiring it replaced.
pub(super) fn write_routed(
    loaded: &mut Loaded,
    original: &BTreeSet<fixed_wiring::Wiring>,
    output: &Path,
) -> Result<(), String> {
    let superseded = fixed_wiring::supersede(&mut loaded.board, original, &loaded.transform);
    let mut bytes = Vec::new();
    copper_core::save_as_specctra_session_ses(&loaded.board, &loaded.transform, &loaded.job.name, &mut bytes)
        .map_err(|e| e.to_string())?;
    std::fs::write(output, &bytes).map_err(|e| e.to_string())?;
    fixed_wiring::write_sidecar(output, &superseded).map_err(|e| e.to_string())
}

fn open_count(airlines: &[copper_drc::AirLine], net: i32) -> usize {
    airlines.iter().filter(|a| a.net_number == net).count()
}

pub(super) fn violation_counts(board: &mut Board) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for v in copper_drc::DesignRulesChecker::new(board).get_all_violations() {
        let kind = format!("{:?}", v.kind);
        let name = kind.split(['(', ' ', '{']).next().unwrap_or_default().to_lowercase();
        *counts.entry(name).or_insert(0) += 1;
    }
    counts
}

pub(super) fn step_stats(
    label: &str,
    board: &mut Board,
    baseline: &BTreeMap<String, usize>,
    open: usize,
    seconds: f64,
) -> serde_json::Value {
    let now = violation_counts(board);
    let kinds: BTreeSet<&String> = now.keys().chain(baseline.keys()).collect();
    let added: BTreeMap<&String, i64> = kinds
        .into_iter()
        .map(|k| (k, now.get(k).copied().unwrap_or(0) as i64 - baseline.get(k).copied().unwrap_or(0) as i64))
        .filter(|(_, d)| *d != 0)
        .collect();
    let fixed = board
        .get_items()
        .filter(|i| matches!(i, Item::Trace(_)) && i.is_user_fixed())
        .count();
    serde_json::json!({
        "shape": label,
        "new violations": added.values().filter(|d| **d > 0).sum::<i64>(),
        "violation changes": serde_json::to_value(&added).unwrap_or_default(),
        "airlines": open,
        "fixed traces": fixed,
        "step s": (seconds * 10.0).round() / 10.0,
    })
}

/// What a violation is between, not where: kind and the nets on each side.
pub(super) type ViolationKey = (String, Vec<String>);

pub(super) struct Found {
    pub key: ViolationKey,
    pub first: ItemId,
    pub second: Option<ItemId>,
    pub position: FloatPoint,
    pub layer: Option<usize>,
}

pub(super) fn violation_keys(board: &mut Board) -> Vec<Found> {
    let net_names = |board: &Board, id: Option<ItemId>| -> String {
        id.and_then(|id| board.get_item(id))
            .map(|item| {
                item.net_nos()
                    .iter()
                    .filter_map(|n| board.rules.nets.get(*n).map(|net| net.name.clone()))
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .unwrap_or_default()
    };
    let violations = copper_drc::DesignRulesChecker::new(board).get_all_violations();
    violations
        .into_iter()
        .map(|v| {
            let kind = format!("{:?}", v.kind);
            let kind = kind.split(['(', ' ', '{']).next().unwrap_or_default().to_lowercase();
            let mut nets = vec![net_names(board, Some(v.first_item)), net_names(board, v.second_item)];
            nets.sort();
            Found { key: (kind, nets), first: v.first_item, second: v.second_item, position: v.position, layer: v.layer }
        })
        .collect()
}

/// The differential pairs, by net name: the last DP, _P or + swapped for DM, _N or - names a net
/// too (USB_DP_C and USB_DM_C).
fn diff_pairs(board: &Board) -> Vec<(String, String)> {
    let names: BTreeSet<String> = (1..=board.rules.nets.max_net_number())
        .filter_map(|n| board.rules.nets.get(n).map(|net| net.name.clone()))
        .collect();
    let mut pairs = Vec::new();
    for name in &names {
        for (p, n) in [("DP", "DM"), ("_P", "_N"), ("+", "-")] {
            if let Some(i) = name.rfind(p) {
                let partner = format!("{}{n}{}", &name[..i], &name[i + p.len()..]);
                if names.contains(&partner) {
                    pairs.push((name.clone(), partner));
                }
            }
        }
    }
    pairs
}

const SIMPLIFY_MM: f64 = 0.01;
/// How many of the violations that stop a reshape it names.
const STOP_REPORT: usize = 15;
/// Smaller pushes are pads lying snug against copper, not pads pressed into it.
const NUDGE_MIN_MM: f64 = 0.02;
const OPT_CHANGED_AREA_MS: i32 = 1000;

/// The violations beyond `allowed`, counted per kind and net pair, in the checker's order.
pub(super) fn new_violations(board: &mut Board, allowed: &BTreeMap<ViolationKey, usize>) -> Vec<Found> {
    let mut seen: BTreeMap<ViolationKey, usize> = BTreeMap::new();
    violation_keys(board)
        .into_iter()
        .filter(|found| {
            let count = seen.entry(found.key.clone()).or_insert(0);
            *count += 1;
            *count > allowed.get(&found.key).copied().unwrap_or(0)
        })
        .collect()
}

/// Where each new violation is, for the viewer: kind, nets, SVG position (y flipped) and layer.
pub(super) fn error_marks(board: &mut Board, allowed: &BTreeMap<ViolationKey, usize>) -> serde_json::Value {
    new_violations(board, allowed)
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "kind": f.key.0,
                "nets": f.key.1,
                "at": [f.position.x.round(), (-f.position.y).round()],
                "layer": f.layer,
            })
        })
        .collect()
}

/// The items in violations beyond `allowed`, in the checker's order.
pub(super) fn new_violation_items(board: &mut Board, allowed: &BTreeMap<ViolationKey, usize>) -> Vec<ItemId> {
    let mut items = Vec::new();
    for Found { key, first, second, .. } in new_violations(board, allowed) {
        tracing::debug!(
            "new {key:?} between {:?} and {:?}",
            board.get_item(first).map(Item::kind),
            second.and_then(|id| board.get_item(id)).map(Item::kind)
        );
        for id in std::iter::once(first).chain(second) {
            if !items.contains(&id) {
                items.push(id);
            }
        }
    }
    items
}

/// Rips the traces and vias in violations beyond `allowed` (counted per kind and net pair), never
/// on a differential pair's nets, then lets the router close the gaps. Returns how many items were
/// ripped.
fn repair(
    board: &mut Board,
    allowed: &BTreeMap<ViolationKey, usize>,
    protected: &BTreeSet<i32>,
    settings: &copper_settings::RouterSettings,
    deadline_ms: i32,
    backend: &'static dyn copper_core::RouterBackend,
) -> usize {
    let doomed: BTreeSet<ItemId> = new_violation_items(board, allowed)
        .into_iter()
        .filter(|id| {
            board.get_item(*id).is_some_and(|item| {
                (item.is_trace() || matches!(item, Item::Via(_)))
                    && !item.net_nos().iter().any(|n| protected.contains(n))
            })
        })
        .collect();
    for &id in &doomed {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(copper_board::structure::FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    if !doomed.is_empty() {
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let simplified = copper_morph::simplify_traces(board, SIMPLIFY_MM * per_mm);
        tracing::info!("repair: {simplified} traces simplified");
        let mut quick = settings.clone();
        quick.set_run_optimizer(false);
        // Unbounded, the via optimizer can shuffle vias on warped copper for as long as it is let.
        if quick.opt_changed_area_ms.unwrap_or(0) <= 0 {
            quick.opt_changed_area_ms = Some(OPT_CHANGED_AREA_MS);
        }
        let started = std::time::Instant::now();
        let progress = copper_core::SyncProgressSink::new(move |event| {
            tracing::debug!("repair {:.1} s: {event:?}", started.elapsed().as_secs_f64());
        });
        let ctx = copper_core::Ctx {
            settings: &quick,
            cancel: copper_core::CancelToken::with_timeout(std::time::Duration::from_millis(
                u64::try_from(deadline_ms).unwrap_or(0),
            )),
            progress: &progress,
            budget: crate::ops::route::budget_for(&quick),
            backend,
        };
        let open_before = copper_drc::all_airlines(board).len();
        match copper_core::RoutingPipeline::run(board, &ctx) {
            Ok(result) => tracing::info!(
                "repair reroute: {open_before} airlines before, {} after; {} passes, timed out {}; {}",
                copper_drc::all_airlines(board).len(),
                result.pipeline.router_passes_completed,
                result.timed_out,
                result.unrouted_report.lines().take(6).collect::<Vec<_>>().join(" | ")
            ),
            Err(error) => tracing::warn!("repair reroute: {error}"),
        }
    }
    doomed.len()
}

/// Rips every trace and via the design didn't fix (all of them with `design_too`) and routes the
/// board again, with the optimizer. Returns how many items were ripped.
fn reroute(
    board: &mut Board,
    settings: &copper_settings::RouterSettings,
    deadline_ms: i32,
    backend: &'static dyn copper_core::RouterBackend,
    design_too: bool,
) -> usize {
    let doomed: Vec<ItemId> = board
        .get_items()
        .filter(|i| {
            (i.is_trace() || matches!(i, Item::Via(_)))
                && (design_too || i.get_fixed_state() < copper_board::structure::FixedState::SystemFixed)
        })
        .map(Item::id)
        .collect();
    for &id in &doomed {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(copper_board::structure::FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    let mut settings = settings.clone();
    if settings.opt_changed_area_ms.unwrap_or(0) <= 0 {
        settings.opt_changed_area_ms = Some(OPT_CHANGED_AREA_MS);
    }
    let started = std::time::Instant::now();
    let progress = copper_core::SyncProgressSink::new(move |event| {
        tracing::debug!("reroute {:.1} s: {event:?}", started.elapsed().as_secs_f64());
    });
    let ctx = copper_core::Ctx {
        settings: &settings,
        cancel: copper_core::CancelToken::with_timeout(std::time::Duration::from_millis(
            u64::try_from(deadline_ms).unwrap_or(0),
        )),
        progress: &progress,
        budget: crate::ops::route::budget_for(&settings),
        backend,
    };
    let open_before = copper_drc::all_airlines(board).len();
    match copper_core::RoutingPipeline::run(board, &ctx) {
        Ok(result) => tracing::info!(
            "reroute: {} items ripped, {open_before} airlines before, {} after; {} passes, timed out {}, {:.0} s",
            doomed.len(),
            copper_drc::all_airlines(board).len(),
            result.pipeline.router_passes_completed,
            result.timed_out,
            started.elapsed().as_secs_f64()
        ),
        Err(error) => tracing::warn!("reroute: {error}"),
    }
    doomed.len()
}

/// Each pair's smallest gap between its two nets' traces on a shared layer, and the difference of
/// their copper lengths, in millimetres.
fn pair_metrics(board: &Board, pairs: &[(String, String)], per_mm: f64) -> serde_json::Value {
    let traces = |name: &str| -> Vec<(usize, f64, Vec<FloatPoint>)> {
        let Some(net) = board.rules.nets.get_by_name(name).first().map(|n| n.net_number) else {
            return Vec::new();
        };
        board
            .get_items()
            .filter_map(|item| match item {
                Item::Trace(t) if item.contains_net(net) => {
                    let pl = t.polyline();
                    let pts = (0..pl.corner_count()).filter_map(|i| pl.corner(i).map(|p| p.to_float())).collect();
                    Some((t.get_layer(), f64::from(t.get_half_width()), pts))
                }
                _ => None,
            })
            .collect()
    };
    let length = |ts: &[(usize, f64, Vec<FloatPoint>)]| -> f64 {
        ts.iter()
            .flat_map(|(_, _, p)| p.windows(2).map(|w| (w[0].x - w[1].x).hypot(w[0].y - w[1].y)))
            .sum()
    };
    let mut out = serde_json::Map::new();
    for (p, n) in pairs {
        let (a, b) = (traces(p), traces(n));
        let mut gap = f64::INFINITY;
        let mut at = FloatPoint::new(0.0, 0.0);
        for (la, wa, pa) in &a {
            for (lb, wb, pb) in &b {
                if la != lb {
                    continue;
                }
                for sa in pa.windows(2) {
                    for sb in pb.windows(2) {
                        let g = segment_distance(sa[0], sa[1], sb[0], sb[1]) - wa - wb;
                        if g < gap {
                            gap = g;
                            at = FloatPoint::new((sa[0].x + sa[1].x) / 2.0, (sa[0].y + sa[1].y) / 2.0);
                        }
                    }
                }
            }
        }
        out.insert(
            format!("{p}/{n}"),
            serde_json::json!({
                "gap mm": if gap.is_finite() { ((gap / per_mm) * 1000.0).round() / 1000.0 } else { -1.0 },
                "skew mm": (((length(&a) - length(&b)).abs() / per_mm) * 1000.0).round() / 1000.0,
                "gap at": [at.x.round(), at.y.round()],
            }),
        );
    }
    serde_json::Value::Object(out)
}

fn segment_distance(a: FloatPoint, b: FloatPoint, c: FloatPoint, d: FloatPoint) -> f64 {
    let point = |p: FloatPoint, s: FloatPoint, e: FloatPoint| {
        let (dx, dy) = (e.x - s.x, e.y - s.y);
        let len = dx * dx + dy * dy;
        let t = if len == 0.0 { 0.0 } else { (((p.x - s.x) * dx + (p.y - s.y) * dy) / len).clamp(0.0, 1.0) };
        (p.x - s.x - t * dx).hypot(p.y - s.y - t * dy)
    };
    let cross = |o: FloatPoint, p: FloatPoint, q: FloatPoint| (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    point(a, c, d).min(point(b, c, d)).min(point(c, a, b)).min(point(d, a, b))
}
