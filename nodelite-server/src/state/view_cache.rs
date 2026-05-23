//! 概览 API 与 Prometheus 输出的瞬时缓存。

use std::time::{Duration, Instant};

use axum::body::Bytes;

use crate::ServerReadiness;

/// `/api/overview` 与 `/api/nodes` 的缓存键。
#[derive(Debug, Clone, Copy)]
pub(super) enum ApiBodyKind {
    Nodes,
    Overview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReadinessSnapshot {
    ready: bool,
    history_available: bool,
    registry_reload_healthy: bool,
}

impl ReadinessSnapshot {
    pub(super) fn new(ready: bool, history_available: bool, registry_reload_healthy: bool) -> Self {
        Self {
            ready,
            history_available,
            registry_reload_healthy,
        }
    }

    pub(super) fn capture(readiness: &ServerReadiness) -> Self {
        Self::new(
            readiness.is_ready(),
            readiness.history_available(),
            readiness.registry_reload_healthy(),
        )
    }
}

#[derive(Debug, Default)]
pub(super) struct ViewCache {
    revision: u64,
    nodes_json: Option<Bytes>,
    overview_json: Option<Bytes>,
    metrics_revision: u64,
    metrics_readiness: Option<ReadinessSnapshot>,
    metrics_cached_at: Option<Instant>,
    metrics_text: Option<Bytes>,
}

impl ViewCache {
    pub(super) fn api_body(&self, revision: u64, kind: ApiBodyKind) -> Option<Bytes> {
        if self.revision != revision {
            return None;
        }

        match kind {
            ApiBodyKind::Nodes => self.nodes_json.clone(),
            ApiBodyKind::Overview => self.overview_json.clone(),
        }
    }

    pub(super) fn store_api_body(&mut self, revision: u64, kind: ApiBodyKind, body: Bytes) {
        if self.revision != revision {
            self.revision = revision;
            self.nodes_json = None;
            self.overview_json = None;
        }

        match kind {
            ApiBodyKind::Nodes => self.nodes_json = Some(body),
            ApiBodyKind::Overview => self.overview_json = Some(body),
        }
    }

    pub(super) fn metrics_body(
        &self,
        revision: u64,
        readiness: ReadinessSnapshot,
        max_age: Duration,
    ) -> Option<Bytes> {
        if self.metrics_revision != revision {
            return None;
        }
        if self.metrics_readiness != Some(readiness) {
            return None;
        }
        if self
            .metrics_cached_at
            .is_none_or(|cached_at| cached_at.elapsed() > max_age)
        {
            return None;
        }

        self.metrics_text.clone()
    }

    pub(super) fn store_metrics_body(
        &mut self,
        revision: u64,
        readiness: ReadinessSnapshot,
        body: Bytes,
    ) {
        self.metrics_revision = revision;
        self.metrics_readiness = Some(readiness);
        self.metrics_cached_at = Some(Instant::now());
        self.metrics_text = Some(body);
    }
}
