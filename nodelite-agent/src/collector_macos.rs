//! macOS 主机指标采集器:优先使用 libc / Mach 暴露的系统接口,
//! 在无法读取某个非关键指标时降级为保守值,避免整次采样直接失败。

use std::collections::HashSet;
use std::ffi::CStr;
use std::fs;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::slice;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use nodelite_proto::{
    AgentConfig, DiskUsage, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
    percentage,
};
use tracing::warn;

use super::shared::{
    CpuSample, NetworkRateBaselines, NetworkSample, NetworkTotals, compute_cpu_usage,
    compute_network_rates,
};

/// 采集器状态:为了计算 CPU/网络的"差分速率",需要保留上一次的采样值。
pub struct HostCollector {
    previous_cpu: Option<CpuSample>,
    previous_network: Option<ObservedNetworkSample>,
    network_interfaces: NetworkInterfaceCache,
    network_rate_baselines: NetworkRateBaselines,
}

#[derive(Debug, Clone)]
struct ObservedNetworkSample {
    sample: NetworkSample,
    signature: NetworkInterfaceSignature,
}

#[derive(Debug, Clone)]
struct NetworkReading {
    totals: NetworkTotals,
    signature: NetworkInterfaceSignature,
}

/// macOS 的完整接口列表来自 `NET_RT_IFLIST2`,返回体会随 VPN/虚拟网卡变化。
/// 稳态下只保留 up/non-loopback 的 index,后续每轮用 `IFMIB_IFDATA` 轻量读取计数。
#[derive(Debug, Default)]
struct NetworkInterfaceCache {
    list_len: Option<usize>,
    indices: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkInterfaceSignature(Vec<String>);

impl NetworkInterfaceSignature {
    fn empty() -> Self {
        Self(Vec::new())
    }

    fn from_indices(indices: &[u16]) -> Self {
        let mut parts = indices
            .iter()
            .map(|index| format!("idx:{index}"))
            .collect::<Vec<_>>();
        parts.sort();
        Self(parts)
    }

