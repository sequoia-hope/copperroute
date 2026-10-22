use copper_dsn::parser::DsnRouterSettings;
use copper_settings::sources::{
    ApiSettings, DefaultSettings, DsnFileSettings, RulesFileSettings, SesFileSettings,
};
use copper_settings::{
    BoardUpdateStrategy, HostEnvironment, ItemSelectionStrategy, RouterSettings, SettingsMerger,
    SettingsSource, SourceKind, priority,
};

fn host() -> HostEnvironment {
    HostEnvironment::with_processors(4)
}

fn boxed<S: SettingsSource + 'static>(source: S) -> Box<dyn SettingsSource> {
    Box::new(source)
}

struct NullSource;

impl SettingsSource for NullSource {
    fn get_settings(&self) -> Option<&RouterSettings> {
        None
    }

    fn get_source_name(&self) -> String {
        "Null Source".to_string()
    }

    fn get_priority(&self) -> i32 {
        100
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Custom("Null Source")
    }
}

struct FixedSource {
    settings: RouterSettings,
    priority: i32,
    name: &'static str,
}

impl SettingsSource for FixedSource {
    fn get_settings(&self) -> Option<&RouterSettings> {
        Some(&self.settings)
    }

    fn get_source_name(&self) -> String {
        self.name.to_string()
    }

    fn get_priority(&self) -> i32 {
        self.priority
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Custom("Fixed Source")
    }
}

#[test]
fn default_settings_only() {
    let host = host();
    let merged = SettingsMerger::new(vec![boxed(DefaultSettings::new(&host))]).merge(&host);

    assert_eq!(merged.max_passes, Some(9999));
    assert!(merged.get_run_router());
    assert!(merged.get_vias_allowed());
    assert!(merged.get_run_optimizer());
    assert_eq!(merged.max_threads, Some(2));
    assert_eq!(merged.get_layer_count(), 0);
}

#[test]
fn empty_sources_list() {
    let merged = SettingsMerger::new(Vec::new()).merge(&host());

    assert_eq!(merged.max_passes, None);
    assert!(merged.fanout.is_some());
    assert!(merged.optimizer.is_some());
    assert!(merged.scoring.is_some());
}

#[test]
#[should_panic(expected = "maxPasses is dereferenced unboxed")]
fn only_null_sources_reproduces_javas_npe() {
    let _merged = SettingsMerger::new(vec![boxed(NullSource)]).merge(&host());
}

#[test]
fn source_priorities_and_names() {
    let host = host();
    let defaults = DefaultSettings::new(&host);
    assert_eq!(defaults.get_priority(), 0);
    assert_eq!(defaults.get_source_name(), "Default Settings");

    assert_eq!(priority::DEFAULT, 0);
    assert_eq!(priority::JSON_FILE, 10);
    assert_eq!(priority::DSN_FILE, 20);
    assert_eq!(priority::SES_FILE, 30);
    assert_eq!(priority::RULES_FILE, 40);
    assert_eq!(priority::GUI, 65);
    assert_eq!(priority::ENVIRONMENT, 55);
    assert_eq!(priority::CLI, 60);
    assert_eq!(priority::API, 70);

    let ses = SesFileSettings::new("board.ses");
    assert_eq!(ses.get_priority(), 30);
    assert_eq!(ses.get_source_name(), "SES file: board.ses");

    let rules = RulesFileSettings::new(&b""[..], "dummy.rules");
    assert_eq!(rules.get_priority(), 40);
    assert_eq!(rules.get_source_name(), "RULES file: dummy.rules");

    let api = ApiSettings::new(None);
    assert_eq!(api.get_priority(), 70);
    assert_eq!(api.get_source_name(), "API Settings");
}

#[test]
fn null_value_handling() {
    let host = host();
    let merged = SettingsMerger::new(vec![boxed(DefaultSettings::new(&host)), boxed(NullSource)])
        .merge(&host);

    assert_eq!(merged.max_passes, Some(9999));
}

