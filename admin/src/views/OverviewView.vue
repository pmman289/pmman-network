<script setup lang="ts">
import { computed } from 'vue';
import { User, Connection, Monitor, Coin, Right } from '@element-plus/icons-vue';
import { useAdminStore } from '../stores/admin';
import PageHeading from '../components/PageHeading.vue';
import { formatLastActivity } from '../composables/useAdmin';
const store = useAdminStore();
const active = computed(() => store.devices.filter((d) => d.status === 'active').length);
const online = computed(() => store.nodes.filter((n) => n.healthy).length);
const stats = computed(() => [
  {
    label: '用户总数',
    value: store.users.length,
    note: `${store.users.filter((u) => u.enabled).length} 个已启用`,
    icon: User,
    color: 'blue',
  },
  {
    label: '在线节点',
    value: online.value,
    note: `共 ${store.nodes.length} 个连接节点`,
    icon: Connection,
    color: 'green',
  },
  {
    label: '活动连接',
    value: active.value,
    note: `共 ${store.devices.length} 个设备租约`,
    icon: Monitor,
    color: 'purple',
  },
  {
    label: '地址池',
    value: store.addressPools?.pools.length ?? '—',
    note: 'IPv4 / IPv6 独立分配',
    icon: Coin,
    color: 'amber',
  },
]);
const recent = computed(() =>
  [...store.devices].sort((a, b) => b.last_seen_at.localeCompare(a.last_seen_at)).slice(0, 5),
);
const primaryName = (family: 'ipv4' | 'ipv6') =>
  store.addressPools?.pools.find(
    (p) => p.pool_id === store.addressPools?.[family === 'ipv4' ? 'ipv4_pool_id' : 'ipv6_pool_id'],
  )?.name ?? '—';
const available = (family: 'ipv4' | 'ipv6') =>
  (
    store.addressPools?.pools.reduce((sum, p) => sum + BigInt(p[family].available), 0n) ?? 0n
  ).toString();
</script>
<template>
  <PageHeading title="运行概览" description="查看接入规模、节点状态和地址资源" />
  <div v-loading="store.loading" class="stats-grid">
    <el-card v-for="stat in stats" :key="stat.label" shadow="never" class="stat-card"
      ><div class="stat-top">
        <span>{{ stat.label }}</span
        ><el-icon :class="['stat-icon', stat.color]"><component :is="stat.icon" /></el-icon>
      </div>
      <div class="stat-value">{{ stat.value }}</div>
      <div class="muted">{{ stat.note }}</div></el-card
    >
  </div>
  <div class="overview-grid">
    <el-card shadow="never"
      ><template #header
        ><div class="card-heading">
          <span>节点状态</span
          ><el-button link type="primary" @click="$router.push('/nodes')"
            >查看节点<el-icon><Right /></el-icon
          ></el-button></div
      ></template>
      <el-empty v-if="!store.nodes.length" description="暂无连接节点" :image-size="72" />
      <div v-for="node in store.nodes" :key="node.nodeId" class="node-summary">
        <div class="node-summary-icon">
          <el-icon><Connection /></el-icon>
        </div>
        <div class="node-summary-name">
          <strong>{{ node.name }}</strong
          ><span class="muted">{{ node.region || '未设置区域' }} · {{ node.nodeId }}</span>
        </div>
        <el-tag :type="node.healthy ? 'success' : 'info'" effect="light">{{
          !node.registered ? '待接入' : !node.enabled ? '已停用' : node.healthy ? '在线' : '离线'
        }}</el-tag>
      </div>
    </el-card>
    <el-card shadow="never"
      ><template #header
        ><div class="card-heading">
          <span>地址资源</span
          ><el-button link type="primary" @click="$router.push('/address-pools')"
            >查看配置<el-icon><Right /></el-icon
          ></el-button></div
      ></template>
      <el-descriptions v-if="store.addressPools" :column="1"
        ><el-descriptions-item label="IPv4 主地址池">{{ primaryName('ipv4') }}</el-descriptions-item
        ><el-descriptions-item label="IPv6 主地址池">{{ primaryName('ipv6') }}</el-descriptions-item
        ><el-descriptions-item label="IPv4 剩余地址">{{ available('ipv4') }}</el-descriptions-item
        ><el-descriptions-item label="IPv6 剩余地址">{{
          available('ipv6')
        }}</el-descriptions-item></el-descriptions
      >
    </el-card>
  </div>
  <el-card shadow="never" class="section-card"
    ><template #header
      ><div class="card-heading">
        <span>最近活动设备</span
        ><el-button link type="primary" @click="$router.push('/devices')"
          >查看全部<el-icon><Right /></el-icon
        ></el-button></div
    ></template>
    <el-table :data="recent" row-key="device_id" empty-text="暂无设备租约"
      ><el-table-column prop="username" label="用户" min-width="130" /><el-table-column
        prop="node_id"
        label="连接节点"
        min-width="140"
      /><el-table-column label="状态" width="110"
        ><template #default="{ row }"
          ><el-tag :type="row.status === 'active' ? 'success' : 'info'">{{
            row.status === 'active' ? '连接中' : '已撤销'
          }}</el-tag></template
        ></el-table-column
      ><el-table-column prop="ipv4" label="IPv4" min-width="150" /><el-table-column
        label="最后活动"
        min-width="180"
        ><template #default="{ row }">{{
          formatLastActivity(row.last_seen_at)
        }}</template></el-table-column
      ></el-table
    >
  </el-card>
</template>
