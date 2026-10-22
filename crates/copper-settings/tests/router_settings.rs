use copper_settings::prelude::*;

fn host() -> HostEnvironment {
    HostEnvironment::with_processors(4)
}

fn validatable() -> RouterSettings {
    let mut s = RouterSettings::new();
    s.max_passes = Some(50);
    s.trace_pull_tight_accuracy = Some(500);
    s.max_threads = Some(2);
    s
}

#[test]
fn default_bend_cost() {
    let mut settings = RouterSettings::new();
    assert!(settings.scoring.is_some(), "new() allocates scoring");
    assert_eq!(settings.scoring.as_ref().unwrap().default_bend_cost, None);

    settings.set_layer_count(2);
    assert_eq!(settings.get_bend_cost(0), 0.0);
    assert_eq!(settings.get_bend_cost(1), 0.0);
}

#[test]
fn set_get_bend_cost() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);

    assert_eq!(settings.get_bend_cost(0), 0.0);
    assert_eq!(settings.get_bend_cost(1), 0.0);

    settings.set_bend_cost(0, 2.5);
    settings.set_bend_cost(1, 5.0);
    assert_eq!(settings.get_bend_cost(0), 2.5);
    assert_eq!(settings.get_bend_cost(1), 5.0);

    settings.set_bend_cost(0, -1.0);
    assert_eq!(settings.get_bend_cost(0), RouterSettings::MIN_BEND_COST);

    settings.set_bend_cost(1, 150.0);
    assert_eq!(settings.get_bend_cost(1), RouterSettings::MAX_BEND_COST);
    settings.set_bend_cost(1, 999.0);
    assert_eq!(settings.get_bend_cost(1), 100.0);
}

#[test]
fn bend_cost_falls_back_to_a_clamped_default_bend_cost() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);

    settings.scoring.as_mut().unwrap().default_bend_cost = Some(3.0);
    assert_eq!(settings.get_bend_cost(0), 3.0);

    settings.scoring.as_mut().unwrap().default_bend_cost = Some(150.0);
    assert_eq!(settings.get_bend_cost(0), 100.0);

    settings.scoring.as_mut().unwrap().default_bend_cost = Some(-3.0);
    assert_eq!(settings.get_bend_cost(0), 0.0);

    settings.set_bend_cost(1, 2.0);
    assert_eq!(settings.get_bend_cost(1), 2.0);

    assert_eq!(settings.get_bend_cost(9), 0.0);
    settings.set_bend_cost(9, 4.0);
    assert_eq!(settings.get_layer_count(), 2);
}

#[test]
fn null_scoring_safety() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);
    settings.scoring = None;

    assert!(settings.duplicate().scoring.is_some());

    assert_eq!(settings.get_start_ripup_costs(), 1);
    settings.set_start_ripup_costs(5);
    assert_eq!(settings.get_start_ripup_costs(), 5);

    assert_eq!(settings.get_via_costs(), 1);
    settings.set_via_costs(3);
    assert_eq!(settings.get_via_costs(), 3);

    assert_eq!(settings.get_preferred_direction_trace_costs(0), 1.0);
    settings.scoring = None;
    settings.set_preferred_direction_trace_costs(0, 2.0);
    assert_eq!(settings.get_preferred_direction_trace_costs(0), 2.0);
}

#[test]
fn neck_width() {
    assert_eq!(RouterSettings::new().get_neck_width_um(), 0.0);

    let mut settings = RouterSettings::new();
    settings.neck_width_um = Some(130.0);
    assert_eq!(settings.duplicate().get_neck_width_um(), 130.0);

    settings.neck_width_um = Some(-5.0);
    assert_eq!(settings.get_neck_width_um(), 0.0);

    settings.neck_width_um = Some(0.0);
    assert_eq!(settings.get_neck_width_um(), 0.0);
}

