//! Web assets module: serves the Vue SPA and static files embedded at compile time.
//!
//! The Vite build output from `web/dist/` is embedded into the binary using `include_dir!`.
//! This module provides handlers for serving the SPA entry point and static assets with
//! appropriate cache headers.

use std::sync::OnceLock;

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};

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

/// Serves the standalone 2FA verification page (`verify-2fa.html`).
///
/// This page is deliberately *not* part of the Vue SPA: it is a self-contained
/// document with inline `<script>`/`<style>` blocks, served at the auth gate
/// before the SPA bundle ever loads (the dev server also proxies `/verify-2fa`
/// straight to the backend). Its CSP pins each inline block by sha256 so the page
/// stays compatible with a strict `script-src 'self'` policy.
pub fn verify_2fa_page() -> Response {
    let file = match WEB_ASSETS.get_file("verify-2fa.html") {
        Some(f) => f,
        None => {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, NO_CACHE)
        .header(header::PRAGMA, "no-cache")
        .header(header::CONTENT_SECURITY_POLICY, verify_2fa_csp())
        .body(Body::from(file.contents()))
        .unwrap()
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
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::CONTENT_SECURITY_POLICY, SPA_CSP);
    // Legacy HTTP/1.0 proxies honour Pragma; pair it with no-cache responses so
    // the SPA shell matches every other no-cache protected response.
    if cache_control == NO_CACHE {
        builder = builder.header(header::PRAGMA, "no-cache");
    }
    builder.body(Body::from(file.contents())).unwrap()
}

/// Determines if a path is a content-hashed asset (safe to cache forever).
///
/// Vite emits hashed files as `assets/<name>.<hash>.<ext>` (see vite.config.ts
/// `assetFileNames` / `chunkFileNames` / `entryFileNames`), where `<hash>` uses
/// Vite's base64url alphabet (`A-Za-z0-9_-`). We treat a file as immutable only
/// when it has that `name.hash.ext` shape with an 8+ char base64url hash segment.
/// Unhashed build files (`index.html`, `assets/ui-i18n.json`,
/// `assets/brand-logo-dark.webp`) must keep revalidating, so they fall through.
fn is_hashed_asset(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let segments: Vec<&str> = filename.split('.').collect();
    // Need at least `name` + `hash` + `ext`.
    if segments.len() < 3 {
        return false;
    }
    let hash = segments[segments.len() - 2];
    hash.len() >= 8
        && hash
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

/// Shared trailing CSP directives for the standalone 2FA page (everything past
/// `default-src`/`script-src`/`style-src`, which `build_page_csp` computes).
const PAGE_CSP_DIRECTIVES: &str = "default-src 'self'; img-src 'self' data:; \
    connect-src 'self' https://raw.githubusercontent.com https://api.github.com; \
    font-src 'self'; object-src 'none'; media-src 'none'; worker-src 'none'; \
    base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

static VERIFY_2FA_CSP: OnceLock<String> = OnceLock::new();

/// Computes (once) the page-specific CSP for the embedded `verify-2fa.html`,
/// hashing its inline blocks so the served bytes and the CSP stay in lockstep.
fn verify_2fa_csp() -> &'static str {
    VERIFY_2FA_CSP
        .get_or_init(|| {
            let template = WEB_ASSETS
                .get_file("verify-2fa.html")
                .map(|file| String::from_utf8_lossy(file.contents()).into_owned())
                .unwrap_or_default();
            build_page_csp(&template)
        })
        .as_str()
}

/// Pins a page's inline `<script>`/`<style>` blocks by sha256 so a strict
/// `script-src 'self'` policy still permits them.
fn build_page_csp(template: &str) -> String {
    let script_hashes = extract_inline_tag_bodies(template, "script")
        .into_iter()
        .map(csp_hash)
        .collect::<Vec<_>>()
        .join(" ");
    let style_hashes = extract_inline_tag_bodies(template, "style")
        .into_iter()
        .map(csp_hash)
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "default-src 'self'; script-src 'self' {script_hashes}; \
         style-src 'self' 'unsafe-inline' {style_hashes}; \
         style-src-elem 'self' {style_hashes}; style-src-attr 'unsafe-inline'; {}",
        PAGE_CSP_DIRECTIVES.trim_start_matches("default-src 'self'; ")
    )
}

