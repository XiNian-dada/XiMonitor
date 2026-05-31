<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { blankRule, type RuleDraft } from '@/lib/alertsDraft';
import RuleEditorCard from './RuleEditorCard.vue';

/**
 * The rule collection editor. Binds the parent's reactive `draft.rules` array
 * and mutates it in place (push/splice) — the reactive draft is the single
 * source of truth, so add/remove never loses sibling edits. Each card is keyed
 * by the stable `uid` (not the array index, and not the user-editable `id`) so
 * Vue keeps each editor's local DOM state when rules are added or removed.
 */
const rules = defineModel<RuleDraft[]>({ required: true });

const { t } = useI18n();

function add(): void {
  rules.value.push(blankRule());
}

function remove(index: number): void {
  rules.value.splice(index, 1);
}

// Fired only if a card reassigns its whole model; field edits mutate in place.
function update(index: number, next: RuleDraft): void {
  rules.value[index] = next;
}
</script>

<template>
  <article class="panel rules" data-test="rule-list">
    <header class="rules-head">
      <div class="rules-intro">
        <h2 class="card-title">{{ t('alerts.rules.title') }}</h2>
        <p class="rules-note">{{ t('alerts.rules.note') }}</p>
      </div>
      <button type="button" class="btn" data-test="rule-add" @click="add">
        {{ t('alerts.rules.add') }}
      </button>
    </header>

    <p v-if="!rules.length" class="rules-empty" data-test="rule-list-empty">
      {{ t('alerts.rules.empty') }}
    </p>
    <div v-else class="rules-items">
      <RuleEditorCard
        v-for="(rule, index) in rules"
        :key="rule.uid"
        :model-value="rule"
        @update:model-value="(next) => update(index, next)"
        @remove="remove(index)"
      />
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
.rules-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}
.rules-intro {
  min-width: 0;
}
.card-title {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.rules-note {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 12px;
}
.rules-empty {
  margin: 14px 0 0;
  color: var(--text-muted);
  font-size: 13px;
}
.rules-items {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-top: 14px;
}
.btn {
  flex-shrink: 0;
  background: var(--bg-card-soft);
  color: var(--text-secondary);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  padding: 8px 14px;
  font: inherit;
}
</style>
