use std::path::PathBuf;

use tokio::runtime::Runtime;

use super::{AuditEventType, AuditLog, AuditLogError, AuditQuery, NewAuditEvent};
use crate::audit::support::sample_config;

#[test]
fn disabled_audit_log_rejects_queries_but_ignores_records() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let mut config = sample_config(PathBuf::from("/tmp/disabled-audit.sqlite3"));
        config.enabled = false;
        let audit = AuditLog::new(config, 5);

        audit
            .record(NewAuditEvent::now(
                AuditEventType::LoginFailure,
                "127.0.0.1",
                false,
            ))
            .await
            .expect("disabled audit log should no-op on record");

        let error = audit
            .query(AuditQuery {
                start: None,
                end: None,
                event_type: None,
                success: None,
                limit: 10,
            })
            .await
            .expect_err("disabled audit log should reject queries");
        assert!(matches!(error, AuditLogError::Disabled));
    });
}