/// Extracts the text bodies of every `<tag>…</tag>` block, matching the exact
/// content the browser hashes for a CSP `'sha256-…'` source.
fn extract_inline_tag_bodies<'a>(document: &'a str, tag_name: &str) -> Vec<&'a str> {
    let mut bodies = Vec::new();
    let mut rest = document;
    let open_tag_prefix = format!("<{tag_name}");
    let close_tag = format!("</{tag_name}>");

    while let Some(open_index) = rest.find(&open_tag_prefix) {
        let after_open_start = &rest[open_index + open_tag_prefix.len()..];
        let Some(open_end_index) = after_open_start.find('>') else {
            break;
        };
        let after_open = &after_open_start[open_end_index + 1..];
        let Some(close_index) = after_open.find(&close_tag) else {
            break;
        };
        bodies.push(&after_open[..close_index]);
        rest = &after_open[close_index + close_tag.len()..];
    }

    bodies
}

/// Renders a single inline block as a CSP `'sha256-<base64>'` source token.
fn csp_hash(block: &str) -> String {
    let digest = Sha256::digest(block.as_bytes());
    format!("'sha256-{}'", STANDARD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hashed_asset() {
        // Vite emits `<name>.<hash>.<ext>` with a base64url hash.
        assert!(is_hashed_asset("assets/index.B_MrJhzj.js"));
        assert!(is_hashed_asset("assets/AccountView.B5MJM2zL.css"));
        assert!(is_hashed_asset("assets/index.CHYP72L6.css"));

        // Unhashed build files must keep revalidating, not be cached immutably.
        assert!(!is_hashed_asset("index.html"));
        assert!(!is_hashed_asset("verify-2fa.html"));
        assert!(!is_hashed_asset("assets/brand-logo-dark.webp"));
        assert!(!is_hashed_asset("assets/ui-i18n.json"));
    }

    #[test]
    fn test_mime_type_for_path() {
        assert_eq!(mime_type_for_path("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            mime_type_for_path("app.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_type_for_path("style.css"), "text/css; charset=utf-8");
        assert_eq!(
            mime_type_for_path("data.json"),
            "application/json; charset=utf-8"
        );
        assert_eq!(mime_type_for_path("font.woff2"), "font/woff2");
        assert_eq!(mime_type_for_path("image.webp"), "image/webp");
    }

    #[test]
    fn test_spa_index_exists() {
        // This will fail at compile time if web/dist/index.html doesn't exist
        let response = spa_index();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_verify_2fa_page_pins_inline_blocks() {
        let response = verify_2fa_page();
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("verify-2fa page should set a CSP")
            .to_str()
            .expect("CSP should be valid ascii");
        // The page carries inline <script>/<style>, so the CSP must pin them.
        assert!(csp.contains("script-src 'self' 'sha256-"), "csp={csp}");
        assert!(
            csp.contains("style-src 'self' 'unsafe-inline'"),
            "csp={csp}"
        );
        assert!(
            !csp.contains("script-src 'self' 'unsafe-inline'"),
            "csp={csp}"
        );
    }

    #[test]
    fn build_page_csp_pins_scripts_and_allows_inline_styles() {
        let csp =
            build_page_csp("<html><style>body{color:red}</style><script>boot()</script></html>");
        assert!(csp.contains("script-src 'self' 'sha256-"), "csp={csp}");
        assert!(
            !csp.contains("script-src 'self' 'unsafe-inline'"),
            "csp={csp}"
        );
        assert!(
            csp.contains("style-src 'self' 'unsafe-inline' 'sha256-"),
            "csp={csp}"
        );
        assert!(csp.contains("style-src-attr 'unsafe-inline'"), "csp={csp}");
        assert!(
            csp.contains(
                "connect-src 'self' https://raw.githubusercontent.com https://api.github.com"
            ),
            "csp={csp}"
        );
    }

    #[test]
    fn extract_inline_tag_bodies_reads_each_block() {
        let bodies = extract_inline_tag_bodies(
            "<script type=\"module\">first()</script><script>second()</script>",
            "script",
        );
        assert_eq!(bodies, vec!["first()", "second()"]);
    }
}
