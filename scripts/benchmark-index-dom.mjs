#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = parseArgs(process.argv.slice(2));
const nodeCount = positiveInteger(args.nodes ?? "1000", "nodes");
const diskEntries = positiveInteger(args.disks ?? "1", "disks");
const refreshMs = positiveInteger(args.refreshMs ?? "600000", "refresh-ms");
const outPath = path.resolve(
  repoRoot,
  args.out ?? `target/load-test/index-dom-${nodeCount}.html`,
);

const indexPath = path.join(repoRoot, "nodelite-server/assets/index.html");
const i18nPath = path.join(repoRoot, "nodelite-server/assets/ui-i18n.json");
let html = await fs.readFile(indexPath, "utf8");
const i18n = JSON.parse(await fs.readFile(i18nPath, "utf8"));

html = html
  .replaceAll("__REFRESH_MS__", String(refreshMs))
  .replaceAll("__I18N_ASSET_PATH__", "/assets/ui-i18n.json");
const scriptMarker = "\n    <script>\n      const PAGE_CONFIG";
if (!html.includes(scriptMarker)) {
  throw new Error("failed to find index.html PAGE_CONFIG script marker");
}
html = html.replace(
  scriptMarker,
  `${benchmarkInjection({ nodeCount, diskEntries, i18n })}${scriptMarker}`,
);

await fs.mkdir(path.dirname(outPath), { recursive: true });
await fs.writeFile(outPath, html);
console.log(`wrote ${outPath}`);
console.log(
  "open it in a browser and read the result panel or window.__NODELITE_DOM_BENCHMARK__",
);

function benchmarkInjection({ nodeCount, diskEntries, i18n }) {
  return `
    <script>
      (() => {
        const benchmarkStartedAt = performance.now();
        const nodeCount = ${nodeCount};
        const diskEntries = ${diskEntries};
        const i18n = ${JSON.stringify(i18n)};
        const now = new Date().toISOString();
        const nodePayload = Array.from({ length: nodeCount }, (_, index) => makeNode(index));
        const overviewPayload = {
          generated_at: now,
          total_nodes: nodeCount,
          online_nodes: nodePayload.filter((node) => node.online).length,
          offline_nodes: nodePayload.filter((node) => !node.online).length,
          total_rx_bytes: nodePayload.reduce((sum, node) => sum + node.snapshot.network.total_rx_bytes, 0),
          total_tx_bytes: nodePayload.reduce((sum, node) => sum + node.snapshot.network.total_tx_bytes, 0),
          current_rx_bytes_per_sec: nodePayload.reduce((sum, node) => sum + node.snapshot.network.rx_bytes_per_sec, 0),
          current_tx_bytes_per_sec: nodePayload.reduce((sum, node) => sum + node.snapshot.network.tx_bytes_per_sec, 0),
          average_latency_ms: 93,
        };

        window.fetch = async (input) => {
          const url = String(typeof input === "string" ? input : input.url);
          if (url.endsWith("/assets/ui-i18n.json")) return jsonResponse(i18n);
          if (url === "/api/overview") return jsonResponse(overviewPayload);
          if (url === "/api/nodes") return jsonResponse(nodePayload);
          if (url.includes("/api/nodes/") && url.includes("/history")) return jsonResponse([]);
          if (url.includes("geojson")) return jsonResponse({ type: "FeatureCollection", features: [] });
          return new Response("not found", { status: 404 });
        };

        function makeNode(index) {
          const online = index % 11 !== 0;
          const latency = online ? 20 + (index % 240) : null;
          return {
            identity: {
              node_id: \`bench-node-\${String(index).padStart(4, "0")}\`,
              node_label: \`Bench Node \${String(index).padStart(4, "0")}\`,
              hostname: \`bench-\${index}.example.internal\`,
              os: "Linux",
              kernel_version: "6.8.0-benchmark",
              cpu_model: "Synthetic CPU",
              cpu_cores: 4 + (index % 8),
              agent_version: "dom-benchmark",
              boot_time: now,
              tags: [\`country:\${["US", "JP", "DE", "SG", "CN"][index % 5]}\`],
            },
            remote_ip: "127.0.0.1",
            snapshot: {
              collected_at: now,
              cpu_usage_percent: online ? 10 + (index % 88) : null,
              load: { one: 0.2 + (index % 9) / 10, five: 0.4, fifteen: 0.5 },
              memory: {
                total_bytes: 8589934592,
                used_bytes: 2147483648 + index * 4096,
                available_bytes: 5368709120,
                swap_total_bytes: 1073741824,
                swap_used_bytes: 67108864,
              },
              uptime_secs: 3600 + index,
              disks: Array.from({ length: diskEntries }, (_, diskIndex) => ({
                device: \`/dev/vd\${String.fromCharCode(97 + (diskIndex % 26))}\`,
                mount_point: \`/mnt/bench-\${diskIndex}\`,
                fs_type: "ext4",
                total_bytes: 85899345920,
                available_bytes: 42949672960,
                used_bytes: 42949672960,
                used_percent: 35 + (diskIndex % 60),
              })),
              network: {
                total_rx_bytes: 524288 * (index + 1),
                total_tx_bytes: 262144 * (index + 1),
                rx_bytes_per_sec: 32768 + index,
                tx_bytes_per_sec: 16384 + index,
              },
            },
            last_seen: now,
            latency_ms: latency,
            online,
          };
        }

        function jsonResponse(value) {
          return new Response(JSON.stringify(value), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }

        function reportWhenRendered(deadline = performance.now() + 30000) {
          const cards = document.querySelectorAll(".node-card").length;
          if (cards === nodeCount) {
            requestAnimationFrame(() => {
              const result = {
                nodeCount,
                diskEntries,
                renderMs: Number((performance.now() - benchmarkStartedAt).toFixed(2)),
                jsHeapBytes: performance.memory?.usedJSHeapSize ?? null,
                domNodeCount: document.getElementsByTagName("*").length,
                nodeCardCount: cards,
              };
              window.__NODELITE_DOM_BENCHMARK__ = result;
              console.table(result);
              const panel = document.createElement("pre");
              panel.id = "dom-benchmark-result";
              panel.style.cssText = "position:fixed;z-index:9999;right:12px;bottom:12px;max-width:360px;padding:12px;border-radius:8px;background:#111827;color:#f9fafb;font:12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace;box-shadow:0 16px 44px rgba(0,0,0,.32);white-space:pre-wrap";
              panel.textContent = JSON.stringify(result, null, 2);
              document.body.appendChild(panel);
            });
            return;
          }
          if (performance.now() > deadline) {
            console.error("DOM benchmark timed out", { cards, nodeCount });
            return;
          }
          requestAnimationFrame(() => reportWhenRendered(deadline));
        }

        window.addEventListener("load", () => requestAnimationFrame(() => reportWhenRendered()));
      })();
    </script>`;
}

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (!arg.startsWith("--")) continue;
    const key = camelCaseArg(arg.slice(2));
    const next = argv[index + 1];
    if (!next || next.startsWith("--")) {
      result[key] = "true";
      continue;
    }
    result[key] = next;
    index++;
  }
  return result;
}

function camelCaseArg(key) {
  return key.replaceAll(/-([a-z])/g, (_, letter) => letter.toUpperCase());
}

function positiveInteger(value, label) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`--${label} must be a positive integer`);
  }
  return parsed;
}
