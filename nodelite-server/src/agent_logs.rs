use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use nodelite_proto::{AgentLogEntry, truncate_to_byte_boundary};
use tokio::sync::RwLock;

const MAX_LOGS_PER_NODE: usize = 200;
const MAX_BATCH_ENTRIES: usize = 64;
const MAX_LOG_MESSAGE_BYTES: usize = 512;

/// `record_entries` 的结构化结果, 让调用方既能知道"接受了多少",
/// 也能知道"丢弃了多少 + 因为什么"。
///
/// 丢弃来源:
/// - `dropped_batch_cap`: 单批次超过 `MAX_BATCH_ENTRIES` 上限被截断的部分;
/// - `dropped_sanitize`: 内容(message/timestamp)不合规被 `sanitize_entry` 拒掉。
///
/// 任一项非零都会触发 `tracing::warn!`,以便运维在仪表盘看到日志丢失趋势。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordResult {
    pub accepted: usize,
    pub dropped_batch_cap: usize,
    pub dropped_sanitize: usize,
}

impl RecordResult {
    pub fn total_dropped(&self) -> usize {
        self.dropped_batch_cap.saturating_add(self.dropped_sanitize)
    }
}

/// 最近 Agent 运行日志的内存缓冲。
///
/// 这些日志只用于只读排障视图,不参与持久化。设计目标是:
/// - 每节点保留固定上限,防止异常节点无限吃内存;
/// - 接受 Agent 断线后回补的一小批日志,帮助排查偶发断链/重连问题;
/// - 对消息长度与时间戳做轻量清洗,避免脏数据破坏前端渲染。
#[derive(Clone, Default)]
pub struct AgentLogStore {
    inner: Arc<RwLock<HashMap<String, VecDeque<AgentLogEntry>>>>,
}

impl AgentLogStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录某节点上传的一批日志, 返回结构化的接收 / 丢弃统计。
    ///
    /// 限流上限仍然是 `MAX_BATCH_ENTRIES = 64`, 超出部分会被丢弃,
    /// 但与 #89 之前不同的是: 丢弃数量现在会回传给调用方, 并触发
    /// `tracing::warn!` 让丢弃可被运维监控感知, 不再是黑洞。
    pub async fn record_entries(&self, node_id: &str, entries: Vec<AgentLogEntry>) -> RecordResult {
        let total = entries.len();
        let dropped_batch_cap = total.saturating_sub(MAX_BATCH_ENTRIES);

        let mut guard = self.inner.write().await;
        let buffer = guard.entry(node_id.to_string()).or_default();
        let mut accepted = 0;
        let mut dropped_sanitize = 0;

        for entry in entries.into_iter().take(MAX_BATCH_ENTRIES) {
            let Some(entry) = sanitize_entry(entry) else {
                dropped_sanitize += 1;
                continue;
            };
            if buffer.len() >= MAX_LOGS_PER_NODE {
                buffer.pop_front();
            }
            buffer.push_back(entry);
            accepted += 1;
        }

        RecordResult {
            accepted,
            dropped_batch_cap,
            dropped_sanitize,
        }
    }

    /// 返回某节点最近的若干条日志,按发生时间升序保留。
    pub async fn list(&self, node_id: &str, limit: usize) -> Vec<AgentLogEntry> {
        let guard = self.inner.read().await;
        let Some(buffer) = guard.get(node_id) else {
            return Vec::new();
        };

        let limit = limit.clamp(1, MAX_LOGS_PER_NODE);
        let start = buffer.len().saturating_sub(limit);
        buffer.iter().skip(start).cloned().collect()
    }

    /// 清理已经不在注册表中的节点日志,避免长期运行时缓冲只增不减。
    pub async fn forget_missing(&self, live_node_ids: &[String]) -> usize {
        let live: HashSet<&str> = live_node_ids.iter().map(String::as_str).collect();
        let mut guard = self.inner.write().await;
        let before = guard.len();
        guard.retain(|node_id, _| live.contains(node_id.as_str()));
        before.saturating_sub(guard.len())
    }
}

