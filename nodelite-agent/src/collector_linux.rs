//! Linux 主机指标采集器:读取 `/proc`、`statvfs` 等内核接口,
//! 把原始数据归并成 `nodelite-proto` 中定义的快照与身份结构。

use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use nodelite_proto::{
    AgentConfig, DiskUsage, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
    percentage,
};
use tracing::warn;

use super::shared::{
    CpuSample, NetworkSample, NetworkTotals, compute_cpu_usage, compute_network_rates,
};

/// 采集器状态:为了计算 CPU/网络的"差分速率",需要保留上一次的采样值。
pub struct HostCollector {
    sys_root: std::path::PathBuf,
    previous_cpu: Option<CpuSample>,
    previous_network: Option<NetworkSample>,
}

pub fn new_collector() -> HostCollector {
    HostCollector {
        sys_root: std::path::PathBuf::from("/"),
        previous_cpu: None,
        previous_network: None,
    }
}

impl HostCollector {
    #[cfg(test)]
    pub fn new_with_root(sys_root: std::path::PathBuf) -> Self {
        Self {
            sys_root,
            previous_cpu: None,
            previous_network: None,
        }
    }

    /// 组装节点身份。`agent_version` 来源于编译期注入,运行期固定不变。
    pub fn collect_identity(
        &self,
        config: &AgentConfig,
        agent_version: &str,
    ) -> Result<NodeIdentity> {
        let uptime_path = self.sys_root.join("proc/uptime");
        let uptime_secs = read_uptime(&uptime_path)?;
        // 由当前时刻反推启动时间,在 i64 转换溢出时退化为 i64::MAX 防止 panic。
        let boot_time =
            Utc::now() - Duration::seconds(i64::try_from(uptime_secs).unwrap_or(i64::MAX));

        let hostname_path = self.sys_root.join("proc/sys/kernel/hostname");
        let os_release_path = self.sys_root.join("etc/os-release");
        let osrelease_path = self.sys_root.join("proc/sys/kernel/osrelease");
        let cpuinfo_path = self.sys_root.join("proc/cpuinfo");

        Ok(NodeIdentity {
            node_id: config.node_id.clone(),
            node_label: config.node_label.clone(),
            hostname: config
                .hostname_override
                .clone()
                .unwrap_or(read_hostname(&hostname_path)?),
            os: read_os_name(&os_release_path).unwrap_or_else(|_| "linux".to_string()),
            kernel_version: read_trimmed(&osrelease_path).ok(),
            cpu_model: read_cpu_model(&cpuinfo_path).ok(),
            cpu_cores: count_cpu_cores(&cpuinfo_path).unwrap_or(1),
            agent_version: agent_version.to_string(),
            boot_time: Some(boot_time),
            tags: config.tags.clone(),
        })
    }

    /// 采集一张完整快照。
    ///
    /// 首次调用时由于没有"上一次"的数据,`cpu_usage_percent` 与网络速率
    /// 都会返回 `None`,这是符合预期的初始状态。
    pub fn collect_snapshot(&mut self) -> Result<NodeSnapshot> {
        let stat_path = self.sys_root.join("proc/stat");
        let cpu_sample =
            parse_cpu_sample(&fs::read_to_string(&stat_path).context("read /proc/stat")?)?;
        let cpu_usage_percent = self
            .previous_cpu
            .map(|previous| compute_cpu_usage(previous, cpu_sample));
        self.previous_cpu = Some(cpu_sample);

        let dev_path = self.sys_root.join("proc/net/dev");
        let network_totals = parse_network_totals(
            &fs::read_to_string(&dev_path).context("read /proc/net/dev")?,
        )?;
        let observed_at = Instant::now();
        let (rx_bytes_per_sec, tx_bytes_per_sec) = if let Some(previous) = self.previous_network {
            compute_network_rates(previous, observed_at, network_totals)
        } else {
            (None, None)
        };
        self.previous_network = Some(NetworkSample {
            observed_at,
            rx_bytes: network_totals.rx_bytes,
            tx_bytes: network_totals.tx_bytes,
        });

        let loadavg_path = self.sys_root.join("proc/loadavg");
        let load = parse_load_average(
            &fs::read_to_string(&loadavg_path).context("read /proc/loadavg")?,
        )?;
        let meminfo_path = self.sys_root.join("proc/meminfo");
        let memory = parse_memory_usage(
            &fs::read_to_string(&meminfo_path).context("read /proc/meminfo")?,
        )?;
        let uptime_path = self.sys_root.join("proc/uptime");
        let uptime_secs = read_uptime(&uptime_path)?;
        let mounts_path = self.sys_root.join("proc/mounts");
        let disks = collect_disks(&mounts_path)?;

        Ok(NodeSnapshot {
            collected_at: Utc::now(),
            cpu_usage_percent,
            load,
            memory,
            uptime_secs,
            disks,
            network: NetworkCounters {
                total_rx_bytes: network_totals.rx_bytes,
                total_tx_bytes: network_totals.tx_bytes,
                rx_bytes_per_sec,
                tx_bytes_per_sec,
            },
        })
    }
}