#[test]
fn merge_sorts_stably_so_a_priority_tie_keeps_registration_order() {
    let host = host();
    let mut tie = RouterSettings::new();
    tie.max_passes = Some(42);
    let merged = SettingsMerger::new(vec![
        boxed(DefaultSettings::new(&host)),
        boxed(FixedSource {
            settings: tie,
            priority: 0,
            name: "Tie Source",
        }),
    ])
    .merge(&host);

    assert_eq!(merged.max_passes, Some(42));
}

#[test]
fn add_or_replace_sources_replaces_on_the_same_kind() {
    let host = host();
    let mut merger = SettingsMerger::new(vec![boxed(DefaultSettings::new(&host))]);
    assert_eq!(merger.sources().len(), 1);

    merger.add_or_replace_sources(vec![boxed(DefaultSettings::new(&host))]);
    assert_eq!(merger.sources().len(), 1);

    merger.add_or_replace_sources(vec![boxed(SesFileSettings::new("a.ses"))]);
    assert_eq!(merger.sources().len(), 2);

    merger.add_or_replace_sources(vec![boxed(SesFileSettings::new("b.ses"))]);
    assert_eq!(merger.sources().len(), 2);
    assert_eq!(merger.sources()[1].get_source_name(), "SES file: b.ses");
}

#[test]
#[allow(clippy::too_many_lines)]
fn default_settings_pins_every_java_value() {
    let host = host();
    let source = DefaultSettings::new(&host);
    let s = source
        .get_settings()
        .expect("DefaultSettings is never null");

    assert_eq!(s.enabled, Some(true));
    assert_eq!(s.algorithm.as_deref(), Some("copperroute-router"));
    assert_eq!(s.copper_to_edge_clearance_um, Some(500.0));
    assert_eq!(s.hole_clearance_um, Some(0.0));
    assert_eq!(s.neck_width_um, Some(0.0));
    assert_eq!(s.strict_drc, Some(false));
    assert_eq!(s.job_timeout_string.as_deref(), Some("00:30:00"));
    assert_eq!(s.max_passes, Some(9999));
    assert_eq!(s.max_items, Some(i32::MAX));
    assert_eq!(s.save_intermediate_stages, Some(false));
    assert_eq!(s.ignore_net_classes.as_deref(), Some(&[][..]));
    assert_eq!(s.trace_pull_tight_accuracy, Some(500));
    assert_eq!(s.vias_allowed, Some(true));
    assert_eq!(s.automatic_neckdown, Some(true));
    assert_eq!(s.max_threads, Some(2));
    assert_eq!(s.result_json_path, None);
    assert_eq!(s.layers, None);
    assert_eq!(s.get_layer_count(), 0);

    let fanout = s
        .fanout
        .as_ref()
        .expect("new RouterSettings() allocates it");
    assert_eq!(fanout.enabled, Some(true));
    assert_eq!(fanout.max_passes, Some(20));
    assert_eq!(fanout.max_items, Some(i32::MAX));
    assert_eq!(fanout.max_milliseconds_per_pin, Some(10_000));
    assert_eq!(fanout.ripup_allowed, Some(true));
    assert_eq!(fanout.min_escape_length_mm, Some(2.5));
    assert_eq!(fanout.max_escape_length_mm, Some(4.5));
    assert_eq!(fanout.start_via_diameter_mm, Some(0.250));
    assert_eq!(fanout.end_via_diameter_mm, Some(0.250));
    assert_eq!(fanout.pin_sorting_order.as_deref(), Some("outer_first"));
    assert_eq!(fanout.fallback_to_board_vias, Some(true));
    assert_eq!(fanout.timeout_string, None);

    let optimizer = s
        .optimizer
        .as_ref()
        .expect("new RouterSettings() allocates it");
    assert_eq!(optimizer.enabled, Some(true));
    assert_eq!(
        optimizer.algorithm.as_deref(),
        Some("copperroute-optimizer")
    );
    assert_eq!(optimizer.max_passes, Some(100));
    assert_eq!(optimizer.max_items, Some(i32::MAX));
    assert_eq!(optimizer.max_threads, Some(2));
    assert_eq!(optimizer.optimization_improvement_threshold, Some(0.01));
    assert_eq!(optimizer.max_consecutive_failures, Some(50));
    assert_eq!(optimizer.additional_ripup_cost_factor_at_start, Some(10));
    assert_eq!(optimizer.trace_ripup_cost_factor, Some(0.6));
    assert_eq!(optimizer.max_autoroute_passes, Some(6));
    assert_eq!(
        optimizer.board_update_strategy,
        Some(BoardUpdateStrategy::Greedy)
    );
    assert_eq!(optimizer.hybrid_ratio.as_deref(), Some("1:1"));
    assert_eq!(
        optimizer.item_selection_strategy,
        Some(ItemSelectionStrategy::Prioritized)
    );
    assert_eq!(optimizer.timeout_string, None);

    let scoring = s
        .scoring
        .as_ref()
        .expect("new RouterSettings() allocates it");
    assert_eq!(scoring.preferred_direction_trace_cost, None);
    assert_eq!(scoring.undesired_direction_trace_cost, None);
    assert_eq!(scoring.default_preferred_direction_trace_cost, Some(1.0));
    assert_eq!(scoring.default_undesired_direction_trace_cost, Some(1.0));
    assert_eq!(scoring.via_costs, Some(50));
    assert_eq!(scoring.plane_via_costs, Some(5));
    assert_eq!(scoring.start_ripup_costs, Some(100));
    assert_eq!(scoring.unrouted_net_penalty, Some(5_000_000.0));
    assert_eq!(scoring.clearance_violation_penalty, Some(1_000_000.0));
    assert_eq!(scoring.bend_penalty, Some(10.0));
    assert_eq!(scoring.default_bend_cost, Some(0.0));
    assert_eq!(scoring.smd_via_cost_factor, Some(0.1));
}

