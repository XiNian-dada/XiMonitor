use axum::extract::Path as AxumPath;
use axum::response::Response;

use crate::web_assets;

/// SPA 首页 — 返回 index.html
pub(crate) async fn index() -> Response {
    web_assets::spa_index()
}

/// SPA 节点详情页 — 返回 index.html (路由由前端处理)
pub(crate) async fn node_detail(AxumPath(_node_id): AxumPath<String>) -> Response {
    web_assets::spa_index()
}

/// 静态资源 — 从 /assets/* 路径提供
pub(crate) async fn static_asset(AxumPath(path): AxumPath<String>) -> Response {
    web_assets::static_asset(&path)
}

/// 2FA 验证页面 — 返回 verify-2fa.html (独立 Vite entry)
pub(crate) async fn verify_2fa_page() -> Response {
    web_assets::static_asset("verify-2fa.html")
}