/// 读取文件文本并去除首尾空白。
fn read_trimmed(path: &std::path::Path) -> Result<String> {
    Ok(fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .to_string())
}

fn read_hostname(path: &std::path::Path) -> Result<String> {
    read_trimmed(path)
}

/// 解析 `/etc/os-release`,优先返回 `PRETTY_NAME`,缺失时退化到 `NAME`。
fn read_os_name(path: &std::path::Path) -> Result<String> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
            return Ok(strip_quotes(value));
        }
        if let Some(value) = line.strip_prefix("NAME=") {
            return Ok(strip_quotes(value));
        }
    }
    Err(anyhow!("NAME not found in {}", path.display()))
}

fn strip_quotes(value: &str) -> String {
    value.trim_matches('"').to_string()
}

/// 从 `/proc/cpuinfo` 中提取第一处 `model name`。
fn read_cpu_model(path: &std::path::Path) -> Result<String> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("model name\t: ") {
            return Ok(value.trim().to_string());
        }
    }
    Err(anyhow!("model name not found in {}", path.display()))
}

/// 通过统计 `processor` 行的数量得到逻辑核心数;至少返回 1。
fn count_cpu_cores(path: &std::path::Path) -> Result<u32> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let count = content
        .lines()
        .filter(|line| line.starts_with("processor\t:"))
        .count();
    Ok(u32::try_from(count).unwrap_or(u32::MAX).max(1))
}

/// 读取 `/proc/uptime` 的整数秒部分。
fn read_uptime(path: &std::path::Path) -> Result<u64> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw = content
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("missing uptime field in {}", path.display()))?;
    let seconds = raw
        .split('.')
        .next()
        .ok_or_else(|| anyhow!("invalid uptime field in {}", path.display()))?;
    seconds
        .parse::<u64>()
        .with_context(|| format!("invalid uptime value in {}", path.display()))
}

/// 解析 `/proc/stat` 中的 `cpu ` 聚合行。
///
/// 字段顺序:user / nice / system / idle / iowait / ...
/// 这里我们只关心 `total = 全部之和`,以及 `idle = idle + iowait`。
fn parse_cpu_sample(content: &str) -> Result<CpuSample> {
    let line = content
        .lines()
        .find(|line| line.starts_with("cpu "))
        .ok_or_else(|| anyhow!("missing aggregate cpu line"))?;
    let mut total = 0_u64;
    let mut idle = 0_u64;
    let mut counter_count = 0_usize;
    for (index, raw_value) in line.split_whitespace().skip(1).enumerate() {
        let value = raw_value.parse::<u64>().context("invalid cpu counter")?;
        total = total.saturating_add(value);
        if index == 3 || index == 4 {
            idle = idle.saturating_add(value);
        }
        counter_count += 1;
    }
    if counter_count < 5 {
        return Err(anyhow!("expected at least 5 cpu counters"));
    }
    Ok(CpuSample { total, idle })
}

/// 解析 `/proc/loadavg` 的前三个字段(1/5/15 分钟平均负载)。
fn parse_load_average(content: &str) -> Result<LoadAverage> {
    let mut fields = content.split_whitespace();
    let one = parse_next_load_field(&mut fields, "1m")?;
    let five = parse_next_load_field(&mut fields, "5m")?;
    let fifteen = parse_next_load_field(&mut fields, "15m")?;
    Ok(LoadAverage { one, five, fifteen })
}

fn parse_next_load_field<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<f64> {
    fields
        .next()
        .ok_or_else(|| anyhow!("expected 3 load average values"))?
        .parse::<f64>()
        .with_context(|| format!("invalid {label} load average"))
}

