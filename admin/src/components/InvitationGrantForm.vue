<script setup lang="ts">
import { ref, watch } from 'vue';
import type { InvitationGrant } from '../../../shared/invitations';
import type { PortForwardEntry } from '../types';
const model = defineModel<InvitationGrant>({ required: true });
defineProps<{ entries: PortForwardEntry[] }>();
const portError = ref('');
const ports = ref('');
watch(
  () => model.value,
  () => {
    ports.value = model.value.port_forward.allowed_target_ports?.join(', ') ?? '';
    portError.value = '';
  },
  { immediate: true },
);
watch(ports, (v) => {
  if (!v.trim()) {
    model.value.port_forward.allowed_target_ports = null;
    portError.value = '';
    return;
  }
  const list = v
    .split(/[,，\s]+/)
    .filter(Boolean)
    .map(Number);
  if (list.some((p) => !Number.isInteger(p) || p < 1 || p > 65535)) {
    portError.value = '目标端口需为 1 至 65535 的整数';
    return;
  }
  portError.value = '';
  model.value.port_forward.allowed_target_ports = [...new Set(list)];
});
defineExpose({ valid: () => !model.value.port_forward.enabled || !portError.value });
</script>
<template>
  <el-divider content-position="left">受邀用户权限</el-divider>
  <el-form-item label="接入模式"
    ><div class="capability-grid">
      <el-checkbox v-model="model.allow_ipv4_split">IPv4 境外分流</el-checkbox
      ><el-checkbox v-model="model.allow_ipv4_global">IPv4 全局</el-checkbox>
      <el-checkbox v-model="model.allow_ipv6_split">IPv6 境外分流</el-checkbox
      ><el-checkbox v-model="model.allow_ipv6_global">IPv6 全局</el-checkbox>
    </div></el-form-item
  >
  <el-form-item label="端口转发"><el-switch v-model="model.port_forward.enabled" /></el-form-item>
  <template v-if="model.port_forward.enabled">
    <el-form-item label="转发额度" required
      ><el-input-number
        v-model="model.port_forward.max_active_forwards"
        :min="1"
        :max="1024"
        :precision="0"
    /></el-form-item>
    <el-form-item label="可用入口" required
      ><el-select
        v-model="model.port_forward.allowed_entry_ids"
        multiple
        filterable
        placeholder="选择入口"
        ><el-option
          v-for="entry in entries"
          :key="entry.entry_id"
          :label="entry.name"
          :value="entry.entry_id"
          :disabled="!entry.enabled" /></el-select
    ></el-form-item>
    <el-form-item label="允许协议"
      ><el-checkbox-group v-model="model.port_forward.allowed_protocols"
        ><el-checkbox value="tcp">TCP</el-checkbox
        ><el-checkbox value="udp">UDP</el-checkbox></el-checkbox-group
      ></el-form-item
    >
    <el-form-item label="地址族"
      ><el-checkbox-group v-model="model.port_forward.allowed_families"
        ><el-checkbox value="ipv4">IPv4</el-checkbox
        ><el-checkbox value="ipv6">IPv6</el-checkbox></el-checkbox-group
      ></el-form-item
    >
    <el-form-item label="目标端口" :error="portError"
      ><el-input v-model="ports" placeholder="留空允许全部；多个端口用逗号分隔"
    /></el-form-item>
  </template>
  <el-text type="info" size="small">新用户为普通账户，不包含邀请资格。</el-text>
</template>
