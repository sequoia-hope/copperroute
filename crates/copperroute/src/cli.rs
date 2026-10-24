use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use crate::ops::route::OutputFormat;
use copper_core::RouterBackend;
use copper_governor::MemoryCeiling;

#[derive(Parser, Debug)]
#[command(
    name = "copperroute",
    version,
    propagate_version = true,
    about = "Headless PCB autorouter"
)]
pub struct Cli {
    /// Raise the log level: -v for debug, -vv for trace.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Set the log level outright: off, error, warn, info, debug, trace.
    #[arg(long, global = true)]
    pub log_level: Option<String>,
    /// A settings JSON, applied below the design's own settings and below --set.
    #[arg(long, global = true, value_name = "FILE")]
    pub settings: Option<PathBuf>,
    /// Scheduling niceness for route and mcp, 0..=19. Defaults to $COPPERROUTE_NICE, else 10.
    #[arg(long, global = true)]
    pub nice: Option<i32>,
    /// Stop routing once resident memory reaches this size (512M, 4G), keeping what is routed.
    /// `auto` is half the memory available at start; `off` disables the ceiling. Defaults to
    /// $COPPERROUTE_MAX_MEMORY, else auto.
    #[arg(long, global = true, value_parser = MemoryCeiling::parse)]
    pub max_memory: Option<MemoryCeiling>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Route a board and write the session.
    Route(RouteArgs),
    /// Check a board against its design rules and write the KiCad DRC report.
    Drc(DrcArgs),
    /// Print the board summary as JSON.
    Info(InfoArgs),
    /// Move parts on a routed board toward target displacements, bending the copper with them,
    /// and write the session with the new placement.
    Settle(SettleArgs),
    /// Report, for each open connection, what stands in its way and how far each movable part
    /// in the way must move to clear it.
    Blame(BlameArgs),
    /// Change the board's shape step by step along a plan (tools/morph_outline.py), carrying
    /// parts and copper with it, and write the session of the last step.
    Reshape(ReshapeArgs),
    /// Pull every trace of a routed board taut like a rubber band, the clearances pushing back,
    /// then let the vias go too, and write the session.
    Band(BandArgs),
    /// Serve the routing tools over JSON-RPC on stdin/stdout.
    Mcp,
}

#[derive(Args, Debug)]
pub struct RouteArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// Where to write the session: a .ses (Specctra) or a .json (KiCad session).
    #[arg(short, long, value_parser = parse_output_path)]
    pub output: PathBuf,
    /// A Specctra .rules file. Without it, a .rules beside the input is used when present.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// A session to import before routing, so the run continues from it.
    #[arg(long)]
    pub ses: Option<PathBuf>,
    /// A KiCad .kicad_pro whose design rules are applied before routing.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// How many routing passes to run; 0 means unlimited.
    #[arg(long)]
    pub max_passes: Option<u32>,
    /// Wall-clock budget for the whole job, as hh:mm:ss or seconds.
    #[arg(long)]
    pub timeout: Option<String>,
    /// Write the result manifest here.
    #[arg(long)]
    pub result_json: Option<PathBuf>,
    /// The routing approach to run.
    #[arg(long, default_value = copper_core::DEFAULT_BACKEND, value_parser = parse_backend)]
    pub backend: &'static dyn RouterBackend,
    /// Parts placement tools must not move, by reference: --lock J1,U7. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "REF")]
    pub lock: Vec<String>,
    /// Write an SVG of the board after every nudge kept, with frames.jsonl, into this empty
    /// directory.
    #[arg(long)]
    pub morph_frames: Option<PathBuf>,
    /// Unfix the user-fixed traces and vias on these nets (or `all`) so the router may rip and
    /// reroute them. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "NET")]
    pub retire_fixed: Vec<String>,
    /// Override one setting: --set router.<section>.<field>=<value>. Repeatable.
    /// router.max_items=N stops the optimizer as well as the router; --max-passes stops
    /// only the router.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub set: Vec<String>,
    /// Write SVG frames showing maze rooms over the routed PCB.
    #[arg(long, value_name = "DIR")]
    pub visualize: Option<PathBuf>,
    /// Capture one visualization frame for every N maze steps; route commits are always kept.
    #[arg(long, default_value_t = 1, requires = "visualize")]
    pub visualize_every: u64,
    /// Stop writing visualization frames after this many images; routing continues.
    #[arg(long, default_value_t = 1_000, requires = "visualize")]
    pub visualize_max_frames: u64,
    #[arg(long, default_value_t = 1280, requires = "visualize")]
    pub visualize_width: u32,
    #[arg(long, default_value_t = 720, requires = "visualize")]
    pub visualize_height: u32,
}

