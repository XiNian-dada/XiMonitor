//! 节点运行态注册表与会话生命周期。

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nodelite_proto::{NodeIdentity, NodeSnapshot, NodeStatus, OverviewData};
use tokio::sync::mpsc;

use super::SessionCommand;
use super::overview::{build_overview, build_overview_from_iter};

#[derive(Debug, Default)]
pub(super) struct Registry {
    nodes: HashMap<String, NodeEntry>,
}

/// 单节点的注册项:对外暴露的 `status` 与内部的"当前活跃会话 ID"。
#[derive(Debug, Clone)]
struct NodeEntry {
    status: NodeStatus,
    active_session_id: Option<u64>,
    control_tx: Option<mpsc::UnboundedSender<SessionCommand>>,
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
        let entry = self.nodes.entry(node_id).or_insert_with(|| NodeEntry {
            status: NodeStatus {
                identity: identity.clone(),
                remote_ip: remote_ip.clone(),
                snapshot: None,
                last_seen: Some(now),
                latency_ms: None,
                online: true,
            },
            active_session_id: Some(session_id),
            control_tx: None,
        });

        entry.status.identity = identity;
        entry.status.remote_ip = remote_ip;
        entry.status.online = true;
        entry.status.last_seen = Some(now);
        entry.status.latency_ms = None;
        entry.active_session_id = Some(session_id);
        entry.control_tx = None;
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
        true
    }

    pub(super) fn mark_disconnected(&mut self, node_id: &str, session_id: u64) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id == Some(session_id) {
            entry.active_session_id = None;
            entry.status.online = false;
            entry.control_tx = None;
            return true;
        }
        false
    }

    pub(super) fn attach_session_control(
        &mut self,
        node_id: &str,
        session_id: u64,
        control_tx: mpsc::UnboundedSender<SessionCommand>,
    ) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id != Some(session_id) {
            return false;
        }

        entry.control_tx = Some(control_tx);
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
                entry.control_tx = None;
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
        let mut statuses: Vec<NodeStatus> = self
            .nodes
            .values()
            .map(|entry| entry.status.clone())
            .collect();
        statuses.sort_by(|left, right| {
            left.identity
                .node_label
                .cmp(&right.identity.node_label)
                .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
        });
        statuses
    }

    pub(super) fn get_status(&self, node_id: &str) -> Option<NodeStatus> {
        self.nodes.get(node_id).map(|entry| entry.status.clone())
    }

    pub(super) fn session_control(
        &self,
        node_id: &str,
    ) -> Option<mpsc::UnboundedSender<SessionCommand>> {
        let entry = self.nodes.get(node_id)?;
        if entry.active_session_id.is_none() || !entry.status.online {
            return None;
        }
        entry.control_tx.clone()
    }

    pub(super) fn overview(&self) -> OverviewData {
        build_overview_from_iter(self.nodes.values().map(|entry| &entry.status))
    }

    pub(super) fn overview_from_statuses(&self, statuses: &[NodeStatus]) -> OverviewData {
        build_overview(statuses)
    }

    pub(super) fn restore_statuses(&mut self, statuses: Vec<NodeStatus>) {
        self.nodes.clear();
        for mut status in statuses {
            status.online = false;
            let node_id = status.identity.node_id.clone();
            self.nodes.insert(
                node_id,
                NodeEntry {
                    status,
                    active_session_id: None,
                    control_tx: None,
                },
            );
        }
    }
}
