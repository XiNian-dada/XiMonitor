//! 把 Agent 上报的 NodeSnapshot 二次校验为"可信"形态。
//!
//! Agent 进程是不受信任的输入源(可能是 buggy 版本、可能被攻陷),所以在
//! 进入聚合/历史表/UI 之前必须把不可能值卡到上限,否则会扭曲仪表盘汇总、
//! 压垮加和、或污染历史样本。本模块只负责清洗;什么时候断开异常会话由
//! 调用方根据 [`SanitizationReport`] + 滑动窗口决定。

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use nodelite_proto::{
    DiskUsage, LoadAverage, MemoryUsage, NetworkCounters, NodeSnapshot, ServerConfig, percentage,
};

/// 历史采样中允许的最大磁盘条目数,防止恶意 Agent 制造海量条目。
pub const MAX_SANITIZED_DISKS: usize = 64;
/// 单个磁盘字段(device/mount_point/fs_type)允许的最大字节数,防止 Agent
/// 上报巨型字符串撑爆 UI 与历史库。
pub const MAX_SANITIZED_STRING_BYTES: usize = 256;
/// 网络速率字段的合法上限(字节/秒)。
pub const MAX_SANITIZED_RATE_BYTES_PER_SEC: f64 = 1_000_000_000_000.0;
/// 负载平均数的合法上限。
pub const MAX_SANITIZED_LOAD: f64 = 1_000_000.0;
/// 一个 WebSocket 会话在 `METRIC_ANOMALY_WINDOW_SECS` 窗口内允许出现的异常
/// metrics 报告次数,超过即主动断开。
/// 滑动窗口的设计避免了"长会话偶发异常累积"造成的误判:任何 anomaly 在窗口
/// 过去之后都会被忽略,只有真正持续上报异常的 Agent 才会触发断连。
pub const METRIC_ANOMALY_SESSION_LIMIT: usize = 5;
/// 计算 anomaly 触发阈值时使用的滑动窗口(秒),默认 5 分钟。
pub const METRIC_ANOMALY_WINDOW_SECS: u64 = 300;

/// 对来自 Agent 的快照进行二次校验。
/// 把所有疑似越界的字段统一约束到合法范围,避免它们污染 UI 汇总、聚合或历史表。
///
/// 返回值中的 `SanitizationReport` 记录了本次清洗触发的各类异常计数,
/// 上层会话循环据此输出告警并在异常持续时主动断开连接。
pub fn sanitize_snapshot(
    config: &ServerConfig,
    mut snapshot: NodeSnapshot,
) -> (NodeSnapshot, SanitizationReport) {
    let mut report = SanitizationReport::default();
    snapshot.cpu_usage_percent =
        sanitize_percentage(snapshot.cpu_usage_percent, &mut report.clamped_percents);
    snapshot.load = sanitize_load_average(snapshot.load, &mut report);
    snapshot.memory = sanitize_memory_usage(snapshot.memory, &mut report);
    snapshot.network = sanitize_network_counters(snapshot.network, &mut report);
    let mut sanitized_disks = Vec::new();
    let mut seen_disk_devices = HashSet::new();
    for disk in snapshot.disks {
        if config
            .ignored_filesystems
            .iter()
            .any(|fs| fs == &disk.fs_type)
        {
            continue;
        }

        let Some(disk) = sanitize_disk_usage(disk, &mut report) else {
            continue;
        };
        let disk_identity = disk_device_identity(&disk);
        if !seen_disk_devices.insert(disk_identity) {
            report.dropped_disks = report.dropped_disks.saturating_add(1);
            continue;
        }
        if sanitized_disks.len() >= MAX_SANITIZED_DISKS {
            report.dropped_disks = report.dropped_disks.saturating_add(1);
            continue;
        }
        sanitized_disks.push(disk);
    }
    snapshot.disks = sanitized_disks;
    (snapshot, report)
}

