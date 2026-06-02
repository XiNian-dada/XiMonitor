use tokio::runtime::Runtime;

use super::{AuditEventType, AuditLog, NewAuditEvent};
use crate::audit::support::{sample_config, unique_temp_dir};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
#[cfg(unix)]
fn audit_database_artifacts_are_mode_600() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let temp_dir = unique_temp_dir("nodelite-audit-mode");
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let db_path = temp_dir.join("audit.sqlite3");
        let audit = AuditLog::new(sample_config(db_path.clone()), 5);
        audit.initialize().await.expect("audit should initialize");
        audit
            .record(NewAuditEvent::now(
                AuditEventType::NodeConnected,
                "198.51.100.20",
                true,
            ))
            .await
            .expect("audit event should persist");
        audit.shutdown().await;

        let data_dir_mode = std::fs::metadata(&temp_dir)
            .expect("temp dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(data_dir_mode, 0o700);

        let db_mode = std::fs::metadata(&db_path)
            .expect("db metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(db_mode, 0o600);

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}
