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

// --- Settings (handlers/settings/types.rs) ---

export interface SettingsAuth {
  enabled: boolean;
  username: string | null;
  two_factor_enabled: boolean;
  totp_secret_configured: boolean;
  session_ttl_secs: number;
  pending_ttl_secs: number;
}

export interface SettingsUpdates {
  latest_release_url: string;
  server_upgrade_command: string;
  agent_upgrade_command: string;
}

export interface SettingsAgentToken {
  node_id: string;
  node_label: string;
  online: boolean;
  agent_version: string | null;
  remote_ip: string | null;
  tags: string[];
  token_expires_at: string | null;
  token_expires_in_secs: number | null;
}

/** GET /api/settings — SettingsResponse (flat + nested) */
export interface SettingsResponse {
  service: string;
  server_version: string;
  repository: string;
  public_base_url: string;
  listen: string;
  config_path: string;
  registry_path: string;
  history_db_path: string;
  snapshot_path: string;
  history_retention_hours: number;
  refresh_interval_secs: number;
  auth: SettingsAuth;
  updates: SettingsUpdates;
  agents: SettingsAgentToken[];
}

/** Standard mutation response for the settings endpoints. */
export interface SettingsActionResponse {
  ok: boolean;
  message: string;
}

/** Reauth carried by every sensitive settings write. */
export interface ReauthPayload {
  current_password?: string;
  code?: string;
}

/** GET /api/settings/2fa/start — TwoFactorSetupResponse */
export interface TwoFactorSetupResponse {
  secret: string;
  otpauth_uri: string;
  qr_svg: string;
}

/** POST /api/settings/2fa/enable */
export interface EnableTwoFactorRequest {
  current_password: string;
  secret: string;
  code: string;
}

/** POST /api/settings/2fa/disable */
export interface DisableTwoFactorRequest {
  current_password: string;
  code: string;
}

/** POST /api/settings/password */
export interface ChangePasswordRequest {
  current_password: string;
  new_password: string;
}

// --- Alerts (handlers/settings/types.rs + nodelite-proto/config/alerts.rs) ---
// All alert enums serialize snake_case (verified in config/alerts.rs).

export type AlertChannel = 'smtp' | 'webhook';
export type AlertSmtpTransport = 'start_tls' | 'tls' | 'plain';
export type AlertMetric =
  | 'cpu_usage_percent'
  | 'memory_usage_percent'
  | 'disk_usage_percent'
  | 'latency_ms'
  | 'offline_minutes';
export type AlertComparator = 'gt' | 'lt';
export type AlertSeverity = 'warning' | 'critical';
export type AlertScopeMode = 'all' | 'node_ids' | 'tags';

export interface AlertSmtpSettingsView {
  enabled: boolean;
  host: string;
  port: number;
  username: string;
  sender: string;
  recipients: string[];
  transport: AlertSmtpTransport;
  send_resolved: boolean;
  /** Read-only: true if a password is stored. The secret is never echoed. */
  password_configured: boolean;
}

export interface AlertWebhookSettingsView {
  enabled: boolean;
  url: string;
  send_resolved: boolean;
  /** Read-only: true if a secret is stored. The secret is never echoed. */
  secret_configured: boolean;
}

export interface AlertRuleView {
  id: string;
  name: string;
  enabled: boolean;
  metric: AlertMetric;
  comparator: AlertComparator;
  threshold: number;
  window_minutes: number;
  severity: AlertSeverity;
  scope_mode: AlertScopeMode;
  node_ids: string[];
  tags: string[];
  delivery: AlertChannel[];
  cooldown_minutes: number;
  send_resolved: boolean;
}

export interface InspectionSettingsView {
  enabled: boolean;
  local_time: string;
  lookback_hours: number;
  delivery: AlertChannel[];
  offline_grace_minutes: number;
  latency_warn_ms: number;
  cpu_warn_percent: number;
  memory_warn_percent: number;
}

