pub mod drc;
pub mod fixed_wiring;
pub mod info;
pub mod load;
pub mod route;
pub mod settings;

pub use drc::{DrcOutcome, DrcRequest, drc};
pub use info::{InfoOutcome, InfoRequest, info};
pub use load::{BoardSource, LoadRequest, Loaded, load};
pub use route::{OutputFormat, OutputTarget, RouteOutcome, RouteRequest, route};
pub use settings::SettingsOverrides;

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("{0}")]
    Input(String),
    #[error("{0}")]
    Load(String),
    #[error("{0}")]
    Settings(String),
    #[error(transparent)]
    Router(#[from] copper_router::RouterError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<copper_core::Error> for OpError {
    fn from(error: copper_core::Error) -> Self {
        match error {
            copper_core::Error::Router(inner) => OpError::Router(inner),
            copper_core::Error::Io(inner) => OpError::Io(inner),
            other => OpError::Load(other.to_string()),
        }
    }
}
