//! 概览聚合与数值安全辅助逻辑。

use chrono::Utc;
use nodelite_proto::{NodeStatus, OverviewData};

pub(super) fn build_overview(statuses: &[NodeStatus]) -> OverviewData {
    build_overview_from_iter(statuses.iter())
}

pub(super) fn build_overview_from_iter<'a>(
    statuses: impl IntoIterator<Item = &'a NodeStatus>,
) -> OverviewData {
    let mut total_nodes = 0_usize;
    let mut online_nodes = 0_usize;
    let mut total_rx_bytes = 0_u64;
    let mut total_tx_bytes = 0_u64;
    let mut current_rx_bytes_per_sec = 0.0;
    let mut current_tx_bytes_per_sec = 0.0;
    let mut latency_total = 0_u128;
    let mut latency_samples = 0_usize;

    for status in statuses {
        total_nodes += 1;
        if status.online {
            online_nodes += 1;
            if let Some(latency) = status.latency_ms {
                latency_total = latency_total.saturating_add(latency as u128);
                latency_samples += 1;
            }
        }

        let Some(snapshot) = status.snapshot.as_ref() else {
            continue;
        };
        total_rx_bytes = total_rx_bytes.saturating_add(snapshot.network.total_rx_bytes);
        total_tx_bytes = total_tx_bytes.saturating_add(snapshot.network.total_tx_bytes);
        if let Some(rx) = snapshot.network.rx_bytes_per_sec {
            current_rx_bytes_per_sec = sum_finite_f64(current_rx_bytes_per_sec, rx);
        }
        if let Some(tx) = snapshot.network.tx_bytes_per_sec {
            current_tx_bytes_per_sec = sum_finite_f64(current_tx_bytes_per_sec, tx);
        }
    }

    let offline_nodes = total_nodes.saturating_sub(online_nodes);
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