#[test]
fn default_settings_constants_match_the_table() {
    let host = host();
    let source = DefaultSettings::new(&host);
    let s = source.get_settings().expect("never null");
    let scoring = s.scoring.as_ref().expect("allocated");

    assert_eq!(
        scoring.unrouted_net_penalty,
        Some(DefaultSettings::DEFAULT_UNROUTED_NET_PENALTY)
    );
    assert_eq!(
        scoring.clearance_violation_penalty,
        Some(DefaultSettings::DEFAULT_CLEARANCE_VIOLATION_PENALTY)
    );
    assert_eq!(
        scoring.bend_penalty,
        Some(DefaultSettings::DEFAULT_BEND_PENALTY)
    );
    assert_eq!(scoring.via_costs, Some(DefaultSettings::DEFAULT_VIA_COSTS));
    assert_eq!(
        scoring.plane_via_costs,
        Some(DefaultSettings::DEFAULT_PLANE_VIA_COSTS)
    );
    assert_eq!(
        scoring.start_ripup_costs,
        Some(DefaultSettings::DEFAULT_START_RIPUP_COSTS)
    );
    assert_eq!(
        scoring.default_preferred_direction_trace_cost,
        Some(DefaultSettings::DEFAULT_PREFERRED_DIRECTION_TRACE_COST)
    );
    assert_eq!(
        scoring.default_undesired_direction_trace_cost,
        Some(DefaultSettings::DEFAULT_UNDESIRED_DIRECTION_TRACE_COST)
    );
    assert_eq!(
        s.copper_to_edge_clearance_um,
        Some(DefaultSettings::DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM)
    );
    assert_eq!(
        s.hole_clearance_um,
        Some(DefaultSettings::DEFAULT_HOLE_CLEARANCE_UM)
    );
}

#[test]
fn default_settings_are_not_mutated_by_a_merge() {
    let host = host();
    let source = DefaultSettings::new(&host);
    let before = source.get_settings().expect("never null").max_passes;

    let mut merger = SettingsMerger::new(vec![boxed(DefaultSettings::new(&host))]);
    let mut override_max_passes = RouterSettings::new();
    override_max_passes.max_passes = Some(11);
    merger.add_or_replace_sources(vec![boxed(FixedSource {
        settings: override_max_passes,
        priority: 70,
        name: "API-ish",
    })]);
    assert_eq!(merger.merge(&host).max_passes, Some(11));
    assert_eq!(merger.merge(&host).max_passes, Some(11));
    assert_eq!(
        source.get_settings().expect("never null").max_passes,
        before
    );
}

