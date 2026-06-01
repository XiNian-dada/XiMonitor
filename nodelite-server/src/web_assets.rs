//! Web assets module: serves the Vue SPA and static files embedded at compile time.
//!
//! The Vite build output from `web/dist/` is embedded into the binary using `include_dir!`.
//! This module provides handlers for serving the SPA entry point and static assets with
//! appropriate cache headers.

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use include_dir::{include_dir, Dir};

/// Embedded web assets from `web/dist/`
static WEB_ASSETS: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Content Security Policy for the SPA
/// No inline scripts/styles needed since Vite outputs only external files
const SPA_CSP: &str = "default-src 'self'; \
    img-src 'self' data:; \
    connect-src 'self' https://raw.githubusercontent.com https://api.github.com; \
    font-src 'self'; \
    object-src 'none'; \
    media-src 'none'; \
    worker-src 'none'; \
    base-uri 'none'; \
    frame-ancestors 'none'; \
    form-action 'self'";

/// Cache control for SPA entry points (never cache)
const NO_CACHE: &str = "no-store, no-cache, must-revalidate";

/// Cache control for hashed static assets (cache forever)
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Serves the SPA index.html (for `/` and `/nodes/:id` routes)
pub fn spa_index() -> Response {
    serve_file("index.html", NO_CACHE)
}

/// Serves static assets from `/assets/*` path
pub fn static_asset(path: &str) -> Response {
    // The route captures everything after /assets/, so we need to prepend "assets/"
    let full_path = format!("assets/{}", path);

    // Determine cache policy based on filename
    let cache_control = if is_hashed_asset(&full_path) {
        IMMUTABLE
    } else {
        NO_CACHE
    };

    serve_file(&full_path, cache_control)
}

/// Serves a file from the embedded assets
fn serve_file(path: &str, cache_control: &str) -> Response {
    let file = match WEB_ASSETS.get_file(path) {
        Some(f) => f,
        None => {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }
    };

    let content_type = mime_type_for_path(path);
    let body = Body::from(file.contents());

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::CONTENT_SECURITY_POLICY, SPA_CSP)
        .body(body)
        .unwrap()
}

/// Determines if a path is a content-hashed asset (can be cached forever)
fn is_hashed_asset(path: &str) -> bool {
    // Vite outputs files like: assets/index-abc123.js
    // Pattern: contains a hash-like segment (8+ hex chars) before the extension
    path.contains("/assets/")
        && path
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .and_then(|(stem, _)| stem.rsplit_once('-'))
            .map(|(_, hash)| hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .unwrap_or(false)
}

/// Returns MIME type based on file extension
fn mime_type_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hashed_asset() {
        assert!(is_hashed_asset("assets/index-a1b2c3d4.js"));
        assert!(is_hashed_asset("assets/style-deadbeef.css"));
        assert!(is_hashed_asset("assets/vendor-12345678.js"));

        assert!(!is_hashed_asset("index.html"));
        assert!(!is_hashed_asset("assets/logo.png"));
        assert!(!is_hashed_asset("assets/ui-i18n.json"));
    }

    #[test]
    fn test_mime_type_for_path() {
        assert_eq!(mime_type_for_path("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_type_for_path("app.js"), "application/javascript; charset=utf-8");
        assert_eq!(mime_type_for_path("style.css"), "text/css; charset=utf-8");
        assert_eq!(mime_type_for_path("data.json"), "application/json; charset=utf-8");
        assert_eq!(mime_type_for_path("font.woff2"), "font/woff2");
        assert_eq!(mime_type_for_path("image.webp"), "image/webp");
    }

    #[test]
    fn test_spa_index_exists() {
        // This will fail at compile time if web/dist/index.html doesn't exist
        let response = spa_index();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
