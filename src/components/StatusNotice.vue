<script setup lang="ts">
import { computed } from "vue";
import { CircleHelp, TriangleAlert, X } from "@lucide/vue";
import { messageSummary } from "../utils/presentation";
const props = withDefaults(
  defineProps<{
    text: string;
    kind?: "error" | "info" | "warning";
    summaryText?: string;
    closable?: boolean;
  }>(),
  { kind: "error", closable: false },
);
defineEmits<{ close: [] }>();
const summary = computed(
  () =>
    props.summaryText ??
    messageSummary(
      props.text,
      props.kind === "error" ? "操作失败，请重试。" : "操作已完成。",
    ),
);
</script>

<template>
  <div
    :class="['inline-notice', kind]"
    :role="kind === 'error' ? 'alert' : 'status'"
  >
    <TriangleAlert v-if="kind === 'warning'" :size="18" />
    <CircleHelp v-else :size="18" />
    <div class="notice-content">
      <p>{{ summary }}</p>
      <details v-if="summary !== text" class="diagnostic-disclosure">
        <summary>查看详情</summary>
        <pre>{{ text }}</pre>
      </details>
    </div>
    <button
      v-if="closable"
      type="button"
      class="icon-button"
      aria-label="关闭提示"
      @click="$emit('close')"
    >
      <X :size="16" />
    </button>
  </div>
</template>
