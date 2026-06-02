<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { useNodesStore } from '@/stores/nodes';
import { useWorldGeoJson } from '@/composables/useWorldGeoJson';
import { useTheme } from '@/composables/useTheme';
import { nodePosition, nodeStatusKey } from '@/lib/map/projection';
import { drawFallbackMask, drawGeoJsonMask, paintWorldDotMap } from '@/lib/map/landMask';

const nodesStore = useNodesStore();
const { geojson, load } = useWorldGeoJson();
const { theme } = useTheme();

const canvas = ref<HTMLCanvasElement | null>(null);

const dots = computed(() =>
  nodesStore.nodes.map((node) => {
    const pos = nodePosition(node);
    return {
      id: node.identity.node_id,
      label: node.identity.node_label,
      status: nodeStatusKey(node),
      left: `${(pos.x * 100).toFixed(2)}%`,
      top: `${(pos.y * 100).toFixed(2)}%`,
    };
  }),
);

function dotColor(): string {
  const el = canvas.value ?? document.documentElement;
  const value = getComputedStyle(el).getPropertyValue('--map-land-dot').trim();
  return value || 'rgba(148,163,184,0.58)';
}

function repaint(): void {
  const el = canvas.value;
  if (!el) return;
  const geo = geojson.value;
  paintWorldDotMap(
    el,
    geo ? (ctx) => drawGeoJsonMask(ctx, geo) : drawFallbackMask,
    dotColor(),
  );
}

onMounted(() => {
  // Paint the built-in fallback immediately, then upgrade to the fetched
  // GeoJSON when it arrives. A fetch failure simply keeps the fallback.
  repaint();
  void load();
});

// Re-render the land mask when the real GeoJSON resolves, and again on
// theme change — the land-dot colour comes from the --map-land-dot CSS var,
// which is read at paint time (matches legacy's repaint-on-toggle).
watch([geojson, theme], repaint);
</script>

<template>
  <article class="panel map-card" data-test="node-map">
    <div class="panel-head">
      <div class="panel-title">
        <span>{{ $t('index.map.title') }}</span>
      </div>
      <div class="map-legend">
        <span class="legend-online">
          <span class="legend-swatch" />{{ $t('index.map.legend_online') }}
        </span>
        <span class="legend-latency">
          <span class="legend-swatch" />{{ $t('index.map.legend_latency') }}
        </span>
        <span class="legend-offline">
          <span class="legend-swatch" />{{ $t('index.map.legend_offline') }}
        </span>
      </div>
    </div>
    <div class="map-stage">
      <div class="map-grid" />
      <canvas ref="canvas" class="map-canvas" width="1200" height="600" aria-hidden="true" />
      <div class="map-dots" data-test="map-dots">
        <div
          v-for="dot in dots"
          :key="dot.id"
          class="map-dot"
          :class="dot.status"
          :style="{ left: dot.left, top: dot.top }"
          :title="dot.label"
          data-test="map-dot"
        />
      </div>
    </div>
  </article>
</template>

<style scoped>
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 14px;
}
.panel-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}
.map-legend {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
  color: var(--text-muted);
  font-size: 12px;
}
.map-legend span {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.legend-swatch {
  width: 8px;
  height: 8px;
  border-radius: 50%;
}
.legend-online .legend-swatch {
  background: var(--accent-green);
}
.legend-latency .legend-swatch {
  background: var(--accent-yellow);
}
.legend-offline .legend-swatch {
  background: var(--accent-red);
}
.map-stage {
  position: relative;
  aspect-ratio: 16 / 8;
  min-height: 280px;
  background:
    radial-gradient(circle at 20% 30%, rgba(59, 130, 246, 0.09), transparent 55%),
    radial-gradient(circle at 78% 62%, rgba(34, 197, 94, 0.06), transparent 52%),
    linear-gradient(135deg, rgba(15, 23, 42, 0.18), rgba(30, 41, 59, 0.04));
  border-radius: 14px;
  overflow: hidden;
}
@media (max-width: 640px) {
  .map-stage {
    aspect-ratio: 16 / 10;
    min-height: 200px;
  }
}
.map-grid {
  position: absolute;
  inset: 0;
  background-image:
    linear-gradient(rgba(255, 255, 255, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(255, 255, 255, 0.04) 1px, transparent 1px);
  background-size: 5% 10%;
}
.map-canvas {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  filter: drop-shadow(0 18px 36px rgba(0, 0, 0, 0.2));
}
.map-dots {
  position: absolute;
  inset: 0;
}
.map-dot {
  position: absolute;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  transform: translate(-50%, -50%);
  box-shadow:
    0 0 0 3px rgba(0, 0, 0, 0.34),
    0 0 18px currentColor;
}
.map-dot::after {
  content: '';
  position: absolute;
  inset: -5px;
  border-radius: 50%;
  animation: dotPulse 2.4s infinite ease-out;
  background: currentColor;
  opacity: 0.14;
}
.map-dot.online {
  color: var(--accent-green);
  background: var(--accent-green);
}
.map-dot.latency {
  color: var(--accent-yellow);
  background: var(--accent-yellow);
}
.map-dot.offline {
  color: var(--accent-red);
  background: var(--accent-red);
}
@keyframes dotPulse {
  0% {
    transform: scale(0.6);
    opacity: 0.45;
  }
  100% {
    transform: scale(1.6);
    opacity: 0;
  }
}
</style>