#[test]
fn accessor_defaults_on_a_fresh_settings_object() {
    let settings = RouterSettings::new();
    assert!(settings.get_run_router());
    assert!(!settings.get_run_optimizer());
    assert!(settings.get_vias_allowed());
    assert_eq!(settings.get_via_costs(), 1);
    assert_eq!(settings.get_plane_via_costs(), 1);
    assert_eq!(settings.get_start_ripup_costs(), 1);
    assert_eq!(settings.get_neck_width_um(), 0.0);
    assert!(!settings.is_strict_drc());
    assert!(!settings.get_automatic_neckdown());
    assert_eq!(settings.get_layer_count(), 0);

    assert!(!settings.get_layer_active(0));
    assert_eq!(settings.get_bend_cost(0), 0.0);
    assert!(!settings.get_preferred_direction_is_horizontal(0));
    assert_eq!(settings.get_preferred_direction_trace_costs(0), 0.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(0), 0.0);
    assert_eq!(settings.get_horizontal_trace_costs(0), 0.0);
    assert_eq!(settings.get_vertical_trace_costs(0), 0.0);
    assert!(settings.get_trace_costs().is_empty());
}

#[test]
fn run_router_and_run_optimizer() {
    let mut settings = RouterSettings::new();
    settings.optimizer = None;
    assert!(!settings.get_run_optimizer());
    settings.set_run_optimizer(true);
    assert!(settings.get_run_optimizer());
    assert!(settings.optimizer.is_some());

    settings.set_run_router(false);
    assert!(!settings.get_run_router());
    assert_eq!(settings.enabled, Some(false));

    settings.set_enabled(None);
    assert!(settings.get_run_router());
}

#[test]
fn scalar_setter_clamps() {
    let mut settings = RouterSettings::new();
    settings.set_via_costs(-4);
    settings.set_plane_via_costs(0);
    settings.set_start_ripup_costs(-9);
    assert_eq!(settings.get_via_costs(), 1);
    assert_eq!(settings.get_plane_via_costs(), 1);
    assert_eq!(settings.get_start_ripup_costs(), 1);

    settings.set_via_costs(7);
    settings.set_plane_via_costs(8);
    settings.set_start_ripup_costs(9);
    assert_eq!(settings.get_via_costs(), 7);
    assert_eq!(settings.get_plane_via_costs(), 8);
    assert_eq!(settings.get_start_ripup_costs(), 9);
}

#[test]
fn layer_active_and_preferred_direction() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(3);

    assert!(settings.get_layer_active(0));
    settings.set_layer_active(1, false);
    assert!(!settings.get_layer_active(1));
    assert!(!settings.get_layer_active(3));
    settings.set_layer_active(3, false);
    assert_eq!(settings.get_layer_count(), 3);

    assert!(!settings.get_preferred_direction_is_horizontal(0));
    assert!(settings.get_preferred_direction_is_horizontal(1));
    assert!(!settings.get_preferred_direction_is_horizontal(2));
    settings.set_preferred_direction_is_horizontal(0, true);
    assert!(settings.get_preferred_direction_is_horizontal(0));
    assert!(!settings.get_preferred_direction_is_horizontal(3));
}

#[test]
fn trace_cost_setters_clamp_and_reallocate() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);

    settings.set_preferred_direction_trace_costs(0, 0.05);
    assert_eq!(settings.get_preferred_direction_trace_costs(0), 0.1);
    settings.set_against_preferred_direction_trace_costs(0, -1.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(0), 0.1);

    settings
        .scoring
        .as_mut()
        .unwrap()
        .preferred_direction_trace_cost = Some(vec![7.0]);
    settings.set_preferred_direction_trace_costs(1, 2.0);
    assert_eq!(
        settings
            .scoring
            .as_ref()
            .unwrap()
            .preferred_direction_trace_cost,
        Some(vec![0.0, 2.0])
    );

    assert_eq!(settings.get_preferred_direction_trace_costs(9), 0.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(9), 0.0);
    let before = settings.clone();
    settings.set_preferred_direction_trace_costs(9, 5.0);
    settings.set_against_preferred_direction_trace_costs(9, 5.0);
    assert_eq!(settings, before);
}

#[test]
fn trace_cost_getters_fall_back_to_one() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);
    settings
        .scoring
        .as_mut()
        .unwrap()
        .preferred_direction_trace_cost = Some(vec![3.0]);
    settings
        .scoring
        .as_mut()
        .unwrap()
        .undesired_direction_trace_cost = None;

    assert_eq!(settings.get_preferred_direction_trace_costs(0), 3.0);
    assert_eq!(settings.get_preferred_direction_trace_costs(1), 1.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(0), 1.0);
}

