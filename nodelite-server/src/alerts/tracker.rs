use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use nodelite_proto::AlertRuleConfig;

use super::{AlertMetricReading, EvaluatedRule};

const DELIVERY_FAILURE_RETRY_DELAY_MINUTES: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlertEventKind {
    Triggered,
    Resolved,
}

impl AlertEventKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Triggered => "triggered",
            Self::Resolved => "resolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AlertEvent {
    pub(crate) kind: AlertEventKind,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) rule: AlertRuleConfig,
    pub(crate) node_id: String,
    pub(crate) node_label: String,
    pub(crate) reading: Option<AlertMetricReading>,
}

#[derive(Debug, Default)]
pub(crate) struct AlertStateTracker {
    active: HashMap<AlertInstanceKey, ActiveAlertState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AlertInstanceKey {
    rule_id: String,
    node_id: String,
}

#[derive(Debug, Clone)]
struct ActiveAlertState {
    node_label: String,
    last_reading: AlertMetricReading,
    last_notified_at: DateTime<Utc>,
}

impl AlertStateTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.active.clear();
    }

    pub(crate) fn update(
        &mut self,
        rules: &[AlertRuleConfig],
        matches: &[EvaluatedRule],
        now: DateTime<Utc>,
    ) -> Vec<AlertEvent> {
        let rules_by_id = rules
            .iter()
            .filter(|rule| rule.enabled)
            .map(|rule| (rule.id.as_str(), rule))
            .collect::<HashMap<_, _>>();
        let mut current_keys = HashSet::new();
        let mut events = Vec::new();

        for matched in matches {
            let Some(rule) = rules_by_id.get(matched.rule_id.as_str()) else {
                continue;
            };
            let key = AlertInstanceKey {
                rule_id: matched.rule_id.clone(),
                node_id: matched.node_id.clone(),
            };
            current_keys.insert(key.clone());

            match self.active.get_mut(&key) {
                Some(active) => {
                    active.node_label.clone_from(&matched.node_label);
                    active.last_reading = matched.reading.clone();
                    if should_repeat_alert(active.last_notified_at, rule.cooldown_minutes, now) {
                        active.last_notified_at = now;
                        events.push(triggered_event(rule, matched, now));
                    }
                }
                None => {
                    self.active.insert(
                        key,
                        ActiveAlertState {
                            node_label: matched.node_label.clone(),
                            last_reading: matched.reading.clone(),
                            last_notified_at: now,
                        },
                    );
                    events.push(triggered_event(rule, matched, now));
                }
            }
        }

        let resolved = self
            .active
            .keys()
            .filter(|key| !current_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in resolved {
            let Some(active) = self.active.remove(&key) else {
                continue;
            };
            let Some(rule) = rules_by_id.get(key.rule_id.as_str()) else {
                continue;
            };
            if rule.send_resolved {
                events.push(AlertEvent {
                    kind: AlertEventKind::Resolved,
                    occurred_at: now,
                    rule: (*rule).clone(),
                    node_id: key.node_id,
                    node_label: active.node_label,
                    reading: Some(active.last_reading),
                });
            }
        }

        events
    }

    pub(crate) fn record_delivery_failure(&mut self, event: &AlertEvent, now: DateTime<Utc>) {
        if event.kind != AlertEventKind::Triggered {
            return;
        }
        let key = AlertInstanceKey {
            rule_id: event.rule.id.clone(),
            node_id: event.node_id.clone(),
        };
        let Some(active) = self.active.get_mut(&key) else {
            return;
        };
        active.last_notified_at = retry_notified_at(event.rule.cooldown_minutes, now);
    }
}

fn should_repeat_alert(
    last_notified_at: DateTime<Utc>,
    cooldown_minutes: u64,
    now: DateTime<Utc>,
) -> bool {
    let cooldown_minutes = i64::try_from(cooldown_minutes).unwrap_or(i64::MAX);
    now.signed_duration_since(last_notified_at) >= Duration::minutes(cooldown_minutes)
}

fn retry_notified_at(cooldown_minutes: u64, now: DateTime<Utc>) -> DateTime<Utc> {
    let cooldown_minutes = i64::try_from(cooldown_minutes).unwrap_or(i64::MAX);
    let retry_offset = cooldown_minutes.saturating_sub(DELIVERY_FAILURE_RETRY_DELAY_MINUTES);
    now - Duration::minutes(retry_offset)
}