/// 记录一次 `sanitize_snapshot` 期间各类清洗操作的发生次数。
///
/// 字段都按"被改动的次数"计;上层只关心 `modified()` 与各字段非零情况,
/// 不依赖于精确次数语义,所以使用 `saturating_add` 即可。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SanitizationReport {
    pub clamped_percents: u32,
    pub clamped_loads: u32,
    pub clamped_memory_bytes: u32,
    pub clamped_disk_bytes: u32,
    pub truncated_strings: u32,
    pub dropped_disks: u32,
    pub sanitized_rates: u32,
}

impl SanitizationReport {
    pub fn total(&self) -> u32 {
        self.clamped_percents
            .saturating_add(self.clamped_loads)
            .saturating_add(self.clamped_memory_bytes)
            .saturating_add(self.clamped_disk_bytes)
            .saturating_add(self.truncated_strings)
            .saturating_add(self.dropped_disks)
            .saturating_add(self.sanitized_rates)
    }

    pub fn modified(&self) -> bool {
        self.total() > 0
    }
}

/// 把"本次清洗是否触发了 anomaly"折算到滑动窗口里。
///
/// `window` 中保留的是最近若干次 anomaly 的发生时刻;早于
/// `now - METRIC_ANOMALY_WINDOW_SECS` 的条目在此被剔除,确保长会话里
/// 偶发的 sanitize 修正不会无限累积成"会话级"误判。
pub fn update_metric_anomaly_window(
    window: &mut VecDeque<Instant>,
    report: &SanitizationReport,
    now: Instant,
) {
    let horizon = Duration::from_secs(METRIC_ANOMALY_WINDOW_SECS);
    while window
        .front()
        .is_some_and(|recorded_at| now.duration_since(*recorded_at) > horizon)
    {
        window.pop_front();
    }
    if report.modified() {
        window.push_back(now);
    }
}

pub fn should_disconnect_for_metric_anomalies(window: &VecDeque<Instant>) -> bool {
    window.len() >= METRIC_ANOMALY_SESSION_LIMIT
}

fn sanitize_percentage(value: f64, counter: &mut u32) -> f64 {
    let sanitized = sanitize_non_negative_f64(value, 100.0);
    if value != sanitized {
        *counter = counter.saturating_add(1);
    }
    sanitized
}

fn sanitize_non_negative_f64(value: f64, max: f64) -> f64 {
    if value.is_nan() || value < 0.0 {
        return 0.0;
    }
    if value.is_infinite() {
        return max;
    }

    value.min(max)
}

fn sanitize_load_average(load: LoadAverage, report: &mut SanitizationReport) -> LoadAverage {
    LoadAverage {
        one: sanitize_load_value(load.one, &mut report.clamped_loads),
        five: sanitize_load_value(load.five, &mut report.clamped_loads),
        fifteen: sanitize_load_value(load.fifteen, &mut report.clamped_loads),
    }
}

fn sanitize_load_value(value: f64, counter: &mut u32) -> f64 {
    let sanitized = sanitize_non_negative_f64(value, MAX_SANITIZED_LOAD);
    if value != sanitized {
        *counter = counter.saturating_add(1);
    }
    sanitized
}

fn sanitize_memory_usage(mut memory: MemoryUsage, report: &mut SanitizationReport) -> MemoryUsage {
    let original_used = memory.used_bytes;
    let original_available = memory.available_bytes;
    let original_swap_used = memory.swap_used_bytes;

    memory.used_bytes = memory.used_bytes.min(memory.total_bytes);
    memory.available_bytes = memory.available_bytes.min(memory.total_bytes);
    if memory.used_bytes.saturating_add(memory.available_bytes) > memory.total_bytes {
        // 当 used + available 大于 total 时,以 used 为准重新算 available,保持口径一致。
        memory.available_bytes = memory.total_bytes.saturating_sub(memory.used_bytes);
    }

    memory.swap_used_bytes = memory.swap_used_bytes.min(memory.swap_total_bytes);

    if memory.used_bytes != original_used
        || memory.available_bytes != original_available
        || memory.swap_used_bytes != original_swap_used
    {
        report.clamped_memory_bytes = report.clamped_memory_bytes.saturating_add(1);
    }
    memory
}

