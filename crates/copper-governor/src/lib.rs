#![forbid(unsafe_code)]

mod memory;
mod priority;
mod size;

pub use memory::{MemoryGuard, MemoryPressure, mem_available_bytes, rss_bytes};
pub use priority::lower_priority;
pub use size::parse_byte_size;

pub const NICE_ENV: &str = "COPPERROUTE_NICE";
pub const MAX_MEMORY_ENV: &str = "COPPERROUTE_MAX_MEMORY";
pub const DEFAULT_NICE: i32 = 10;
pub const HARD_CEILING_FACTOR: f64 = 1.25;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("{name}: {message}")]
    Invalid { name: String, message: String },
    #[error("couldn't lower the process priority: {0}")]
    Priority(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCeiling {
    Auto,
    Off,
    Bytes(u64),
}

impl MemoryCeiling {
    pub fn parse(value: &str) -> Result<MemoryCeiling, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(MemoryCeiling::Auto),
            "off" | "0" => Ok(MemoryCeiling::Off),
            other => parse_byte_size(other).map(MemoryCeiling::Bytes),
        }
    }

    pub fn resolve(self, available: Option<u64>) -> Option<u64> {
        match self {
            MemoryCeiling::Off => None,
            MemoryCeiling::Bytes(bytes) => Some(bytes),
            MemoryCeiling::Auto => available.map(|bytes| bytes / 2),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub nice: i32,
    pub memory: MemoryCeiling,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            nice: DEFAULT_NICE,
            memory: MemoryCeiling::Auto,
        }
    }
}

impl Limits {
    /// Flags win over the environment, which wins over the defaults.
    pub fn resolve(
        nice_flag: Option<i32>,
        memory_flag: Option<MemoryCeiling>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Limits, Error> {
        let nice = match nice_flag {
            Some(nice) => nice,
            None => match env(NICE_ENV) {
                Some(raw) => raw.trim().parse().map_err(|_| Error::Invalid {
                    name: NICE_ENV.to_string(),
                    message: format!("'{raw}' is not an integer niceness"),
                })?,
                None => DEFAULT_NICE,
            },
        };
        if !(0..=19).contains(&nice) {
            return Err(Error::Invalid {
                name: "nice".to_string(),
                message: format!("{nice} is outside 0..=19"),
            });
        }
        let memory = match memory_flag {
            Some(memory) => memory,
            None => match env(MAX_MEMORY_ENV) {
                Some(raw) => MemoryCeiling::parse(&raw).map_err(|message| Error::Invalid {
                    name: MAX_MEMORY_ENV.to_string(),
                    message,
                })?,
                None => MemoryCeiling::Auto,
            },
        };
        Ok(Limits { nice, memory })
    }

    pub fn from_env(
        nice_flag: Option<i32>,
        memory_flag: Option<MemoryCeiling>,
    ) -> Result<Limits, Error> {
        Limits::resolve(nice_flag, memory_flag, |name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_are_niced_with_an_automatic_ceiling() {
        assert_eq!(
            Limits::resolve(None, None, no_env).unwrap(),
            Limits::default()
        );
    }

    #[test]
    fn flags_override_the_environment() {
        let env = |name: &str| match name {
            NICE_ENV => Some("5".to_string()),
            MAX_MEMORY_ENV => Some("2G".to_string()),
            _ => None,
        };
        assert_eq!(
            Limits::resolve(None, None, env).unwrap(),
            Limits {
                nice: 5,
                memory: MemoryCeiling::Bytes(2 << 30)
            }
        );
        assert_eq!(
            Limits::resolve(Some(0), Some(MemoryCeiling::Off), env).unwrap(),
            Limits {
                nice: 0,
                memory: MemoryCeiling::Off
            }
        );
    }

    #[test]
    fn rejects_out_of_range_niceness_and_bad_sizes() {
        assert!(Limits::resolve(Some(-5), None, no_env).is_err());
        assert!(Limits::resolve(Some(20), None, no_env).is_err());
        let env = |name: &str| (name == MAX_MEMORY_ENV).then(|| "lots".to_string());
        assert!(Limits::resolve(None, None, env).is_err());
    }

    #[test]
    fn auto_ceiling_is_half_of_available_memory() {
        assert_eq!(MemoryCeiling::Auto.resolve(Some(8 << 30)), Some(4 << 30));
        assert_eq!(MemoryCeiling::Auto.resolve(None), None);
        assert_eq!(MemoryCeiling::Off.resolve(Some(8 << 30)), None);
        assert_eq!(MemoryCeiling::parse("off"), Ok(MemoryCeiling::Off));
        assert_eq!(MemoryCeiling::parse("AUTO"), Ok(MemoryCeiling::Auto));
    }
}