#[test]
fn ses_file_settings_is_a_structural_no_op() {
    let ses = SesFileSettings::new("board.ses");
    let s = ses.get_settings().expect("never null");
    assert_eq!(s.max_passes, None);
    assert_eq!(s.get_layer_count(), 0);
    assert!(s.fanout.is_some());
    assert!(s.optimizer.is_some());
    assert!(s.scoring.is_some());
}

#[test]
fn api_settings_wraps_or_blanks() {
    assert_eq!(
        ApiSettings::new(None)
            .get_settings()
            .expect("never null")
            .max_passes,
        None
    );

    let mut payload = RouterSettings::new();
    payload.max_passes = Some(7);
    assert_eq!(
        ApiSettings::new(Some(payload))
            .get_settings()
            .expect("never null")
            .max_passes,
        Some(7)
    );
}

#[test]
fn rules_file_settings_priority_is_40() {
    let source = RulesFileSettings::from_path(std::path::Path::new("dummy.rules"));
    assert_eq!(source.get_priority(), 40);
    assert_eq!(source.get_source_name(), "RULES file: dummy.rules");
    let s = source.get_settings().expect("never null");
    assert_eq!(s.max_passes, None);
    assert_eq!(s.get_layer_count(), 0);
}

#[test]
fn rules_file_settings_parses_processor_z80_rules() {
    let path = testkit::fixture("Issue191-processor.Z80/processor.rules");
    let bytes = std::fs::read(&path).expect("golden fixture");
    let source = RulesFileSettings::new(&bytes[..], "processor.rules");
    assert_eq!(source.get_source_name(), "RULES file: processor.rules");
    assert_eq!(source.get_priority(), 40);
    let s = source.get_settings().expect("never null");

    assert!(s.get_vias_allowed());
    assert_eq!(s.get_via_costs(), 50);
    assert_eq!(s.get_plane_via_costs(), 5);
    assert_eq!(s.get_start_ripup_costs(), 100);
    assert!(s.get_run_router());
    assert!(s.get_run_optimizer());
    assert_eq!(s.get_layer_count(), 2);

    assert!(s.get_layer_active(0));
    assert!(s.get_layer_active(1));

    assert!(s.get_preferred_direction_is_horizontal(0));
    assert_eq!(s.get_preferred_direction_trace_costs(0), 1.0);
    assert_eq!(s.get_against_preferred_direction_trace_costs(0), 2.5);

    assert!(!s.get_preferred_direction_is_horizontal(1));
    assert_eq!(s.get_preferred_direction_trace_costs(1), 1.0);
    assert_eq!(s.get_against_preferred_direction_trace_costs(1), 1.7);

    assert_eq!(s.enabled, Some(true));
    assert_eq!(s.vias_allowed, Some(true));
    assert_eq!(s.algorithm, None);
    assert_eq!(s.max_passes, None);
    let layers = s.layers.as_ref().expect("2 layers");
    assert_eq!(layers[0].routable, Some(true));
    assert_eq!(layers[0].preferred_direction_horizontal, Some(true));
    assert_eq!(layers[0].bend_cost, None);
    assert_eq!(layers[1].preferred_direction_horizontal, Some(false));
    let scoring = s.scoring.as_ref().expect("allocated");
    assert_eq!(
        scoring.preferred_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0][..])
    );
    assert_eq!(
        scoring.undesired_direction_trace_cost.as_deref(),
        Some(&[2.5, 1.7][..])
    );
    assert_eq!(scoring.default_bend_cost, None);
    assert_eq!(s.fanout.as_ref().expect("allocated").enabled, None);
    assert_eq!(s.optimizer.as_ref().expect("allocated").enabled, Some(true));
}

