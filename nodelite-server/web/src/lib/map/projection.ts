/**
 * Pure projection + node-placement helpers, ported verbatim from the legacy
 * map code in assets/index.html (mapProject / nodePosition / nodeRegionKey /
 * hashString). No DOM access — safe to unit test directly.
 *
 * Node dots are NOT placed by real geography: nodePosition picks a region
 * anchor (REGION_HINTS) by tag/hostname/id, then adds deterministic jitter so
 * multiple nodes in the same country don't fully overlap. mapProject (real
 * Mercator) is used only for drawing the land mask.
 */

import type { NodeListItem } from '@/api';

export const MAP_WIDTH = 1200;
export const MAP_HEIGHT = 600;
export const MAP_MAX_LAT = 82;
export const MAP_MIN_LAT = -58;
export const MAP_DOT_GAP = 4;
export const MAP_DOT_SIZE = 1.05;
export const MAP_VERTICAL_SHIFT = 70;
export const LATENCY_WARN_MS = 200;

export type NodeStatus = 'online' | 'offline' | 'latency';

/** Region anchor points as {x, y} fractions of the map (0..1). */
export const REGION_HINTS: Record<string, readonly [number, number]> = {
  cn: [0.78, 0.42], china: [0.78, 0.42],
  hk: [0.79, 0.5], tw: [0.82, 0.5],
  jp: [0.86, 0.42], japan: [0.86, 0.42],
  kr: [0.83, 0.4], korea: [0.83, 0.4],
  sg: [0.77, 0.62], singapore: [0.77, 0.62],
  in: [0.69, 0.5], india: [0.69, 0.5],
  ae: [0.62, 0.5], au: [0.88, 0.78], australia: [0.88, 0.78],
  ru: [0.7, 0.28], russia: [0.7, 0.28],
  de: [0.49, 0.32], germany: [0.49, 0.32], eu: [0.5, 0.34],
  fr: [0.48, 0.34], uk: [0.46, 0.3], gb: [0.46, 0.3],
  nl: [0.49, 0.31], es: [0.46, 0.4], it: [0.5, 0.38],
  us: [0.22, 0.4], usa: [0.22, 0.4],
  ca: [0.22, 0.28], canada: [0.22, 0.28],
  br: [0.34, 0.7], brazil: [0.34, 0.7],
  ar: [0.32, 0.82], mx: [0.18, 0.5],
  za: [0.55, 0.74], ng: [0.5, 0.6], eg: [0.55, 0.5],
};

/** Flag emoji per region key; falls back to a globe. */
export const COUNTRY_FLAGS: Record<string, string> = {
  cn: '🇨🇳', hk: '🇭🇰', tw: '🇹🇼', jp: '🇯🇵', kr: '🇰🇷', sg: '🇸🇬',
  in: '🇮🇳', ae: '🇦🇪', au: '🇦🇺', ru: '🇷🇺', de: '🇩🇪', fr: '🇫🇷',
  uk: '🇬🇧', gb: '🇬🇧', nl: '🇳🇱', es: '🇪🇸', it: '🇮🇹', us: '🇺🇸',
  usa: '🇺🇸', ca: '🇨🇦', br: '🇧🇷', ar: '🇦🇷', mx: '🇲🇽', za: '🇿🇦',
  ng: '🇳🇬', eg: '🇪🇬',
};

export function hashString(value: string): number {
  let h = 5381;
  for (let i = 0; i < value.length; i++) {
    h = ((h << 5) + h + value.charCodeAt(i)) | 0;
  }
  return Math.abs(h);
}

export function clamp01(v: number): number {
  return Math.min(0.98, Math.max(0.02, v));
}

export function nodeRegionKey(node: NodeListItem): string | null {
  const tags = node.identity.tags || [];
  for (const tag of tags) {
    const lower = String(tag).toLowerCase();
    if (REGION_HINTS[lower]) return lower;
    const m = lower.match(/^(?:country|region|cc|loc)[:=](\w+)$/);
    if (m && m[1] && REGION_HINTS[m[1]]) return m[1];
  }
  const hostname = String(node.identity.hostname || '').toLowerCase();
  for (const key of Object.keys(REGION_HINTS)) {
    if (
      hostname.includes(`-${key}-`) ||
      hostname.startsWith(`${key}-`) ||
      hostname.endsWith(`-${key}`)
    ) {
      return key;
    }
  }
  const idLower = String(node.identity.node_id || '').toLowerCase();
  for (const key of Object.keys(REGION_HINTS)) {
    if (idLower.includes(key)) return key;
  }
  return null;
}

/** Returns {x, y} as 0..1 fractions of the map stage. Deterministic per node. */
export function nodePosition(node: NodeListItem): { x: number; y: number } {
  const region = nodeRegionKey(node);
  const seed = hashString(node.identity.node_id || node.identity.node_label || '');
  if (region) {
    const anchor = REGION_HINTS[region];
    if (anchor) {
      const [x, y] = anchor;
      const jx = ((seed % 23) - 11) / 600;
      const jy = (((seed >> 5) % 19) - 9) / 600;
      return { x: clamp01(x + jx), y: clamp01(y + jy) };
    }
  }
  const x = ((seed % 1000) / 1000) * 0.7 + 0.15;
  const y = (((seed >> 7) % 1000) / 1000) * 0.6 + 0.2;
  return { x, y };
}

export function nodeFlag(node: NodeListItem): string {
  const region = nodeRegionKey(node);
  if (region && COUNTRY_FLAGS[region]) return COUNTRY_FLAGS[region];
  return '🌐';
}

export function nodeStatusKey(node: NodeListItem): NodeStatus {
  if (!node.online) return 'offline';
  if (node.latency_ms != null && node.latency_ms >= LATENCY_WARN_MS) return 'latency';
  return 'online';
}

/** Real Mercator projection (lon/lat → map pixels). Used for the land mask. */
export function mapProject(lon: number, lat: number): { x: number; y: number } {
  const safeLat = Math.max(-85, Math.min(MAP_MAX_LAT, Number(lat)));
  const latRad = (safeLat * Math.PI) / 180;
  const mercator = Math.log(Math.tan(Math.PI / 4 + latRad / 2));
  return {
    x: (Number(lon) + 180) * (MAP_WIDTH / 360),
    y: MAP_HEIGHT / 2 - (MAP_WIDTH * mercator) / (2 * Math.PI) + MAP_VERTICAL_SHIFT,
  };
}