#[test]
fn horizontal_vertical_and_trace_costs() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);
    settings.set_preferred_direction_trace_costs(0, 2.0);
    settings.set_against_preferred_direction_trace_costs(0, 3.0);
    settings.set_preferred_direction_trace_costs(1, 4.0);
    settings.set_against_preferred_direction_trace_costs(1, 5.0);

    assert!(!settings.get_preferred_direction_is_horizontal(0));
    assert_eq!(settings.get_horizontal_trace_costs(0), 3.0);
    assert_eq!(settings.get_vertical_trace_costs(0), 2.0);

    assert!(settings.get_preferred_direction_is_horizontal(1));
    assert_eq!(settings.get_horizontal_trace_costs(1), 4.0);
    assert_eq!(settings.get_vertical_trace_costs(1), 5.0);

    assert_eq!(
        settings.get_trace_costs(),
        vec![
            ExpansionCostFactor {
                horizontal: 3.0,
                vertical: 2.0
            },
            ExpansionCostFactor {
                horizontal: 4.0,
                vertical: 5.0
            },
        ]
    );
}

#[test]
fn horizontal_and_vertical_trace_costs_are_guarded_without_the_array() {
    let mut settings = RouterSettings::new();
    settings.layers = Some(vec![LayerSettings::default(); 2]);
    settings.scoring = Some(ScoringSettings::default());

    assert_eq!(settings.get_horizontal_trace_costs(0), 1.0);
    assert_eq!(settings.get_vertical_trace_costs(0), 1.0);
    assert_eq!(settings.get_preferred_direction_trace_costs(0), 1.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(0), 1.0);

    let mut no_scoring = RouterSettings::new();
    no_scoring.layers = Some(vec![LayerSettings::default(); 2]);
    no_scoring.scoring = None;
    assert_eq!(no_scoring.get_horizontal_trace_costs(0), 1.0);
    assert_eq!(no_scoring.get_vertical_trace_costs(0), 1.0);
}

#[test]
fn trace_costs_survives_a_half_populated_scoring() {
    let mut settings = RouterSettings::new();
    settings.layers = Some(vec![LayerSettings::default(); 2]);
    settings.scoring = Some(ScoringSettings {
        preferred_direction_trace_cost: Some(vec![2.0, 3.0]),
        undesired_direction_trace_cost: None,
        ..ScoringSettings::default()
    });

    assert!(!settings.get_preferred_direction_is_horizontal(0));
    assert!(settings.get_preferred_direction_is_horizontal(1));

    let costs = settings.get_trace_costs();
    assert_eq!(costs.len(), 2);
    assert_eq!(costs[0].vertical, 2.0);
    assert_eq!(costs[0].horizontal, 1.0);
    assert_eq!(costs[1].horizontal, 3.0);
    assert_eq!(costs[1].vertical, 1.0);
}

#[test]
fn horizontal_trace_costs_out_of_range_is_checked_first() {
    let mut settings = RouterSettings::new();
    settings.layers = Some(vec![LayerSettings::default(); 2]);
    settings.scoring = Some(ScoringSettings::default());
    assert_eq!(settings.get_horizontal_trace_costs(9), 0.0);
    assert_eq!(settings.get_vertical_trace_costs(9), 0.0);
    assert!(settings.get_trace_costs().is_empty());
}

#[test]
fn trace_costs_are_sized_by_the_cost_array_not_by_the_layer_count() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);
    settings.set_preferred_direction_trace_costs(0, 2.0);
    settings.set_against_preferred_direction_trace_costs(0, 3.0);
    settings.set_preferred_direction_trace_costs(1, 4.0);
    settings.set_against_preferred_direction_trace_costs(1, 5.0);

    let scoring = settings.scoring.as_mut().expect("allocated");
    scoring.preferred_direction_trace_cost = Some(vec![2.0, 4.0, 6.0, 8.0]);
    scoring.undesired_direction_trace_cost = Some(vec![3.0, 5.0, 7.0, 9.0]);
    assert_eq!(settings.get_layer_count(), 2);

    let costs = settings.get_trace_costs();
    assert_eq!(costs.len(), 4, "sized by the array, not by `layers`");
    assert_eq!(
        costs[..2].to_vec(),
        vec![
            ExpansionCostFactor {
                horizontal: 3.0,
                vertical: 2.0
            },
            ExpansionCostFactor {
                horizontal: 4.0,
                vertical: 5.0
            },
        ]
    );
    assert_eq!(
        costs[2..].to_vec(),
        vec![
            ExpansionCostFactor {
                horizontal: 0.0,
                vertical: 0.0
            };
            2
        ],
        "layers 2 and 3 do not exist, so both accessors take their out-of-range arm"
    );

    let scoring = settings.scoring.as_mut().expect("allocated");
    scoring.preferred_direction_trace_cost = Some(vec![2.0]);
    assert_eq!(settings.get_trace_costs().len(), 1);
}