/// 解析 `/proc/meminfo`,把字段单位从 KB 转换为字节。
///
/// `MemAvailable` 若缺失(老内核),则用 `MemFree + Buffers + Cached` 兜底。
fn parse_memory_usage(content: &str) -> Result<MemoryUsage> {
    let mut mem_total_bytes = None;
    let mut mem_available_bytes = None;
    let mut mem_free_bytes = None;
    let mut buffers_bytes = None;
    let mut cached_bytes = None;
    let mut swap_total_bytes = None;
    let mut swap_free_bytes = None;

    for line in content.lines() {
        let Some((key, raw_value)) = line.split_once(':') else {
            continue;
        };
        if !matches!(
            key,
            "MemTotal"
                | "MemAvailable"
                | "MemFree"
                | "Buffers"
                | "Cached"
                | "SwapTotal"
                | "SwapFree"
        ) {
            continue;
        }
        let kilobytes = raw_value
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow!("missing meminfo value for {key}"))?
            .parse::<u64>()
            .with_context(|| format!("invalid meminfo value for {key}"))?;
        let bytes = kilobytes.saturating_mul(1024);
        match key {
            "MemTotal" => mem_total_bytes = Some(bytes),
            "MemAvailable" => mem_available_bytes = Some(bytes),
            "MemFree" => mem_free_bytes = Some(bytes),
            "Buffers" => buffers_bytes = Some(bytes),
            "Cached" => cached_bytes = Some(bytes),
            "SwapTotal" => swap_total_bytes = Some(bytes),
            "SwapFree" => swap_free_bytes = Some(bytes),
            _ => {}
        }
    }

    let total_bytes =
        mem_total_bytes.ok_or_else(|| anyhow!("MemTotal missing from /proc/meminfo"))?;
    let available_bytes = mem_available_bytes
        .or_else(|| {
            Some(
                mem_free_bytes?
                    .saturating_add(buffers_bytes.unwrap_or(0))
                    .saturating_add(cached_bytes.unwrap_or(0)),
            )
        })
        .ok_or_else(|| anyhow!("unable to infer available memory"))?;
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let swap_total_bytes = swap_total_bytes.unwrap_or(0);
    let swap_free_bytes = swap_free_bytes.unwrap_or(0);

    Ok(MemoryUsage {
        total_bytes,
        used_bytes,
        available_bytes,
        swap_total_bytes,
        swap_used_bytes: swap_total_bytes.saturating_sub(swap_free_bytes),
    })
}

/// 汇总 `/proc/net/dev` 中所有物理网卡的累计收发字节数。
/// 跳过 `lo`(回环口),避免本机通信被统计为外部流量。
fn parse_network_totals(content: &str) -> Result<NetworkTotals> {
    let mut rx_bytes = 0_u64;
    let mut tx_bytes = 0_u64;

    for line in content.lines().skip(2) {
        let Some((iface, counters)) = line.split_once(':') else {
            continue;
        };
        if iface.trim() == "lo" {
            continue;
        }
        let (iface_rx_bytes, iface_tx_bytes) = parse_network_line_counters(counters, iface.trim())?;
        rx_bytes = rx_bytes.saturating_add(iface_rx_bytes);
        tx_bytes = tx_bytes.saturating_add(iface_tx_bytes);
    }

    Ok(NetworkTotals { rx_bytes, tx_bytes })
}

fn parse_network_line_counters(counters: &str, iface: &str) -> Result<(u64, u64)> {
    let mut rx_bytes = None;
    let mut tx_bytes = None;
    let mut counter_count = 0_usize;

    for (index, raw_value) in counters.split_whitespace().enumerate() {
        let value = raw_value
            .parse::<u64>()
            .context("invalid network counter")?;
        if index == 0 {
            rx_bytes = Some(value);
        } else if index == 8 {
            tx_bytes = Some(value);
        }
        counter_count += 1;
    }

    if counter_count < 16 {
        return Err(anyhow!(
            "expected 16 network counters for interface {iface}"
        ));
    }

    Ok((rx_bytes.unwrap_or(0), tx_bytes.unwrap_or(0)))
}

