pub const UI_I18N_JSON: &str = include_str!("../assets/ui-i18n.json");
pub const UI_I18N_ASSET_PATH: &str = "/assets/ui-i18n.json";

pub fn index_html(refresh_interval_secs: u64) -> String {
    INDEX_TEMPLATE
        .replace(
            "__REFRESH_MS__",
            &(refresh_interval_secs * 1000).to_string(),
        )
        .replace("__I18N_ASSET_PATH__", UI_I18N_ASSET_PATH)
}

pub fn node_html(node_id: &str, refresh_interval_secs: u64) -> String {
    NODE_TEMPLATE
        .replace(
            "__REFRESH_MS__",
            &(refresh_interval_secs * 1000).to_string(),
        )
        .replace("__I18N_ASSET_PATH__", UI_I18N_ASSET_PATH)
        .replace(
            "__NODE_ID_JSON__",
            &serde_json::to_string(node_id).unwrap_or_else(|_| "\"\"".to_string()),
        )
}

const INDEX_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>XiMonitor</title>
    <style>
      :root {
        color-scheme: light;
        --bg-a: #f2ede2;
        --bg-b: #eef2f4;
        --ink: #18212c;
        --muted: #55616f;
        --line: rgba(24, 33, 44, 0.08);
        --panel: rgba(255, 255, 255, 0.84);
        --good: #1d6a43;
        --bad: #b04736;
        --accent: #0e7490;
        font-family: "Avenir Next", "Segoe UI", sans-serif;
      }
      * { box-sizing: border-box; }
      body {
        margin: 0;
        min-height: 100vh;
        color: var(--ink);
        background:
          radial-gradient(circle at top left, rgba(205, 226, 236, 0.9), transparent 35%),
          radial-gradient(circle at top right, rgba(244, 221, 196, 0.65), transparent 28%),
          linear-gradient(135deg, var(--bg-a), var(--bg-b));
      }
      .shell {
        width: min(1320px, calc(100vw - 32px));
        margin: 0 auto;
        padding: 28px 0 48px;
      }
      .hero {
        display: flex;
        justify-content: space-between;
        gap: 20px;
        align-items: end;
        margin-bottom: 24px;
      }
      .hero h1 {
        margin: 0;
        font: 700 clamp(2.7rem, 5vw, 4.8rem) / 0.9 "Iowan Old Style", "Palatino Linotype", serif;
        letter-spacing: -0.06em;
      }
      .hero p {
        margin: 14px 0 0;
        max-width: 760px;
        color: var(--muted);
        font-size: 1.03rem;
        line-height: 1.7;
      }
      .hero-side {
        display: flex;
        flex-direction: column;
        align-items: end;
        gap: 12px;
      }
      .lang-picker {
        display: inline-flex;
        align-items: center;
        gap: 10px;
        color: var(--muted);
        font-size: 0.92rem;
      }
      .lang-select {
        border: 1px solid rgba(24, 33, 44, 0.12);
        border-radius: 999px;
        padding: 10px 14px;
        background: rgba(255, 255, 255, 0.82);
        color: var(--ink);
        font: inherit;
      }
      .stamp {
        text-align: right;
        color: var(--muted);
        font-size: 0.92rem;
      }
      .cards {
        display: grid;
        grid-template-columns: repeat(4, minmax(0, 1fr));
        gap: 16px;
        margin-bottom: 22px;
      }
      .card, .node-card {
        background: var(--panel);
        border: 1px solid var(--line);
        border-radius: 22px;
        box-shadow: 0 18px 60px rgba(24, 33, 44, 0.08);
        backdrop-filter: blur(18px);
      }
      .card {
        padding: 18px 20px;
      }
      .card .label {
        color: var(--muted);
        font-size: 0.9rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
      }
      .card .value {
        margin-top: 10px;
        font-size: clamp(1.8rem, 3vw, 2.5rem);
        font-weight: 700;
        letter-spacing: -0.05em;
      }
      .node-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        gap: 16px;
      }
      .node-card {
        display: block;
        padding: 18px 18px 16px;
        color: inherit;
        text-decoration: none;
        transition: transform 180ms ease, box-shadow 180ms ease;
      }
      .node-card:hover {
        transform: translateY(-3px);
        box-shadow: 0 24px 70px rgba(24, 33, 44, 0.12);
      }
      .node-head {
        display: flex;
        justify-content: space-between;
        gap: 12px;
        align-items: start;
      }
      .node-title {
        margin: 0;
        font-size: 1.25rem;
      }
      .node-id {
        color: var(--muted);
        font-size: 0.92rem;
        margin-top: 4px;
      }
      .badge {
        border-radius: 999px;
        padding: 6px 10px;
        font-size: 0.78rem;
        font-weight: 700;
        text-transform: uppercase;
        letter-spacing: 0.08em;
      }
      .online { background: rgba(29, 106, 67, 0.12); color: var(--good); }
      .offline { background: rgba(176, 71, 54, 0.12); color: var(--bad); }
      .kv {
        display: grid;
        grid-template-columns: repeat(2, minmax(0, 1fr));
        gap: 12px 16px;
        margin-top: 16px;
      }
      .kv strong {
        display: block;
        font-size: 1.05rem;
      }
      .kv span {
        color: var(--muted);
        font-size: 0.84rem;
      }
      .empty {
        padding: 26px;
        background: var(--panel);
        border: 1px dashed rgba(24, 33, 44, 0.18);
        border-radius: 20px;
        color: var(--muted);
        text-align: center;
      }
      @media (max-width: 980px) {
        .cards { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      }
      @media (max-width: 720px) {
        .shell { width: calc(100vw - 20px); }
        .hero { display: block; }
        .hero-side {
          align-items: start;
          margin-top: 16px;
        }
        .stamp { text-align: left; }
        .cards { grid-template-columns: 1fr; }
      }
    </style>
  </head>
  <body>
    <div class="shell">
      <section class="hero">
        <div>
          <h1 data-i18n="index.heading">XiMonitor</h1>
          <p data-i18n="index.tagline">Read-only node telemetry for CPU, load, memory, disks, throughput, and WebSocket RTT. Configuration stays on disk; the web view stays observational.</p>
        </div>
        <div class="hero-side">
          <label class="lang-picker">
            <span data-i18n="common.language">Language</span>
            <select id="language-select" class="lang-select" aria-label="Language"></select>
          </label>
          <div class="stamp">
            <div id="refresh-note">Refreshes every 5s</div>
            <div id="updated-at">Waiting for data…</div>
          </div>
        </div>
      </section>

      <section class="cards" id="overview"></section>
      <section id="nodes"></section>
    </div>

    <script>
      const REFRESH_MS = __REFRESH_MS__;
      const I18N_ASSET_PATH = "__I18N_ASSET_PATH__";
      const LANGUAGE_STORAGE_KEY = "ximonitor.ui.language";
      let I18N = { en: { "__label": "English" } };
      let currentLanguage = "en";
      let latestOverview = null;
      let latestNodes = [];

      function escapeHtml(value) {
        return String(value)
          .replaceAll("&", "&amp;")
          .replaceAll("<", "&lt;")
          .replaceAll(">", "&gt;")
          .replaceAll('"', "&quot;")
          .replaceAll("'", "&#39;");
      }

      function templateText(value, vars = {}) {
        return String(value).replace(/\{(\w+)\}/g, (_, key) => String(vars[key] ?? ""));
      }

      function supportedLanguages() {
        return Object.keys(I18N).filter((key) => key && typeof I18N[key] === "object");
      }

      function resolveLanguage(candidate) {
        const languages = supportedLanguages();
        if (candidate && languages.includes(candidate)) {
          return candidate;
        }
        const base = String(candidate || "").split("-")[0].toLowerCase();
        const matched = languages.find((language) => language.toLowerCase().startsWith(base));
        return matched || (languages.includes("en") ? "en" : languages[0] || "en");
      }

      function t(key, vars = {}) {
        const primary = I18N[currentLanguage] || {};
        const fallback = I18N.en || {};
        return templateText(primary[key] ?? fallback[key] ?? key, vars);
      }

      function languageLabel(language) {
        return (I18N[language] && I18N[language].__label) || language;
      }

      function storeLanguage(language) {
        try {
          window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
        } catch (_error) {
          // Ignore storage failures in private or restricted browsers.
        }
      }

      function loadStoredLanguage() {
        try {
          return window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
        } catch (_error) {
          return null;
        }
      }

      async function loadI18n() {
        try {
          const response = await fetch(I18N_ASSET_PATH, {
            headers: { "accept": "application/json" },
          });
          if (!response.ok) {
            throw new Error(`${I18N_ASSET_PATH} -> ${response.status}`);
          }
          I18N = await response.json();
        } catch (error) {
          console.warn("failed to load ui translations", error);
        }
        currentLanguage = resolveLanguage(loadStoredLanguage() || navigator.language);
        storeLanguage(currentLanguage);
      }

      function bindLanguageSelector(onChange) {
        const select = document.getElementById("language-select");
        const renderOptions = () => {
          select.innerHTML = supportedLanguages().map((language) => `
            <option value="${escapeHtml(language)}">${escapeHtml(languageLabel(language))}</option>
          `).join("");
          select.value = currentLanguage;
        };

        renderOptions();
        select.addEventListener("change", (event) => {
          currentLanguage = resolveLanguage(event.target.value);
          storeLanguage(currentLanguage);
          renderOptions();
          onChange();
        });
      }

      function fmtBytes(bytes) {
        if (bytes == null) return t("common.not_available");
        const units = ["B", "KB", "MB", "GB", "TB", "PB"];
        let value = Number(bytes);
        let index = 0;
        while (value >= 1024 && index < units.length - 1) {
          value /= 1024;
          index += 1;
        }
        return `${value.toFixed(value >= 100 || index === 0 ? 0 : 1)} ${units[index]}`;
      }

      function fmtRate(bytes) {
        if (bytes == null) return t("common.not_available");
        return `${fmtBytes(bytes)}/s`;
      }

      function fmtPercent(value) {
        if (value == null || Number.isNaN(Number(value))) return t("common.not_available");
        return `${Number(value).toFixed(1)}%`;
      }

      function fmtLatency(value) {
        if (value == null) return t("common.not_available");
        return `${Math.round(value)} ms`;
      }

      function fmtUptime(seconds) {
        if (seconds == null || Number.isNaN(Number(seconds))) {
          return t("common.not_available");
        }
        const totalHours = Math.floor(Number(seconds) / 3600);
        const days = Math.floor(totalHours / 24);
        const hours = totalHours % 24;
        if (days > 0) {
          return t("node.uptime.days_hours", { days, hours });
        }
        return t("node.uptime.hours", { hours: totalHours });
      }

      function diskSummary(disks) {
        if (!Array.isArray(disks) || disks.length === 0) return t("common.not_available");
        const total = disks.reduce((sum, disk) => sum + (disk.total_bytes || 0), 0);
        const used = disks.reduce((sum, disk) => sum + (disk.used_bytes || 0), 0);
        if (!total) return t("common.not_available");
        return fmtPercent((used / total) * 100);
      }

      function applyChrome() {
        document.documentElement.lang = currentLanguage;
        document.title = t("index.page_title");
        document.querySelectorAll("[data-i18n]").forEach((node) => {
          node.textContent = t(node.dataset.i18n);
        });
        document.getElementById("refresh-note").textContent = t("index.refreshes_every", {
          seconds: `${Math.round(REFRESH_MS / 1000)}s`,
        });
        document.getElementById("updated-at").textContent = latestOverview
          ? t("common.updated_at", { time: new Date(latestOverview.generated_at).toLocaleString() })
          : t("common.waiting_for_data");
      }

      function setOverview(data) {
        latestOverview = data;
        const cards = [
          [t("index.nodes"), `${data.online_nodes}/${data.total_nodes}`, t("index.online_total")],
          [t("index.latency"), fmtLatency(data.average_latency_ms), t("index.mean_rtt")],
          [t("index.traffic"), t("index.traffic_in", { value: fmtBytes(data.total_rx_bytes) }), t("index.traffic_out", { value: fmtBytes(data.total_tx_bytes) })],
          [t("index.realtime"), t("index.realtime_down", { value: fmtRate(data.current_rx_bytes_per_sec) }), t("index.realtime_up", { value: fmtRate(data.current_tx_bytes_per_sec) })],
        ];

        document.getElementById("overview").innerHTML = cards.map(([label, value, sub]) => `
          <article class="card">
            <div class="label">${escapeHtml(label)}</div>
            <div class="value">${escapeHtml(value)}</div>
            <div class="label" style="margin-top:8px;">${escapeHtml(sub)}</div>
          </article>
        `).join("");

        applyChrome();
      }

      function setNodes(nodes) {
        latestNodes = Array.isArray(nodes) ? nodes : [];
        const root = document.getElementById("nodes");
        if (latestNodes.length === 0) {
          root.innerHTML = `<div class="empty">${escapeHtml(t("index.no_agents"))}</div>`;
          return;
        }

        root.innerHTML = `<div class="node-grid">${latestNodes.map((node) => {
          const snapshot = node.snapshot || {};
          const memory = snapshot.memory || {};
          return `
            <a class="node-card" href="/nodes/${encodeURIComponent(node.identity.node_id)}">
              <div class="node-head">
                <div>
                  <h2 class="node-title">${escapeHtml(node.identity.node_label)}</h2>
                  <div class="node-id">${escapeHtml(node.identity.node_id)} · ${escapeHtml(node.identity.hostname || t("common.unknown_host"))}</div>
                </div>
                <span class="badge ${node.online ? "online" : "offline"}">${escapeHtml(node.online ? t("common.online") : t("common.offline"))}</span>
              </div>
              <div class="kv">
                <div><strong>${fmtPercent(snapshot.cpu_usage_percent)}</strong><span>${escapeHtml(t("index.node.cpu"))}</span></div>
                <div><strong>${fmtPercent(memory.total_bytes ? (memory.used_bytes / memory.total_bytes) * 100 : null)}</strong><span>${escapeHtml(t("index.node.memory"))}</span></div>
                <div><strong>${fmtRate(snapshot.network?.rx_bytes_per_sec)}</strong><span>${escapeHtml(t("index.node.download"))}</span></div>
                <div><strong>${fmtRate(snapshot.network?.tx_bytes_per_sec)}</strong><span>${escapeHtml(t("index.node.upload"))}</span></div>
                <div><strong>${fmtLatency(node.latency_ms)}</strong><span>${escapeHtml(t("index.node.rtt"))}</span></div>
                <div><strong>${diskSummary(snapshot.disks)}</strong><span>${escapeHtml(t("index.node.disks"))}</span></div>
              </div>
            </a>
          `;
        }).join("")}</div>`;
      }

      async function fetchJson(url) {
        const response = await fetch(url, { headers: { "accept": "application/json" } });
        if (!response.ok) throw new Error(`${url} -> ${response.status}`);
        return response.json();
      }

      async function refresh() {
        try {
          const [overview, nodes] = await Promise.all([
            fetchJson("/api/overview"),
            fetchJson("/api/nodes"),
          ]);
          setOverview(overview);
          setNodes(nodes);
        } catch (error) {
          document.getElementById("nodes").innerHTML = `<div class="empty">${escapeHtml(t("index.dashboard_load_failed", { error: error.message }))}</div>`;
        } finally {
          window.setTimeout(refresh, REFRESH_MS);
        }
      }

      async function init() {
        await loadI18n();
        bindLanguageSelector(() => {
          applyChrome();
          if (latestOverview) {
            setOverview(latestOverview);
          }
          setNodes(latestNodes);
        });
        applyChrome();
        refresh();
      }

      init();
    </script>
  </body>
</html>
"#;

const NODE_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>XiMonitor Node</title>
    <style>
      :root {
        color-scheme: light;
        --bg: #f7f2e9;
        --ink: #1a202b;
        --muted: #5d6875;
        --line: rgba(26, 32, 43, 0.1);
        --panel: rgba(255, 255, 255, 0.87);
        --accent: #0f766e;
        --chart-a: #0f766e;
        --chart-b: #b45309;
        --chart-c: #1d4ed8;
        --chart-d: #be185d;
        font-family: "Avenir Next", "Segoe UI", sans-serif;
      }
      * { box-sizing: border-box; }
      body {
        margin: 0;
        min-height: 100vh;
        color: var(--ink);
        background:
          radial-gradient(circle at top left, rgba(208, 228, 227, 0.9), transparent 30%),
          radial-gradient(circle at top right, rgba(250, 228, 195, 0.6), transparent 24%),
          linear-gradient(135deg, var(--bg), #eef1f2);
      }
      .shell {
        width: min(1280px, calc(100vw - 32px));
        margin: 0 auto;
        padding: 24px 0 48px;
      }
      a { color: inherit; }
      .topline {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 18px;
        margin-bottom: 18px;
      }
      .topline .back {
        text-decoration: none;
        color: var(--muted);
        font-weight: 600;
      }
      .topline-actions {
        display: flex;
        align-items: center;
        gap: 14px;
      }
      .lang-picker {
        display: inline-flex;
        align-items: center;
        gap: 10px;
        color: var(--muted);
        font-size: 0.92rem;
      }
      .lang-select {
        border: 1px solid rgba(26, 32, 43, 0.12);
        border-radius: 999px;
        padding: 10px 14px;
        background: rgba(255, 255, 255, 0.82);
        color: var(--ink);
        font: inherit;
      }
      .hero, .panel {
        background: var(--panel);
        border: 1px solid var(--line);
        border-radius: 24px;
        box-shadow: 0 18px 60px rgba(26, 32, 43, 0.08);
        backdrop-filter: blur(18px);
      }
      .hero {
        padding: 24px;
        margin-bottom: 18px;
      }
      .hero h1 {
        margin: 0;
        font: 700 clamp(2.4rem, 4.8vw, 4.1rem) / 0.92 "Iowan Old Style", "Palatino Linotype", serif;
        letter-spacing: -0.05em;
      }
      .meta {
        margin-top: 10px;
        color: var(--muted);
        line-height: 1.7;
      }
      .stats, .charts {
        display: grid;
        gap: 16px;
      }
      .stats {
        grid-template-columns: repeat(4, minmax(0, 1fr));
        margin-bottom: 18px;
      }
      .panel {
        padding: 18px 20px;
      }
      .label {
        color: var(--muted);
        text-transform: uppercase;
        letter-spacing: 0.08em;
        font-size: 0.84rem;
      }
      .value {
        margin-top: 8px;
        font-size: clamp(1.5rem, 2.7vw, 2.2rem);
        font-weight: 700;
      }
      .controls-panel {
        margin-bottom: 18px;
      }
      .controls-head {
        display: flex;
        justify-content: space-between;
        gap: 18px;
        align-items: center;
      }
      .control-value {
        margin-top: 8px;
        font-size: 1.28rem;
        font-weight: 700;
        letter-spacing: -0.03em;
      }
      .toggle-button {
        border: 1px solid rgba(26, 32, 43, 0.12);
        border-radius: 999px;
        background: rgba(255, 255, 255, 0.75);
        color: var(--ink);
        padding: 12px 16px;
        font: inherit;
        font-weight: 700;
        cursor: pointer;
        transition: background 160ms ease, color 160ms ease, border-color 160ms ease;
      }
      .toggle-button.active {
        background: rgba(15, 118, 110, 0.12);
        border-color: rgba(15, 118, 110, 0.28);
        color: var(--accent);
      }
      .window-slider {
        width: 100%;
        margin-top: 18px;
        accent-color: var(--accent);
      }
      .window-legend {
        display: grid;
        grid-template-columns: repeat(6, minmax(0, 1fr));
        gap: 8px;
        margin-top: 12px;
      }
      .window-chip {
        border-radius: 999px;
        padding: 8px 10px;
        font-size: 0.82rem;
        text-align: center;
        color: var(--muted);
        background: rgba(93, 104, 117, 0.08);
      }
      .window-chip.active {
        color: var(--accent);
        background: rgba(15, 118, 110, 0.12);
      }
      .charts {
        grid-template-columns: repeat(2, minmax(0, 1fr));
        margin-bottom: 18px;
      }
      .chart-box {
        height: 210px;
        margin-top: 14px;
        border-radius: 18px;
        background: linear-gradient(180deg, rgba(255,255,255,0.4), rgba(242,245,247,0.85));
        border: 1px solid rgba(26, 32, 43, 0.07);
        display: grid;
        place-items: center;
        overflow: hidden;
        position: relative;
      }
      .disks table {
        width: 100%;
        border-collapse: collapse;
      }
      .disks th, .disks td {
        padding: 12px 0;
        text-align: left;
        border-bottom: 1px solid rgba(26, 32, 43, 0.08);
      }
      .disks th {
        color: var(--muted);
        font-size: 0.83rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
      }
      .empty {
        color: var(--muted);
      }
      @media (max-width: 960px) {
        .stats, .charts { grid-template-columns: 1fr; }
        .window-legend { grid-template-columns: repeat(3, minmax(0, 1fr)); }
      }
      @media (max-width: 720px) {
        .shell { width: calc(100vw - 20px); }
        .topline { display: block; }
        .topline-actions {
          justify-content: space-between;
          margin-top: 12px;
        }
        .controls-head { display: block; }
        .toggle-button { margin-top: 14px; width: 100%; }
      }
    </style>
  </head>
  <body>
    <div class="shell">
      <div class="topline">
        <a class="back" href="/">← <span data-i18n="node.back">Back to dashboard</span></a>
        <div class="topline-actions">
          <label class="lang-picker">
            <span data-i18n="common.language">Language</span>
            <select id="language-select" class="lang-select" aria-label="Language"></select>
          </label>
          <div id="updated" class="label">Waiting for node data…</div>
        </div>
      </div>

      <section class="hero">
        <h1 id="title" data-i18n="node.loading">Loading node…</h1>
        <div class="meta" id="meta"></div>
      </section>

      <section class="stats" id="stats"></section>

      <section class="panel controls-panel">
        <div class="controls-head">
          <div>
            <div class="label" data-i18n="node.history_window">History Window</div>
            <div class="control-value" id="history-window-value">Last 24 hours</div>
          </div>
          <button type="button" class="toggle-button" id="peak-clip-toggle">Clip Spikes: Off</button>
        </div>
        <input class="window-slider" id="history-window-slider" type="range" min="0" max="5" step="1" value="2" />
        <div class="window-legend" id="history-window-legend"></div>
      </section>

      <section class="charts">
        <article class="panel">
          <div class="label" data-i18n="node.cpu_usage">CPU Usage</div>
          <div class="chart-box" id="chart-cpu"></div>
        </article>
        <article class="panel">
          <div class="label" data-i18n="node.memory_usage">Memory Usage</div>
          <div class="chart-box" id="chart-memory"></div>
        </article>
        <article class="panel">
          <div class="label" data-i18n="node.download_upload">Download / Upload</div>
          <div class="chart-box" id="chart-network"></div>
        </article>
        <article class="panel">
          <div class="label" data-i18n="node.websocket_rtt">WebSocket RTT</div>
          <div class="chart-box" id="chart-latency"></div>
        </article>
      </section>

      <section class="panel disks">
        <div class="label" data-i18n="node.mounted_disks">Mounted Disks</div>
        <div id="disks" style="margin-top: 14px;"></div>
      </section>
    </div>

    <script>
      const NODE_ID = __NODE_ID_JSON__;
      const REFRESH_MS = __REFRESH_MS__;
      const I18N_ASSET_PATH = "__I18N_ASSET_PATH__";
      const LANGUAGE_STORAGE_KEY = "ximonitor.ui.language";
      const HISTORY_MAX_POINTS = 480;
      const HISTORY_WINDOWS = [6, 12, 24, 72, 168, 336];
      let I18N = { en: { "__label": "English" } };
      let currentLanguage = "en";
      let latestNode = null;
      let latestHistory = [];
      let refreshTimer = null;
      const chartState = {
        windowIndex: 2,
        peakClipEnabled: false,
      };

      function escapeHtml(value) {
        return String(value)
          .replaceAll("&", "&amp;")
          .replaceAll("<", "&lt;")
          .replaceAll(">", "&gt;")
          .replaceAll('"', "&quot;")
          .replaceAll("'", "&#39;");
      }

      function templateText(value, vars = {}) {
        return String(value).replace(/\{(\w+)\}/g, (_, key) => String(vars[key] ?? ""));
      }

      function supportedLanguages() {
        return Object.keys(I18N).filter((key) => key && typeof I18N[key] === "object");
      }

      function resolveLanguage(candidate) {
        const languages = supportedLanguages();
        if (candidate && languages.includes(candidate)) {
          return candidate;
        }
        const base = String(candidate || "").split("-")[0].toLowerCase();
        const matched = languages.find((language) => language.toLowerCase().startsWith(base));
        return matched || (languages.includes("en") ? "en" : languages[0] || "en");
      }

      function t(key, vars = {}) {
        const primary = I18N[currentLanguage] || {};
        const fallback = I18N.en || {};
        return templateText(primary[key] ?? fallback[key] ?? key, vars);
      }

      function languageLabel(language) {
        return (I18N[language] && I18N[language].__label) || language;
      }

      function storeLanguage(language) {
        try {
          window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
        } catch (_error) {
          // Ignore storage failures in private or restricted browsers.
        }
      }

      function loadStoredLanguage() {
        try {
          return window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
        } catch (_error) {
          return null;
        }
      }

      async function loadI18n() {
        try {
          const response = await fetch(I18N_ASSET_PATH, {
            headers: { "accept": "application/json" },
          });
          if (!response.ok) {
            throw new Error(`${I18N_ASSET_PATH} -> ${response.status}`);
          }
          I18N = await response.json();
        } catch (error) {
          console.warn("failed to load ui translations", error);
        }
        currentLanguage = resolveLanguage(loadStoredLanguage() || navigator.language);
        storeLanguage(currentLanguage);
      }

      function bindLanguageSelector(onChange) {
        const select = document.getElementById("language-select");
        const renderOptions = () => {
          select.innerHTML = supportedLanguages().map((language) => `
            <option value="${escapeHtml(language)}">${escapeHtml(languageLabel(language))}</option>
          `).join("");
          select.value = currentLanguage;
        };

        renderOptions();
        select.addEventListener("change", (event) => {
          currentLanguage = resolveLanguage(event.target.value);
          storeLanguage(currentLanguage);
          renderOptions();
          onChange();
        });
      }

      function fmtBytes(bytes) {
        if (bytes == null) return t("common.not_available");
        const units = ["B", "KB", "MB", "GB", "TB", "PB"];
        let value = Number(bytes);
        let index = 0;
        while (value >= 1024 && index < units.length - 1) {
          value /= 1024;
          index += 1;
        }
        return `${value.toFixed(value >= 100 || index === 0 ? 0 : 1)} ${units[index]}`;
      }

      function fmtRate(bytes) {
        if (bytes == null) return t("common.not_available");
        return `${fmtBytes(bytes)}/s`;
      }

      function fmtPercent(value) {
        if (value == null || Number.isNaN(Number(value))) return t("common.not_available");
        return `${Number(value).toFixed(1)}%`;
      }

      function fmtLatency(value) {
        if (value == null) return t("common.not_available");
        return `${Math.round(value)} ms`;
      }

      function fetchJson(url) {
        return fetch(url, { headers: { "accept": "application/json" } }).then((response) => {
          if (!response.ok) throw new Error(`${url} -> ${response.status}`);
          return response.json();
        });
      }

      function currentWindowHours() {
        return HISTORY_WINDOWS[chartState.windowIndex] || HISTORY_WINDOWS[2];
      }

      function formatWindowLongLabel(hours) {
        if (hours < 24) {
          return t("node.window.last_hours", { hours });
        }
        return t("node.window.last_days", { days: hours / 24 });
      }

      function formatWindowShortLabel(hours) {
        if (hours < 24) {
          return t("node.window.short_hours", { hours });
        }
        return t("node.window.short_days", { days: hours / 24 });
      }

      function renderWindowLegend() {
        document.getElementById("history-window-legend").innerHTML = HISTORY_WINDOWS.map((hours, index) => `
          <div class="window-chip ${index === chartState.windowIndex ? "active" : ""}">${escapeHtml(formatWindowShortLabel(hours))}</div>
        `).join("");
      }

      function syncControls() {
        document.getElementById("history-window-slider").value = String(chartState.windowIndex);
        document.getElementById("history-window-value").textContent = formatWindowLongLabel(currentWindowHours());
        const toggle = document.getElementById("peak-clip-toggle");
        toggle.textContent = chartState.peakClipEnabled ? t("node.clip.on") : t("node.clip.off");
        toggle.classList.toggle("active", chartState.peakClipEnabled);
        renderWindowLegend();
      }

      function quantile(values, ratio) {
        if (!Array.isArray(values) || values.length === 0) return null;
        const sorted = [...values].sort((left, right) => left - right);
        const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * ratio) - 1));
        return sorted[index];
      }

      function chartBounds(values, clipSpikes) {
        const actualMin = Math.min(...values);
        const actualMax = Math.max(...values);
        let displayMax = actualMax;
        let clipped = false;

        if (clipSpikes && values.length >= 12) {
          const clippedMax = quantile(values, 0.98);
          if (clippedMax != null && clippedMax > actualMin && clippedMax < actualMax) {
            displayMax = clippedMax;
            clipped = true;
          }
        }

        return {
          actualMin,
          actualMax,
          displayMin: actualMin,
          displayMax,
          clipped,
        };
      }

      function renderSparkline(points, colors, formatter, options = {}) {
        if (!Array.isArray(points) || points.length === 0) {
          return `<div class="empty">${escapeHtml(t("node.waiting_history"))}</div>`;
        }

        const width = 640;
        const height = 210;
        const padding = 16;
        const allValues = points.flatMap((point) => point.values).filter((value) => value != null);
        if (allValues.length === 0) {
          return `<div class="empty">${escapeHtml(t("node.no_numeric_history"))}</div>`;
        }

        const bounds = chartBounds(allValues, options.clipSpikes);
        const span = Math.max(bounds.displayMax - bounds.displayMin, 1);
        const series = colors.map((color, seriesIndex) => {
          let started = false;
          const path = points.map((point, pointIndex) => {
            const value = point.values[seriesIndex];
            if (value == null) return null;
            const plottedValue = Math.min(Math.max(value, bounds.displayMin), bounds.displayMax);
            const x = padding + ((width - padding * 2) * pointIndex) / Math.max(points.length - 1, 1);
            const y = height - padding - (((plottedValue - bounds.displayMin) / span) * (height - padding * 2));
            const command = started ? "L" : "M";
            started = true;
            return `${command}${x.toFixed(1)},${y.toFixed(1)}`;
          }).filter(Boolean).join(" ");
          return `<path d="${path}" fill="none" stroke="${color}" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round" />`;
        }).join("");

        const footer = bounds.clipped
          ? t("node.chart.clipped_range", {
              start: formatter(bounds.displayMin),
              end: formatter(bounds.displayMax),
              peak: formatter(bounds.actualMax),
            })
          : t("node.chart.range", {
              start: formatter(bounds.displayMin),
              end: formatter(bounds.actualMax),
            });

        return `
          <svg viewBox="0 0 ${width} ${height}" width="100%" height="100%" preserveAspectRatio="none" aria-hidden="true">
            <rect x="0" y="0" width="${width}" height="${height}" fill="transparent" />
            ${series}
          </svg>
          <div style="position:absolute;left:18px;bottom:16px;font-size:0.82rem;color:#5d6875;">${escapeHtml(footer)}</div>
        `;
      }

      function renderStats(node) {
        const snapshot = node.snapshot || {};
        const memory = snapshot.memory || {};
        const cards = [
          [t("node.stats.cpu"), fmtPercent(snapshot.cpu_usage_percent)],
          [t("node.stats.load"), snapshot.load ? `${snapshot.load.one.toFixed(2)} / ${snapshot.load.five.toFixed(2)} / ${snapshot.load.fifteen.toFixed(2)}` : t("common.not_available")],
          [t("node.stats.download_upload"), `${fmtRate(snapshot.network?.rx_bytes_per_sec)} / ${fmtRate(snapshot.network?.tx_bytes_per_sec)}`],
          [t("node.stats.latency"), fmtLatency(node.latency_ms)],
          [t("node.stats.memory"), `${fmtBytes(memory.used_bytes)} / ${fmtBytes(memory.total_bytes)}`],
          [t("node.stats.swap"), `${fmtBytes(memory.swap_used_bytes)} / ${fmtBytes(memory.swap_total_bytes)}`],
          [t("node.stats.uptime"), fmtUptime(snapshot.uptime_secs)],
          [t("node.stats.agent"), node.identity.agent_version || t("common.not_available")],
        ];
        document.getElementById("stats").innerHTML = cards.map(([label, value]) => `
          <article class="panel">
            <div class="label">${escapeHtml(label)}</div>
            <div class="value">${escapeHtml(value)}</div>
          </article>
        `).join("");
      }

      function renderDisks(node) {
        const disks = node.snapshot?.disks || [];
        const root = document.getElementById("disks");
        if (disks.length === 0) {
          root.innerHTML = `<div class="empty">${escapeHtml(t("node.no_disks"))}</div>`;
          return;
        }
        root.innerHTML = `
          <table>
            <thead>
              <tr>
                <th>${escapeHtml(t("node.disk.device"))}</th>
                <th>${escapeHtml(t("node.disk.mount"))}</th>
                <th>${escapeHtml(t("node.disk.filesystem"))}</th>
                <th>${escapeHtml(t("node.disk.usage"))}</th>
                <th>${escapeHtml(t("node.disk.capacity"))}</th>
              </tr>
            </thead>
            <tbody>
              ${disks.map((disk) => `
                <tr>
                  <td>${escapeHtml(disk.device)}</td>
                  <td>${escapeHtml(disk.mount_point)}</td>
                  <td>${escapeHtml(disk.fs_type)}</td>
                  <td>${fmtPercent(disk.used_percent)}</td>
                  <td>${fmtBytes(disk.used_bytes)} / ${fmtBytes(disk.total_bytes)}</td>
                </tr>
              `).join("")}
            </tbody>
          </table>
        `;
      }

      function renderHistory(history) {
        document.getElementById("chart-cpu").innerHTML = renderSparkline(
          history.map((point) => ({ values: [point.cpu_usage_percent] })),
          ["var(--chart-a)"],
          (value) => `${value.toFixed(1)}%`,
          { clipSpikes: chartState.peakClipEnabled }
        );
        document.getElementById("chart-memory").innerHTML = renderSparkline(
          history.map((point) => ({ values: [point.memory_used_percent] })),
          ["var(--chart-b)"],
          (value) => `${value.toFixed(1)}%`,
          { clipSpikes: chartState.peakClipEnabled }
        );
        document.getElementById("chart-network").innerHTML = renderSparkline(
          history.map((point) => ({ values: [point.rx_bytes_per_sec, point.tx_bytes_per_sec] })),
          ["var(--chart-c)", "var(--chart-a)"],
          (value) => fmtRate(value),
          { clipSpikes: chartState.peakClipEnabled }
        );
        document.getElementById("chart-latency").innerHTML = renderSparkline(
          history.map((point) => ({ values: [point.latency_ms] })),
          ["var(--chart-d)"],
          (value) => `${Math.round(value)} ms`,
          { clipSpikes: chartState.peakClipEnabled }
        );
      }

      function renderNodeHeader(node) {
        document.getElementById("title").textContent = node.identity.node_label || t("common.node_unavailable");
        document.getElementById("meta").innerHTML = `
          ${escapeHtml(node.identity.node_id)} · ${escapeHtml(node.identity.hostname || t("common.unknown_host"))} ·
          ${escapeHtml(node.identity.os || t("common.unknown_os"))} ·
          ${escapeHtml(node.online ? t("common.online") : t("common.offline"))}
        `;
      }

      function renderUpdatedAt(node) {
        document.getElementById("updated").textContent = node.last_seen
          ? t("common.last_seen", { time: new Date(node.last_seen).toLocaleString() })
          : t("common.no_heartbeat_yet");
      }

      function rerenderNode() {
        document.documentElement.lang = currentLanguage;
        document.title = t("node.page_title");
        document.querySelectorAll("[data-i18n]").forEach((element) => {
          element.textContent = t(element.dataset.i18n);
        });
        syncControls();
        if (latestNode) {
          renderNodeHeader(latestNode);
          renderUpdatedAt(latestNode);
          renderStats(latestNode);
          renderDisks(latestNode);
        } else {
          document.getElementById("updated").textContent = t("common.waiting_for_node_data");
          document.getElementById("title").textContent = t("node.loading");
        }
        renderHistory(latestHistory);
      }

      function scheduleRefresh() {
        if (refreshTimer != null) {
          window.clearTimeout(refreshTimer);
        }
        refreshTimer = window.setTimeout(refresh, REFRESH_MS);
      }

      function requestHistoryRefresh() {
        if (refreshTimer != null) {
          window.clearTimeout(refreshTimer);
        }
        refresh();
      }

      function bindControls() {
        const slider = document.getElementById("history-window-slider");
        slider.addEventListener("input", (event) => {
          chartState.windowIndex = Number(event.target.value);
          syncControls();
        });
        slider.addEventListener("change", requestHistoryRefresh);
        document.getElementById("peak-clip-toggle").addEventListener("click", () => {
          chartState.peakClipEnabled = !chartState.peakClipEnabled;
          syncControls();
          renderHistory(latestHistory);
        });
        syncControls();
      }

      async function refresh() {
        try {
          const historyParams = new URLSearchParams({
            window_hours: String(currentWindowHours()),
            max_points: String(HISTORY_MAX_POINTS),
          });
          const [node, history] = await Promise.all([
            fetchJson(`/api/nodes/${encodeURIComponent(NODE_ID)}`),
            fetchJson(`/api/nodes/${encodeURIComponent(NODE_ID)}/history?${historyParams.toString()}`),
          ]);
          latestNode = node;
          latestHistory = history;
          rerenderNode();
        } catch (error) {
          document.getElementById("title").textContent = t("common.node_unavailable");
          document.getElementById("meta").textContent = error.message;
        } finally {
          scheduleRefresh();
        }
      }

      async function init() {
        await loadI18n();
        bindLanguageSelector(() => {
          rerenderNode();
        });
        bindControls();
        rerenderNode();
        refresh();
      }

      init();
    </script>
  </body>
</html>
"#;