#[test]
fn set_layer_count_rewipes_costs() {
    let mut settings = RouterSettings::new();
    settings.set_layer_count(2);
    settings.set_preferred_direction_trace_costs(0, 2.5);
    settings.set_against_preferred_direction_trace_costs(1, 3.5);
    settings.set_bend_cost(0, 4.0);
    settings.set_layer_active(1, false);
    assert_eq!(settings.get_preferred_direction_trace_costs(0), 2.5);

    settings.set_layer_count(2);

    assert_eq!(settings.get_preferred_direction_trace_costs(0), 1.0);
    assert_eq!(settings.get_against_preferred_direction_trace_costs(1), 1.0);
    assert_eq!(settings.get_bend_cost(0), 0.0);
    assert!(settings.get_layer_active(1));
}

#[test]
fn duplicate_replays_the_assignment_sequence() {
    let source = populated();

    let mut replay = RouterSettings::new();
    let layer_count = source.get_layer_count();
    if layer_count > 0 {
        replay.set_layer_count(layer_count);
    }
    replay.algorithm = source.algorithm.clone();
    replay.job_timeout_string = source.job_timeout_string.clone();
    if source.layers.is_some() {
        replay.layers = source.layers.clone();
    }
    replay.max_passes = source.max_passes;
    replay.max_items = source.max_items;
    replay.save_intermediate_stages = source.save_intermediate_stages;
    replay.copper_to_edge_clearance_um = source.copper_to_edge_clearance_um;
    replay.hole_clearance_um = source.hole_clearance_um;
    replay.neck_width_um = source.neck_width_um;
    replay.strict_drc = source.strict_drc;
    replay.ignore_net_classes = source.ignore_net_classes.clone();
    replay.trace_pull_tight_accuracy = source.trace_pull_tight_accuracy;
    replay.enabled = source.enabled;
    replay.vias_allowed = source.vias_allowed;
    replay.automatic_neckdown = source.automatic_neckdown;
    replay.max_threads = source.max_threads;
    replay.optimizer = Some(source.optimizer.clone().unwrap_or_default());
    replay.scoring = Some(source.scoring.clone().unwrap_or_default());
    replay.fanout = Some(source.fanout.clone().unwrap_or_default());

    let cloned = source.duplicate();
    assert_eq!(cloned.result_json_path, None);
    assert_eq!(replay.result_json_path, None);
    assert_eq!(cloned, replay);
}

#[test]
fn duplicate_differs_from_the_derived_clone() {
    let source = populated();
    assert_eq!(source.result_json_path.as_deref(), Some("/tmp/result.json"));
    assert_eq!(source.duplicate().result_json_path, None);
    assert_eq!(
        source.clone().result_json_path.as_deref(),
        Some("/tmp/result.json"),
        "the derived Clone is a plain deep copy and does NOT reproduce the Java bug"
    );

    let mut nulled = RouterSettings::new();
    nulled.scoring = None;
    nulled.optimizer = None;
    nulled.fanout = None;
    let cloned = nulled.duplicate();
    assert_eq!(cloned.scoring, Some(ScoringSettings::default()));
    assert_eq!(cloned.optimizer, Some(OptimizerSettings::default()));
    assert_eq!(cloned.fanout, Some(FanoutSettings::default()));
    assert_eq!(nulled.clone().scoring, None);
}

