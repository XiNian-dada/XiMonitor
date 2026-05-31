<script setup lang="ts">
import { ref, watch } from 'vue';

/**
 * Text input that v-models a string[] — the array stays the canonical form in
 * the draft, while the user edits a comma-separated string. A local `text` ref
 * holds the raw input so typing (trailing commas, spaces) isn't clobbered; we
 * only resync from the model when the incoming array genuinely diverges from
 * what the text currently parses to (e.g. an external load/reset).
 */
const model = defineModel<string[]>({ default: () => [] });

withDefaults(defineProps<{ placeholder?: string }>(), { placeholder: '' });

function parse(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

// Parsed values never contain a comma, so a comma join is a safe equality key.
function key(items: string[]): string {
  return items.join(',');
}

const text = ref(model.value.join(', '));

watch(model, (next) => {
  if (key(parse(text.value)) !== key(next)) {
    text.value = next.join(', ');
  }
});

function onInput(event: Event): void {
  text.value = (event.target as HTMLInputElement).value;
  model.value = parse(text.value);
}
</script>

<template>
  <input
    class="csv-field"
    type="text"
    :value="text"
    :placeholder="placeholder"
    data-test="csv-field"
    @input="onInput"
  />
</template>

<style scoped>
.csv-field {
  width: 100%;
  background: var(--bg-card-soft);
  color: var(--text-primary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font: inherit;
}
</style>