#[derive(Args, Debug)]
pub struct DrcArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// A session to apply before checking, so the report describes the routed board.
    #[arg(long)]
    pub ses: Option<PathBuf>,
    /// A Specctra .rules file to apply before checking.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// A KiCad .kicad_pro whose design rules are applied before checking.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Where to write the report; stdout when omitted.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct SettleArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// The routed session to start from.
    #[arg(long)]
    pub ses: PathBuf,
    /// JSON object of component reference to [dx, dy] in millimetres.
    #[arg(long)]
    pub targets: Option<PathBuf>,
    /// A push field adding to the targets of every free part near (x, y), in millimetres; the
    /// push falls off as (1 + d²/radius²)^(-decay/2). Defaults: radius 3, decay 2. Repeatable.
    #[arg(long, value_name = "X,Y,DX,DY[,RADIUS[,DECAY]]")]
    pub dipole: Vec<String>,
    /// Before settling, move each of these parts to where its pins centre on the pins they
    /// connect to, or the nearest admissible place to it.
    #[arg(long, value_delimiter = ',', value_name = "REF")]
    pub near_partners: Vec<String>,
    /// Parts that may not move, by reference: --lock J1,U7. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "REF")]
    pub lock: Vec<String>,
    /// Unfix the user-fixed traces and vias on these nets (or `all`) so the router may rip and
    /// reroute them. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "NET")]
    pub retire_fixed: Vec<String>,
    /// Let the warp field bend fixed copper attached to a moving part instead of refusing the
    /// move; the copper stays fixed.
    #[arg(long)]
    pub bend_fixed: bool,
    /// Settle with one move kind alone.
    #[arg(long, value_parser = ["warp", "shove", "push", "group"])]
    pub only: Option<String>,
    /// Write an SVG of the board after every move kept, with frames.jsonl, into this empty
    /// directory.
    #[arg(long)]
    pub frames: Option<PathBuf>,
    #[arg(long, default_value_t = 2000)]
    pub max_frames: usize,
    /// Where to write the settled session (.ses), placement included.
    #[arg(short, long)]
    pub output: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Allow ripping routed copper that is in the way; it then needs rerouting.
    #[arg(long)]
    pub soft: bool,
    /// Largest step per part per cycle, in millimetres.
    #[arg(long, default_value_t = 0.05)]
    pub step_mm: f64,
    #[arg(long, default_value_t = 600)]
    pub max_cycles: usize,
    /// Clearance kept beyond the rules, in millimetres.
    #[arg(long, default_value_t = 0.015)]
    pub guard_mm: f64,
}

#[derive(Args, Debug)]
pub struct BlameArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// A routed session to apply first, placement included.
    #[arg(long)]
    pub ses: Option<PathBuf>,
    /// A KiCad .kicad_pro whose design rules are applied.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Parts that may not move, by reference: --lock J1,U7. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "REF")]
    pub lock: Vec<String>,
    /// Unfix the user-fixed traces and vias on these nets (or `all`) so the router may rip and
    /// reroute them. Repeatable.
    #[arg(long, value_delimiter = ',', value_name = "NET")]
    pub retire_fixed: Vec<String>,
    /// Let the warp field bend fixed copper attached to a moving part instead of refusing the
    /// move; the copper stays fixed.
    #[arg(long)]
    pub bend_fixed: bool,
    /// Cost of crossing a cell of routed copper, which the router may rip, against 10 per
    /// step; omit to make routed copper a wall.
    #[arg(long)]
    pub routed_copper: Option<u32>,
    #[arg(long, default_value_t = 0.05)]
    pub cell_mm: f64,
    /// Clearance kept beyond the rules, in millimetres.
    #[arg(long, default_value_t = 0.045)]
    pub margin_mm: f64,
    #[arg(long, default_value_t = 1.5)]
    pub max_move_mm: f64,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied before summarising.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
}

fn parse_backend(value: &str) -> Result<&'static dyn RouterBackend, String> {
    copper_core::backend(value).ok_or_else(|| {
        let known: Vec<_> = copper_core::backends()
            .iter()
            .map(|b| format!("{} ({})", b.name(), b.summary()))
            .collect();
        format!("unknown backend '{value}'; known: {}", known.join(", "))
    })
}