    fn from_names(mut names: Vec<String>) -> Self {
        names.sort();
        Self(
            names
                .into_iter()
                .map(|name| format!("name:{name}"))
                .collect(),
        )
    }
}

#[repr(C)]
struct IfMibData {
    ifmd_name: [libc::c_char; libc::IFNAMSIZ],
    ifmd_pcount: libc::c_uint,
    ifmd_flags: libc::c_uint,
    ifmd_snd_len: libc::c_uint,
    ifmd_snd_maxlen: libc::c_uint,
    ifmd_snd_drops: libc::c_uint,
    ifmd_filler: [libc::c_uint; 4],
    ifmd_data: libc::if_data64,
}

const IFDATA_GENERAL: libc::c_int = 1;
const IFMIB_IFDATA: libc::c_int = 2;
const NETLINK_GENERIC: libc::c_int = 0;

pub fn new_collector() -> HostCollector {
    HostCollector {
        previous_cpu: None,
        previous_network: None,
        network_interfaces: NetworkInterfaceCache::default(),
        network_rate_baselines: NetworkRateBaselines::default(),
    }
}

impl HostCollector {
    /// 组装节点身份。个别元数据拿不到时尽量回退,避免影响 Agent 启动。
    pub fn collect_identity(
        &self,
        config: &AgentConfig,
        agent_version: &str,
    ) -> Result<NodeIdentity> {
        let uptime_secs = read_uptime_secs()?;
        let boot_time =
            Utc::now() - Duration::seconds(i64::try_from(uptime_secs).unwrap_or(i64::MAX));

        Ok(NodeIdentity {
            node_id: config.node_id.clone(),
            node_label: config.node_label.clone(),
            hostname: config.hostname_override.clone().unwrap_or(read_hostname()?),
            os: read_os_name().unwrap_or_else(|_| "macOS".to_string()),
            kernel_version: read_kernel_version().ok(),
            cpu_model: read_cpu_model().ok(),
            cpu_cores: count_cpu_cores().unwrap_or(1),
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
        let cpu_sample = collect_cpu_sample()?;
        let cpu_usage_percent = self
            .previous_cpu
            .map(|previous| compute_cpu_usage(previous, cpu_sample));
        self.previous_cpu = Some(cpu_sample);

        let network_reading = match collect_network_totals(&mut self.network_interfaces) {
            Ok(reading) => reading,
            Err(error) => {
                warn!(error = ?error, "failed to collect macOS network counters; using zeros");
                NetworkReading {
                    totals: NetworkTotals {
                        rx_bytes: 0,
                        tx_bytes: 0,
                    },
                    signature: NetworkInterfaceSignature::empty(),
                }
            }
        };
        let observed_at = Instant::now();
        let network_totals = network_reading.totals;
        let network_signature = network_reading.signature;
        let interfaces_changed = self
            .previous_network
            .as_ref()
            .is_some_and(|previous| previous.signature != network_signature);
        let (rx_bytes_per_sec, tx_bytes_per_sec) = if let Some(previous) = &self.previous_network {
            compute_network_rates_if_same_interfaces(
                previous,
                observed_at,
                network_totals,
                &network_signature,
            )
        } else {
            (None, None)
        };
        if interfaces_changed {
            self.network_rate_baselines.clear();
        }
        super::log_network_rate_anomalies(
            self.network_rate_baselines
                .observe(rx_bytes_per_sec, tx_bytes_per_sec),
        );
        self.previous_network = Some(ObservedNetworkSample {
            sample: NetworkSample {
                observed_at,
                rx_bytes: network_totals.rx_bytes,
                tx_bytes: network_totals.tx_bytes,
            },
            signature: network_signature,
        });

        let load = match collect_load_average() {
            Ok(load) => load,
            Err(error) => {
                warn!(error = ?error, "failed to collect macOS load average; using zeros");
                LoadAverage {
                    one: 0.0,
                    five: 0.0,
                    fifteen: 0.0,
                }
            }
        };
        let memory = collect_memory_usage()?;
        let uptime_secs = read_uptime_secs()?;
        let disks = match collect_disks() {
            Ok(disks) => disks,
            Err(error) => {
                warn!(error = ?error, "failed to collect macOS disks; using empty list");
                Vec::new()
            }
        };

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

fn read_hostname() -> Result<String> {
    let uts = read_uname()?;
    c_chars_to_string(&uts.nodename)
}

fn read_kernel_version() -> Result<String> {
    let uts = read_uname()?;
    c_chars_to_string(&uts.release)
}

fn read_uname() -> Result<libc::utsname> {
    let mut uts = unsafe { mem::zeroed::<libc::utsname>() };
    let result = unsafe { libc::uname(&mut uts) };
    if result != 0 {
        return Err(anyhow!("uname failed"));
    }
    Ok(uts)
}

fn c_chars_to_string(value: &[libc::c_char]) -> Result<String> {
    let text = unsafe { CStr::from_ptr(value.as_ptr()) }
        .to_str()
        .context("invalid utf-8 in C string")?;
    Ok(text.to_string())
}

/// 解析系统版本 plist,优先输出 `ProductName ProductVersion`。
fn read_os_name() -> Result<String> {
    let content = fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        .context("read /System/Library/CoreServices/SystemVersion.plist")?;
    let product_name = extract_plist_value(&content, "ProductName");
    let product_version = extract_plist_value(&content, "ProductVersion");

    match (product_name, product_version) {
        (Some(name), Some(version)) => Ok(format!("{name} {version}")),
        (Some(name), None) => Ok(name),
        (None, Some(version)) => Ok(format!("macOS {version}")),
        (None, None) => Err(anyhow!(
            "ProductName/ProductVersion missing from SystemVersion.plist"
        )),
    }
}

fn extract_plist_value(content: &str, key: &str) -> Option<String> {
    let key_marker = format!("<key>{key}</key>");
    let key_index = content.find(&key_marker)?;
    let remainder = &content[key_index + key_marker.len()..];
    let string_start = remainder.find("<string>")?;
    let remainder = &remainder[string_start + "<string>".len()..];
    let string_end = remainder.find("</string>")?;
    Some(remainder[..string_end].trim().to_string())
}

fn read_cpu_model() -> Result<String> {
    read_sysctl_string(b"machdep.cpu.brand_string\0").or_else(|_| read_sysctl_string(b"hw.model\0"))
}

fn read_sysctl_string(name: &[u8]) -> Result<String> {
    let mut size = 0_usize;
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 {
        return Err(anyhow!("sysctlbyname failed"));
    }

    let mut buffer = vec![0_u8; size];
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(anyhow!("sysctlbyname read failed"));
    }
    if matches!(buffer.last(), Some(0)) {
        buffer.pop();
    }
    String::from_utf8(buffer).context("sysctl returned non-utf8 string")
}

fn count_cpu_cores() -> Result<u32> {
    let cores = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if cores <= 0 {
        return Err(anyhow!("_SC_NPROCESSORS_ONLN returned {cores}"));
    }
    Ok(u32::try_from(cores).unwrap_or(u32::MAX).max(1))
}

fn read_uptime_secs() -> Result<u64> {
    for clock_id in [libc::CLOCK_UPTIME_RAW, libc::CLOCK_MONOTONIC_RAW] {
        let mut spec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let result = unsafe { libc::clock_gettime(clock_id, &mut spec) };
        if result == 0 {
            return u64::try_from(spec.tv_sec).context("negative uptime reported by clock_gettime");
        }
    }
    Err(anyhow!("clock_gettime failed for uptime clocks"))
}

/// 使用 `host_processor_info` 汇总所有逻辑核心的累计 tick。
fn collect_cpu_sample() -> Result<CpuSample> {
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    let mut cpu_count: libc::natural_t = 0;
    let mut cpu_info: libc::processor_cpu_load_info_t = ptr::null_mut();
    let mut info_count: libc::mach_msg_type_number_t = 0;

    let status = unsafe {
        libc::host_processor_info(
            host,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            (&mut cpu_info as *mut libc::processor_cpu_load_info_t).cast(),
            &mut info_count,
        )
    };
    if status != libc::KERN_SUCCESS || cpu_count == 0 || cpu_info.is_null() {
        return Err(anyhow!("host_processor_info failed"));
    }

    let samples = unsafe { slice::from_raw_parts(cpu_info, cpu_count as usize) };
    let mut total = 0_u64;
    let mut idle = 0_u64;
    for sample in samples {
        for tick in sample.cpu_ticks {
            total = total.saturating_add(u64::from(tick));
        }
        idle = idle.saturating_add(u64::from(sample.cpu_ticks[libc::CPU_STATE_IDLE as usize]));
    }

    let deallocate_size = mem::size_of::<libc::integer_t>().saturating_mul(info_count as usize);
    unsafe {
        libc::vm_deallocate(
            #[allow(deprecated)]
            libc::mach_task_self(),
            cpu_info as _,
            deallocate_size,
        );
    }

    Ok(CpuSample { total, idle })
}

fn collect_load_average() -> Result<LoadAverage> {
    let mut loads = [0_f64; 3];
    let result = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    if result != 3 {
        return Err(anyhow!("getloadavg returned {result}"));
    }
    Ok(LoadAverage {
        one: loads[0],
        five: loads[1],
        fifteen: loads[2],
    })
}

fn collect_memory_usage() -> Result<MemoryUsage> {
    let total_pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if total_pages <= 0 || page_size <= 0 {
        return Err(anyhow!(
            "sysconf returned invalid memory sizes: pages={total_pages} page_size={page_size}"
        ));
    }

    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let mut stats = unsafe { mem::zeroed::<libc::vm_statistics64>() };
    let status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut stats as *mut libc::vm_statistics64).cast(),
            &mut count,
        )
    };
    if status != libc::KERN_SUCCESS {
        return Err(anyhow!("host_statistics64 failed"));
    }