fn sanitize_disk_usage(mut disk: DiskUsage, report: &mut SanitizationReport) -> Option<DiskUsage> {
    disk.device = disk.device.trim().to_string();
    disk.mount_point = disk.mount_point.trim().to_string();
    disk.fs_type = disk.fs_type.trim().to_string();
    if disk.device.is_empty() || disk.mount_point.is_empty() || disk.fs_type.is_empty() {
        report.dropped_disks = report.dropped_disks.saturating_add(1);
        return None;
    }
    // 字符串字段做硬截断,避免 Agent 上报巨型字符串污染 UI 或历史库。
    let original_device_len = disk.device.len();
    let original_mount_len = disk.mount_point.len();
    let original_fs_len = disk.fs_type.len();
    truncate_to_byte_boundary(&mut disk.device, MAX_SANITIZED_STRING_BYTES);
    truncate_to_byte_boundary(&mut disk.mount_point, MAX_SANITIZED_STRING_BYTES);
    truncate_to_byte_boundary(&mut disk.fs_type, MAX_SANITIZED_STRING_BYTES);
    if disk.device.len() != original_device_len
        || disk.mount_point.len() != original_mount_len
        || disk.fs_type.len() != original_fs_len
    {
        report.truncated_strings = report.truncated_strings.saturating_add(1);
    }

    let original_used = disk.used_bytes;
    let original_available = disk.available_bytes;
    disk.available_bytes = disk.available_bytes.min(disk.total_bytes);
    disk.used_bytes = disk.used_bytes.min(disk.total_bytes);
    if disk.used_bytes.saturating_add(disk.available_bytes) > disk.total_bytes {
        // 当两个字段相互矛盾时,以 available 为基线重算 used,得到自洽的 used 部分。
        disk.used_bytes = disk.total_bytes.saturating_sub(disk.available_bytes);
    }
    if disk.used_bytes != original_used || disk.available_bytes != original_available {
        report.clamped_disk_bytes = report.clamped_disk_bytes.saturating_add(1);
    }
    // 这里 percentage() 输入是已被裁剪过的 u64,理论上不会越界;
    // 用 sanitize_percentage 仍能在未来字段语义改变时兜底,并保持 used_percent 与 used_bytes 自洽。
    disk.used_percent = sanitize_percentage(
        percentage(disk.used_bytes, disk.total_bytes),
        &mut report.clamped_percents,
    );
    Some(disk)
}

fn disk_device_identity(disk: &DiskUsage) -> String {
    format!("{}:{}", disk.device, disk.total_bytes)
}

fn sanitize_network_counters(
    mut network: NetworkCounters,
    report: &mut SanitizationReport,
) -> NetworkCounters {
    network.rx_bytes_per_sec = sanitize_optional_rate(
        network.rx_bytes_per_sec,
        MAX_SANITIZED_RATE_BYTES_PER_SEC,
        &mut report.sanitized_rates,
    );
    network.tx_bytes_per_sec = sanitize_optional_rate(
        network.tx_bytes_per_sec,
        MAX_SANITIZED_RATE_BYTES_PER_SEC,
        &mut report.sanitized_rates,
    );
    network
}

fn sanitize_optional_rate(value: Option<f64>, max: f64, counter: &mut u32) -> Option<f64> {
    value.map(|v| {
        let sanitized = sanitize_non_negative_f64(v, max);
        if v != sanitized {
            *counter = counter.saturating_add(1);
        }
        sanitized
    })
}

/// 把字符串截到不超过 `max_bytes` 字节,且必须落在 UTF-8 字符边界上。
pub(crate) fn truncate_to_byte_boundary(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }

    for cutoff in (max_bytes.saturating_sub(3)..=max_bytes).rev() {
        if value.is_char_boundary(cutoff) {
            value.truncate(cutoff);
            return;
        }
    }

    // UTF-8 code points are at most 4 bytes, so this is only a defensive fallback.
    value.clear();
}