fn parse_output_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    match OutputFormat::from_path(&path) {
        Some(_) => Ok(path),
        None => Err(format!(
            "'{value}' must end in .ses (a Specctra session) or .json (a KiCad session)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_route_with_max_passes_timeout_and_set() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--max-passes",
            "3",
            "--timeout",
            "0:05:00",
            "--set",
            "router.scoring.via_costs=1",
        ])
        .unwrap();
        let Command::Route(r) = cli.command else {
            panic!("expected route")
        };
        assert_eq!(r.input, PathBuf::from("a.dsn"));
        assert_eq!(r.max_passes, Some(3));
        assert_eq!(r.timeout.as_deref(), Some("0:05:00"));
        assert_eq!(r.set, vec!["router.scoring.via_costs=1".to_string()]);
    }

    #[test]
    fn the_output_must_be_a_session_extension() {
        for bad in ["b.dsn", "b.txt", "b"] {
            let error =
                Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", bad]).expect_err(bad);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{bad}"
            );
        }
        for good in ["b.ses", "b.json", "B.SES"] {
            Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", good]).expect(good);
        }
    }

    #[test]
    fn the_dead_flags_are_gone() {
        for flag in [
            "--threads",
            "--kicad-json",
            "--optimizer-improvement-threshold",
            "--update-strategy",
            "--hybrid-ratio",
            "--item-selection",
            "--ignore-net-classes",
        ] {
            let error =
                Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", "b.ses", flag, "1"])
                    .expect_err(flag);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}"
            );
        }
        let error = Cli::try_parse_from(["copperroute", "-de", "a.dsn", "-do", "b.ses"])
            .expect_err("the legacy form");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn parses_bounded_routing_visualization() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--visualize",
            "frames",
            "--visualize-every",
            "25",
            "--visualize-max-frames",
            "80",
        ])
        .unwrap();
        let Command::Route(route) = cli.command else {
            panic!("expected route")
        };
        assert_eq!(route.visualize, Some(PathBuf::from("frames")));
        assert_eq!(route.visualize_every, 25);
        assert_eq!(route.visualize_max_frames, 80);
        assert_eq!((route.visualize_width, route.visualize_height), (1280, 720));
    }

    #[test]
    fn parses_drc_and_info_and_mcp() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "drc",
            "a.dsn",
            "--kicad-project",
            "p.kicad_pro",
        ])
        .unwrap();
        let Command::Drc(d) = cli.command else {
            panic!("expected drc")
        };
        assert_eq!(d.kicad_project, Some(PathBuf::from("p.kicad_pro")));
        assert!(matches!(
            Cli::try_parse_from(["copperroute", "info", "a.dsn"])
                .unwrap()
                .command,
            Command::Info(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["copperroute", "mcp"]).unwrap().command,
            Command::Mcp
        ));
    }

    #[test]
    fn backend_defaults_to_maze_and_rejects_unknown_names() {
        let parse = |extra: &[&str]| {
            let base = ["copperroute", "route", "a.dsn", "-o", "b.ses"];
            Cli::try_parse_from(base.iter().chain(extra))
        };
        let Command::Route(r) = parse(&[]).unwrap().command else {
            panic!("expected route")
        };
        assert_eq!(r.backend.name(), "maze");
        let Command::Route(r) = parse(&["--backend", "none"]).unwrap().command else {
            panic!("expected route")
        };
        assert_eq!(r.backend.name(), "none");
        assert!(parse(&["--backend", "quantum"]).is_err());
    }

    #[test]
    fn lock_takes_comma_lists_and_repeats() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--lock",
            "J1,U7",
            "--lock",
            "U20",
        ])
        .unwrap();
        let Command::Route(r) = cli.command else {
            panic!("expected route")
        };
        assert_eq!(r.lock, ["J1", "U7", "U20"]);
    }

    #[test]
    fn governance_flags_are_global() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--nice",
            "15",
            "--max-memory",
            "2G",
        ])
        .unwrap();
        assert_eq!(cli.nice, Some(15));
        assert_eq!(cli.max_memory, Some(MemoryCeiling::Bytes(2 << 30)));
        let cli = Cli::try_parse_from(["copperroute", "--max-memory", "off", "mcp"]).unwrap();
        assert_eq!(cli.max_memory, Some(MemoryCeiling::Off));
        assert!(Cli::try_parse_from(["copperroute", "--max-memory", "lots", "mcp"]).is_err());
    }

    #[test]
    fn settings_is_global_and_names_a_json_file() {
        let cli =
            Cli::try_parse_from(["copperroute", "--settings", "s.json", "info", "a.dsn"]).unwrap();
        assert_eq!(cli.settings, Some(PathBuf::from("s.json")));
    }
}