fn triggered_event(
    rule: &AlertRuleConfig,
    matched: &EvaluatedRule,
    now: DateTime<Utc>,
) -> AlertEvent {
    AlertEvent {
        kind: AlertEventKind::Triggered,
        occurred_at: now,
        rule: rule.clone(),
        node_id: matched.node_id.clone(),
        node_label: matched.node_label.clone(),
        reading: Some(matched.reading.clone()),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
    };

    use super::{AlertEventKind, AlertStateTracker};
    use crate::alerts::{AlertMetricReading, EvaluatedRule};

    fn rule(send_resolved: bool) -> AlertRuleConfig {
        AlertRuleConfig {
            id: "cpu-hot".to_string(),
            name: "CPU".to_string(),
            enabled: true,
            metric: AlertMetric::CpuUsagePercent,
            comparator: AlertComparator::Gt,
            threshold: 90,
            window_minutes: 5,
            severity: AlertSeverity::Critical,
            scope_mode: AlertScopeMode::All,
            node_ids: Vec::new(),
            tags: Vec::new(),
            delivery: vec![AlertChannel::Webhook],
            cooldown_minutes: 30,
            send_resolved,
        }
    }

    fn matched(value: u64) -> EvaluatedRule {
        EvaluatedRule {
            rule_id: "cpu-hot".to_string(),
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong".to_string(),
            reading: AlertMetricReading {
                metric: AlertMetric::CpuUsagePercent,
                value,
                threshold: 90,
            },
        }
    }

    #[test]
    fn update_emits_trigger_once_inside_cooldown() {
        let now = Utc::now();
        let rules = vec![rule(true)];
        let mut tracker = AlertStateTracker::new();

        let first = tracker.update(&rules, &[matched(91)], now);
        let second = tracker.update(&rules, &[matched(92)], now + Duration::minutes(10));

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, AlertEventKind::Triggered);
        assert!(second.is_empty());
    }

    #[test]
    fn update_repeats_trigger_after_cooldown() {
        let now = Utc::now();
        let rules = vec![rule(true)];
        let mut tracker = AlertStateTracker::new();

        let _ = tracker.update(&rules, &[matched(91)], now);
        let repeated = tracker.update(&rules, &[matched(93)], now + Duration::minutes(30));

        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].kind, AlertEventKind::Triggered);
        assert_eq!(
            repeated[0].reading.as_ref().map(|reading| reading.value),
            Some(93)
        );
    }

    #[test]
    fn record_delivery_failure_allows_early_retry() {
        let now = Utc::now();
        let rules = vec![rule(true)];
        let mut tracker = AlertStateTracker::new();
        let first = tracker.update(&rules, &[matched(91)], now);

        tracker.record_delivery_failure(&first[0], now);
        let retry_too_soon = tracker.update(&rules, &[matched(92)], now + Duration::minutes(4));
        let retry = tracker.update(&rules, &[matched(93)], now + Duration::minutes(5));

        assert!(retry_too_soon.is_empty());
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].kind, AlertEventKind::Triggered);
        assert_eq!(
            retry[0].reading.as_ref().map(|reading| reading.value),
            Some(93)
        );
    }

    #[test]
    fn update_emits_resolved_when_match_disappears() {
        let now = Utc::now();
        let rules = vec![rule(true)];
        let mut tracker = AlertStateTracker::new();

        let _ = tracker.update(&rules, &[matched(91)], now);
        let resolved = tracker.update(&rules, &[], now + Duration::minutes(5));

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].kind, AlertEventKind::Resolved);
        assert_eq!(resolved[0].node_id, "hk-01");
    }

    #[test]
    fn update_skips_resolved_event_when_rule_disables_it() {
        let now = Utc::now();
        let rules = vec![rule(false)];
        let mut tracker = AlertStateTracker::new();

        let _ = tracker.update(&rules, &[matched(91)], now);
        let resolved = tracker.update(&rules, &[], now + Duration::minutes(5));

        assert!(resolved.is_empty());
    }
}
