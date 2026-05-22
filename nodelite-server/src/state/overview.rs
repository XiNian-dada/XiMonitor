//! 概览聚合与数值安全辅助逻辑。

use chrono::Utc;
use nodelite_proto::{NodeStatus, OverviewData};

pub(super) fn build_overview(statuses: &[NodeStatus]) -> OverviewData {
    let total_nodes = statuses.len();
    let online_nodes = statuses.iter().filter(|status| status.online).count();
    let offline_nodes = total_nodes.saturating_sub(online_nodes);
    let total_rx_bytes = statuses
        .iter()
        .filter_map(|status| status.snapshot.as_ref())
        .fold(0_u64, |total, snapshot| {
            total.saturating_add(snapshot.network.total_rx_bytes)
        });
    let total_tx_bytes = statuses
        .iter()
        .filter_map(|status| status.snapshot.as_ref())
        .fold(0_u64, |total, snapshot| {
            total.saturating_add(snapshot.network.total_tx_bytes)
        });
    let current_rx_bytes_per_sec = statuses
        .iter()
        .filter_map(|status| status.snapshot.as_ref())
        .filter_map(|snapshot| snapshot.network.rx_bytes_per_sec)
        .fold(0.0, sum_finite_f64);
    let current_tx_bytes_per_sec = statuses
        .iter()
        .filter_map(|status| status.snapshot.as_ref())
        .filter_map(|snapshot| snapshot.network.tx_bytes_per_sec)
        .fold(0.0, sum_finite_f64);

    let mut latency_total = 0_u128;
    let mut latency_samples = 0_usize;
    for latency in statuses
        .iter()
        .filter(|status| status.online)
        .filter_map(|status| status.latency_ms)
    {
        latency_total = latency_total.saturating_add(latency as u128);
        latency_samples += 1;
    }
    let average_latency_ms =
        (latency_samples > 0).then(|| latency_total as f64 / latency_samples as f64);

    OverviewData {
        generated_at: Utc::now(),
        total_nodes,
        online_nodes,
        offline_nodes,
        total_rx_bytes,
        total_tx_bytes,
        current_rx_bytes_per_sec,
        current_tx_bytes_per_sec,
        average_latency_ms,
    }
}

/// 把浮点数累加器中的非法值(NaN / 负值 / 溢出)安全过滤掉。
fn sum_finite_f64(total: f64, value: f64) -> f64 {
    if !value.is_finite() || value < 0.0 {
        return total;
    }

    let next = total + value;
    if next.is_finite() { next } else { f64::MAX }
}
