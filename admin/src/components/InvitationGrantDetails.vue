<script setup lang="ts">
import { grantSummary, type InvitationGrant } from '../../../shared/invitations';
defineProps<{ grant: InvitationGrant; revision: number }>();
</script>
<template>
  <el-descriptions :column="1" border class="invitation-grant-details">
    <el-descriptions-item label="权限版本">{{ revision }}</el-descriptions-item>
    <el-descriptions-item label="接入权限">{{ grantSummary(grant) }}</el-descriptions-item>
    <template v-if="grant.port_forward.enabled"
      ><el-descriptions-item label="转发入口">{{
        grant.port_forward.allowed_entry_ids.join('、')
      }}</el-descriptions-item
      ><el-descriptions-item label="允许协议">{{
        grant.port_forward.allowed_protocols.join(' / ').toUpperCase()
      }}</el-descriptions-item
      ><el-descriptions-item label="地址族">{{
        grant.port_forward.allowed_families.join(' / ')
      }}</el-descriptions-item
      ><el-descriptions-item label="目标端口">{{
        grant.port_forward.allowed_target_ports?.join('、') ?? '全部'
      }}</el-descriptions-item></template
    >
  </el-descriptions>
</template>
<style scoped>
.invitation-grant-details {
  margin: 16px;
}
</style>
