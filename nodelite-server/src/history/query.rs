//! 历史查询聚合逻辑。

use chrono::{DateTime, TimeZone, Utc};
use nodelite_proto::HistoryPoint;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Error)]
pub(super) enum HistoryQueryError {
    #[error("sqlite history query failed")]
    Sql(#[from] rusqlite::Error),
}

pub(crate) const HISTORY_QUERY_SQL: &str = r#"
        SELECT
            ?1 AS node_id,
            MAX(recorded_at) AS recorded_at,
            AVG(cpu_usage_percent) AS cpu_usage_percent,
            AVG(memory_used_percent) AS memory_used_percent,
            AVG(rx_bytes_per_sec) AS rx_bytes_per_sec,
            AVG(tx_bytes_per_sec) AS tx_bytes_per_sec,
            AVG(latency_ms) AS latency_ms,
            AVG(disk_used_percent) AS disk_used_percent
        FROM history_points INDEXED BY idx_history_points_covering_metrics
        WHERE node_id = ?1 AND recorded_at >= ?2 AND recorded_at <= ?3
        GROUP BY ((recorded_at - ?2) / ?4)
        ORDER BY recorded_at ASC
        LIMIT ?5
        "#;

pub(super) fn query_history(
    connection: &Connection,
    node_id: &str,
    since: DateTime<Utc>,
    max_points: usize,
) -> Result<Vec<HistoryPoint>, HistoryQueryError> {
    query_history_between(connection, node_id, since, Utc::now(), max_points)
}

/// 在 `[since, until]` 之间查询某节点的历史点,并按时间升序返回。
pub(super) fn query_history_between(
    connection: &Connection,
    node_id: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    max_points: usize,
) -> Result<Vec<HistoryPoint>, HistoryQueryError> {
    let max_points = max_points.max(1);
    let span_seconds = (until.timestamp() - since.timestamp()).max(1);
    let bucket_seconds = ((span_seconds as usize).div_ceil(max_points)).max(1) as i64;
    let mut statement = connection.prepare(HISTORY_QUERY_SQL)?;
    let rows = statement.query_map(
        params![
            node_id,
            since.timestamp(),
            until.timestamp(),
            bucket_seconds,
            max_points as i64
        ],
        |row| {
            let recorded_at = row.get::<_, i64>(1)?;
            Ok(HistoryPoint {
                node_id: row.get(0)?,
                recorded_at: Utc
                    .timestamp_opt(recorded_at, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
                cpu_usage_percent: row.get(2)?,
                memory_used_percent: row.get(3)?,
                rx_bytes_per_sec: row.get(4)?,
                tx_bytes_per_sec: row.get(5)?,
                latency_ms: row
                    .get::<_, Option<f64>>(6)?
                    .map(|value| value.max(0.0).round() as u64),
                disk_used_percent: row.get(7)?,
            })
        },
    )?;

    let mut points = Vec::new();
    for row in rows {
        points.push(row?);
    }
    Ok(points)
}