#[test]
fn rules_file_settings_parses_hw48na_rules() {
    let source = RulesFileSettings::from_path(&testkit::fixture("Issue029-hw48na_valid.rules"));
    assert_eq!(
        source.get_source_name(),
        "RULES file: Issue029-hw48na_valid.rules"
    );
    let s = source.get_settings().expect("never null");

    assert!(s.get_vias_allowed());
    assert_eq!(s.get_via_costs(), 50);
    assert_eq!(s.get_plane_via_costs(), 5);
    assert_eq!(s.get_start_ripup_costs(), 100);
    assert_eq!(s.get_layer_count(), 2);

    assert!(!s.get_preferred_direction_is_horizontal(0));
    assert_eq!(s.get_preferred_direction_trace_costs(0), 1.0);
    assert_eq!(s.get_against_preferred_direction_trace_costs(0), 2.0);

    assert!(s.get_preferred_direction_is_horizontal(1));
    assert_eq!(s.get_preferred_direction_trace_costs(1), 1.0);
    assert_eq!(s.get_against_preferred_direction_trace_costs(1), 2.0);

    let scoring = s.scoring.as_ref().expect("allocated");
    assert_eq!(
        scoring.undesired_direction_trace_cost.as_deref(),
        Some(&[2.0, 2.0][..])
    );
}

#[test]
fn dsn_file_settings_seeds_the_layer_count() {
    for (name, layer_count) in [
        ("Issue413-test.dsn", 2),
        ("Issue066-Project_GP8B.dsn", 4),
        ("Issue026-J2_reference.dsn", 2),
        ("Issue143-rpi_splitter.dsn", 2),
    ] {
        let bytes = std::fs::read(testkit::fixture(name)).expect("fixture");
        let source = DsnFileSettings::new(&bytes[..], name);
        assert_eq!(source.get_priority(), 20);
        assert_eq!(source.get_source_name(), format!("DSN file: {name}"));

        let s = source.get_settings().expect("never null");
        assert_eq!(s.get_layer_count(), layer_count, "{name}");

        let layers = s.layers.as_ref().expect("seeded");
        for (i, layer) in layers.iter().enumerate() {
            assert_eq!(layer.routable, Some(true), "{name} layer {i}");
            assert_eq!(
                layer.preferred_direction_horizontal, None,
                "{name} layer {i}"
            );
            assert_eq!(layer.bend_cost, None, "{name} layer {i}");
        }

        let scoring = s.scoring.as_ref().expect("allocated");
        assert_eq!(
            scoring.preferred_direction_trace_cost.as_deref(),
            Some(&vec![1.0; layer_count][..]),
            "{name}"
        );
        assert_eq!(
            scoring.undesired_direction_trace_cost.as_deref(),
            Some(&vec![1.0; layer_count][..]),
            "{name}"
        );
        assert_eq!(s.enabled, None, "{name}");
        assert_eq!(s.vias_allowed, None, "{name}");
        assert_eq!(scoring.via_costs, None, "{name}");
    }
}

#[test]
fn dsn_source_seeds_the_arrays_that_block_later_sources() {
    let host = host();
    let bytes = std::fs::read(testkit::fixture("Issue066-Project_GP8B.dsn")).expect("fixture");
    let merged = SettingsMerger::new(vec![
        boxed(DefaultSettings::new(&host)),
        boxed(DsnFileSettings::new(
            &bytes[..],
            "Issue066-Project_GP8B.dsn",
        )),
    ])
    .merge(&host);

    assert_eq!(merged.get_layer_count(), 4);
    let scoring = merged.scoring.as_ref().expect("allocated");
    assert_eq!(
        scoring.preferred_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0, 1.0, 1.0][..])
    );
    assert_eq!(
        scoring.undesired_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0, 1.0, 1.0][..])
    );
    assert_eq!(merged.max_passes, Some(9999));
    assert_eq!(scoring.via_costs, Some(50));
    for layer in merged.layers.as_ref().expect("seeded") {
        assert_eq!(layer.preferred_direction_horizontal, None);
    }
}

fn probe_data(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/")).join(name)
}

