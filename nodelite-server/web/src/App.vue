<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue';
import { useWebSocket } from '@/ws';

// App is a thin shell now — layout + data wiring live in the route views.
// Theme/24h bootstrap runs from the inline shim in index.html before this
// module loads (see src/composables/useTheme.ts, src/auth/expiry.ts).

// WebSocket singleton: owned by App.vue, persists across route changes.
// Views subscribe to messages via ws.on() in their own setup().
const ws = useWebSocket();

onMounted(() => {
  ws.connect();
});

onUnmounted(() => {
  ws.destroy();
});
</script>

<template>
  <RouterView />
</template>
