<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue';
import { Delete, Edit, Plus, Refresh } from '@element-plus/icons-vue';
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus';
import { api } from '../api/admin';
import { useAdminStore } from '../stores/admin';
import type {
  Node,
  PortForward,
  PortForwardEntry,
  PortForwardProtocol,
  PortForwardRange,
} from '../types';
import { formatDate } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';
import ListPagination from '../components/ListPagination.vue';

const props = defineProps<{ section: 'entries' | 'allocations' }>();
const isEntries = computed(() => props.section === 'entries');

const store = useAdminStore();
const entries = ref<PortForwardEntry[]>([]);
const allocations = ref<PortForward[]>([]);
const loading = ref(false);
const dialogVisible = ref(false);
const saving = ref(false);
let refreshTimer: ReturnType<typeof setInterval> | undefined;
const editing = ref<PortForwardEntry | null>(null);
const formRef = ref<FormInstance>();
const filters = reactive({ query: '', state: '', node: '', entry: '' });
const entryQuery = ref('');
const entryPage = ref(1);
const allocationPage = ref(1);
const pageSize = ref(20);
const filteredEntries = computed(() =>
  entries.value.filter((entry) =>
    `${entry.name} ${entry.entry_id} ${entry.public_address} ${entry.hostname ?? ''}`
      .toLowerCase()
      .includes(entryQuery.value.trim().toLowerCase()),
  ),
);
const entryRows = computed(() =>
  filteredEntries.value.slice(
    (entryPage.value - 1) * pageSize.value,
    entryPage.value * pageSize.value,
  ),
);
const allocationRows = computed(() =>
  filteredAllocations.value.slice(
    (allocationPage.value - 1) * pageSize.value,
    allocationPage.value * pageSize.value,
  ),
);
watch(entryQuery, () => {
  entryPage.value = 1;
});
watch(filters, () => {
  allocationPage.value = 1;
});
watch(pageSize, () => {
  entryPage.value = 1;
  allocationPage.value = 1;
});
watch(
  () => filteredEntries.value.length,
  (count) => {
    entryPage.value = Math.min(entryPage.value, Math.max(1, Math.ceil(count / pageSize.value)));
  },
);
watch(
  () => filteredAllocations.value.length,
  (count) => {
    allocationPage.value = Math.min(
      allocationPage.value,
      Math.max(1, Math.ceil(count / pageSize.value)),
    );
  },
);
const form = reactive({
  entry_id: '',
  node_id: '',
  name: '',
  public_address: '',
  hostname: '',
  family: 'ipv4' as 'ipv4' | 'ipv6',
  protocols: ['tcp'] as PortForwardProtocol[],
  port_ranges: '20000-20999',
  reserved_ports: '',
  enabled: true,
});
const rules: FormRules = {
  entry_id: [
    { required: true, message: '请输入入口 ID', trigger: 'blur' },
    {
      pattern: /^[A-Za-z0-9_.-]{1,64}$/,
      message: '仅支持字母、数字、点、下划线和短横线',
      trigger: 'blur',
    },
  ],
  node_id: [{ required: true, message: '请选择节点', trigger: 'change' }],
  name: [{ required: true, message: '请输入入口名称', trigger: 'blur' }],
  public_address: [{ required: true, message: '请输入公网地址', trigger: 'blur' }],
  port_ranges: [{ required: true, message: '请输入端口范围', trigger: 'blur' }],
};
const nodes = computed(() => store.nodes.filter((node) => node.registered));
const filteredAllocations = computed(() =>
  allocations.value.filter((row) => {
    const query = filters.query.trim().toLowerCase();
    return (
      (!query ||
        `${row.username} ${row.public_address} ${row.forward_id} ${row.target_address}`
          .toLowerCase()
          .includes(query)) &&
      (!filters.state || row.state === filters.state) &&
      (!filters.node || row.node_id === filters.node) &&
      (!filters.entry || row.entry_id === filters.entry)
    );
  }),
);

