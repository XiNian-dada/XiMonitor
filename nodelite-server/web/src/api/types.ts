/**
 * Hand-written TS mirrors of the server's JSON response shapes.
 *
 * Sourced by reading the Rust handlers + nodelite-proto structs (type
 * generation from the proto crate is deferred — plan §6.2). Field names
 * match the wire format exactly (no serde renames on these response
 * bodies). `T | null` marks `Option<T>` fields that are nullable in JSON.
 */

/** GET /api/bootstrap — handlers/api.rs BootstrapResponse */
export interface BootstrapResponse {
  service: string;
  status: string;
  ready: boolean;
  history_available: boolean;
  public_base_url: string;
  refresh_interval_secs: number;
  registered_nodes: number;
}

/** GET /api/overview — nodelite-proto OverviewData */
export interface OverviewData {
  generated_at: string;
  total_nodes: number;
  online_nodes: number;
  offline_nodes: number;
  total_rx_bytes: number;
  total_tx_bytes: number;
  current_rx_bytes_per_sec: number;
  current_tx_bytes_per_sec: number;
  average_latency_ms: number | null;
}

export interface NodeListIdentity {
  node_id: string;
  node_label: string;
  hostname: string;
  tags: string[];
}

export interface NodeListSnapshot {
  cpu_usage_percent: number | null;
  load: { one: number };
  memory: { total_bytes: number; used_bytes: number };
}

/** GET /api/nodes — array of nodelite-proto NodeListItem (lightweight list shape) */
export interface NodeListItem {
  identity: NodeListIdentity;
  snapshot: NodeListSnapshot | null;
  latency_ms: number | null;
  online: boolean;
}

/** GET /api/nodes/{id}/history — array of nodelite-proto HistoryPoint */
export interface HistoryPoint {
  node_id: string;
  recorded_at: string;
  cpu_usage_percent: number | null;
  memory_used_percent: number;
  rx_bytes_per_sec: number | null;
  tx_bytes_per_sec: number | null;
  latency_ms: number | null;
  disk_used_percent: number | null;
}

export interface HistoryQuery {
  windowHours?: number;
  maxPoints?: number;
  /** Unix seconds; start + end must be supplied together for a range query. */
  start?: number;
  end?: number;
}

/** Full per-node identity — GET /api/nodes/{id}, nodelite-proto NodeIdentity */
export interface NodeIdentity {
  node_id: string;
  node_label: string;
  hostname: string;
  os: string;
  kernel_version: string | null;
  cpu_model: string | null;
  cpu_cores: number;
  agent_version: string;
  boot_time: string | null;
  tags: string[];
}

export interface LoadAverage {
  one: number;
  five: number;
  fifteen: number;
}

export interface MemoryUsage {
  total_bytes: number;
  used_bytes: number;
  available_bytes: number;
  swap_total_bytes: number;
  swap_used_bytes: number;
}

export interface DiskUsage {
  device: string;
  mount_point: string;
  fs_type: string;
  total_bytes: number;
  available_bytes: number;
  used_bytes: number;
  used_percent: number;
}

export interface NetworkCounters {
  total_rx_bytes: number;
  total_tx_bytes: number;
  rx_bytes_per_sec: number | null;
  tx_bytes_per_sec: number | null;
}

export interface NodeSnapshot {
  collected_at: string;
  cpu_usage_percent: number | null;
  load: LoadAverage;
  memory: MemoryUsage;
  uptime_secs: number;
  disks: DiskUsage[];
  network: NetworkCounters;
}

/** GET /api/nodes/{id} — full nodelite-proto NodeStatus */
export interface NodeStatus {
  identity: NodeIdentity;
  remote_ip: string | null;
  snapshot: NodeSnapshot | null;
  last_seen: string | null;
  latency_ms: number | null;
  online: boolean;
}

export type LogLevel = 'info' | 'warn' | 'error';

/** GET /api/nodes/{id}/logs — nodelite-proto AgentLogEntry */
export interface AgentLogEntry {
  occurred_at: string;
  level: LogLevel;
  message: string;
}
