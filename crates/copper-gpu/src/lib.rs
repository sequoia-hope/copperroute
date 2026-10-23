#![forbid(unsafe_code)]
//! Grid routing whose search can run on the GPU: a conservative per-net grid, a cost field from
//! the sources (CPU reference or GPU kernel), and a walk back down it.

pub mod gpu;
pub mod grid;
pub mod negotiate;
pub mod route;
pub mod search;

pub use gpu::GpuSolver;
pub use grid::RouteGrid;
pub use negotiate::{NegotiationOptions, NegotiationStats, route_negotiated};
pub use route::{CpuSolver, FieldSolver, GridOptions, GridStats, route_open_connections};
