// 前端 UI 资源:
// - `index_html` / `node_html` 把 HTML 模板和注入参数拼接后返回给浏览器;
// - 模板里嵌入的 CSS + JavaScript 在编译期不做加工,运行期由浏览器执行;
// - 两个模板均放在 `assets/` 目录:`index.html` 与 `node.html`,通过 `include_str!`
//   在编译期嵌入二进制,便于直接在编辑器里维护完整视图;
// - 国际化字典放在 `assets/ui-i18n.json`,同样通过 `include_str!` 一并编译进二进制。

use axum::body::Bytes;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::error;

/// 编译期嵌入的前端 i18n 字典,前端通过 `/assets/ui-i18n.json` 拉取。
pub const UI_I18N_JSON: &str = include_str!("../assets/ui-i18n.json");
/// 前端 i18n 字典对应的 HTTP 路径,统一注入到模板中。
pub const UI_I18N_ASSET_PATH: &str = "/assets/ui-i18n.json";

const PAGE_CSP_DIRECTIVES: &str = "default-src 'self'; img-src 'self' data:; connect-src 'self' https://raw.githubusercontent.com https://api.github.com; font-src 'self'; object-src 'none'; media-src 'none'; worker-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// 渲染首页 HTML:把刷新间隔与 i18n 资源路径替换到模板占位符里。
pub fn index_html(refresh_interval_secs: u64) -> Bytes {
    cached_index_template(refresh_interval_secs)
}

/// 渲染节点详情页 HTML;通过属性值注入当前节点 ID,避免脚本体随请求变化。
pub fn node_html(node_id: &str, refresh_interval_secs: u64) -> String {
    cached_node_template(refresh_interval_secs)
        .replace("__NODE_ID_ATTR__", &html_attr_escape(node_id))
}

pub fn index_page_csp() -> &'static str {
    INDEX_PAGE_CSP.get_or_init(|| build_page_csp(INDEX_TEMPLATE))
}

pub fn node_page_csp() -> &'static str {
    NODE_PAGE_CSP.get_or_init(|| build_page_csp(NODE_TEMPLATE))
}

pub fn verify_2fa_page_csp() -> &'static str {
    VERIFY_2FA_PAGE_CSP.get_or_init(|| build_page_csp(VERIFY_2FA_TEMPLATE))
}

pub fn verify_2fa_html() -> &'static str {
    VERIFY_2FA_TEMPLATE
}

const INDEX_TEMPLATE: &str = include_str!("../assets/index.html");

const NODE_TEMPLATE: &str = include_str!("../assets/node.html");

const VERIFY_2FA_TEMPLATE: &str = include_str!("../assets/verify-2fa.html");

static INDEX_TEMPLATE_CACHE: OnceLock<Mutex<HashMap<u64, Arc<[u8]>>>> = OnceLock::new();
static NODE_TEMPLATE_CACHE: OnceLock<Mutex<HashMap<u64, Arc<String>>>> = OnceLock::new();
static INDEX_PAGE_CSP: OnceLock<String> = OnceLock::new();
static NODE_PAGE_CSP: OnceLock<String> = OnceLock::new();
static VERIFY_2FA_PAGE_CSP: OnceLock<String> = OnceLock::new();

fn cached_index_template(refresh_interval_secs: u64) -> Bytes {
    let cache = INDEX_TEMPLATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            error!("template cache mutex poisoned; recovering cached templates");
            poisoned.into_inner()
        }
    };

    if let Some(rendered) = cache.get(&refresh_interval_secs) {
        return Bytes::from_owner(rendered.clone());
    }

    let rendered = Arc::<[u8]>::from(
        render_template_shell(INDEX_TEMPLATE, refresh_interval_secs).into_bytes(),
    );
    cache.insert(refresh_interval_secs, rendered.clone());
    Bytes::from_owner(rendered)
}