export interface AlertSettingsView {
  enabled: boolean;
  smtp: AlertSmtpSettingsView;
  webhook: AlertWebhookSettingsView;
  rules: AlertRuleView[];
  inspection: InspectionSettingsView;
}

export interface TriggeredRulePreview {
  rule_id: string;
  rule_name: string;
  severity: AlertSeverity;
  node_ids: string[];
}

export interface InspectionHighlight {
  node_id: string;
  node_label: string;
  reasons: string[];
}

export interface InspectionPreview {
  total_nodes: number;
  offline_nodes: number;
  latency_nodes: number;
  cpu_hot_nodes: number;
  memory_hot_nodes: number;
  highlights: InspectionHighlight[];
}

export interface AlertPreview {
  generated_at: string;
  triggered_rules: TriggeredRulePreview[];
  inspection: InspectionPreview;
}

/** GET /api/settings/alerts — AlertSettingsResponse */
export interface AlertSettingsResponse {
  config: AlertSettingsView;
  preview: AlertPreview;
}

// Update payloads (POST /api/settings/alerts). Secrets follow the server's
// merge rule: clear_* wipes; else a non-empty value replaces; else keep.

export interface UpdateAlertSmtpSettingsRequest {
  enabled: boolean;
  host: string;
  port: number;
  username: string;
  password?: string;
  clear_password: boolean;
  sender: string;
  recipients: string[];
  transport: AlertSmtpTransport;
  send_resolved: boolean;
}

export interface UpdateAlertWebhookSettingsRequest {
  enabled: boolean;
  url: string;
  secret?: string;
  clear_secret: boolean;
  send_resolved: boolean;
}

export interface UpdateAlertRuleRequest {
  id: string;
  name: string;
  enabled: boolean;
  metric: AlertMetric;
  comparator: AlertComparator;
  threshold: number;
  window_minutes: number;
  severity: AlertSeverity;
  scope_mode: AlertScopeMode;
  node_ids: string[];
  tags: string[];
  delivery: AlertChannel[];
  cooldown_minutes: number;
  send_resolved: boolean;
}

export interface UpdateInspectionSettingsRequest {
  enabled: boolean;
  local_time: string;
  lookback_hours: number;
  delivery: AlertChannel[];
  offline_grace_minutes: number;
  latency_warn_ms: number;
  cpu_warn_percent: number;
  memory_warn_percent: number;
}

/** POST /api/settings/alerts — UpdateAlertSettingsRequest (carries reauth). */
export interface UpdateAlertSettingsRequest {
  current_password?: string;
  code?: string;
  enabled: boolean;
  smtp: UpdateAlertSmtpSettingsRequest;
  webhook: UpdateAlertWebhookSettingsRequest;
  rules: UpdateAlertRuleRequest[];
  inspection: UpdateInspectionSettingsRequest;
}

/** POST /api/nodes/{id}/refresh-token — NodeTokenRefreshResponse */
export interface NodeTokenRefreshResponse {
  ok: boolean;
  message: string;
  token_expires_at: string | null;
  token_expires_in_secs: number | null;
}

/** POST /api/nodes/{id}/refresh-token — request body (carries reauth). */
export interface RefreshNodeTokenRequest {
  current_password?: string;
  code?: string;
}

// --- WebSocket Browser Messages (nodelite-proto/src/message.rs BrowserMessage) ---
// TODO(Stage 6.2): Auto-generate from proto crate instead of hand-writing.

export type BrowserMessage =
  | {
      type: 'initial_state';
      generated_at: string;
      overview: OverviewData;
      nodes: NodeListItem[];
    }
  | {
      type: 'overview_update';
      generated_at: string;
      overview: OverviewData;
    }
  | {
      type: 'node_upsert';
      generated_at: string;
      node: NodeListItem;
    }
  | {
      type: 'node_removed';
      generated_at: string;
      node_id: string;
    }
  | { type: 'ping' }
  | { type: 'pong' };