    let total_bytes = u64::try_from(total_pages)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(page_size).unwrap_or(u64::MAX));
    let available_bytes = compute_available_memory_bytes(
        &stats,
        u64::try_from(page_size).unwrap_or(u64::MAX),
        total_bytes,
    );
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let (swap_total_bytes, swap_used_bytes) = read_swap_usage().unwrap_or((0, 0));

    Ok(MemoryUsage {
        total_bytes,
        used_bytes,
        available_bytes,
        swap_total_bytes,
        swap_used_bytes,
    })
}

/// 近似"可立即回收或可直接分配"的 RAM。
///
/// macOS 的 compressor 页本身并不是空闲页,但把它从 `inactive` 再减一次会把
/// 常见桌面负载下的 available 误压到 0,因此这里采用更保守、也更贴近 `vm_stat`
/// 直觉的估算:`free + inactive + purgeable`。
fn compute_available_memory_bytes(
    stats: &libc::vm_statistics64,
    page_size: u64,
    total_bytes: u64,
) -> u64 {
    u64::from(stats.free_count)
        .saturating_add(u64::from(stats.inactive_count))
        .saturating_add(u64::from(stats.purgeable_count))
        .saturating_mul(page_size)
        .min(total_bytes)
}

fn read_swap_usage() -> Result<(u64, u64)> {
    let mut mib = [libc::CTL_VM, libc::VM_SWAPUSAGE];
    let mut swap = unsafe { mem::zeroed::<libc::xsw_usage>() };
    let mut size = mem::size_of::<libc::xsw_usage>();
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            (&mut swap as *mut libc::xsw_usage).cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(anyhow!("sysctl VM_SWAPUSAGE failed"));
    }
    Ok((swap.xsu_total, swap.xsu_used))
}