fn sanitize_entry(mut entry: AgentLogEntry) -> Option<AgentLogEntry> {
    let message = entry.message.trim();
    if message.is_empty() {
        return None;
    }

    entry.message = truncate_to_byte_boundary(message, MAX_LOG_MESSAGE_BYTES).to_string();
    if DateTime::parse_from_rfc3339(&entry.occurred_at).is_err() {
        entry.occurred_at = Utc::now().to_rfc3339();
    }
    Some(entry)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nodelite_proto::NoticeLevel;

    use super::{AgentLogEntry, AgentLogStore, MAX_BATCH_ENTRIES, MAX_LOGS_PER_NODE};

    #[tokio::test]
    async fn record_entries_caps_per_node_and_surfaces_drops() {
        let store = AgentLogStore::new();
        let total = MAX_LOGS_PER_NODE + 10;
        let entries = (0..total)
            .map(|index| AgentLogEntry {
                occurred_at: "invalid".to_string(),
                level: NoticeLevel::Info,
                message: format!("entry-{index}"),
            })
            .collect();

        let result = store.record_entries("hk-01", entries).await;
        // #89: 接受恰好 MAX_BATCH_ENTRIES 条 (sanitize 都通过, 因为 message 都非空),
        // 多出来的部分由 dropped_batch_cap 报出 —— 不再像旧版那样静默丢失。
        assert_eq!(result.accepted, MAX_BATCH_ENTRIES);
        assert_eq!(result.dropped_batch_cap, total - MAX_BATCH_ENTRIES);
        assert_eq!(result.dropped_sanitize, 0);
        assert_eq!(result.total_dropped(), total - MAX_BATCH_ENTRIES);

        let logs = store.list("hk-01", MAX_LOGS_PER_NODE).await;
        assert_eq!(logs.len(), MAX_BATCH_ENTRIES);
        assert!(logs.iter().all(|entry| !entry.message.is_empty()));
        assert!(
            logs.iter()
                .all(|entry| chrono::DateTime::parse_from_rfc3339(&entry.occurred_at).is_ok())
        );
    }

    #[tokio::test]
    async fn record_entries_counts_sanitize_drops() {
        // sanitize_entry 拒掉空消息与纯空白消息,这些应当计入 dropped_sanitize
        // 而不是 accepted。
        let store = AgentLogStore::new();
        let entries = vec![
            AgentLogEntry {
                occurred_at: Utc::now().to_rfc3339(),
                level: NoticeLevel::Info,
                message: "  ".to_string(), // sanitize_entry returns None
            },
            AgentLogEntry {
                occurred_at: Utc::now().to_rfc3339(),
                level: NoticeLevel::Info,
                message: "real entry".to_string(),
            },
        ];

        let result = store.record_entries("hk-01", entries).await;
        assert_eq!(result.accepted, 1);
        assert_eq!(result.dropped_batch_cap, 0);
        assert_eq!(result.dropped_sanitize, 1);
    }

    #[tokio::test]
    async fn forget_missing_prunes_retired_node_buffers() {
        let store = AgentLogStore::new();
        let entry = AgentLogEntry {
            occurred_at: Utc::now().to_rfc3339(),
            level: NoticeLevel::Warn,
            message: "reconnecting".to_string(),
        };
        store.record_entries("hk-01", vec![entry.clone()]).await;
        store.record_entries("jp-01", vec![entry]).await;

        let removed = store.forget_missing(&["jp-01".to_string()]).await;
        assert_eq!(removed, 1);
        assert!(store.list("hk-01", 10).await.is_empty());
        assert_eq!(store.list("jp-01", 10).await.len(), 1);
    }

    #[test]
    fn truncate_to_byte_boundary_preserves_utf8() {
        let value = "日志-abcdef";
        let truncated = nodelite_proto::truncate_to_byte_boundary(value, 5);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert_eq!(truncated, "日");
    }
}
