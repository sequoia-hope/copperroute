#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostEnvironment {
    available_processors: usize,
}

impl HostEnvironment {
    pub fn detect() -> Self {
        let available_processors = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        Self {
            available_processors,
        }
    }

    pub fn with_processors(n: usize) -> Self {
        Self {
            available_processors: n,
        }
    }

    pub fn available_processors(&self) -> usize {
        self.available_processors
    }

    pub fn default_max_threads(&self) -> i32 {
        if self.available_processors == 0 {
            return 1;
        }
        std::cmp::max(1, self.available_processors as i32 / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_threads_leaves_half_the_host_free() {
        assert_eq!(HostEnvironment::with_processors(1).default_max_threads(), 1);
        assert_eq!(HostEnvironment::with_processors(8).default_max_threads(), 4);
        assert_eq!(
            HostEnvironment::with_processors(24).default_max_threads(),
            12
        );
        assert_eq!(HostEnvironment::with_processors(0).default_max_threads(), 1);
    }

    #[test]
    fn detect_reports_at_least_one_processor() {
        assert!(HostEnvironment::detect().available_processors() >= 1);
    }
}