function parseRanges(value: string): PortForwardRange[] {
  const result = value
    .split(/[\s,;]+/)
    .filter(Boolean)
    .map((part) => {
      const [start, end = start] = part.split('-', 2).map((item) => Number(item));
      if (
        !Number.isInteger(start) ||
        !Number.isInteger(end) ||
        start < 1 ||
        end > 65535 ||
        start > end
      )
        throw new Error(`端口范围无效：${part}`);
      return { start, end };
    })
    .sort((left, right) => left.start - right.start);
  for (let index = 1; index < result.length; index += 1) {
    if (result[index].start <= result[index - 1].end) throw new Error('端口范围不能重叠');
  }
  return result;
}
function parsePorts(value: string) {
  return [
    ...new Set(
      value
        .split(/[\s,;]+/)
        .filter(Boolean)
        .map(Number),
    ),
  ]
    .map((port) => {
      if (!Number.isInteger(port) || port < 1 || port > 65535)
        throw new Error(`保留端口无效：${port}`);
      return port;
    })
    .sort((left, right) => left - right);
}
function protocolEnabled(protocol: PortForwardProtocol) {
  return form.protocols.includes(protocol);
}
function toggleProtocol(protocol: PortForwardProtocol) {
  form.protocols = form.protocols.includes(protocol)
    ? form.protocols.filter((item) => item !== protocol)
    : [...form.protocols, protocol];
}
function openCreate() {
  editing.value = null;
  Object.assign(form, {
    entry_id: '',
    node_id: nodes.value[0]?.nodeId ?? '',
    name: '',
    public_address: '',
    hostname: '',
    family: 'ipv4',
    protocols: ['tcp'],
    port_ranges: '20000-20999',
    reserved_ports: '',
    enabled: true,
  });
  dialogVisible.value = true;
}
function openEdit(entry: PortForwardEntry) {
  editing.value = entry;
  Object.assign(form, {
    entry_id: entry.entry_id,
    node_id: entry.node_id,
    name: entry.name,
    public_address: entry.public_address,
    hostname: entry.hostname ?? '',
    family: entry.family,
    protocols: [...entry.protocols],
    port_ranges: entry.port_ranges
      .map((range) =>
        range.start === range.end ? `${range.start}` : `${range.start}-${range.end}`,
      )
      .join(', '),
    reserved_ports: entry.reserved_ports.join(', '),
    enabled: entry.enabled,
  });
  dialogVisible.value = true;
}
async function load() {
  if (loading.value) return;
  loading.value = true;
  try {
    [entries.value, allocations.value] = await Promise.all([
      api.portForwardEntries(),
      api.portForwardAllocations(),
    ]);
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '端口转发数据读取失败');
  } finally {
    loading.value = false;
  }
}
async function save() {
  if (!(await formRef.value?.validate().catch(() => false))) return;
  if (!form.protocols.length) {
    ElMessage.warning('至少选择一个协议');
    return;
  }
  saving.value = true;
  try {
    const payload = {
      entry_id: form.entry_id.trim(),
      node_id: form.node_id,
      name: form.name.trim(),
      public_address: form.public_address.trim(),
      hostname: form.hostname.trim() || null,
      family: form.family,
      protocols: form.protocols,
      port_ranges: parseRanges(form.port_ranges),
      reserved_ports: parsePorts(form.reserved_ports),
      enabled: form.enabled,
    };
    if (editing.value) await api.editPortForwardEntry(editing.value.entry_id, payload);
    else await api.savePortForwardEntry(payload);
    ElMessage.success('入口配置已保存');
    dialogVisible.value = false;
    await load();
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '入口保存失败');
  } finally {
    saving.value = false;
  }
}
async function removeEntry(entry: PortForwardEntry) {
  try {
    await ElMessageBox.confirm(`删除入口“${entry.name}”后，新的转发将不能使用它。`, '删除入口', {
      type: 'warning',
      confirmButtonText: '删除',
      cancelButtonText: '取消',
    });
    await api.deletePortForwardEntry(entry.entry_id);
    ElMessage.success('入口已删除');
    await load();
  } catch (cause) {
    if (cause instanceof Error) ElMessage.error(cause.message);
  }
}
async function revoke(row: PortForward) {
  try {
    await ElMessageBox.confirm(
      `确认释放 ${row.username} 的公网端口 ${row.public_port}？`,
      '强制释放转发',
      { type: 'warning', confirmButtonText: '释放', cancelButtonText: '取消' },
    );
    await api.revokePortForward(row.forward_id);
    ElMessage.success('转发已释放');
    await load();
  } catch (cause) {
    if (cause instanceof Error) ElMessage.error(cause.message);
  }
}
function nodeName(nodeId: string) {
  return nodes.value.find((node: Node) => node.nodeId === nodeId)?.name ?? nodeId;
}
function stateType(state: string) {
  return state === 'active'
    ? 'success'
    : state === 'error'
      ? 'danger'
      : state === 'released' || state === 'expired'
        ? 'info'
        : 'warning';
}
function stateLabel(state: string) {
  return (
    (
      {
        pending: '待处理',
        applying: '启用中',
        active: '已启用',
        error: '异常',
        releasing: '释放中',
        released: '已释放',
        expired: '已过期',
      } as Record<string, string>
    )[state] ?? '未知状态'
  );
}
onMounted(() => {
  void load();
  refreshTimer = setInterval(() => {
    if (!loading.value && !saving.value) void load();
  }, 5000);
});
onUnmounted(() => {
  if (refreshTimer) clearInterval(refreshTimer);
});
</script>

