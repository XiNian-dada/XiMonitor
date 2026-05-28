//! HTTP 路由处理器:面板页面、只读 JSON API、认证流程与 Agent 安装脚本下发。
//!
//! 这里的 `mod.rs` 只负责拼装子模块导出,把 handler 按职责拆到更窄的文件里:
//! - `pages`: HTML 页面与静态 UI 资源;
//! - `auth_routes`: 只读认证、2FA 校验与健康探针;
//! - `api`: 仪表盘读取接口与 Prometheus 导出;
//! - `install`: Agent 安装脚本与 bootstrap 下发;
//! - `settings`: 管理面板的写操作与设置查询。

mod api;
mod auth_routes;
mod install;
pub(crate) mod metrics_exporter;
mod pages;
mod settings;

use crate::AppState;
use crate::audit::{AuditEventType, NewAuditEvent};

pub(crate) use api::{
    audit_log, bootstrap, metrics, node_history, node_logs, node_status, nodes, overview,
};
pub(crate) use auth_routes::{
    healthz, logout_and_reauth, readyz, require_readonly_auth, verify_2fa_api,
};
pub(crate) use install::{install_agent_script, install_bootstrap};
pub(crate) use pages::{
    brand_logo_dark_asset, brand_logo_light_asset, index, index_alert_settings_js_asset,
    index_settings_js_asset, node_detail, ui_i18n_asset, verify_2fa_page,
};
pub(crate) use settings::{
    alert_settings, change_readonly_password, disable_two_factor, enable_two_factor,
    refresh_node_token, server_update_log, settings, start_server_update, start_two_factor_setup,
    update_alert_settings,
};

async fn record_audit_event(
    state: &AppState,
    event_type: AuditEventType,
    client_ip: String,
    success: bool,
    user_agent: Option<String>,
    details: serde_json::Value,
) {
    let mut event = NewAuditEvent::now(event_type, client_ip, success);
    event.user_agent = user_agent;
    event.details = details;
    state.audit_log.record_best_effort(event).await;
}

#[cfg(test)]
pub(crate) fn is_well_formed_install_token(token: &str) -> bool {
    install::is_well_formed_install_token(token)
}
