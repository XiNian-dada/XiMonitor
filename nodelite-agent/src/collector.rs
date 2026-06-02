//! 主机指标采集器入口:按目标平台分派到具体实现。

#[cfg(any(target_os = "linux", target_os = "macos"))]
use tracing::warn;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "collector/shared.rs"]
mod shared;

#[cfg(target_os = "linux")]
#[path = "collector_linux.rs"]
mod collector_linux;
#[cfg(target_os = "linux")]
pub use collector_linux::{HostCollector, new_collector};

#[cfg(target_os = "macos")]
#[path = "collector_macos.rs"]
mod collector_macos;
#[cfg(target_os = "macos")]
pub use collector_macos::{HostCollector, new_collector};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "collector_unsupported.rs"]
mod collector_unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use collector_unsupported::{HostCollector, new_collector};

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn log_network_rate_anomalies(anomalies: shared::NetworkRateAnomalies) {
    for anomaly in [anomalies.rx, anomalies.tx].into_iter().flatten() {
        warn!(
            direction = anomaly.direction.as_str(),
            rate_bytes_per_sec = anomaly.rate_bytes_per_sec,
            baseline_avg_bytes_per_sec = anomaly.baseline_avg_bytes_per_sec,
            effective_baseline_bytes_per_sec = anomaly.effective_baseline_bytes_per_sec,
            multiplier = anomaly.multiplier,
            sample_count = anomaly.sample_count,
            "network rate is more than 100x above recent baseline; keeping sample",
        );
    }
}
