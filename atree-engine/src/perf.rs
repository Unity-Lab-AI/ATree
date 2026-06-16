//! Performance timing infrastructure.
//!
//! Enable with `--features perf`. When the feature is off, all macros expand
//! to nothing — zero instructions in the binary.

/// RAII timer. Only exists when `perf` feature is enabled.
#[cfg(feature = "perf")]
pub struct PerfTimer {
    label: &'static str,
    start: std::time::Instant,
}

#[cfg(feature = "perf")]
impl PerfTimer {
    pub fn start(label: &'static str) -> Self {
        Self { label, start: std::time::Instant::now() }
    }
}

#[cfg(feature = "perf")]
impl Drop for PerfTimer {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_millis();
        tracing::info!(phase = %self.label, elapsed_ms = ms as u64, "perf");
    }
}

/// Start a named perf timer. Compiles to nothing when `perf` feature is off.
#[macro_export]
macro_rules! perf_timer {
    ($label:expr) => {
        #[cfg(feature = "perf")]
        let _perf_timer = $crate::perf::PerfTimer::start($label);
    };
}

/// Print to stderr when `perf` feature is on. Compiles to nothing when off.
#[macro_export]
macro_rules! perf_print {
    ($($arg:tt)*) => {
        #[cfg(feature = "perf")]
        eprintln!($($arg)*);
    };
}

/// Get current process RSS in bytes from /proc/self/status.
/// Returns 0 if unavailable.
#[cfg(target_os = "linux")]
pub fn current_rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_ascii_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .map(|kb| kb * 1024)
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
pub fn current_rss_bytes() -> u64 {
    0 // unsupported platform
}

/// Format bytes as human-readable string.
#[inline]
pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    for unit in UNITS {
        if size < 1024.0 {
            return format!("{:.1} {}", size, unit);
        }
        size /= 1024.0;
    }
    format!("{:.1} TB", size)
}
