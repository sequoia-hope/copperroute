#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
pub mod logging;
pub mod mcp;
pub mod ops;

use clap::Parser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Ok = 0,
    Failure = 1,
    UsageError = 2,
}

impl ExitCode {
    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }
}

#[must_use]
pub fn run(raw: &[String]) -> ExitCode {
    let cli = match cli::Cli::try_parse_from(
        std::iter::once("copperroute".to_string()).chain(raw.iter().cloned()),
    ) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return if error.use_stderr() {
                ExitCode::UsageError
            } else {
                ExitCode::Ok
            };
        }
    };
    logging::init(logging::level_for(cli.verbose, cli.log_level.as_deref()));

    match &cli.command {
        cli::Command::Route(args) => match commands::govern(&cli) {
            Ok(ceiling) => commands::route::run(&cli, args, ceiling),
            Err(code) => code,
        },
        cli::Command::Drc(args) => commands::drc::run(&cli, args),
        cli::Command::Info(args) => commands::info::run(&cli, args),
        cli::Command::Settle(args) => match commands::govern(&cli) {
            Ok(_) => commands::settle::run(&cli, args),
            Err(code) => code,
        },
        cli::Command::Blame(args) => match commands::govern(&cli) {
            Ok(_) => commands::blame::run(&cli, args),
            Err(code) => code,
        },
        cli::Command::Band(args) => match commands::govern(&cli) {
            Ok(_) => commands::band::run(&cli, args),
            Err(code) => code,
        },
        cli::Command::Reshape(args) => match commands::govern(&cli) {
            Ok(_) => commands::reshape::run(&cli, args),
            Err(code) => code,
        },
        cli::Command::Mcp => match commands::govern(&cli) {
            Ok(ceiling) => match mcp::stdio::run(commands::overrides(&cli, &[]), ceiling) {
                0 => ExitCode::Ok,
                _ => ExitCode::Failure,
            },
            Err(code) => code,
        },
    }
}
