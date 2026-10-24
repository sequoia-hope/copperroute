pub mod band;
pub mod blame;
pub mod drc;
pub mod frames;
pub mod info;
pub mod reshape;
pub mod route;
pub mod settle;

use crate::cli::Cli;
use crate::ops::SettingsOverrides;

pub fn overrides(cli: &Cli, set: &[String]) -> SettingsOverrides {
    SettingsOverrides {
        settings_file: cli.settings.clone(),
        set: set.to_vec(),
        sparse: None,
    }
}

/// Lowers this process's priority and resolves its memory ceiling, before any routing starts.
pub fn govern(cli: &Cli) -> Result<Option<u64>, crate::ExitCode> {
    let limits = copper_governor::Limits::from_env(cli.nice, cli.max_memory).map_err(|error| {
        tracing::error!("{error}");
        crate::ExitCode::UsageError
    })?;
    match copper_governor::lower_priority(limits.nice) {
        Ok(nice) => tracing::info!("running at niceness {nice}"),
        Err(error) => tracing::warn!("{error}"),
    }
    let ceiling = limits
        .memory
        .resolve(copper_governor::mem_available_bytes());
    match ceiling {
        Some(bytes) => tracing::info!("memory ceiling {} MiB", bytes >> 20),
        None => tracing::info!("no memory ceiling"),
    }
    Ok(ceiling)
}
