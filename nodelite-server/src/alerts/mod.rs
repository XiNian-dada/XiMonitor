//! 告警运行时:把配置规则转换为可复用的触发与巡检摘要视图。

mod delivery;
mod evaluator;
mod runtime;
mod tracker;

pub(crate) use delivery::{
    InspectionSummary, deliver_alert_event, deliver_inspection_summary, smtp_endpoint_label,
    webhook_endpoint_label,
};
pub(crate) use evaluator::{
    AlertMetricReading, EvaluatedRule, InspectionReport, build_inspection_report, evaluate_rules,
};
pub(crate) use runtime::spawn_alert_runtime;
pub(crate) use tracker::{AlertEvent, AlertEventKind, AlertStateTracker};
