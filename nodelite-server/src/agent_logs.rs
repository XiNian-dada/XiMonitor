use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use nodelite_proto::{AgentLogEntry, truncate_to_byte_boundary};
use tokio::sync::{Mutex, RwLock};

const MAX_LOGS_PER_NODE: usize = 200;
const MAX_BATCH_ENTRIES: usize = 64;
const MAX_LOG_MESSAGE_BYTES: usize = 512;
const MAX_LOG_ENTRIES_TOTAL: usize = 10_000;
const MAX_LOG_ESTIMATED_BYTES: usize = 8 * 1024 * 1024;
const ESTIMATED_LOG_ENTRY_OVERHEAD_BYTES: usize = 96;
const AGENT_LOG_SHARDS: usize = 16;

/// `record_entries` 的结构化结果, 让调用方既能知道"接受了多少",
/// 也能知道"丢弃了多少 + 因为什么"。
///
/// 丢弃来源:
/// - `dropped_batch_cap`: 单批次超过 `MAX_BATCH_ENTRIES` 上限被截断的部分;
/// - `dropped_sanitize`: 内容(message/timestamp)不合规被 `sanitize_entry` 拒掉;
/// - `evicted_global_budget`: 全局日志条数/字节预算触发后驱逐的旧日志。
///
/// 任一项非零都会触发 `tracing::warn!`,以便运维在仪表盘看到日志丢失趋势。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordResult {
    pub accepted: usize,
    pub dropped_batch_cap: usize,
    pub dropped_sanitize: usize,
    pub evicted_global_budget: usize,
}

impl RecordResult {
    pub fn total_dropped(&self) -> usize {
        self.dropped_batch_cap
            .saturating_add(self.dropped_sanitize)
            .saturating_add(self.evicted_global_budget)
    }
}

/// `AgentLogStore` 当前内存占用的轻量统计,用于测试和 Prometheus 导出。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentLogStats {
    pub nodes: usize,
    pub entries: usize,
    pub estimated_bytes: usize,
    pub max_entries: usize,
    pub max_estimated_bytes: usize,
}

/// 最近 Agent 运行日志的内存缓冲。
///
/// 这些日志只用于只读排障视图,不参与持久化。设计目标是:
/// - 每节点保留固定上限,防止异常节点无限吃内存;
/// - 全局条数和估算字节数都有上限,避免多节点同时刷日志时挤爆内存;
/// - 接受 Agent 断线后回补的一小批日志,帮助排查偶发断链/重连问题;
/// - 对消息长度与时间戳做轻量清洗,避免脏数据破坏前端渲染。
#[derive(Clone, Default)]
pub struct AgentLogStore {
    inner: Arc<AgentLogStoreInner>,
}

#[derive(Default)]
struct AgentLogStoreInner {
    shards: Vec<RwLock<AgentLogShard>>,
    total_entries: AtomicUsize,
    estimated_bytes: AtomicUsize,
    next_sequence: AtomicU64,
    eviction_lock: Mutex<()>,
}

#[derive(Default)]
struct AgentLogShard {
    buffers: HashMap<String, VecDeque<StoredAgentLogEntry>>,
}

#[derive(Clone)]
struct StoredAgentLogEntry {
    entry: AgentLogEntry,
    sequence: u64,
    estimated_bytes: usize,
}