fn collect_disks() -> Result<Vec<DiskUsage>> {
    let mut mounts: *mut libc::statfs = ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Err(anyhow!("getmntinfo returned no mounts"));
    }

    let mounts = unsafe { slice::from_raw_parts(mounts, count as usize) };
    let mut seen_mounts = HashSet::new();
    let mut disks = Vec::new();

    for mount in mounts {
        let device = c_chars_to_string(&mount.f_mntfromname)?;
        let mount_point = c_chars_to_string(&mount.f_mntonname)?;
        let fs_type = c_chars_to_string(&mount.f_fstypename)?;
        if ignored_filesystems().contains(&fs_type.as_str())
            || ignored_mount_point(&mount_point)
            || !seen_mounts.insert(mount_point.clone())
        {
            continue;
        }

        let block_size = u64::from(mount.f_bsize);
        let total_bytes = mount.f_blocks.saturating_mul(block_size);
        if total_bytes == 0 {
            continue;
        }
        let available_bytes = mount.f_bavail.saturating_mul(block_size);
        let used_bytes = total_bytes.saturating_sub(available_bytes);

        disks.push(DiskUsage {
            device,
            mount_point,
            fs_type,
            total_bytes,
            available_bytes,
            used_bytes,
            used_percent: percentage(used_bytes, total_bytes),
        });
    }

    disks.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
    Ok(disks)
}

fn ignored_filesystems() -> &'static [&'static str] {
    &["autofs", "devfs", "fdesc", "procfs", "volfs"]
}

fn ignored_mount_point(mount_point: &str) -> bool {
    matches!(
        mount_point,
        "/System/Volumes/Hardware"
            | "/System/Volumes/iSCPreboot"
            | "/System/Volumes/Preboot"
            | "/System/Volumes/Update"
            | "/System/Volumes/xarts"
    )
}

fn collect_network_totals(cache: &mut NetworkInterfaceCache) -> Result<NetworkReading> {
    collect_network_totals_via_sysctl(cache).or_else(|error| {
        warn!(
            error = ?error,
            "failed to read macOS network counters via sysctl; falling back to getifaddrs",
        );
        cache.clear();
        collect_network_totals_via_ifaddrs()
    })
}

fn compute_network_rates_if_same_interfaces(
    previous: &ObservedNetworkSample,
    observed_at: Instant,
    current: NetworkTotals,
    current_signature: &NetworkInterfaceSignature,
) -> (Option<f64>, Option<f64>) {
    if &previous.signature != current_signature {
        return (None, None);
    }
    compute_network_rates(previous.sample, observed_at, current)
}

/// 优先用缓存接口 index + `IFMIB_IFDATA` 读取 64-bit 网卡计数。
/// 接口列表长度变化时再重读完整 `NET_RT_IFLIST2` buffer,刷新缓存。
fn collect_network_totals_via_sysctl(cache: &mut NetworkInterfaceCache) -> Result<NetworkReading> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut len = 0_usize;
    let size_result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            ptr::null_mut(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if size_result != 0 || len == 0 {
        return Err(anyhow!("sysctl NET_RT_IFLIST2 size query failed"));
    }

    if cache.can_sample_cached_indices(len) {
        match collect_cached_network_totals(cache) {
            Ok(totals) => {
                return Ok(NetworkReading {
                    totals,
                    signature: NetworkInterfaceSignature::from_indices(&cache.indices),
                });
            }
            Err(error) => {
                warn!(
                    error = ?error,
                    "failed to read cached macOS interface counters; refreshing interface list",
                );
                cache.clear();
            }
        }
    }

    let (totals, indices) = collect_network_totals_and_indices_via_iflist2(&mut mib, len)?;
    let signature = NetworkInterfaceSignature::from_indices(&indices);
    cache.list_len = Some(len);
    cache.indices = indices;
    Ok(NetworkReading { totals, signature })
}