/// 遍历 `/proc/mounts` 并通过 `statvfs` 获取各挂载点的容量信息。
/// 同一挂载点重复出现时只保留第一条;特殊虚拟文件系统会被忽略。
fn collect_disks(mounts_path: &std::path::Path) -> Result<Vec<DiskUsage>> {
    let content = fs::read_to_string(mounts_path).with_context(|| format!("read {}", mounts_path.display()))?;
    let mut seen_mounts = HashSet::new();
    let mut seen_devices = HashSet::new();
    let mut disks = Vec::new();

    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let Some(raw_device) = fields.next() else {
            continue;
        };
        let Some(raw_mount_point) = fields.next() else {
            continue;
        };
        let Some(raw_fs_type) = fields.next() else {
            continue;
        };
        let device = unescape_mount_field(raw_device);
        let mount_point = unescape_mount_field(raw_mount_point);
        let fs_type = raw_fs_type.to_string();

        if ignored_filesystems().contains(&fs_type.as_str())
            || !seen_mounts.insert(mount_point.clone())
        {
            continue;
        }

        let stats = match statvfs(&mount_point) {
            Ok(stats) => stats,
            Err(error) => {
                warn!(
                    mount_point = %mount_point,
                    fs_type = %fs_type,
                    error = ?error,
                    "skipping disk mount after statvfs failure",
                );
                continue;
            }
        };
        if stats.total_bytes == 0 {
            continue;
        }
        let device_identity = format!("{device}:{}", stats.total_bytes);
        if !seen_devices.insert(device_identity) {
            continue;
        }

        disks.push(DiskUsage {
            device,
            mount_point,
            fs_type,
            total_bytes: stats.total_bytes,
            available_bytes: stats.available_bytes,
            used_bytes: stats.used_bytes,
            used_percent: percentage(stats.used_bytes, stats.total_bytes),
        });
    }

    disks.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
    Ok(disks)
}

/// 默认忽略的"非物理"文件系统,这些通常代表内核虚拟视图或临时挂载。
fn ignored_filesystems() -> &'static [&'static str] {
    &[
        "autofs",
        "bpf",
        "cgroup",
        "cgroup2",
        "configfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "fusectl",
        "mqueue",
        "overlay",
        "proc",
        "pstore",
        "ramfs",
        "securityfs",
        "squashfs",
        "sysfs",
        "tmpfs",
        "tracefs",
    ]
}

/// `/proc/mounts` 中的空格会被转义为 `\040`,这里还原回真实字符。
fn unescape_mount_field(value: &str) -> String {
    value.replace("\\040", " ")
}

struct FilesystemStats {
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
}

