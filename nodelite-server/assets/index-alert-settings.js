export function createAlertSettingsPanel(deps) {
  const {
    t,
    escapeHtml,
    fetchJson,
    postSettingsJson,
  } = deps;

  let latestAlerts = null;
  let alertsDraft = emptyAlertsConfig();
  let alertMessage = null;

  function alertsRoot() {
    return document.getElementById("alerts-root");
  }

  function emptyAlertsConfig() {
    return {
      enabled: false,
      smtp: {
        enabled: false,
        host: "",
        port: 587,
        username: "",
        sender: "",
        recipients: [],
        transport: "start_tls",
        password_configured: false,
      },
      webhook: {
        enabled: false,
        url: "",
        send_resolved: true,
        secret_configured: false,
      },
      rules: [],
      inspection: {
        enabled: false,
        local_time: "09:00",
        lookback_hours: 24,
        delivery: ["smtp"],
        offline_grace_minutes: 10,
        latency_warn_ms: 250,
        cpu_warn_percent: 85,
        memory_warn_percent: 90,
      },
    };
  }

  function deepClone(value) {
    return JSON.parse(JSON.stringify(value));
  }

  async function loadAlertSettings() {
    const root = alertsRoot();
    if (!root) return;
    root.innerHTML = `<div class="empty">${escapeHtml(t("alerts.loading"))}</div>`;
    try {
      latestAlerts = await fetchJson("/api/settings/alerts");
      alertsDraft = normalizeAlertConfig(latestAlerts.config || emptyAlertsConfig());
      alertMessage = null;
      renderAlertSettings();
    } catch (error) {
      root.innerHTML = `<div class="empty">${escapeHtml(t("alerts.load_failed", { error: error.message }))}</div>`;
    }
  }

  function applyChrome(activeTab) {
    if (activeTab === "alerts" && latestAlerts) {
      renderAlertSettings();
    }
  }

  function normalizeAlertConfig(config) {
    const base = emptyAlertsConfig();
    const merged = {
      ...base,
      ...deepClone(config),
      smtp: { ...base.smtp, ...(config.smtp || {}) },
      webhook: { ...base.webhook, ...(config.webhook || {}) },
      inspection: { ...base.inspection, ...(config.inspection || {}) },
    };
    merged.rules = Array.isArray(config.rules) ? config.rules.map((rule, index) => ({ ...blankRule(index), ...rule })) : [];
    merged.smtp.recipients = Array.isArray(merged.smtp.recipients) ? merged.smtp.recipients : [];
    merged.inspection.delivery = Array.isArray(merged.inspection.delivery) ? merged.inspection.delivery : [];
    return merged;
  }

  function renderAlertSettings() {
    const root = alertsRoot();
    if (!root) return;
    const config = alertsDraft || emptyAlertsConfig();
    const preview = latestAlerts?.preview || null;
    root.innerHTML = `
      <article class="settings-card alerts-overview-card">
        <div class="alerts-overview-main">
          <div class="alerts-title-row">
            <h2>${escapeHtml(t("alerts.overview.title"))}</h2>
            ${statusPill(config.enabled)}
          </div>
          <p class="settings-note">${escapeHtml(t("alerts.overview.note"))}</p>
          <div class="alerts-summary-strip">
            ${summaryTile(t("alerts.summary.channels"), enabledChannelSummary(config))}
            ${summaryTile(t("alerts.summary.rules"), ruleSummary(config.rules))}
            ${summaryTile(t("alerts.summary.inspection"), inspectionSummary(config.inspection))}
          </div>
        </div>
        <div class="alerts-toolbar">
          <label class="settings-checkbox">
            <input type="checkbox" id="alerts-enabled" ${config.enabled ? "checked" : ""}>
            <span>${escapeHtml(t("alerts.overview.enabled"))}</span>
          </label>
          <label>${escapeHtml(t("settings.password.current"))}<input class="settings-input" type="password" id="alerts-current-password" autocomplete="current-password"></label>
          <label>${escapeHtml(t("settings.security.verification_code"))}<input class="settings-input" type="text" id="alerts-code" inputmode="numeric" pattern="[0-9]{6}" maxlength="6" autocomplete="one-time-code"></label>
          <button type="button" class="settings-button primary" id="alerts-save">${escapeHtml(t("alerts.save"))}</button>
        </div>
        ${alertMessageMarkup()}
      </article>

      <section class="alerts-column alerts-primary-column">
        ${smtpCard(config.smtp)}
        ${webhookCard(config.webhook)}
      </section>

      <section class="alerts-column alerts-secondary-column">
        ${previewCard(preview)}
        ${inspectionCard(config.inspection)}
      </section>

      <article class="settings-card settings-card-wide alerts-rules-card">
        <div class="section-head">
          <div>
            <h2>${escapeHtml(t("alerts.rules.title"))}</h2>
            <p class="settings-note">${escapeHtml(t("alerts.rules.note"))}</p>
          </div>
          <button type="button" class="settings-button" id="alerts-add-rule">${escapeHtml(t("alerts.rules.add"))}</button>
        </div>
        <div id="alerts-rules-list" class="rule-list">
          ${config.rules.length ? config.rules.map(alertRuleCard).join("") : `<div class="empty compact">${escapeHtml(t("alerts.rules.empty"))}</div>`}
        </div>
      </article>
    `;
    bindAlertActions();
  }

  function smtpCard(config) {
    return `<article class="settings-card alert-config-card">
      ${cardHead(t("alerts.smtp.title"), config.enabled, smtpSummary(config))}
      <form class="settings-form alert-compact-form" id="alerts-smtp-form">
        <div class="alert-card-controls">
          <label class="settings-checkbox"><input type="checkbox" name="enabled" ${config.enabled ? "checked" : ""}><span>${escapeHtml(t("alerts.smtp.enabled"))}</span></label>
        </div>
        ${summaryKv([
          [t("alerts.smtp.host"), config.host || t("common.not_available")],
          [t("alerts.smtp.sender"), config.sender || t("common.not_available")],
          [t("alerts.smtp.recipients"), config.recipients.length ? config.recipients.join(", ") : t("common.not_available")],
        ])}
        <details class="settings-details">
          <summary>${escapeHtml(t("alerts.details"))}</summary>
          <div class="settings-form">
            <div class="settings-split">
              <label>${escapeHtml(t("alerts.smtp.host"))}<input class="settings-input" name="host" value="${escapeHtml(config.host)}"></label>
              <label>${escapeHtml(t("alerts.smtp.sender"))}<input class="settings-input" name="sender" value="${escapeHtml(config.sender)}"></label>
            </div>
            <label>${escapeHtml(t("alerts.smtp.recipients"))}<input class="settings-input" name="recipients" value="${escapeHtml(config.recipients.join(", "))}"></label>
            <div class="settings-split">
              <label>${escapeHtml(t("alerts.smtp.port"))}<input class="settings-input" type="number" min="1" max="65535" name="port" value="${escapeHtml(config.port)}"></label>
              <label>${escapeHtml(t("alerts.smtp.transport"))}<select class="settings-input" name="transport">${transportOptions(config.transport)}</select></label>
            </div>
            <label>${escapeHtml(t("alerts.smtp.username"))}<input class="settings-input" name="username" value="${escapeHtml(config.username)}"></label>
            <label>${escapeHtml(t("alerts.smtp.password"))}<input class="settings-input" type="password" name="password" placeholder="${config.password_configured ? escapeHtml(t("alerts.secret.keep")) : ""}"></label>
            <label class="settings-checkbox"><input type="checkbox" name="clear_password"><span>${escapeHtml(t("alerts.secret.clear"))}</span></label>
          </div>
        </details>
      </form>
    </article>`;
  }

  function webhookCard(config) {
    return `<article class="settings-card alert-config-card">
      ${cardHead(t("alerts.webhook.title"), config.enabled, webhookSummary(config))}
      <form class="settings-form alert-compact-form" id="alerts-webhook-form">
        <div class="alert-card-controls">
          <label class="settings-checkbox"><input type="checkbox" name="enabled" ${config.enabled ? "checked" : ""}><span>${escapeHtml(t("alerts.webhook.enabled"))}</span></label>
        </div>
        ${summaryKv([
          [t("alerts.webhook.url"), config.url || t("common.not_available")],
          [t("alerts.webhook.send_resolved"), statusText(config.send_resolved)],
        ])}
        <details class="settings-details">
          <summary>${escapeHtml(t("alerts.details"))}</summary>
          <div class="settings-form">
            <label>${escapeHtml(t("alerts.webhook.url"))}<input class="settings-input" name="url" value="${escapeHtml(config.url)}"></label>
            <label>${escapeHtml(t("alerts.webhook.secret"))}<input class="settings-input" type="password" name="secret" placeholder="${config.secret_configured ? escapeHtml(t("alerts.secret.keep")) : ""}"></label>
            <label class="settings-checkbox"><input type="checkbox" name="clear_secret"><span>${escapeHtml(t("alerts.secret.clear"))}</span></label>
            <label class="settings-checkbox"><input type="checkbox" name="send_resolved" ${config.send_resolved ? "checked" : ""}><span>${escapeHtml(t("alerts.webhook.send_resolved"))}</span></label>
          </div>
        </details>
      </form>
    </article>`;
  }

  function inspectionCard(config) {
    return `<article class="settings-card alert-config-card">
      ${cardHead(t("alerts.inspection.title"), config.enabled, inspectionCardSummary(config))}
      <form class="settings-form alert-compact-form" id="alerts-inspection-form">
        <div class="alert-card-controls">
          <label class="settings-checkbox"><input type="checkbox" name="enabled" ${config.enabled ? "checked" : ""}><span>${escapeHtml(t("alerts.inspection.enabled"))}</span></label>
        </div>
        ${summaryKv([
          [t("alerts.inspection.local_time"), config.local_time || "09:00"],
          [t("alerts.inspection.lookback_hours"), `${config.lookback_hours || 24}h`],
          [t("alerts.inspection.delivery"), channelList(config.delivery)],
        ])}
        <details class="settings-details">
          <summary>${escapeHtml(t("alerts.details"))}</summary>
          <div class="settings-form">
            <div class="settings-split">
              <label>${escapeHtml(t("alerts.inspection.local_time"))}<input class="settings-input" name="local_time" value="${escapeHtml(config.local_time)}" placeholder="09:00"></label>
              <label>${escapeHtml(t("alerts.inspection.lookback_hours"))}<input class="settings-input" type="number" min="1" max="720" name="lookback_hours" value="${escapeHtml(config.lookback_hours)}"></label>
            </div>
            <div>
              <div class="settings-label">${escapeHtml(t("alerts.inspection.delivery"))}</div>
              <div class="settings-chip-row">${deliveryCheckboxes(config.delivery, "inspection-delivery")}</div>
            </div>
            <div class="settings-split">
              <label>${escapeHtml(t("alerts.inspection.offline_grace_minutes"))}<input class="settings-input" type="number" min="1" name="offline_grace_minutes" value="${escapeHtml(config.offline_grace_minutes)}"></label>
              <label>${escapeHtml(t("alerts.inspection.latency_warn_ms"))}<input class="settings-input" type="number" min="1" name="latency_warn_ms" value="${escapeHtml(config.latency_warn_ms)}"></label>
            </div>
            <div class="settings-split">
              <label>${escapeHtml(t("alerts.inspection.cpu_warn_percent"))}<input class="settings-input" type="number" min="1" max="100" name="cpu_warn_percent" value="${escapeHtml(config.cpu_warn_percent)}"></label>
              <label>${escapeHtml(t("alerts.inspection.memory_warn_percent"))}<input class="settings-input" type="number" min="1" max="100" name="memory_warn_percent" value="${escapeHtml(config.memory_warn_percent)}"></label>
            </div>
          </div>
        </details>
      </form>
    </article>`;
  }

  function previewCard(preview) {
    return `<article class="settings-card alert-preview-card">
      <h2>${escapeHtml(t("alerts.preview.title"))}</h2>
      ${alertPreviewMarkup(preview)}
    </article>`;
  }

  function cardHead(title, enabled, summary) {
    return `<div class="alert-card-head">
      <div>
        <h2>${escapeHtml(title)}</h2>
        <p class="settings-note">${escapeHtml(summary)}</p>
      </div>
      ${statusPill(enabled)}
    </div>`;
  }

  function statusPill(enabled) {
    const label = enabled ? t("settings.enabled") : t("settings.disabled");
    return `<span class="status-pill ${enabled ? "ok" : ""}">${escapeHtml(label)}</span>`;
  }

  function statusText(enabled) {
    return enabled ? t("settings.enabled") : t("settings.disabled");
  }

  function summaryTile(label, value) {
    return `<div class="alerts-summary-tile"><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></div>`;
  }

  function summaryKv(entries) {
    return `<div class="settings-kv alert-summary-kv">${entries.map(([label, value]) => kv(label, value)).join("")}</div>`;
  }

  function enabledChannelSummary(config) {
    const enabled = [
      config.smtp.enabled ? t("alerts.channel.smtp") : "",
      config.webhook.enabled ? t("alerts.channel.webhook") : "",
    ].filter(Boolean);
    return enabled.length ? enabled.join(" + ") : t("settings.disabled");
  }

  function ruleSummary(rules) {
    const total = rules.length;
    const enabled = rules.filter((rule) => rule.enabled).length;
    return total ? `${enabled}/${total}` : t("alerts.rules.empty_short");
  }

  function inspectionSummary(config) {
    if (!config.enabled) return t("settings.disabled");
    const delivery = channelList(config.delivery);
    return `${config.local_time || "09:00"} · ${config.lookback_hours || 24}h · ${delivery}`;
  }

  function inspectionCardSummary(config) {
    const delivery = channelList(config.delivery);
    return `${config.local_time || "09:00"} · ${config.lookback_hours || 24}h · ${delivery}`;
  }

  function smtpSummary(config) {
    const recipients = config.recipients.length ? config.recipients.join(", ") : t("common.not_available");
    return `${config.host || t("common.not_available")} · ${recipients}`;
  }

  function webhookSummary(config) {
    const url = config.url || t("common.not_available");
    return `${url} · ${t("alerts.webhook.send_resolved")}: ${statusText(config.send_resolved)}`;
  }

  function channelList(channels) {
    const values = Array.isArray(channels) ? channels : [];
    return values.length
      ? values.map((channel) => t(`alerts.channel.${channel}`)).join(" + ")
      : t("common.not_available");
  }

  function transportOptions(selected) {
    return [
      ["start_tls", t("alerts.smtp.transport.start_tls")],
      ["tls", t("alerts.smtp.transport.tls")],
      ["plain", t("alerts.smtp.transport.plain")],
    ].map(([value, label]) => `<option value="${escapeHtml(value)}" ${selected === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("");
  }

  function deliveryCheckboxes(selected, name) {
    const values = Array.isArray(selected) ? selected : [];
    return [
      ["smtp", t("alerts.channel.smtp")],
      ["webhook", t("alerts.channel.webhook")],
    ].map(([value, label]) => `
      <label class="settings-checkbox">
        <input type="checkbox" name="${escapeHtml(name)}" value="${escapeHtml(value)}" ${values.includes(value) ? "checked" : ""}>
        <span>${escapeHtml(label)}</span>
      </label>
    `).join("");
  }

  function alertRuleCard(rule, index) {
    const title = rule.name || `${t("alerts.rules.title")} ${index + 1}`;
    return `<section class="alert-rule-card" data-rule-index="${index}">
      <div class="alert-rule-summary">
        <div>
          <div class="alert-rule-title">${escapeHtml(title)}</div>
          <div class="alert-rule-expression">${escapeHtml(ruleExpression(rule))}</div>
        </div>
        <div class="alert-rule-actions">
          ${statusPill(rule.enabled)}
          <button type="button" class="settings-button danger alerts-remove-rule" data-rule-index="${index}">${escapeHtml(t("alerts.rules.remove"))}</button>
        </div>
      </div>
      <details class="settings-details alert-rule-details">
        <summary>${escapeHtml(t("alerts.rules.details"))}</summary>
        <div class="rule-grid">
          <label>${escapeHtml(t("alerts.rules.id"))}<input class="settings-input" name="id" value="${escapeHtml(rule.id || `rule-${index + 1}`)}"></label>
          <label>${escapeHtml(t("alerts.rules.name"))}<input class="settings-input" name="name" value="${escapeHtml(rule.name || "")}"></label>
          <label>${escapeHtml(t("alerts.rules.metric"))}<select class="settings-input" name="metric">${metricOptions(rule.metric)}</select></label>
          <label>${escapeHtml(t("alerts.rules.comparator"))}<select class="settings-input" name="comparator">${comparatorOptions(rule.comparator)}</select></label>
          <label>${escapeHtml(t("alerts.rules.threshold"))}<input class="settings-input" type="number" min="0" name="threshold" value="${escapeHtml(rule.threshold ?? 0)}"></label>
          <label>${escapeHtml(t("alerts.rules.window_minutes"))}<input class="settings-input" type="number" min="1" name="window_minutes" value="${escapeHtml(rule.window_minutes ?? 5)}"></label>
          <label>${escapeHtml(t("alerts.rules.cooldown_minutes"))}<input class="settings-input" type="number" min="1" name="cooldown_minutes" value="${escapeHtml(rule.cooldown_minutes ?? 30)}"></label>
          <label>${escapeHtml(t("alerts.rules.severity"))}<select class="settings-input" name="severity">${severityOptions(rule.severity)}</select></label>
          <label>${escapeHtml(t("alerts.rules.scope_mode"))}<select class="settings-input" name="scope_mode">${scopeOptions(rule.scope_mode)}</select></label>
          <label>${escapeHtml(t("alerts.rules.node_ids"))}<input class="settings-input" name="node_ids" value="${escapeHtml((rule.node_ids || []).join(", "))}"></label>
          <label>${escapeHtml(t("alerts.rules.tags"))}<input class="settings-input" name="tags" value="${escapeHtml((rule.tags || []).join(", "))}"></label>
        </div>
        <div>
          <div class="settings-label">${escapeHtml(t("alerts.inspection.delivery"))}</div>
          <div class="settings-chip-row">${deliveryCheckboxes(rule.delivery || [], `rule-delivery-${index}`)}</div>
        </div>
        <div class="settings-chip-row">
          <label class="settings-checkbox"><input type="checkbox" name="enabled" ${rule.enabled ? "checked" : ""}><span>${escapeHtml(t("alerts.rules.enabled"))}</span></label>
          <label class="settings-checkbox"><input type="checkbox" name="send_resolved" ${rule.send_resolved ? "checked" : ""}><span>${escapeHtml(t("alerts.rules.send_resolved"))}</span></label>
        </div>
      </details>
    </section>`;
  }

  function ruleExpression(rule) {
    const scope = scopeLabel(rule);
    const delivery = channelList(rule.delivery || []);
    return `${metricLabel(rule.metric)} ${comparatorLabel(rule.comparator)} ${rule.threshold ?? 0} · ${rule.window_minutes ?? 5}m · ${scope} · ${delivery}`;
  }

  function scopeLabel(rule) {
    if (rule.scope_mode === "node_ids" && rule.node_ids?.length) return rule.node_ids.join(", ");
    if (rule.scope_mode === "tags" && rule.tags?.length) return rule.tags.join(", ");
    return t(`alerts.scope.${rule.scope_mode || "all"}`);
  }

  function metricLabel(value) {
    const labels = {
      cpu_usage_percent: t("alerts.metric.cpu"),
      memory_usage_percent: t("alerts.metric.memory"),
      disk_usage_percent: t("alerts.metric.disk"),
      latency_ms: t("alerts.metric.latency"),
      offline_minutes: t("alerts.metric.offline"),
    };
    return labels[value] || value || t("common.not_available");
  }

  function comparatorLabel(value) {
    return value === "lt" ? t("alerts.comparator.lt") : t("alerts.comparator.gt");
  }

  function metricOptions(selected) {
    return [
      ["cpu_usage_percent", t("alerts.metric.cpu")],
      ["memory_usage_percent", t("alerts.metric.memory")],
      ["disk_usage_percent", t("alerts.metric.disk")],
      ["latency_ms", t("alerts.metric.latency")],
      ["offline_minutes", t("alerts.metric.offline")],
    ].map(([value, label]) => `<option value="${escapeHtml(value)}" ${selected === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("");
  }

  function comparatorOptions(selected) {
    return [
      ["gt", t("alerts.comparator.gt")],
      ["lt", t("alerts.comparator.lt")],
    ].map(([value, label]) => `<option value="${escapeHtml(value)}" ${selected === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("");
  }

  function severityOptions(selected) {
    return [
      ["warning", t("alerts.severity.warning")],
      ["critical", t("alerts.severity.critical")],
    ].map(([value, label]) => `<option value="${escapeHtml(value)}" ${selected === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("");
  }

  function scopeOptions(selected) {
    return [
      ["all", t("alerts.scope.all")],
      ["node_ids", t("alerts.scope.node_ids")],
      ["tags", t("alerts.scope.tags")],
    ].map(([value, label]) => `<option value="${escapeHtml(value)}" ${selected === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("");
  }

  function alertPreviewMarkup(preview) {
    if (!preview) {
      return `<div class="empty compact">${escapeHtml(t("alerts.preview.empty"))}</div>`;
    }
    const triggered = preview.triggered_rules.length
      ? `<ul class="preview-list">${preview.triggered_rules.map((rule) => `<li><strong>${escapeHtml(rule.rule_name)}</strong><span>${escapeHtml(rule.node_ids.join(", "))}</span></li>`).join("")}</ul>`
      : `<div class="empty compact">${escapeHtml(t("alerts.preview.no_triggered_rules"))}</div>`;
    const highlights = preview.inspection.highlights.length
      ? `<ul class="preview-list">${preview.inspection.highlights.map((item) => `<li><strong>${escapeHtml(item.node_label || item.node_id)}</strong><span>${escapeHtml(item.reasons.join(", "))}</span></li>`).join("")}</ul>`
      : `<div class="empty compact">${escapeHtml(t("alerts.preview.no_highlights"))}</div>`;
    return `
      <div class="preview-grid alerts-preview-grid">
        <div class="alerts-summary-strip alerts-preview-strip">
          ${summaryTile(t("alerts.preview.total_nodes"), preview.inspection.total_nodes)}
          ${summaryTile(t("alerts.preview.offline_nodes"), preview.inspection.offline_nodes)}
          ${summaryTile(t("alerts.preview.latency_nodes"), preview.inspection.latency_nodes)}
          ${summaryTile(t("alerts.preview.cpu_hot_nodes"), preview.inspection.cpu_hot_nodes)}
          ${summaryTile(t("alerts.preview.memory_hot_nodes"), preview.inspection.memory_hot_nodes)}
        </div>
        <div>
          <div class="settings-label">${escapeHtml(t("alerts.preview.triggered_rules"))}</div>
          ${triggered}
        </div>
        <div>
          <div class="settings-label">${escapeHtml(t("alerts.preview.highlights"))}</div>
          ${highlights}
        </div>
      </div>
    `;
  }

  function kv(label, value) {
    return `<div><span>${escapeHtml(label)}</span><span>${escapeHtml(value ?? t("common.not_available"))}</span></div>`;
  }

  function alertMessageMarkup() {
    if (!alertMessage) {
      return `<div id="alerts-message" class="settings-message"></div>`;
    }
    return `<div id="alerts-message" class="settings-message ${escapeHtml(alertMessage.type)}">${escapeHtml(alertMessage.text)}</div>`;
  }

  function bindAlertActions() {
    document.getElementById("alerts-add-rule")?.addEventListener("click", () => {
      syncAlertDraftFromDom();
      alertsDraft.rules.push(blankRule(alertsDraft.rules.length));
      alertMessage = null;
      renderAlertSettings();
    });
    document.querySelectorAll(".alerts-remove-rule").forEach((button) => {
      button.addEventListener("click", () => {
        syncAlertDraftFromDom();
        alertsDraft.rules.splice(Number(button.dataset.ruleIndex || 0), 1);
        alertMessage = null;
        renderAlertSettings();
      });
    });
    document.getElementById("alerts-save")?.addEventListener("click", submitAlertSettings);
  }

  function blankRule(index) {
    return {
      id: `rule-${index + 1}`,
      name: "",
      enabled: true,
      metric: "cpu_usage_percent",
      comparator: "gt",
      threshold: 85,
      window_minutes: 5,
      severity: "warning",
      scope_mode: "all",
      node_ids: [],
      tags: [],
      delivery: ["smtp"],
      cooldown_minutes: 30,
      send_resolved: true,
    };
  }

  function syncAlertDraftFromDom() {
    const smtpForm = document.getElementById("alerts-smtp-form");
    const webhookForm = document.getElementById("alerts-webhook-form");
    const inspectionForm = document.getElementById("alerts-inspection-form");
    if (!smtpForm || !webhookForm || !inspectionForm) return;

    alertsDraft.enabled = document.getElementById("alerts-enabled")?.checked || false;
    alertsDraft.smtp = {
      ...alertsDraft.smtp,
      enabled: isChecked(smtpForm, "enabled"),
      host: valueOf(smtpForm, "host").trim(),
      port: Number(valueOf(smtpForm, "port") || 587),
      username: valueOf(smtpForm, "username").trim(),
      sender: valueOf(smtpForm, "sender").trim(),
      recipients: csvToArray(valueOf(smtpForm, "recipients")),
      transport: valueOf(smtpForm, "transport") || "start_tls",
      password: valueOf(smtpForm, "password"),
      clear_password: isChecked(smtpForm, "clear_password"),
    };
    alertsDraft.webhook = {
      ...alertsDraft.webhook,
      enabled: isChecked(webhookForm, "enabled"),
      url: valueOf(webhookForm, "url").trim(),
      send_resolved: isChecked(webhookForm, "send_resolved"),
      secret: valueOf(webhookForm, "secret"),
      clear_secret: isChecked(webhookForm, "clear_secret"),
    };
    alertsDraft.inspection = {
      enabled: isChecked(inspectionForm, "enabled"),
      local_time: valueOf(inspectionForm, "local_time").trim(),
      lookback_hours: Number(valueOf(inspectionForm, "lookback_hours") || 24),
      delivery: checkedValues(inspectionForm, "inspection-delivery"),
      offline_grace_minutes: Number(valueOf(inspectionForm, "offline_grace_minutes") || 10),
      latency_warn_ms: Number(valueOf(inspectionForm, "latency_warn_ms") || 250),
      cpu_warn_percent: Number(valueOf(inspectionForm, "cpu_warn_percent") || 85),
      memory_warn_percent: Number(valueOf(inspectionForm, "memory_warn_percent") || 90),
    };
    alertsDraft.rules = Array.from(document.querySelectorAll(".alert-rule-card")).map((card, index) => ({
      id: card.querySelector("[name=id]")?.value.trim() || `rule-${index + 1}`,
      name: card.querySelector("[name=name]")?.value.trim() || "",
      enabled: card.querySelector("[name=enabled]")?.checked || false,
      metric: card.querySelector("[name=metric]")?.value || "cpu_usage_percent",
      comparator: card.querySelector("[name=comparator]")?.value || "gt",
      threshold: Number(card.querySelector("[name=threshold]")?.value || 0),
      window_minutes: Number(card.querySelector("[name=window_minutes]")?.value || 5),
      severity: card.querySelector("[name=severity]")?.value || "warning",
      scope_mode: card.querySelector("[name=scope_mode]")?.value || "all",
      node_ids: csvToArray(card.querySelector("[name=node_ids]")?.value || ""),
      tags: csvToArray(card.querySelector("[name=tags]")?.value || ""),
      delivery: checkedValues(card, `rule-delivery-${index}`),
      cooldown_minutes: Number(card.querySelector("[name=cooldown_minutes]")?.value || 30),
      send_resolved: card.querySelector("[name=send_resolved]")?.checked || false,
    }));
  }

  function valueOf(form, name) {
    return form.elements.namedItem(name)?.value || "";
  }

  function isChecked(form, name) {
    return form.elements.namedItem(name)?.checked || false;
  }

  function checkedValues(root, name) {
    return Array.from(root.querySelectorAll(`input[name="${name}"]:checked`)).map((input) => input.value);
  }

  function csvToArray(value) {
    return String(value)
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
  }

  async function submitAlertSettings() {
    syncAlertDraftFromDom();
    const message = document.getElementById("alerts-message");
    if (message) {
      message.className = "settings-message";
      message.textContent = t("alerts.saving");
    }
    try {
      latestAlerts = await postSettingsJson("/api/settings/alerts", {
        current_password: document.getElementById("alerts-current-password")?.value || null,
        code: document.getElementById("alerts-code")?.value || null,
        enabled: alertsDraft.enabled,
        smtp: {
          enabled: alertsDraft.smtp.enabled,
          host: alertsDraft.smtp.host,
          port: alertsDraft.smtp.port,
          username: alertsDraft.smtp.username,
          password: alertsDraft.smtp.password || null,
          clear_password: alertsDraft.smtp.clear_password || false,
          sender: alertsDraft.smtp.sender,
          recipients: alertsDraft.smtp.recipients,
          transport: alertsDraft.smtp.transport,
        },
        webhook: {
          enabled: alertsDraft.webhook.enabled,
          url: alertsDraft.webhook.url,
          secret: alertsDraft.webhook.secret || null,
          clear_secret: alertsDraft.webhook.clear_secret || false,
          send_resolved: alertsDraft.webhook.send_resolved,
        },
        rules: alertsDraft.rules.map((rule) => ({
          id: rule.id,
          name: rule.name,
          enabled: rule.enabled,
          metric: rule.metric,
          comparator: rule.comparator,
          threshold: rule.threshold,
          window_minutes: rule.window_minutes,
          severity: rule.severity,
          scope_mode: rule.scope_mode,
          node_ids: rule.node_ids,
          tags: rule.tags,
          delivery: rule.delivery,
          cooldown_minutes: rule.cooldown_minutes,
          send_resolved: rule.send_resolved,
        })),
        inspection: alertsDraft.inspection,
      });
      alertsDraft = normalizeAlertConfig(latestAlerts.config || emptyAlertsConfig());
      const currentPassword = document.getElementById("alerts-current-password");
      const code = document.getElementById("alerts-code");
      if (currentPassword) currentPassword.value = "";
      if (code) code.value = "";
      alertMessage = { type: "ok", text: t("alerts.saved") };
      renderAlertSettings();
    } catch (error) {
      alertMessage = { type: "error", text: t("alerts.save_failed", { error: error.message }) };
      renderAlertSettings();
    }
  }

  return {
    applyChrome,
    loadAlertSettings,
  };
}