#[test]
fn an_unnamed_rules_field_does_not_overwrite_a_lower_priority_source() {
    let host = host();
    let reduced = std::fs::read(probe_data("Issue029-hw48na_reduced.rules")).expect("committed");

    let source = RulesFileSettings::new(&reduced[..], "Issue029-hw48na_reduced.rules");
    let raw = source.get_settings().expect("never null");
    assert_eq!(raw.vias_allowed, None);
    let raw_scoring = raw.scoring.as_ref().expect("allocated");
    assert_eq!(raw_scoring.via_costs, None);
    assert_eq!(raw_scoring.plane_via_costs, None);
    assert_eq!(raw_scoring.start_ripup_costs, None);
    assert!(!raw.are_board_specific_trace_costs_applied());
    assert_eq!(raw.enabled, Some(true));
    assert_eq!(
        raw.optimizer.as_ref().expect("allocated").enabled,
        Some(true)
    );
    assert_eq!(raw.get_layer_count(), 2);
    let raw_layers = raw.layers.as_ref().expect("seeded");
    assert_eq!(raw_layers[0].preferred_direction_horizontal, Some(false));
    assert_eq!(raw_layers[1].preferred_direction_horizontal, Some(true));
    assert_eq!(
        raw_scoring.preferred_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0][..])
    );
    assert_eq!(
        raw_scoring.undesired_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0][..])
    );

    let merged = SettingsMerger::new(vec![
        boxed(DefaultSettings::new(&host)),
        boxed(RulesFileSettings::new(
            &reduced[..],
            "Issue029-hw48na_reduced.rules",
        )),
    ])
    .merge(&host);
    assert_eq!(merged.get_via_costs(), 50);
    assert_eq!(merged.get_plane_via_costs(), 5);
    assert_eq!(merged.get_start_ripup_costs(), 100);
    assert!(merged.get_vias_allowed());
    assert_eq!(merged.get_layer_count(), 2);
    assert!(!merged.are_board_specific_trace_costs_applied());

    let full = std::fs::read(testkit::fixture("Issue029-hw48na_valid.rules")).expect("golden");
    let full = RulesFileSettings::new(&full[..], "Issue029-hw48na_valid.rules");
    let full = full.get_settings().expect("never null");
    assert_eq!(
        full.scoring.as_ref().expect("allocated").via_costs,
        Some(50)
    );
    assert_eq!(full.vias_allowed, Some(true));
    assert!(full.are_board_specific_trace_costs_applied());
}

#[test]
fn is_fanout_enabled_defaults_to_false_when_absent() {
    assert!(!RouterSettings::new().is_fanout_enabled());
    assert!(!RouterSettings::default().is_fanout_enabled());

    let mut settings = RouterSettings::new();
    settings.fanout.as_mut().expect("allocated").enabled = Some(true);
    assert!(settings.is_fanout_enabled());
    settings.fanout.as_mut().expect("allocated").enabled = Some(false);
    assert!(!settings.is_fanout_enabled());

    let host = host();
    assert!(
        DefaultSettings::new(&host)
            .get_settings()
            .expect("never null")
            .is_fanout_enabled()
    );
}

#[test]
fn dsn_router_settings_converts_into_router_settings() {
    let bytes = std::fs::read(testkit::fixture("Issue191-processor.Z80/processor.rules"))
        .expect("golden fixture");
    let dsn = copper_dsn::rules_reader::read_router_settings(&bytes[..])
        .expect("scanner error")
        .expect("processor.rules has an (autoroute_settings) scope");

    let s = RouterSettings::from(dsn);

    assert_eq!(s.enabled, Some(true));
    assert_eq!(s.vias_allowed, Some(true));
    assert_eq!(s.optimizer.as_ref().expect("allocated").enabled, Some(true));
    let scoring = s.scoring.as_ref().expect("allocated");
    assert_eq!(scoring.via_costs, Some(50));
    assert_eq!(scoring.plane_via_costs, Some(5));
    assert_eq!(scoring.start_ripup_costs, Some(100));
    assert_eq!(
        scoring.preferred_direction_trace_cost.as_deref(),
        Some(&[1.0, 1.0][..])
    );
    assert_eq!(
        scoring.undesired_direction_trace_cost.as_deref(),
        Some(&[2.5, 1.7][..])
    );
    let layers = s.layers.as_ref().expect("2 layers");
    assert_eq!(layers[0].routable, Some(true));
    assert_eq!(layers[0].preferred_direction_horizontal, Some(true));
    assert_eq!(layers[1].preferred_direction_horizontal, Some(false));
    assert_eq!(s.algorithm, None);
    assert_eq!(s.max_threads, None);
    assert_eq!(s.result_json_path, None);
    assert_eq!(s.fanout.as_ref().expect("allocated").enabled, None);
}

