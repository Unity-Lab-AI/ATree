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