fn cached_node_template(refresh_interval_secs: u64) -> Arc<String> {
    let cache = NODE_TEMPLATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            error!("template cache mutex poisoned; recovering cached templates");
            poisoned.into_inner()
        }
    };

    if let Some(rendered) = cache.get(&refresh_interval_secs) {
        return rendered.clone();
    }

    let rendered = Arc::new(render_template_shell(NODE_TEMPLATE, refresh_interval_secs));
    cache.insert(refresh_interval_secs, rendered.clone());
    rendered
}

fn render_template_shell(template: &str, refresh_interval_secs: u64) -> String {
    template
        .replace(
            "__REFRESH_MS__",
            &(refresh_interval_secs * 1000).to_string(),
        )
        .replace("__I18N_ASSET_PATH__", UI_I18N_ASSET_PATH)
}

fn build_page_csp(template: &str) -> String {
    let script_hashes = extract_inline_tag_bodies(template, "<script>", "</script>")
        .into_iter()
        .map(csp_hash)
        .collect::<Vec<_>>()
        .join(" ");
    let style_hashes = extract_inline_tag_bodies(template, "<style>", "</style>")
        .into_iter()
        .map(csp_hash)
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "default-src 'self'; script-src 'self' {script_hashes}; style-src 'self' 'unsafe-inline' {style_hashes}; style-src-elem 'self' {style_hashes}; style-src-attr 'unsafe-inline'; {}",
        PAGE_CSP_DIRECTIVES.trim_start_matches("default-src 'self'; ")
    )
}

fn extract_inline_tag_bodies<'a>(
    document: &'a str,
    open_tag: &str,
    close_tag: &str,
) -> Vec<&'a str> {
    let mut bodies = Vec::new();
    let mut rest = document;

    while let Some(open_index) = rest.find(open_tag) {
        let after_open = &rest[open_index + open_tag.len()..];
        let Some(close_index) = after_open.find(close_tag) else {
            break;
        };
        bodies.push(&after_open[..close_index]);
        rest = &after_open[close_index + close_tag.len()..];
    }

    bodies
}

fn csp_hash(block: &str) -> String {
    let digest = Sha256::digest(block.as_bytes());
    format!("'sha256-{}'", STANDARD.encode(digest))
}

fn html_attr_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{index_html, index_page_csp, node_html, node_page_csp, verify_2fa_page_csp};

    #[test]
    fn index_html_reuses_cached_render_for_same_refresh_interval() {
        let first = index_html(5);
        let second = index_html(5);
        assert_eq!(first.as_ptr(), second.as_ptr());
        assert!(
            std::str::from_utf8(first.as_ref())
                .expect("cached index html should stay utf-8")
                .contains("/assets/ui-i18n.json")
        );
    }

    #[test]
    fn node_html_only_injects_node_id_after_cached_shell_render() {
        let rendered = node_html("hk-01", 5);
        assert!(rendered.contains("data-node-id=\"hk-01\""));
        assert!(!rendered.contains("__NODE_ID_ATTR__"));
    }

    #[test]
    fn node_html_escapes_node_id_for_attribute_context() {
        let rendered = node_html("\"<hk>&'\"", 5);
        assert!(rendered.contains("data-node-id=\"&quot;&lt;hk&gt;&amp;&#39;&quot;\""));
    }

    #[test]
    fn node_html_fetches_detail_history_for_overview_and_network_tabs() {
        let rendered = node_html("hk-01", 5);
        assert!(rendered.contains("function detailHistoryNeedsData()"));
        assert!(rendered.contains(
            "activeTab === \"overview\" || activeTab === \"monitor\" || activeTab === \"network\" || chartModalState.key != null"
        ));
    }

    #[test]
    fn page_csps_pin_scripts_and_allow_inline_styles_for_dashboard_layout() {
        for csp in [index_page_csp(), node_page_csp(), verify_2fa_page_csp()] {
            assert!(csp.contains("script-src 'self' 'sha256-"));
            assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
            assert!(csp.contains("style-src 'self' 'unsafe-inline' 'sha256-"));
            assert!(csp.contains("style-src-attr 'unsafe-inline'"));
        }
    }
}