fn collect_network_totals_and_indices_via_iflist2(
    mib: &mut [libc::c_int; 6],
    mut len: usize,
) -> Result<(NetworkTotals, Vec<u16>)> {
    let mut buffer = vec![0_u8; len];
    let read_result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            buffer.as_mut_ptr().cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if read_result != 0 {
        return Err(anyhow!("sysctl NET_RT_IFLIST2 read failed"));
    }

    let mut seen_indices = HashSet::new();
    let mut indices = Vec::new();
    let mut rx_bytes = 0_u64;
    let mut tx_bytes = 0_u64;
    let mut next = buffer.as_ptr();
    let end = unsafe { next.add(len) };

    while next < end {
        let ifm = next.cast::<libc::if_msghdr>();
        let message_len = unsafe { (*ifm).ifm_msglen as usize };
        if message_len == 0 {
            break;
        }

        if unsafe { (*ifm).ifm_type } == libc::RTM_IFINFO2 as u8 {
            let ifm2 = next.cast::<libc::if_msghdr2>();
            let flags = unsafe { (*ifm2).ifm_flags };
            let index = unsafe { (*ifm2).ifm_index };
            if flags & libc::IFF_LOOPBACK == 0
                && flags & libc::IFF_UP != 0
                && seen_indices.insert(index)
            {
                indices.push(index);
                let data = unsafe { &(*ifm2).ifm_data };
                rx_bytes = rx_bytes.saturating_add(data.ifi_ibytes);
                tx_bytes = tx_bytes.saturating_add(data.ifi_obytes);
            }
        }

        next = unsafe { next.add(message_len) };
    }

    Ok((NetworkTotals { rx_bytes, tx_bytes }, indices))
}

fn collect_cached_network_totals(cache: &NetworkInterfaceCache) -> Result<NetworkTotals> {
    let mut rx_bytes = 0_u64;
    let mut tx_bytes = 0_u64;
    for index in &cache.indices {
        let data = collect_interface_data(*index)?;
        if data.ifmd_flags & libc::IFF_LOOPBACK as libc::c_uint != 0
            || data.ifmd_flags & libc::IFF_UP as libc::c_uint == 0
        {
            return Err(anyhow!("cached interface {index} changed flags"));
        }
        rx_bytes = rx_bytes.saturating_add(data.ifmd_data.ifi_ibytes);
        tx_bytes = tx_bytes.saturating_add(data.ifmd_data.ifi_obytes);
    }
    Ok(NetworkTotals { rx_bytes, tx_bytes })
}