#[test]
fn duplicate_carries_every_other_field() {
    let source = populated();
    let cloned = source.duplicate();

    assert_eq!(cloned.algorithm.as_deref(), Some("alg"));
    assert_eq!(cloned.job_timeout_string.as_deref(), Some("1:00:00"));
    assert_eq!(cloned.max_passes, Some(42));
    assert_eq!(cloned.max_items, Some(7));
    assert_eq!(cloned.save_intermediate_stages, Some(true));
    assert_eq!(cloned.copper_to_edge_clearance_um, Some(11.0));
    assert_eq!(cloned.hole_clearance_um, Some(12.0));
    assert_eq!(cloned.neck_width_um, Some(130.0));
    assert_eq!(cloned.strict_drc, Some(true));
    assert_eq!(
        cloned.ignore_net_classes,
        Some(vec!["GND".to_string()]),
        "String[] is deep-copied at :506"
    );
    assert_eq!(cloned.trace_pull_tight_accuracy, Some(33));
    assert_eq!(cloned.enabled, Some(false));
    assert_eq!(cloned.vias_allowed, Some(false));
    assert_eq!(cloned.automatic_neckdown, Some(true));
    assert_eq!(cloned.max_threads, Some(3));
    assert_eq!(cloned.get_layer_count(), 2);
    assert_eq!(cloned.get_bend_cost(0), 2.5);
    assert_eq!(cloned.get_preferred_direction_trace_costs(0), 2.5);
    assert_eq!(
        cloned.scoring.as_ref().unwrap().default_bend_cost,
        Some(1.25)
    );
    assert_eq!(cloned.optimizer.as_ref().unwrap().max_passes, Some(55));
    assert_eq!(cloned.fanout.as_ref().unwrap().max_passes, Some(66));
}

fn populated() -> RouterSettings {
    let mut s = RouterSettings::new();
    s.set_layer_count(2);
    s.algorithm = Some("alg".to_string());
    s.job_timeout_string = Some("1:00:00".to_string());
    s.max_passes = Some(42);
    s.max_items = Some(7);
    s.save_intermediate_stages = Some(true);
    s.copper_to_edge_clearance_um = Some(11.0);
    s.hole_clearance_um = Some(12.0);
    s.neck_width_um = Some(130.0);
    s.strict_drc = Some(true);
    s.ignore_net_classes = Some(vec!["GND".to_string()]);
    s.trace_pull_tight_accuracy = Some(33);
    s.enabled = Some(false);
    s.vias_allowed = Some(false);
    s.automatic_neckdown = Some(true);
    s.max_threads = Some(3);
    s.result_json_path = Some("/tmp/result.json".to_string());
    s.set_bend_cost(0, 2.5);
    s.scoring
        .as_mut()
        .unwrap()
        .preferred_direction_trace_cost
        .as_mut()
        .unwrap()[0] = 2.5;
    s.scoring.as_mut().unwrap().default_bend_cost = Some(1.25);
    s.optimizer.as_mut().unwrap().max_passes = Some(55);
    s.fanout.as_mut().unwrap().max_passes = Some(66);
    s
}

#[test]
fn validate_matrix() {
    for (input, expected) in [
        (-1, 9999),
        (10_000, 9999),
        (0, i32::MAX),
        (50, 50),
        (9999, 9999),
    ] {
        let mut s = validatable();
        s.max_passes = Some(input);
        s.validate(&host());
        assert_eq!(s.max_passes, Some(expected), "max_passes {input}");
    }

    for (input, expected) in [
        (None, 2),
        (Some(-1), 2),
        (Some(0), 0),
        (Some(9), 4),
        (Some(2), 2),
        (Some(4), 4),
    ] {
        let mut s = validatable();
        s.max_threads = input;
        s.validate(&host());
        assert_eq!(s.max_threads, Some(expected), "max_threads {input:?}");
    }

    for (input, expected) in [(0, 500), (1, 1), (500, 500), (-7, 500)] {
        let mut s = validatable();
        s.trace_pull_tight_accuracy = Some(input);
        s.validate(&host());
        assert_eq!(
            s.trace_pull_tight_accuracy,
            Some(expected),
            "trace_pull_tight_accuracy {input}"
        );
    }
}

#[test]
fn an_unrecognised_pin_sorting_order_is_refused() {
    let mut settings = validatable();
    settings
        .fanout
        .get_or_insert_with(Default::default)
        .pin_sorting_order = Some("not_an_order".to_string());

    settings.validate(&host());

    assert_eq!(
        settings
            .fanout
            .as_ref()
            .and_then(|fanout| fanout.pin_sorting_order.as_deref()),
        Some("outer_first")
    );
}

