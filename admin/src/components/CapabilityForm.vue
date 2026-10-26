<script setup lang="ts">
import { capabilityOptions, type Capabilities } from '../types';
const model = defineModel<Capabilities>({ required: true });
</script>
<template>
  <div class="capability-grid">
    <el-checkbox
      v-for="option in capabilityOptions"
      :key="option.key"
      v-model="model[option.key]"
      >{{ option.label }}</el-checkbox
    >
  </div>
</template>