fn collect_interface_data(index: u16) -> Result<IfMibData> {
    let mut mib_data = [
        libc::CTL_NET,
        libc::PF_LINK,
        NETLINK_GENERIC,
        IFMIB_IFDATA,
        index as libc::c_int,
        IFDATA_GENERAL,
    ];
    let mut if_data = MaybeUninit::<IfMibData>::uninit();
    let mut size = mem::size_of::<IfMibData>();
    let result = unsafe {
        libc::sysctl(
            mib_data.as_mut_ptr(),
            mib_data.len() as _,
            if_data.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };

    if result != 0 || size < mem::size_of::<IfMibData>() {
        return Err(anyhow!("sysctl IFMIB_IFDATA failed for interface {index}"));
    }
    Ok(unsafe { if_data.assume_init() })
}

impl NetworkInterfaceCache {
    fn can_sample_cached_indices(&self, current_list_len: usize) -> bool {
        self.list_len == Some(current_list_len) && !self.indices.is_empty()
    }

    fn clear(&mut self) {
        self.list_len = None;
        self.indices.clear();
    }
}

fn collect_network_totals_via_ifaddrs() -> Result<NetworkReading> {
    let mut addrs: *mut libc::ifaddrs = ptr::null_mut();
    let result = unsafe { libc::getifaddrs(&mut addrs) };
    if result != 0 || addrs.is_null() {
        return Err(anyhow!("getifaddrs failed"));
    }
    let _guard = IfAddrsGuard(addrs);

    let mut seen_names = HashSet::new();
    let mut sampled_names = Vec::new();
    let mut rx_bytes = 0_u64;
    let mut tx_bytes = 0_u64;
    let mut current = addrs;
    while !current.is_null() {
        let iface = unsafe { &*current };
        if !iface.ifa_addr.is_null()
            && unsafe { (*iface.ifa_addr).sa_family as i32 } == libc::AF_LINK
            && iface.ifa_flags & libc::IFF_LOOPBACK as u32 == 0
            && iface.ifa_flags & libc::IFF_UP as u32 != 0
            && !iface.ifa_data.is_null()
        {
            let name = unsafe { CStr::from_ptr(iface.ifa_name) }
                .to_string_lossy()
                .into_owned();
            if seen_names.insert(name.clone()) {
                sampled_names.push(name);
                let data = unsafe { &*(iface.ifa_data as *const libc::if_data) };
                rx_bytes = rx_bytes.saturating_add(u64::from(data.ifi_ibytes));
                tx_bytes = tx_bytes.saturating_add(u64::from(data.ifi_obytes));
            }
        }
        current = iface.ifa_next;
    }

    Ok(NetworkReading {
        totals: NetworkTotals { rx_bytes, tx_bytes },
        signature: NetworkInterfaceSignature::from_names(sampled_names),
    })
}

struct IfAddrsGuard(*mut libc::ifaddrs);

impl Drop for IfAddrsGuard {
    fn drop(&mut self) {
        unsafe {
            libc::freeifaddrs(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NetworkInterfaceCache, NetworkInterfaceSignature, ObservedNetworkSample,
        compute_available_memory_bytes, compute_network_rates,
        compute_network_rates_if_same_interfaces, extract_plist_value,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn extracts_plist_string_value() {
        let content = r#"
            <plist version="1.0">
              <dict>
                <key>ProductName</key>
                <string>macOS</string>
                <key>ProductVersion</key>
                <string>15.5</string>
              </dict>
            </plist>
        "#;
        assert_eq!(
            extract_plist_value(content, "ProductName").as_deref(),
            Some("macOS")
        );
        assert_eq!(
            extract_plist_value(content, "ProductVersion").as_deref(),
            Some("15.5")
        );
    }

    #[test]
    fn computes_network_rates_from_deltas() {
        let previous = super::NetworkSample {
            observed_at: Instant::now() - Duration::from_secs(2),
            rx_bytes: 100,
            tx_bytes: 40,
        };
        let current = super::NetworkTotals {
            rx_bytes: 220,
            tx_bytes: 100,
        };
        let (rx_rate, tx_rate) = compute_network_rates(previous, Instant::now(), current);
        assert!(rx_rate.unwrap() > 50.0);
        assert!(tx_rate.unwrap() > 20.0);
    }

    #[test]
    fn skips_network_rates_when_interface_signature_changes() {
        let previous = ObservedNetworkSample {
            sample: super::NetworkSample {
                observed_at: Instant::now() - Duration::from_secs(2),
                rx_bytes: 100,
                tx_bytes: 40,
            },
            signature: NetworkInterfaceSignature::from_indices(&[4]),
        };
        let current = super::NetworkTotals {
            rx_bytes: 10_000_000_000,
            tx_bytes: 4_000_000_000,
        };

        let (rx_rate, tx_rate) = compute_network_rates_if_same_interfaces(
            &previous,
            Instant::now(),
            current,
            &NetworkInterfaceSignature::from_indices(&[4, 5]),
        );

        assert_eq!(rx_rate, None);
        assert_eq!(tx_rate, None);
    }

    #[test]
    fn network_interface_signature_order_is_stable() {
        assert_eq!(
            NetworkInterfaceSignature::from_indices(&[5, 4]),
            NetworkInterfaceSignature::from_indices(&[4, 5]),
        );
        assert_ne!(
            NetworkInterfaceSignature::from_indices(&[4]),
            NetworkInterfaceSignature::from_indices(&[4, 5]),
        );
    }

    #[test]
    fn network_interface_cache_only_matches_same_non_empty_list() {
        let mut cache = NetworkInterfaceCache {
            list_len: Some(4096),
            indices: vec![4, 5],
        };

        assert!(cache.can_sample_cached_indices(4096));
        assert!(!cache.can_sample_cached_indices(4097));

        cache.clear();
        assert!(!cache.can_sample_cached_indices(4096));
        assert!(cache.indices.is_empty());
    }

    #[test]
    fn available_memory_does_not_underflow_when_compressor_is_large() {
        let mut stats = unsafe { std::mem::zeroed::<libc::vm_statistics64>() };
        stats.free_count = 5_431;
        stats.inactive_count = 520_105;
        stats.purgeable_count = 18_475;
        stats.compressor_page_count = 786_007;

        let available = compute_available_memory_bytes(&stats, 16_384, 34_359_738_368);
        assert!(available > 0);
        assert!(available < 34_359_738_368);
    }
}
