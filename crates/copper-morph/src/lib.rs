#![forbid(unsafe_code)]
//! Moving placed parts by as much as the design rules admit.
//!
//! The admissibility rule is the settle engine's: a move is allowed when it creates no clearance
//! conflict that was not already there before the move. Pairs that start in conflict are
//! grandfathered, so a board that is not DRC-clean can still be nudged without being made worse.

mod oracle;
mod settle;
mod shove;
mod warp;

pub use oracle::{Conflict, MoveOracle, scale};
pub use settle::{MoveKind, PartOutcome, SettleOptions, SettleReport};
pub use warp::WarpField;