#[test]
fn set_max_threads_normalizes_and_mirrors() {
    for (input, expected) in [
        (None, 2),
        (Some(-1), 2),
        (Some(0), 4),
        (Some(9), 4),
        (Some(2), 2),
    ] {
        let mut s = RouterSettings::new();
        s.set_max_threads(input, &host());
        assert_eq!(s.max_threads, Some(expected), "set_max_threads {input:?}");
        assert_eq!(
            s.optimizer.as_ref().unwrap().max_threads,
            Some(expected),
            "optimizer mirror for {input:?}"
        );
    }

    let mut s = RouterSettings::new();
    s.optimizer = None;
    s.set_max_threads(Some(0), &host());
    assert_eq!(s.max_threads, Some(4));
    assert!(s.optimizer.is_none());
}

#[test]
fn validate_and_normalize_disagree_about_zero_max_threads() {
    let mut validated = validatable();
    validated.max_threads = Some(0);
    validated.validate(&host());

    let mut normalized = RouterSettings::new();
    normalized.set_max_threads(Some(0), &host());

    assert_eq!(validated.max_threads, Some(0));
    assert_eq!(normalized.max_threads, Some(4));
    assert_ne!(validated.max_threads, normalized.max_threads);
}

#[test]
#[should_panic(expected = "RouterSettings.java:934")]
fn validate_panics_without_max_passes() {
    let mut s = RouterSettings::new();
    s.trace_pull_tight_accuracy = Some(500);
    s.validate(&host());
}

#[test]
#[should_panic(expected = "RouterSettings.java:958")]
fn validate_panics_without_trace_pull_tight_accuracy() {
    let mut s = RouterSettings::new();
    s.max_passes = Some(50);
    s.validate(&host());
}

#[test]
fn plain_setters() {
    let mut s = RouterSettings::new();
    s.set_max_passes(Some(7));
    assert_eq!(s.max_passes, Some(7));
    s.set_max_passes(None);
    assert_eq!(s.max_passes, None);

    s.set_job_timeout_string(Some("0:30:00".to_string()));
    assert_eq!(s.job_timeout_string.as_deref(), Some("0:30:00"));

    s.set_enabled(Some(false));
    assert_eq!(s.enabled, Some(false));

    s.set_vias_allowed(Some(false));
    assert_eq!(s.vias_allowed, Some(false));
    assert!(!s.get_vias_allowed());
    s.set_vias_allowed(None);
    assert!(s.get_vias_allowed());

    s.set_automatic_neckdown(true);
    assert!(s.get_automatic_neckdown());
    s.strict_drc = Some(true);
    assert!(s.is_strict_drc());
}

#[test]
fn opt_changed_area_ms_is_a_settable_field() {
    assert_eq!(RouterSettings::new().opt_changed_area_ms, None);
    assert_eq!(RouterSettings::default().opt_changed_area_ms, None);

    let argv = ["--router.opt_changed_area_ms=250".to_string()];
    let cli = copper_settings::sources::CliSettings::new(&argv);
    let settings = cli
        .get_settings()
        .expect("CliSettings always answers a table");
    assert_eq!(
        settings.opt_changed_area_ms,
        Some(250),
        "`--router.opt_changed_area_ms=250` must reach the field; if this fails the FieldSpec in \
         field_path.rs or the arm in set_router_leaf is missing"
    );
    assert!(cli.errors().is_empty());

    assert_eq!(settings.opt_changed_area_ms, Some(250));

    for (arg, want) in [("1000", 1000), ("0", 0), ("-1", -1)] {
        let argv = [format!("--router.opt_changed_area_ms={arg}")];
        let parsed = copper_settings::sources::CliSettings::new(&argv)
            .get_settings()
            .unwrap()
            .opt_changed_area_ms;
        assert_eq!(parsed, Some(want), "`--router.opt_changed_area_ms={arg}`");
    }
}

#[test]
fn opt_changed_area_ms_round_trips_and_is_absent_when_unset() {
    let unset = RouterSettings::new();
    let json = serde_json::to_value(&unset).expect("serializes");
    assert!(
        json.get("opt_changed_area_ms").is_none(),
        "an unset port-only field must not appear on the wire, or every committed settings \
         snapshot moves for a field nobody set"
    );

    let mut set = RouterSettings::new();
    set.opt_changed_area_ms = Some(1000);
    let json = serde_json::to_value(&set).expect("serializes");
    assert_eq!(json["opt_changed_area_ms"], 1000);

    let back: RouterSettings =
        serde_json::from_str(r#"{"opt_changed_area_ms":250}"#).expect("deserializes");
    assert_eq!(back.opt_changed_area_ms, Some(250));
}
