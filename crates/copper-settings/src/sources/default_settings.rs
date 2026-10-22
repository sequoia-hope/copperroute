use crate::{
    BoardUpdateStrategy, HostEnvironment, ItemSelectionStrategy, RouterSettings, SettingsSource,
    SourceKind, merger::priority,
};

#[derive(Debug, Clone)]
pub struct DefaultSettings {
    settings: RouterSettings,
}

impl DefaultSettings {
    pub const DEFAULT_UNROUTED_NET_PENALTY: f32 = 5_000_000.0;

    pub const DEFAULT_CLEARANCE_VIOLATION_PENALTY: f32 = 1_000_000.0;

    pub const DEFAULT_BEND_PENALTY: f32 = 10.0;

    pub const DEFAULT_VIA_COSTS: i32 = 50;

    pub const DEFAULT_PLANE_VIA_COSTS: i32 = 5;

    pub const DEFAULT_START_RIPUP_COSTS: i32 = 100;

    pub const DEFAULT_SMD_VIA_COST_FACTOR: f64 = 0.1;

    pub const DEFAULT_PREFERRED_DIRECTION_TRACE_COST: f64 = 1.0;

    pub const DEFAULT_UNDESIRED_DIRECTION_TRACE_COST: f64 = 1.0;

    pub const DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM: f64 = 500.0;

    pub const DEFAULT_HOLE_CLEARANCE_UM: f64 = 0.0;

    const PRIORITY: i32 = priority::DEFAULT;

    #[must_use]
    pub fn new(host: &HostEnvironment) -> Self {
        let mut settings = RouterSettings::new();

        settings.enabled = Some(true);
        settings.algorithm = Some(RouterSettings::ALGORITHM_CURRENT.to_string());
        settings.job_timeout_string = Some("00:30:00".to_string());
        settings.max_passes = Some(9999);
        settings.max_items = Some(i32::MAX);
        settings.trace_pull_tight_accuracy = Some(500);
        settings.vias_allowed = Some(true);
        settings.automatic_neckdown = Some(true);
        settings.save_intermediate_stages = Some(false);
        settings.ignore_net_classes = Some(Vec::new());
        settings.max_threads = Some(host.default_max_threads());
        settings.copper_to_edge_clearance_um = Some(Self::DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM);
        settings.hole_clearance_um = Some(Self::DEFAULT_HOLE_CLEARANCE_UM);
        settings.neck_width_um = Some(0.0);
        settings.strict_drc = Some(false);
        settings.smd_via_relaxation = Some(true);
        settings.connection_search_steps = Some(250_000);

        let fanout = settings
            .fanout
            .as_mut()
            .expect("RouterSettings::new sets it");
        fanout.enabled = Some(true);
        fanout.max_passes = Some(20);
        fanout.max_milliseconds_per_pin = Some(10_000);
        fanout.ripup_allowed = Some(true);
        fanout.min_escape_length_mm = Some(2.5);
        fanout.max_escape_length_mm = Some(4.5);
        fanout.start_via_diameter_mm = Some(0.250);
        fanout.end_via_diameter_mm = Some(0.250);
        fanout.pin_sorting_order = Some("outer_first".to_string());
        fanout.max_items = Some(i32::MAX);
        fanout.fallback_to_board_vias = Some(true);

        let optimizer = settings
            .optimizer
            .as_mut()
            .expect("RouterSettings::new sets it");
        optimizer.enabled = Some(true);
        optimizer.algorithm = Some("copperroute-optimizer".to_string());
        optimizer.max_passes = Some(100);
        optimizer.max_items = Some(i32::MAX);
        optimizer.max_threads = Some(host.default_max_threads());
        optimizer.optimization_improvement_threshold = Some(0.01);
        optimizer.board_update_strategy = Some(BoardUpdateStrategy::Greedy);
        optimizer.hybrid_ratio = Some("1:1".to_string());
        optimizer.item_selection_strategy = Some(ItemSelectionStrategy::Prioritized);
        optimizer.additional_ripup_cost_factor_at_start = Some(10);
        optimizer.trace_ripup_cost_factor = Some(0.6);
        optimizer.max_autoroute_passes = Some(6);
        optimizer.max_consecutive_failures = Some(50);

        let scoring = settings
            .scoring
            .as_mut()
            .expect("RouterSettings::new sets it");
        scoring.default_preferred_direction_trace_cost =
            Some(Self::DEFAULT_PREFERRED_DIRECTION_TRACE_COST);
        scoring.default_undesired_direction_trace_cost =
            Some(Self::DEFAULT_UNDESIRED_DIRECTION_TRACE_COST);
        scoring.via_costs = Some(Self::DEFAULT_VIA_COSTS);
        scoring.plane_via_costs = Some(Self::DEFAULT_PLANE_VIA_COSTS);
        scoring.start_ripup_costs = Some(Self::DEFAULT_START_RIPUP_COSTS);
        scoring.unrouted_net_penalty = Some(Self::DEFAULT_UNROUTED_NET_PENALTY);
        scoring.clearance_violation_penalty = Some(Self::DEFAULT_CLEARANCE_VIOLATION_PENALTY);
        scoring.bend_penalty = Some(Self::DEFAULT_BEND_PENALTY);
        scoring.default_bend_cost = Some(0.0);
        scoring.smd_via_cost_factor = Some(Self::DEFAULT_SMD_VIA_COST_FACTOR);

        Self { settings }
    }
}

impl SettingsSource for DefaultSettings {
    fn get_settings(&self) -> Option<&RouterSettings> {
        Some(&self.settings)
    }

    fn get_source_name(&self) -> String {
        "Default Settings".to_string()
    }

    fn get_priority(&self) -> i32 {
        Self::PRIORITY
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Default
    }
}