impl AgentLogStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AgentLogStoreInner {
                shards: (0..AGENT_LOG_SHARDS)
                    .map(|_| RwLock::new(AgentLogShard::default()))
                    .collect(),
                total_entries: AtomicUsize::new(0),
                estimated_bytes: AtomicUsize::new(0),
                next_sequence: AtomicU64::new(0),
                eviction_lock: Mutex::new(()),
            }),
        }
    }

    /// 记录某节点上传的一批日志, 返回结构化的接收 / 丢弃统计。
    ///
    /// 限流上限仍然是 `MAX_BATCH_ENTRIES = 64`, 超出部分会被丢弃,
    /// 但与 #89 之前不同的是: 丢弃数量现在会回传给调用方, 并触发
    /// `tracing::warn!` 让丢弃可被运维监控感知, 不再是黑洞。
    pub async fn record_entries(&self, node_id: &str, entries: Vec<AgentLogEntry>) -> RecordResult {
        let total = entries.len();
        let dropped_batch_cap = total.saturating_sub(MAX_BATCH_ENTRIES);

        let mut accepted = 0;
        let mut dropped_sanitize = 0;
        let shard_index = shard_index(node_id);

        for entry in entries.into_iter().take(MAX_BATCH_ENTRIES) {
            let Some(entry) = sanitize_entry(entry) else {
                dropped_sanitize += 1;
                continue;
            };

            self.push_entry(shard_index, node_id, entry).await;
            accepted += 1;
        }

        let evicted_global_budget = self.enforce_global_budget().await;

        RecordResult {
            accepted,
            dropped_batch_cap,
            dropped_sanitize,
            evicted_global_budget,
        }
    }

    /// 返回某节点最近的若干条日志,按发生时间升序保留。
    pub async fn list(&self, node_id: &str, limit: usize) -> Vec<AgentLogEntry> {
        let guard = self.inner.shards[shard_index(node_id)].read().await;
        let Some(buffer) = guard.buffers.get(node_id) else {
            return Vec::new();
        };

        let limit = limit.clamp(1, MAX_LOGS_PER_NODE);
        let start = buffer.len().saturating_sub(limit);
        buffer
            .iter()
            .skip(start)
            .map(|stored| stored.entry.clone())
            .collect()
    }

    /// 清理已经不在注册表中的节点日志,避免长期运行时缓冲只增不减。
    pub async fn forget_missing(&self, live_node_ids: &[String]) -> usize {
        let live: HashSet<&str> = live_node_ids.iter().map(String::as_str).collect();
        let mut removed = 0;
        for shard in &self.inner.shards {
            let mut guard = shard.write().await;
            let before = guard.buffers.len();
            let removed_nodes = guard
                .buffers
                .keys()
                .filter(|node_id| !live.contains(node_id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            for node_id in removed_nodes {
                let removed_stats = guard.remove_node(&node_id);
                self.inner
                    .total_entries
                    .fetch_sub(removed_stats.entries, Ordering::Relaxed);
                self.inner
                    .estimated_bytes
                    .fetch_sub(removed_stats.estimated_bytes, Ordering::Relaxed);
            }
            removed += before.saturating_sub(guard.buffers.len());
        }
        removed
    }

    pub async fn stats(&self) -> AgentLogStats {
        let mut nodes = 0;
        for shard in &self.inner.shards {
            nodes += shard.read().await.buffers.len();
        }
        AgentLogStats {
            nodes,
            entries: self.inner.total_entries.load(Ordering::Relaxed),
            estimated_bytes: self.inner.estimated_bytes.load(Ordering::Relaxed),
            max_entries: MAX_LOG_ENTRIES_TOTAL,
            max_estimated_bytes: MAX_LOG_ESTIMATED_BYTES,
        }
    }

    async fn push_entry(&self, shard_index: usize, node_id: &str, entry: AgentLogEntry) {
        let estimated_bytes = estimate_entry_bytes(node_id, &entry);
        let stored = StoredAgentLogEntry {
            entry,
            sequence: self.inner.next_sequence.fetch_add(1, Ordering::Relaxed),
            estimated_bytes,
        };
        self.inner.total_entries.fetch_add(1, Ordering::Relaxed);
        self.inner
            .estimated_bytes
            .fetch_add(estimated_bytes, Ordering::Relaxed);

        let mut shard = self.inner.shards[shard_index].write().await;
        let buffer = shard.buffers.entry(node_id.to_string()).or_default();
        buffer.push_back(stored);

        let mut local_evicted_entries = 0;
        let mut local_evicted_bytes = 0;
        while shard
            .buffers
            .get(node_id)
            .is_some_and(|buffer| buffer.len() > MAX_LOGS_PER_NODE)
        {
            if let Some(entry) = shard.pop_front_for_node(node_id) {
                local_evicted_entries += 1;
                local_evicted_bytes += entry.estimated_bytes;
            }
        }
        drop(shard);

        if local_evicted_entries > 0 {
            self.inner
                .total_entries
                .fetch_sub(local_evicted_entries, Ordering::Relaxed);
            self.inner
                .estimated_bytes
                .fetch_sub(local_evicted_bytes, Ordering::Relaxed);
        }
    }

    async fn enforce_global_budget(&self) -> usize {
        if self.inner.total_entries.load(Ordering::Relaxed) <= MAX_LOG_ENTRIES_TOTAL
            && self.inner.estimated_bytes.load(Ordering::Relaxed) <= MAX_LOG_ESTIMATED_BYTES
        {
            return 0;
        }

        let _guard = self.inner.eviction_lock.lock().await;
        let mut evicted = 0;
        while self.inner.total_entries.load(Ordering::Relaxed) > MAX_LOG_ENTRIES_TOTAL
            || self.inner.estimated_bytes.load(Ordering::Relaxed) > MAX_LOG_ESTIMATED_BYTES
        {
            let Some((shard_index, node_id)) = self.oldest_node_location().await else {
                break;
            };
            let mut shard = self.inner.shards[shard_index].write().await;
            let Some(entry) = shard.pop_front_for_node(&node_id) else {
                continue;
            };
            drop(shard);
            self.inner.total_entries.fetch_sub(1, Ordering::Relaxed);
            self.inner
                .estimated_bytes
                .fetch_sub(entry.estimated_bytes, Ordering::Relaxed);
            evicted += 1;
        }
        evicted
    }

    async fn oldest_node_location(&self) -> Option<(usize, String)> {
        let mut oldest: Option<(usize, String, u64)> = None;
        for (index, shard) in self.inner.shards.iter().enumerate() {
            let guard = shard.read().await;
            let Some(candidate) = guard.oldest_node_id() else {
                continue;
            };
            let Some(sequence) = guard
                .buffers
                .get(candidate.as_str())
                .and_then(|buffer| buffer.front())
                .map(|entry| entry.sequence)
            else {
                continue;
            };
            match oldest {
                Some((_, _, best_sequence)) if best_sequence <= sequence => {}
                _ => oldest = Some((index, candidate, sequence)),
            }
        }
        oldest.map(|(index, node_id, _)| (index, node_id))
    }
}

impl AgentLogShard {
    fn oldest_node_id(&self) -> Option<String> {
        self.buffers
            .iter()
            .filter_map(|(node_id, buffer)| {
                buffer
                    .front()
                    .map(|entry| (node_id.as_str(), entry.sequence))
            })
            .min_by_key(|(_, sequence)| *sequence)
            .map(|(node_id, _)| node_id.to_string())
    }

    fn remove_node(&mut self, node_id: &str) -> RemovedNodeStats {
        let Some(buffer) = self.buffers.remove(node_id) else {
            return RemovedNodeStats::default();
        };
        let mut removed = RemovedNodeStats::default();
        for entry in buffer {
            removed.entries += 1;
            removed.estimated_bytes = removed
                .estimated_bytes
                .saturating_add(entry.estimated_bytes);
        }
        removed
    }

    fn pop_front_for_node(&mut self, node_id: &str) -> Option<StoredAgentLogEntry> {
        let entry = {
            let buffer = self.buffers.get_mut(node_id)?;
            buffer.pop_front()
        }?;
        if self.buffers.get(node_id).is_some_and(VecDeque::is_empty) {
            self.buffers.remove(node_id);
        }
        Some(entry)
    }
}

#[derive(Default)]
struct RemovedNodeStats {
    entries: usize,
    estimated_bytes: usize,
}

fn shard_index(node_id: &str) -> usize {
    let mut hash = 5381_u64;
    for byte in node_id.as_bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
    }
    (hash as usize) % AGENT_LOG_SHARDS
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

fn estimate_entry_bytes(node_id: &str, entry: &AgentLogEntry) -> usize {
    ESTIMATED_LOG_ENTRY_OVERHEAD_BYTES
        .saturating_add(node_id.len())
        .saturating_add(entry.occurred_at.len())
        .saturating_add(entry.message.len())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nodelite_proto::NoticeLevel;
    use tokio::task::JoinSet;

    use super::{
        AgentLogEntry, AgentLogStore, MAX_BATCH_ENTRIES, MAX_LOG_ENTRIES_TOTAL,
        MAX_LOG_ESTIMATED_BYTES, MAX_LOGS_PER_NODE,
    };

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
        assert_eq!(result.evicted_global_budget, 0);
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
        assert_eq!(result.evicted_global_budget, 0);
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

        let stats = store.stats().await;
        assert_eq!(stats.nodes, 1);
        assert_eq!(stats.entries, 1);
    }

    #[tokio::test]
    async fn global_entry_budget_evicts_oldest_logs_across_nodes() {
        let store = AgentLogStore::new();
        let mut evicted = 0;

        for node_index in 0..=MAX_LOG_ENTRIES_TOTAL / MAX_LOGS_PER_NODE {
            let node_id = format!("node-{node_index:03}");
            for chunk_start in (0..MAX_LOGS_PER_NODE).step_by(MAX_BATCH_ENTRIES) {
                let chunk_len = MAX_BATCH_ENTRIES.min(MAX_LOGS_PER_NODE - chunk_start);
                let entries = (chunk_start..chunk_start + chunk_len)
                    .map(|entry_index| test_entry(format!("{node_id}-entry-{entry_index:03}")))
                    .collect();
                let result = store.record_entries(&node_id, entries).await;
                assert_eq!(result.dropped_batch_cap, 0);
                assert_eq!(result.dropped_sanitize, 0);
                evicted += result.evicted_global_budget;
            }
        }

        let stats = store.stats().await;
        assert_eq!(stats.entries, MAX_LOG_ENTRIES_TOTAL);
        assert!(stats.estimated_bytes <= stats.max_estimated_bytes);
        assert_eq!(evicted, MAX_LOGS_PER_NODE);
        assert!(store.list("node-000", 1).await.is_empty());

        let newest_node = format!("node-{:03}", MAX_LOG_ENTRIES_TOTAL / MAX_LOGS_PER_NODE);
        let recent = store.list(&newest_node, 3).await;
        assert_eq!(recent.len(), 3);
        assert_eq!(
            recent
                .first()
                .expect("recent log should include first visible entry")
                .message,
            format!("{newest_node}-entry-197")
        );
        assert_eq!(
            recent
                .last()
                .expect("recent log should include newest entry")
                .message,
            format!("{newest_node}-entry-199")
        );
    }

    #[tokio::test]
    async fn global_byte_budget_evicts_oldest_logs_and_reports_stats() {
        let store = AgentLogStore::new();
        let node_id = "heavy-node-".repeat((MAX_LOG_ESTIMATED_BYTES / 4) / "heavy-node-".len());
        let entries = (0..5)
            .map(|index| test_entry(format!("heavy-entry-{index}")))
            .collect();

        let result = store.record_entries(&node_id, entries).await;
        assert_eq!(result.dropped_batch_cap, 0);
        assert_eq!(result.dropped_sanitize, 0);
        assert!(result.evicted_global_budget > 0);
        assert_eq!(result.total_dropped(), result.evicted_global_budget);

        let stats = store.stats().await;
        assert_eq!(stats.nodes, 1);
        assert!(stats.entries < 5);
        assert!(stats.estimated_bytes <= stats.max_estimated_bytes);
        assert_eq!(stats.max_entries, MAX_LOG_ENTRIES_TOTAL);
        assert_eq!(stats.max_estimated_bytes, MAX_LOG_ESTIMATED_BYTES);

        let logs = store.list(&node_id, 10).await;
        assert!(logs.len() < 5);
        assert_eq!(
            logs.last()
                .expect("byte budget should keep the newest entry")
                .message,
            "heavy-entry-4"
        );
    }

    #[tokio::test]
    async fn concurrent_record_entries_preserves_batches_across_many_nodes() {
        let store = AgentLogStore::new();
        let mut tasks = JoinSet::new();
        let node_count = 128;

        for node_index in 0..node_count {
            let store = store.clone();
            tasks.spawn(async move {
                let node_id = format!("node-{node_index:03}");
                let entries = (0..MAX_BATCH_ENTRIES)
                    .map(|entry_index| test_entry(format!("{node_id}-entry-{entry_index:03}")))
                    .collect();
                let result = store.record_entries(&node_id, entries).await;
                (node_id, result)
            });
        }

        while let Some(joined) = tasks.join_next().await {
            let (node_id, result) = joined.expect("concurrent log write task should join");
            assert_eq!(result.accepted, MAX_BATCH_ENTRIES, "{node_id}");
            assert_eq!(result.dropped_batch_cap, 0, "{node_id}");
            assert_eq!(result.dropped_sanitize, 0, "{node_id}");
            assert_eq!(result.evicted_global_budget, 0, "{node_id}");
        }

        let stats = store.stats().await;
        assert_eq!(stats.nodes, node_count);
        assert_eq!(stats.entries, node_count * MAX_BATCH_ENTRIES);

        let sample = store.list("node-064", MAX_BATCH_ENTRIES).await;
        assert_eq!(sample.len(), MAX_BATCH_ENTRIES);
        assert_eq!(
            sample
                .first()
                .expect("sample should contain oldest kept entry")
                .message,
            "node-064-entry-000"
        );
        assert_eq!(
            sample
                .last()
                .expect("sample should contain newest kept entry")
                .message,
            format!("node-064-entry-{:03}", MAX_BATCH_ENTRIES - 1)
        );
    }

    #[test]
    fn truncate_to_byte_boundary_preserves_utf8() {
        let value = "日志-abcdef";
        let truncated = nodelite_proto::truncate_to_byte_boundary(value, 5);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert_eq!(truncated, "日");
    }

    fn test_entry(message: String) -> AgentLogEntry {
        AgentLogEntry {
            occurred_at: Utc::now().to_rfc3339(),
            level: NoticeLevel::Info,
            message,
        }
    }
}
