use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPressure {
    None,
    Soft,
    Hard,
}

impl MemoryPressure {
    fn from_u8(value: u8) -> MemoryPressure {
        match value {
            0 => MemoryPressure::None,
            1 => MemoryPressure::Soft,
            _ => MemoryPressure::Hard,
        }
    }
}

/// Watches resident memory on a background thread. Crossing the soft ceiling fires `on_soft`
/// once; crossing `soft * HARD_CEILING_FACTOR` fires `on_hard` once.
pub struct MemoryGuard {
    stop: Arc<AtomicBool>,
    pressure: Arc<AtomicU8>,
    peak: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl MemoryGuard {
    pub fn spawn(
        soft_ceiling: u64,
        interval: Duration,
        sample: impl Fn() -> Option<u64> + Send + 'static,
        on_soft: impl Fn(u64) + Send + 'static,
        on_hard: impl Fn(u64) + Send + 'static,
    ) -> MemoryGuard {
        let hard_ceiling = (soft_ceiling as f64 * crate::HARD_CEILING_FACTOR) as u64;
        let stop = Arc::new(AtomicBool::new(false));
        let pressure = Arc::new(AtomicU8::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let handle = {
            let (stop, pressure, peak) = (stop.clone(), pressure.clone(), peak.clone());
            std::thread::Builder::new()
                .name("memory-guard".to_string())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(rss) = sample() {
                            peak.fetch_max(rss, Ordering::Relaxed);
                            let level = pressure.load(Ordering::Relaxed);
                            if level < 1 && rss >= soft_ceiling {
                                pressure.store(1, Ordering::Relaxed);
                                on_soft(rss);
                            }
                            if level < 2 && rss >= hard_ceiling {
                                pressure.store(2, Ordering::Relaxed);
                                on_hard(rss);
                            }
                        }
                        std::thread::park_timeout(interval);
                    }
                })
                .ok()
        };
        MemoryGuard {
            stop,
            pressure,
            peak,
            handle,
        }
    }

    pub fn pressure(&self) -> MemoryPressure {
        MemoryPressure::from_u8(self.pressure.load(Ordering::Relaxed))
    }

    pub fn peak_bytes(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

impl Drop for MemoryGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "linux")]
pub fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * rustix::param::page_size() as u64)
}

#[cfg(not(target_os = "linux"))]
pub fn rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
pub fn mem_available_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(target_os = "linux"))]
pub fn mem_available_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn fires_soft_then_hard_once_each() {
        let rss = Arc::new(AtomicU64::new(10));
        let events = Arc::new(Mutex::new(Vec::new()));
        let guard = {
            let (rss, soft_events, hard_events) = (rss.clone(), events.clone(), events.clone());
            MemoryGuard::spawn(
                100,
                Duration::from_millis(1),
                move || Some(rss.load(Ordering::Relaxed)),
                move |_| soft_events.lock().unwrap().push("soft"),
                move |_| hard_events.lock().unwrap().push("hard"),
            )
        };
        let wait_for = |want: MemoryPressure| {
            for _ in 0..2000 {
                if guard.pressure() == want {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("never reached {want:?}");
        };
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(guard.pressure(), MemoryPressure::None);
        rss.store(110, Ordering::Relaxed);
        wait_for(MemoryPressure::Soft);
        rss.store(130, Ordering::Relaxed);
        wait_for(MemoryPressure::Hard);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(guard.peak_bytes(), 130);
        drop(guard);
        assert_eq!(*events.lock().unwrap(), vec!["soft", "hard"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_this_process_and_host() {
        assert!(rss_bytes().unwrap() > 0);
        assert!(mem_available_bytes().unwrap() > 0);
    }
}
