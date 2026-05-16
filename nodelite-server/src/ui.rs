// 前端 UI 资源:
// - `index_html` / `node_html` 把 HTML 模板和注入参数拼接后返回给浏览器;
// - 模板里嵌入的 CSS + JavaScript 在编译期不做加工,运行期由浏览器执行;
// - 两个模板均放在 `assets/` 目录:`index.html` 与 `node.html`,通过 `include_str!`
//   在编译期嵌入二进制,便于直接在编辑器里维护完整视图;
// - 国际化字典放在 `assets/ui-i18n.json`,同样通过 `include_str!` 一并编译进二进制。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tracing::error;

/// 编译期嵌入的前端 i18n 字典,前端通过 `/assets/ui-i18n.json` 拉取。
pub const UI_I18N_JSON: &str = include_str!("../assets/ui-i18n.json");
/// 前端 i18n 字典对应的 HTTP 路径,统一注入到模板中。
pub const UI_I18N_ASSET_PATH: &str = "/assets/ui-i18n.json";

/// 渲染首页 HTML:把刷新间隔与 i18n 资源路径替换到模板占位符里。
pub fn index_html(refresh_interval_secs: u64) -> &'static str {
    cached_template(&INDEX_TEMPLATE_CACHE, refresh_interval_secs, INDEX_TEMPLATE)
}

/// 渲染节点详情页 HTML;额外把当前节点 ID 以 JSON 编码后嵌入模板,避免 XSS。
pub fn node_html(node_id: &str, refresh_interval_secs: u64) -> String {
    cached_template(&NODE_TEMPLATE_CACHE, refresh_interval_secs, NODE_TEMPLATE).replace(
        "__NODE_ID_JSON__",
        &serde_json::to_string(node_id).unwrap_or_else(|_| "\"\"".to_string()),
    )
}

const INDEX_TEMPLATE: &str = include_str!("../assets/index.html");

const NODE_TEMPLATE: &str = include_str!("../assets/node.html");

static INDEX_TEMPLATE_CACHE: OnceLock<Mutex<HashMap<u64, &'static str>>> = OnceLock::new();
static NODE_TEMPLATE_CACHE: OnceLock<Mutex<HashMap<u64, &'static str>>> = OnceLock::new();

fn cached_template(
    cache: &OnceLock<Mutex<HashMap<u64, &'static str>>>,
    refresh_interval_secs: u64,
    template: &str,
) -> &'static str {
    let cache = cache.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = match cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            error!("template cache mutex poisoned; recovering cached templates");
            poisoned.into_inner()
        }
    };
    if let Some(rendered) = cache.get(&refresh_interval_secs) {
        return rendered;
    }

    let rendered = template
        .replace(
            "__REFRESH_MS__",
            &(refresh_interval_secs * 1000).to_string(),
        )
        .replace("__I18N_ASSET_PATH__", UI_I18N_ASSET_PATH);
    let rendered = Box::leak(rendered.into_boxed_str());
    cache.insert(refresh_interval_secs, rendered);
    rendered
}

#[cfg(test)]
mod tests {
    use super::{index_html, node_html};

    #[test]
    fn index_html_reuses_cached_render_for_same_refresh_interval() {
        let first = index_html(5);
        let second = index_html(5);
        assert!(std::ptr::eq(first.as_ptr(), second.as_ptr()));
        assert!(first.contains("/assets/ui-i18n.json"));
    }

    #[test]
    fn node_html_only_injects_node_id_after_cached_shell_render() {
        let rendered = node_html("hk-01", 5);
        assert!(rendered.contains("\"hk-01\""));
        assert!(!rendered.contains("__NODE_ID_JSON__"));
    }
}
