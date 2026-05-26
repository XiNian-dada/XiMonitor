//! 节点运行态注册表与会话生命周期。

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nodelite_proto::{NodeIdentity, NodeListItem, NodeSnapshot, NodeStatus, OverviewData};

use super::overview::build_overview_from_iter;
use super::session_control::SessionControlHandle;
use crate::ServerReadiness;
use crate::handlers::metrics_exporter::render_prometheus_metrics_from_iter;

#[derive(Debug, Default)]
pub(super) struct Registry {
    nodes: HashMap<String, NodeEntry>,
    sorted_node_ids: Vec<String>,
}

/// 单节点的注册项:对外暴露的 `status` 与内部的"当前活跃会话 ID"。
#[derive(Debug, Clone)]
struct NodeEntry {
    status: NodeStatus,
    summary: NodeListItem,
    active_session_id: Option<u64>,
    control: Option<SessionControlHandle>,
}

impl Registry {
    pub(super) fn register_node(
        &mut self,
        session_id: u64,
        identity: NodeIdentity,
        remote_ip: Option<String>,
        now: DateTime<Utc>,
    ) {
        let node_id = identity.node_id.clone();
        let inserted = !self.nodes.contains_key(&node_id);
        let entry = self.nodes.entry(node_id).or_insert_with(|| NodeEntry {
            status: NodeStatus {
                identity: identity.clone(),
                remote_ip: remote_ip.clone(),
                snapshot: None,
                last_seen: Some(now),
                latency_ms: None,
                online: true,
            },
            summary: NodeListItem {
                identity: nodelite_proto::NodeListIdentity::from(&identity),
                snapshot: None,
                latency_ms: None,
                online: true,
            },
            active_session_id: Some(session_id),
            control: None,
        });

        entry.status.identity = identity;
        entry.status.remote_ip = remote_ip;
        entry.status.online = true;
        entry.status.last_seen = Some(now);
        entry.status.latency_ms = None;
        entry.active_session_id = Some(session_id);
        entry.control = None;
        entry.summary = NodeListItem::from(&entry.status);
        if inserted {
            self.sorted_node_ids
                .push(entry.status.identity.node_id.clone());
        }
        self.resort_node_ids();
    }

    pub(super) fn update_snapshot(
        &mut self,
        node_id: &str,
        session_id: u64,
        snapshot: NodeSnapshot,
        now: DateTime<Utc>,
    ) -> Option<NodeStatus> {
        let entry = self.nodes.get_mut(node_id)?;
        if entry.active_session_id != Some(session_id) {
            return None;
        }

        entry.status.snapshot = Some(snapshot);
        entry.status.last_seen = Some(now);
        entry.status.online = true;
        entry.summary = NodeListItem::from(&entry.status);
        Some(entry.status.clone())
    }

    pub(super) fn update_latency(
        &mut self,
        node_id: &str,
        session_id: u64,
        latency_ms: u64,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id != Some(session_id) {
            return false;
        }

        entry.status.latency_ms = Some(latency_ms);
        entry.status.last_seen = Some(now);
        entry.status.online = true;
        entry.summary = NodeListItem::from(&entry.status);
        true
    }

    pub(super) fn mark_disconnected(&mut self, node_id: &str, session_id: u64) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id == Some(session_id) {
            entry.active_session_id = None;
            entry.status.online = false;
            entry.control = None;
            entry.summary.online = false;
            return true;
        }
        false
    }

    pub(super) fn attach_session_control(
        &mut self,
        node_id: &str,
        session_id: u64,
        control: SessionControlHandle,
    ) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id != Some(session_id) {
            return false;
        }

        entry.control = Some(control);
        true
    }

    pub(super) fn mark_stale(&mut self, threshold: Duration, now: DateTime<Utc>) -> usize {
        let mut marked = 0;

        for entry in self.nodes.values_mut() {
            let Some(last_seen) = entry.status.last_seen else {
                continue;
            };
            let Ok(elapsed) = (now - last_seen).to_std() else {
                continue;
            };
            if elapsed >= threshold && entry.status.online {
                entry.status.online = false;
                entry.active_session_id = None;
                entry.control = None;
                entry.summary.online = false;
                marked += 1;
            }
        }

        marked
    }

    pub(super) fn is_current_session(&self, node_id: &str, session_id: u64) -> bool {
        self.nodes
            .get(node_id)
            .and_then(|entry| entry.active_session_id)
            == Some(session_id)
    }

    pub(super) fn list_statuses(&self) -> Vec<NodeStatus> {
        self.sorted_node_ids
            .iter()
            .filter_map(|node_id| self.nodes.get(node_id))
            .map(|entry| entry.status.clone())
            .collect()
    }

    pub(super) fn list_node_summaries(&self) -> Vec<NodeListItem> {
        self.sorted_node_ids
            .iter()
            .filter_map(|node_id| self.nodes.get(node_id))
            .map(|entry| entry.summary.clone())
            .collect()
    }

    pub(super) fn get_status(&self, node_id: &str) -> Option<NodeStatus> {
        self.nodes.get(node_id).map(|entry| entry.status.clone())
    }

    pub(super) fn session_control(&self, node_id: &str) -> Option<SessionControlHandle> {
        let entry = self.nodes.get(node_id)?;
        if entry.active_session_id.is_none() || !entry.status.online {
            return None;
        }
        entry.control.clone()
    }

    pub(super) fn overview(&self) -> OverviewData {
        build_overview_from_iter(self.nodes.values().map(|entry| &entry.status))
    }

    pub(super) fn render_metrics_body(&self, readiness: &ServerReadiness) -> String {
        let overview = self.overview();
        render_prometheus_metrics_from_iter(
            readiness,
            self.sorted_node_ids
                .iter()
                .filter_map(|node_id| self.nodes.get(node_id))
                .map(|entry| &entry.status),
            &overview,
        )
    }

    pub(super) fn disk_entries_total(&self) -> u64 {
        self.nodes
            .values()
            .filter_map(|entry| entry.status.snapshot.as_ref())
            .map(|snapshot| snapshot.disks.len() as u64)
            .sum()
    }

    pub(super) fn restore_statuses(&mut self, statuses: Vec<NodeStatus>) {
        self.nodes.clear();
        self.sorted_node_ids.clear();
        for mut status in statuses {
            status.online = false;
            let summary = NodeListItem::from(&status);
            let node_id = status.identity.node_id.clone();
            self.nodes.insert(
                node_id.clone(),
                NodeEntry {
                    status,
                    summary,
                    active_session_id: None,
                    control: None,
                },
            );
            self.sorted_node_ids.push(node_id);
        }
        self.resort_node_ids();
    }

    fn resort_node_ids(&mut self) {
        self.sorted_node_ids.sort_by(|left_id, right_id| {
            let (Some(left), Some(right)) = (self.nodes.get(left_id), self.nodes.get(right_id))
            else {
                return left_id.cmp(right_id);
            };
            left.status
                .identity
                .node_label
                .cmp(&right.status.identity.node_label)
                .then_with(|| {
                    left.status
                        .identity
                        .node_id
                        .cmp(&right.status.identity.node_id)
                })
        });
    }
}
