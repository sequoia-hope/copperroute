pub mod control;
mod corridor;
pub mod destination_distance;
pub mod engine;
pub mod expand;
pub mod expansion_engine;
pub mod list_element;
pub mod queue;
pub mod ripup_history;
pub mod ripup_resolver;
pub mod search;
pub mod search_element;
pub mod trace_shover;

pub use control::{AutorouteControl, ViaMask, ViaPricing};
pub use destination_distance::DestinationDistance;
pub use engine::{AutorouteEngine, route_connection, route_connection_full};
pub use expansion_engine::MazeExpansionEngine;
pub use list_element::MazeListElement;
pub use queue::MazeQueue;
pub use ripup_resolver::MazeRipupResolver;
pub use search::{ALREADY_RIPPED_COSTS, MazeResult, MazeSearchEngine, ShoveResult};
pub use search_element::{MazeAdjustment, MazeSearchElement};
pub use trace_shover::{DoorSection, MazeTraceShover};

pub const TRACE_WIDTH_TOLERANCE: i32 = 2;