<template>
  <PageHeading
    :title="isEntries ? '公网入口' : '转发租约'"
    :description="isEntries ? '管理节点入口和可用端口范围' : '查看端口分配、运行状态和节点错误'"
  >
    <el-button :icon="Refresh" :loading="loading" @click="load">刷新</el-button>
    <el-button v-if="isEntries" type="primary" :icon="Plus" @click="openCreate">新增入口</el-button>
  </PageHeading>
  <el-card v-if="isEntries" shadow="never">
    <template #header
      ><div class="card-heading">
        <span>公网入口</span><span class="muted">端口范围按入口独立分配</span>
      </div></template
    >
    <div class="filter-bar">
      <el-input
        v-model="entryQuery"
        clearable
        placeholder="搜索入口名称、地址或 ID"
        aria-label="搜索公网入口"
      /><el-button @click="entryQuery = ''">重置</el-button>
    </div>
    <el-table
      v-loading="loading"
      :data="entryRows"
      row-key="entry_id"
      empty-text="暂无符合条件的入口"
    >
      <el-table-column label="入口" min-width="220"
        ><template #default="{ row }"
          ><div class="cell-primary">{{ row.name }}</div>
          <div class="cell-secondary">
            {{ row.entry_id }} · {{ row.hostname || row.public_address }}
          </div></template
        ></el-table-column
      >
      <el-table-column label="节点" min-width="150"
        ><template #default="{ row }"
          >{{ nodeName(row.node_id)
          }}<el-tag
            size="small"
            :type="row.node_healthy ? 'success' : 'danger'"
            effect="plain"
            class="inline-tag"
            >{{ row.node_healthy ? '在线' : '离线' }}</el-tag
          ></template
        ></el-table-column
      >
      <el-table-column label="协议" width="110"
        ><template #default="{ row }">{{
          row.protocols.join(' / ').toUpperCase()
        }}</template></el-table-column
      >
      <el-table-column label="端口资源" min-width="180"
        ><template #default="{ row }"
          ><div>{{ row.allocated_ports }} / {{ row.total_ports }} 已分配</div>
          <el-progress
            :percentage="
              row.total_ports ? Math.round((row.allocated_ports * 100) / row.total_ports) : 0
            "
            :show-text="false" /></template
      ></el-table-column>
      <el-table-column label="状态" width="90"
        ><template #default="{ row }"
          ><el-tag :type="row.enabled ? 'success' : 'info'">{{
            row.enabled ? '启用' : '停用'
          }}</el-tag></template
        ></el-table-column
      >
      <el-table-column label="操作" width="150" fixed="right"
        ><template #default="{ row }"
          ><el-button link type="primary" :icon="Edit" @click="openEdit(row)">编辑</el-button
          ><el-button link type="danger" :icon="Delete" @click="removeEntry(row)"
            >删除</el-button
          ></template
        ></el-table-column
      >
    </el-table>
    <ListPagination
      v-model:page="entryPage"
      v-model:size="pageSize"
      :total="filteredEntries.length"
    />
  </el-card>
  <el-card v-else shadow="never">
    <template #header
      ><div class="card-heading">
        <span>端口分配</span><span class="muted">状态随节点心跳更新</span>
      </div></template
    >
    <div class="filter-bar">
      <el-input
        v-model="filters.query"
        clearable
        placeholder="搜索用户、公网地址或转发 ID"
      /><el-select v-model="filters.state" clearable placeholder="全部状态"
        ><el-option label="已启用" value="active" /><el-option
          label="应用中"
          value="applying" /><el-option label="异常" value="error" /><el-option
          label="已释放"
          value="released" /><el-option label="已过期" value="expired"
      /></el-select>
      <el-select v-model="filters.node" clearable placeholder="全部节点" aria-label="筛选节点"
        ><el-option
          v-for="node in nodes"
          :key="node.nodeId"
          :label="node.name"
          :value="node.nodeId"
      /></el-select>
      <el-select v-model="filters.entry" clearable placeholder="全部入口" aria-label="筛选入口"
        ><el-option
          v-for="entry in entries"
          :key="entry.entry_id"
          :label="entry.name"
          :value="entry.entry_id"
      /></el-select>
      <el-button @click="Object.assign(filters, { query: '', state: '', node: '', entry: '' })"
        >重置</el-button
      >
    </div>
    <el-table
      v-loading="loading"
      :data="allocationRows"
      row-key="forward_id"
      empty-text="暂无端口分配"
    >
      <el-table-column label="公网入口" min-width="190"
        ><template #default="{ row }"
          ><div class="cell-primary">
            {{ row.public_host || row.public_address }}:{{ row.public_port }}
          </div>
          <div class="cell-secondary">
            {{ row.protocols.join(' / ').toUpperCase() }} · {{ row.entry_id }}
          </div></template
        ></el-table-column
      >
      <el-table-column label="用户 / 设备" min-width="170"
        ><template #default="{ row }"
          ><div>{{ row.username }}</div>
          <div class="cell-secondary">{{ row.device_id }}</div></template
        ></el-table-column
      >
      <el-table-column label="目标" min-width="180"
        ><template #default="{ row }"
          >{{ row.target_address }}:{{ row.target_port }}</template
        ></el-table-column
      >
      <el-table-column label="状态" width="110"
        ><template #default="{ row }"
          ><el-tag :type="stateType(row.state)">{{ stateLabel(row.state) }}</el-tag>
          <el-tooltip v-if="row.error_message" :content="row.error_message" placement="top"
            ><div class="cell-secondary allocation-error">
              {{ row.error_message }}
            </div></el-tooltip
          ></template
        ></el-table-column
      >
      <el-table-column label="更新时间" width="175"
        ><template #default="{ row }">{{ formatDate(row.updated_at) }}</template></el-table-column
      >
      <el-table-column label="操作" width="100" fixed="right"
        ><template #default="{ row }"
          ><el-button
            v-if="!['released', 'expired'].includes(row.state)"
            link
            type="danger"
            @click="revoke(row)"
            >释放</el-button
          ></template
        ></el-table-column
      >
    </el-table>
    <ListPagination
      v-model:page="allocationPage"
      v-model:size="pageSize"
      :total="filteredAllocations.length"
    />
  </el-card>
  <el-dialog
    v-model="dialogVisible"
    :title="editing ? '编辑入口' : '新增入口'"
    width="680px"
    destroy-on-close
  >
    <el-form ref="formRef" :model="form" :rules="rules" label-position="top">
      <el-row :gutter="16"
        ><el-col :span="12"
          ><el-form-item label="入口 ID" prop="entry_id"
            ><el-input
              v-model="form.entry_id"
              :disabled="!!editing || saving"
              placeholder="cn-gz02-v4-main" /></el-form-item></el-col
        ><el-col :span="12"
          ><el-form-item label="节点" prop="node_id"
            ><el-select v-model="form.node_id" style="width: 100%" :disabled="saving"
              ><el-option
                v-for="node in nodes"
                :key="node.nodeId"
                :label="`${node.name} (${node.nodeId})`"
                :value="node.nodeId" /></el-select></el-form-item></el-col
      ></el-row>
      <el-row :gutter="16"
        ><el-col :span="12"
          ><el-form-item label="入口名称" prop="name"
            ><el-input v-model="form.name" :disabled="saving" /></el-form-item></el-col
        ><el-col :span="12"
          ><el-form-item label="地址族"
            ><el-radio-group v-model="form.family" :disabled="saving"
              ><el-radio-button value="ipv4">IPv4</el-radio-button
              ><el-radio-button value="ipv6">IPv6</el-radio-button></el-radio-group
            ></el-form-item
          ></el-col
        ></el-row
      >
      <el-row :gutter="16"
        ><el-col :span="12"
          ><el-form-item label="公网地址" prop="public_address"
            ><el-input
              v-model="form.public_address"
              :disabled="saving"
              placeholder="203.0.113.10 或 2001:db8::10" /></el-form-item></el-col
        ><el-col :span="12"
          ><el-form-item label="域名（可选）"
            ><el-input
              v-model="form.hostname"
              :disabled="saving"
              placeholder="gz.example.com" /></el-form-item></el-col
      ></el-row>
      <el-form-item label="支持协议"
        ><el-checkbox :model-value="protocolEnabled('tcp')" @change="toggleProtocol('tcp')"
          >TCP</el-checkbox
        ><el-checkbox :model-value="protocolEnabled('udp')" @change="toggleProtocol('udp')"
          >UDP</el-checkbox
        ></el-form-item
      >
      <el-form-item label="端口范围" prop="port_ranges"
        ><el-input
          v-model="form.port_ranges"
          :disabled="saving"
          placeholder="20000-20999, 30000-30999"
        />
        <div class="form-help">多个范围用逗号或空格分隔，不能重叠。</div></el-form-item
      >
      <el-form-item label="保留端口"
        ><el-input v-model="form.reserved_ports" :disabled="saving" placeholder="20022, 20080"
      /></el-form-item>
      <div class="form-help">SSH、DNS 等已占用端口应列入保留端口。</div>
      <el-form-item label="入口状态"
        ><el-switch
          v-model="form.enabled"
          active-text="启用"
          inactive-text="停用"
          :disabled="saving"
      /></el-form-item>
    </el-form>
    <template #footer
      ><el-button @click="dialogVisible = false">取消</el-button
      ><el-button type="primary" :loading="saving" @click="save">保存</el-button></template
    >
  </el-dialog>
</template>

<style scoped>
.section-card {
  margin-bottom: 20px;
}
.inline-tag {
  margin-left: 8px;
}
.filter-bar .el-input {
  width: 320px;
}
.filter-bar .el-select {
  width: 150px;
}
.el-progress {
  margin-top: 6px;
}
.allocation-error {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
