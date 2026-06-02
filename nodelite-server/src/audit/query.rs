//! Audit log query helpers.

use anyhow::anyhow;
use chrono::{TimeZone, Utc};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde_json::Value;

use super::{AuditEvent, AuditEventType, AuditLogError, AuditQuery};

const AUDIT_QUERY_SELECT: &str = "\
SELECT id, timestamp, event_type, user, node_id, ip_address, user_agent, success, details
FROM audit_log";
const AUDIT_QUERY_ORDER_LIMIT: &str = " ORDER BY timestamp DESC, id DESC LIMIT ?";

#[derive(Debug)]
struct AuditSql {
    sql: String,
    params: Vec<SqlValue>,
}

pub(super) fn query_events(
    connection: &Connection,
    query: &AuditQuery,
) -> std::result::Result<Vec<AuditEvent>, AuditLogError> {
    let audit_sql = build_audit_query(query);

    let mut statement = connection
        .prepare(&audit_sql.sql)
        .map_err(|error| AuditLogError::Query(anyhow!("failed to prepare audit query: {error}")))?;
    let rows = statement
        .query_map(params_from_iter(audit_sql.params.iter()), |row| {
            let event_type = row.get::<_, String>(2)?;
            let details = row.get::<_, String>(8)?;
            let timestamp = row.get::<_, i64>(1)?;
            let event_type = AuditEventType::parse(&event_type).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(format!(
                        "unknown audit event type {event_type}"
                    ))),
                )
            })?;
            let details = serde_json::from_str::<Value>(&details).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(AuditEvent {
                id: row.get(0)?,
                timestamp: Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(std::io::Error::other(format!(
                            "invalid audit timestamp {timestamp}"
                        ))),
                    )
                })?,
                event_type,
                user: row.get(3)?,
                node_id: row.get(4)?,
                ip_address: row.get(5)?,
                user_agent: row.get(6)?,
                success: row.get::<_, i64>(7)? != 0,
                details,
            })
        })
        .map_err(|error| AuditLogError::Query(anyhow!("failed to execute audit query: {error}")))?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| AuditLogError::Query(anyhow!("failed to decode audit rows: {error}")))
}

fn build_audit_query(query: &AuditQuery) -> AuditSql {
    let mut sql = String::from(AUDIT_QUERY_SELECT);
    let mut predicates = Vec::new();
    let mut params = Vec::new();

    if let Some(start) = query.start {
        predicates.push("timestamp >= ?");
        params.push(SqlValue::Integer(start.timestamp()));
    }
    if let Some(end) = query.end {
        predicates.push("timestamp <= ?");
        params.push(SqlValue::Integer(end.timestamp()));
    }
    if let Some(event_type) = query.event_type {
        predicates.push("event_type = ?");
        params.push(SqlValue::Text(event_type.as_str().to_string()));
    }
    if let Some(success) = query.success {
        predicates.push("success = ?");
        params.push(SqlValue::Integer(success as i64));
    }

    if !predicates.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&predicates.join(" AND "));
    }
    sql.push_str(AUDIT_QUERY_ORDER_LIMIT);
    params.push(SqlValue::Integer(query.limit as i64));

    AuditSql { sql, params }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use rusqlite::Connection;

    use super::{super::storage::AUDIT_TABLE_SQL, *};

    #[test]
    fn default_audit_query_uses_timestamp_order_fast_path() {
        let audit_sql = build_audit_query(&AuditQuery {
            start: None,
            end: None,
            event_type: None,
            success: None,
            limit: 100,
        });

        assert!(!audit_sql.sql.contains(" WHERE "));
        assert!(!audit_sql.sql.contains("IS NULL"));
        assert_eq!(audit_sql.params, vec![SqlValue::Integer(100)]);

        let plan = explain_plan(&audit_sql);
        assert!(
            plan.contains("idx_audit_log_timestamp"),
            "default audit query should use timestamp index, got:\n{plan}"
        );
        assert!(
            !plan.contains("TEMP B-TREE"),
            "default audit query should not require temporary sort, got:\n{plan}"
        );
    }

    #[test]
    fn filtered_audit_query_uses_active_predicates_without_or_null() {
        let now = Utc::now();
        let audit_sql = build_audit_query(&AuditQuery {
            start: Some(now - Duration::hours(1)),
            end: Some(now),
            event_type: Some(AuditEventType::LoginFailure),
            success: Some(false),
            limit: 50,
        });

        assert!(!audit_sql.sql.contains("IS NULL"));
        assert!(!audit_sql.sql.contains(" OR "));
        assert!(audit_sql.sql.contains("timestamp >= ?"));
        assert!(audit_sql.sql.contains("timestamp <= ?"));
        assert!(audit_sql.sql.contains("event_type = ?"));
        assert!(audit_sql.sql.contains("success = ?"));
        assert_eq!(audit_sql.params.len(), 5);
    }

    #[test]
    fn event_success_audit_query_uses_composite_index() {
        let audit_sql = build_audit_query(&AuditQuery {
            start: None,
            end: None,
            event_type: Some(AuditEventType::LoginFailure),
            success: Some(false),
            limit: 50,
        });

        assert!(!audit_sql.sql.contains("IS NULL"));
        assert!(!audit_sql.sql.contains(" OR "));

        let plan = explain_plan(&audit_sql);
        assert!(
            plan.contains("idx_audit_log_event_success_time"),
            "event+success audit query should use composite index, got:\n{plan}"
        );
    }

    fn explain_plan(audit_sql: &AuditSql) -> String {
        let connection = Connection::open_in_memory().expect("in-memory audit db should open");
        connection
            .execute_batch(AUDIT_TABLE_SQL)
            .expect("audit schema should initialize");

        let explain_sql = format!("EXPLAIN QUERY PLAN {}", audit_sql.sql);
        let mut statement = connection
            .prepare(&explain_sql)
            .expect("audit explain should prepare");
        let details = statement
            .query_map(params_from_iter(audit_sql.params.iter()), |row| {
                row.get::<_, String>(3)
            })
            .expect("audit explain should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("audit explain rows should decode");
        details.join("\n")
    }
}