/// 调用 libc 的 `statvfs` 获取挂载点容量,以字节为单位返回。
fn statvfs(path: &str) -> Result<FilesystemStats> {
    let c_path =
        CString::new(path.as_bytes()).with_context(|| format!("path contains NUL byte: {path}"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(anyhow!("statvfs failed for {}", Path::new(path).display()));
    }
    let stats = unsafe { stats.assume_init() };

    let block_size = stats.f_frsize;
    let total_blocks = stats.f_blocks;
    let available_blocks = stats.f_bavail;
    let total_bytes = total_blocks.saturating_mul(block_size);
    let available_bytes = available_blocks.saturating_mul(block_size);
    let used_bytes = total_bytes.saturating_sub(available_bytes);

    Ok(FilesystemStats {
        total_bytes,
        available_bytes,
        used_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        compute_cpu_usage, compute_network_rates, parse_cpu_sample, parse_load_average,
        parse_memory_usage, parse_network_totals,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn parses_cpu_sample_and_usage() {
        let previous = parse_cpu_sample("cpu  100 0 50 400 10 0 0 0 0 0\n").unwrap();
        let current = parse_cpu_sample("cpu  160 0 70 430 20 0 0 0 0 0\n").unwrap();
        let usage = compute_cpu_usage(previous, current);
        assert!(usage > 50.0 && usage < 70.0);
    }

    #[test]
    fn parses_load_average() {
        let load = parse_load_average("0.11 0.22 0.33 1/100 12345\n").unwrap();
        assert_eq!(load.one, 0.11);
        assert_eq!(load.five, 0.22);
        assert_eq!(load.fifteen, 0.33);
    }

    #[test]
    fn parses_memory_usage() {
        let memory = parse_memory_usage(
            "MemTotal:       1024 kB\nMemAvailable:    256 kB\nSwapTotal:       512 kB\nSwapFree:        128 kB\n",
        )
        .unwrap();
        assert_eq!(memory.total_bytes, 1024 * 1024);
        assert_eq!(memory.used_bytes, 768 * 1024);
        assert_eq!(memory.swap_used_bytes, 384 * 1024);
    }

    #[test]
    fn parses_network_totals_and_rates() {
        let totals = parse_network_totals(
            "Inter-|   Receive                                                |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 200 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n lo: 50 0 0 0 0 0 0 0 50 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        assert_eq!(totals.rx_bytes, 200);
        assert_eq!(totals.tx_bytes, 100);

        let previous = super::NetworkSample {
            observed_at: Instant::now() - Duration::from_secs(2),
            rx_bytes: 100,
            tx_bytes: 40,
        };
        let (rx_rate, tx_rate) = compute_network_rates(previous, Instant::now(), totals);
        assert!(rx_rate.unwrap() > 40.0);
        assert!(tx_rate.unwrap() > 20.0);
    }

    #[test]
    fn test_host_collector_with_mock_files() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-collector-test-{timestamp}"));
        std::fs::create_dir_all(temp_dir.join("proc/sys/kernel")).unwrap();
        std::fs::create_dir_all(temp_dir.join("proc/net")).unwrap();
        std::fs::create_dir_all(temp_dir.join("etc")).unwrap();

        // Write mock files
        std::fs::write(temp_dir.join("proc/uptime"), "3600.50 12345.67\n").unwrap();
        std::fs::write(temp_dir.join("proc/sys/kernel/hostname"), "mock-host\n").unwrap();
        std::fs::write(temp_dir.join("etc/os-release"), "PRETTY_NAME=\"Mock Linux OS\"\n").unwrap();
        std::fs::write(temp_dir.join("proc/sys/kernel/osrelease"), "6.8.0-mock\n").unwrap();
        std::fs::write(
            temp_dir.join("proc/cpuinfo"),
            "processor\t: 0\nmodel name\t: Mock CPU @ 3.0GHz\n\nprocessor\t: 1\nmodel name\t: Mock CPU @ 3.0GHz\n",
        ).unwrap();
        std::fs::write(temp_dir.join("proc/stat"), "cpu  100 0 50 400 10 0 0 0 0 0\ncpu0 50 0 25 200 5 0 0 0 0 0\n").unwrap();
        std::fs::write(
            temp_dir.join("proc/net/dev"),
            "Inter-|   Receive                                                |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 200 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n",
        ).unwrap();
        std::fs::write(temp_dir.join("proc/loadavg"), "0.15 0.30 0.45 1/100 12345\n").unwrap();
        std::fs::write(
            temp_dir.join("proc/meminfo"),
            "MemTotal:       2097152 kB\nMemFree:         524288 kB\nMemAvailable:   1048576 kB\nSwapTotal:      1048576 kB\nSwapFree:        524288 kB\n",
        ).unwrap();
        std::fs::write(
            temp_dir.join("proc/mounts"),
            "/dev/vda1 / ext4 rw,relatime 0 0\ntmpfs /dev/shm tmpfs rw,nosuid,nodev 0 0\n",
        ).unwrap();

        let mut collector = super::HostCollector::new_with_root(temp_dir.clone());
        let config = nodelite_proto::AgentConfig {
            node_id: "test-node".to_string(),
            node_label: "Test Node".to_string(),
            server: "ws://127.0.0.1:8080/ws".to_string(),
            token: "token".to_string(),
            connect_timeout_secs: 5,
            report_interval_secs: 5,
            max_incoming_message_bytes: 65536,
            insecure_transport_warn_interval_secs: 900,
            tags: vec!["mock-tag".to_string()],
            hostname_override: None,
        };

        // Check identity collection
        let identity = collector.collect_identity(&config, "1.0.0").unwrap();
        assert_eq!(identity.node_id, "test-node");
        assert_eq!(identity.hostname, "mock-host");
        assert_eq!(identity.os, "Mock Linux OS");
        assert_eq!(identity.kernel_version, Some("6.8.0-mock".to_string()));
        assert_eq!(identity.cpu_model, Some("Mock CPU @ 3.0GHz".to_string()));
        assert_eq!(identity.cpu_cores, 2);
        assert_eq!(identity.agent_version, "1.0.0");
        assert_eq!(identity.tags, vec!["mock-tag".to_string()]);

        // Check snapshot collection (first collection has None rates)
        let snapshot1 = collector.collect_snapshot().unwrap();
        assert_eq!(snapshot1.uptime_secs, 3600);
        assert_eq!(snapshot1.load.one, 0.15);
        assert_eq!(snapshot1.load.five, 0.30);
        assert_eq!(snapshot1.load.fifteen, 0.45);
        assert_eq!(snapshot1.memory.total_bytes, 2097152 * 1024);
        assert_eq!(snapshot1.memory.available_bytes, 1048576 * 1024);
        assert_eq!(snapshot1.memory.used_bytes, 1048576 * 1024);
        assert_eq!(snapshot1.memory.swap_total_bytes, 1048576 * 1024);
        assert_eq!(snapshot1.memory.swap_used_bytes, 524288 * 1024);

        // Assert network totals
        assert_eq!(snapshot1.network.total_rx_bytes, 200);
        assert_eq!(snapshot1.network.total_tx_bytes, 100);
        assert_eq!(snapshot1.network.rx_bytes_per_sec, None);
        assert_eq!(snapshot1.network.tx_bytes_per_sec, None);

        // Cleanup
        std::fs::remove_dir_all(temp_dir).unwrap();
    }
}