#[derive(Args, Debug)]
pub struct ReshapeArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// The routed session to start from.
    #[arg(long)]
    pub ses: PathBuf,
    /// The plan: board_outline, then steps of outline (mm) and part moves [dx, dy, turn].
    #[arg(long)]
    pub plan: PathBuf,
    /// Where to write the session of the last step (.ses), placement included; the step's
    /// outline goes beside it as .outline.json.
    #[arg(short, long)]
    pub output: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// The last plan step to reach; the whole plan when omitted.
    #[arg(long)]
    pub to: Option<usize>,
    /// The warp field's cell size, in millimetres.
    #[arg(long, default_value_t = 0.25)]
    pub cell_mm: f64,
    /// Every this many steps (and at the last), rip the copper behind new violations, except on
    /// differential pairs, and reroute what is open; 0 never repairs.
    #[arg(long, default_value_t = 5)]
    pub repair_every: usize,
    /// Repair only at the plan's keyframes, the steps whose label names a shape rather than a
    /// transition, instead of every `--repair-every` steps.
    #[arg(long)]
    pub repair_at_keyframes: bool,
    /// When a repair is due, rip up all the routing, not only the violating copper, and route
    /// the board again from the design's own fixed wiring.
    #[arg(long)]
    pub reroute: bool,
    /// With `--reroute`, rip up the design's own fixed wiring too and route the board from its
    /// pads alone.
    #[arg(long)]
    pub rip_design_wiring: bool,
    /// With `--reroute`, spread crowded parts by diffusion once the routing is ripped, until no
    /// bin of the board is fuller than this density (part and routing demand over area). 0 never
    /// spreads.
    #[arg(long, default_value_t = 0.0)]
    pub diffuse_target: f64,
    /// The furthest diffusion moves one part at one keyframe, in millimetres.
    #[arg(long, default_value_t = 2.0)]
    pub diffuse_max_mm: f64,
    /// The reroute's time limit, in milliseconds.
    #[arg(long, default_value_t = 60_000)]
    pub repair_ms: i32,
    /// After every step, this many rounds of pushing trace corners away from other nets' copper
    /// closer than the clearance; 0 never relaxes.
    #[arg(long, default_value_t = 0)]
    pub relax: usize,
    /// Make every trace a rubber band during relaxation: each round each segment pulls its ends
    /// together by this fraction of its length before the clearances push back. 0 is no pull.
    #[arg(long, default_value_t = 0.0)]
    pub tension: f64,
    /// After the traces relax, this many more rounds with the vias free too.
    #[arg(long, default_value_t = 0)]
    pub via_relax: usize,
    /// Stop after the first step that leaves more than this many new violations, naming where
    /// they are, and write the session of that step.
    #[arg(long)]
    pub stop_at: Option<i64>,
    /// After the relaxation, move each part whose pads the copper still presses on by up to
    /// this many millimetres the way it is pushed, then relax again. Parts sharing a pour move
    /// as one. 0 never nudges.
    #[arg(long, default_value_t = 0.0)]
    pub nudge_mm: f64,
    /// Parts the nudge never moves, as `--lock J1,U7`.
    #[arg(long, value_delimiter = ',')]
    pub lock: Vec<String>,
    /// The router backend for the repair reroute.
    #[arg(long, default_value = "grid")]
    pub repair_backend: String,
    /// Router settings for the repair reroute, as `--set router.fanout.enabled=false`. Repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// Write an SVG of the board after every step, with frames.jsonl, into this empty directory.
    #[arg(long)]
    pub frames: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct BandArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// The routed session to pull taut.
    #[arg(long)]
    pub ses: PathBuf,
    /// Where to write the session.
    #[arg(short, long)]
    pub output: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Rounds with the vias held.
    #[arg(long, default_value_t = 400)]
    pub rounds: usize,
    /// Rounds after that with the vias free.
    #[arg(long, default_value_t = 0)]
    pub via_rounds: usize,
    /// Each round, each segment pulls its ends together by this fraction of its length.
    #[arg(long, default_value_t = 0.2)]
    pub tension: f64,
    /// Traces are split into segments no longer than this, in millimetres, so they can bend.
    #[arg(long, default_value_t = 0.25)]
    pub segment_mm: f64,
    /// Write a frame every this many rounds.
    #[arg(long, default_value_t = 20)]
    pub frame_every: usize,
    /// Router settings, as `--set router.fanout.enabled=false`. Repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// Write an SVG of the board every `--frame-every` rounds, with frames.jsonl, into this empty
    /// directory.
    #[arg(long)]
    pub frames: Option<PathBuf>,
}