#[test]
fn dsn_router_settings_round_trips_through_router_settings() {
    for name in [
        "Issue191-processor.Z80/processor.rules",
        "Issue029-hw48na_valid.rules",
    ] {
        let bytes = std::fs::read(testkit::fixture(name)).expect("golden fixture");
        let before = copper_dsn::rules_reader::read_router_settings(&bytes[..])
            .expect("scanner error")
            .expect("has an (autoroute_settings) scope");

        let after = DsnRouterSettings::from(&RouterSettings::from(before.clone()));

        assert_eq!(after.run_router(), before.run_router(), "{name} run_router");
        assert_eq!(
            after.run_optimizer(),
            before.run_optimizer(),
            "{name} run_optimizer"
        );
        assert_eq!(
            after.vias_allowed(),
            before.vias_allowed(),
            "{name} vias_allowed"
        );
        assert_eq!(after.via_costs(), before.via_costs(), "{name} via_costs");
        assert_eq!(
            after.plane_via_costs(),
            before.plane_via_costs(),
            "{name} plane_via_costs"
        );
        assert_eq!(
            after.start_ripup_costs(),
            before.start_ripup_costs(),
            "{name} start_ripup_costs"
        );
        assert_eq!(
            after.get_layer_count(),
            before.get_layer_count(),
            "{name} layer_count"
        );
        for i in 0..before.get_layer_count() {
            assert_eq!(
                after.get_layer_active(i),
                before.get_layer_active(i),
                "{name} layer {i} active"
            );
            assert_eq!(
                after.get_preferred_direction_is_horizontal(i),
                before.get_preferred_direction_is_horizontal(i),
                "{name} layer {i} direction"
            );
            assert_eq!(
                after.get_preferred_direction_trace_costs(i),
                before.get_preferred_direction_trace_costs(i),
                "{name} layer {i} preferred cost"
            );
            assert_eq!(
                after.get_against_preferred_direction_trace_costs(i),
                before.get_against_preferred_direction_trace_costs(i),
                "{name} layer {i} against cost"
            );
        }
    }
}

#[test]
fn reverse_conversion_applies_the_getters_defaults() {
    let mut blank = RouterSettings::new();
    blank.set_layer_count(3);
    for layer in blank.layers.as_ref().expect("seeded") {
        assert_eq!(layer.preferred_direction_horizontal, None);
    }

    let dsn = DsnRouterSettings::from(&blank);
    assert_eq!(dsn.get_layer_count(), 3);
    assert!(!dsn.get_preferred_direction_is_horizontal(0));
    assert!(dsn.get_preferred_direction_is_horizontal(1));
    assert!(!dsn.get_preferred_direction_is_horizontal(2));
    let back = RouterSettings::from(dsn);
    let layers = back.layers.as_ref().expect("3 layers");
    assert_eq!(layers[0].preferred_direction_horizontal, Some(false));
    assert_eq!(layers[1].preferred_direction_horizontal, Some(true));
    assert_eq!(layers[2].preferred_direction_horizontal, Some(false));

    let blank_dsn = DsnRouterSettings::from(&RouterSettings::new());
    assert!(blank_dsn.run_router());
    assert!(!blank_dsn.run_optimizer());
    assert!(blank_dsn.vias_allowed());
    assert_eq!(blank_dsn.via_costs(), 1);
    assert_eq!(blank_dsn.plane_via_costs(), 1);
    assert_eq!(blank_dsn.start_ripup_costs(), 1);
    assert_eq!(blank_dsn.get_layer_count(), 0);
}
