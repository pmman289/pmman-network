<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { Search } from '@element-plus/icons-vue';
import { api } from '../api/admin';
import { useAdminStore } from '../stores/admin';
import type { Device } from '../types';
import {
  usePagination,
  useMutation,
  confirmAction,
  formatLastActivity,
  trafficLabel,
} from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';
import ListPagination from '../components/ListPagination.vue';
const store = useAdminStore();
const filters = reactive({ query: '', status: '', node: '' });
const filtered = computed(() =>
  store.devices.filter(
    (d) =>
      `${d.username} ${d.device_id} ${d.ipv4} ${d.ipv6}`
        .toLowerCase()
        .includes(filters.query.trim().toLowerCase()) &&
      (!filters.status || d.status === filters.status) &&
      (!filters.node || d.node_id === filters.node),
  ),
);
const { page, size, rows } = usePagination(filtered);
const { saving, run } = useMutation();
const detailId = ref('');
const detail = computed(() => store.devices.find((d) => d.device_id === detailId.value));
const drawer = ref(false);
function statusLabel(status: Device['status']) {
  return { active: '连接中', revoked: '已撤销', released: '已释放' }[status];
}
function bytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MB`;
  return `${(value / 1024 ** 3).toFixed(2)} GB`;
}
function pathTraffic(device: Device, path: 'node' | 'direct' | 'relay') {
  const t = device.traffic;
  if (path === 'node') return bytes(t.nodeRxBytes + t.nodeTxBytes);
  if (path === 'direct') return bytes(t.p2pDirectRxBytes + t.p2pDirectTxBytes);
  return bytes(t.p2pRelayRxBytes + t.p2pRelayTxBytes);
}
async function revoke(device: Device) {
  if (saving.value || device.status !== 'active') return;
  if (
    !(await confirmAction(
      `撤销用户 ${device.username} 的此设备连接？客户端将在下一次心跳时断开，地址租约保留供再次连接。`,
    ))
  )
    return;
  await run(() => api.revoke(device.device_id), '连接已撤销，客户端将自动清理隧道');
}
function showDetail(device: Device) {
  detailId.value = device.device_id;
  drawer.value = true;
}
</script>
<template>
  <PageHeading title="设备与地址" description="查看设备、地址分配和连接状态" />
  <el-card shadow="never"
    ><div class="filter-bar">
      <el-input
        v-model="filters.query"
        :prefix-icon="Search"
        placeholder="搜索用户、设备 ID 或 IP"
        aria-label="搜索设备"
        clearable
      /><el-select v-model="filters.status" placeholder="全部状态" aria-label="设备状态" clearable
        ><el-option label="连接中" value="active" /><el-option
          label="已撤销"
          value="revoked" /><el-option label="已释放" value="released" /></el-select
      ><el-select v-model="filters.node" placeholder="全部节点" aria-label="连接节点" clearable
        ><el-option
          v-for="node in store.nodes"
          :key="node.nodeId"
          :label="node.name"
          :value="node.nodeId" /></el-select
      ><el-button @click="Object.assign(filters, { query: '', status: '', node: '' })"
        >重置</el-button
      >
    </div>
    <el-table
      v-loading="store.loading"
      :data="rows"
      row-key="device_id"
      empty-text="暂无符合条件的设备租约"
      ><el-table-column prop="username" label="用户" min-width="140" /><el-table-column
        prop="node_id"
        label="连接节点"
        min-width="140"
      /><el-table-column label="状态" width="110"
        ><template #default="{ row }"
          ><el-tag :type="row.status === 'active' ? 'success' : 'info'">{{
            statusLabel(row.status)
          }}</el-tag></template
        ></el-table-column
      ><el-table-column prop="ipv4" label="IPv4" min-width="150" /><el-table-column
        prop="ipv6"
        label="IPv6"
        min-width="200"
      /><el-table-column label="流量模式" min-width="200"
        ><template #default="{ row }"
          ><div class="cell-secondary">IPv4 · {{ trafficLabel(row.ipv4_mode) }}</div>
          <div class="cell-secondary">IPv6 · {{ trafficLabel(row.ipv6_mode) }}</div></template
        ></el-table-column
      ><el-table-column label="累计流量" min-width="220"
        ><template #default="{ row }"
          ><div class="cell-secondary">节点 {{ pathTraffic(row, 'node') }}</div>
          <div class="cell-secondary">
            P2P 直连 {{ pathTraffic(row, 'direct') }} · 中转 {{ pathTraffic(row, 'relay') }}
          </div></template
        ></el-table-column
      ><el-table-column label="最后活动" min-width="180"
        ><template #default="{ row }">{{
          formatLastActivity(row.last_seen_at)
        }}</template></el-table-column
      ><el-table-column label="操作" width="160" fixed="right"
        ><template #default="{ row }"
          ><el-button type="primary" link @click="showDetail(row)">详情</el-button
          ><el-button
            type="danger"
            link
            :disabled="row.status !== 'active' || saving"
            @click="revoke(row)"
            >撤销连接</el-button
          ></template
        ></el-table-column
      ></el-table
    ><ListPagination v-model:page="page" v-model:size="size" :total="filtered.length" />
  </el-card>
  <el-drawer v-model="drawer" title="设备详情" size="560px"
    ><el-descriptions v-if="detail" :column="1" border
      ><el-descriptions-item label="设备 ID"
        ><span class="break-all">{{ detail.device_id }}</span></el-descriptions-item
      ><el-descriptions-item label="用户名">{{ detail.username }}</el-descriptions-item
      ><el-descriptions-item label="连接节点">{{ detail.node_id }}</el-descriptions-item
      ><el-descriptions-item label="状态"
        ><el-tag :type="detail.status === 'active' ? 'success' : 'info'">{{
          statusLabel(detail.status)
        }}</el-tag></el-descriptions-item
      ><el-descriptions-item label="IPv4">{{ detail.ipv4 }}/32</el-descriptions-item
      ><el-descriptions-item label="IPv6">{{ detail.ipv6 }}/128</el-descriptions-item
      ><el-descriptions-item label="客户端公钥"
        ><code class="break-all">{{ detail.public_key }}</code></el-descriptions-item
      ><el-descriptions-item label="配置版本">v{{ detail.config_version }}</el-descriptions-item
      ><el-descriptions-item label="最后活动">{{
        formatLastActivity(detail.last_seen_at)
      }}</el-descriptions-item
      ><el-descriptions-item label="累计流量"
        ><div>节点：{{ pathTraffic(detail, 'node') }}</div>
        <div>P2P 直连：{{ pathTraffic(detail, 'direct') }}</div>
        <div>P2P 中转：{{ pathTraffic(detail, 'relay') }}</div></el-descriptions-item
      ></el-descriptions
    ><el-empty v-else description="记录已不存在，请刷新列表" /><template #footer
      ><el-button @click="drawer = false">关闭</el-button
      ><el-button
        v-if="detail?.status === 'active'"
        type="danger"
        :loading="saving"
        @click="revoke(detail)"
        >撤销连接</el-button
      ></template
    ></el-drawer
  >
</template>
