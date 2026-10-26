<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { Edit, Refresh, Search, Setting } from '@element-plus/icons-vue';
import { ElMessage, type FormInstance, type FormRules } from 'element-plus';
import { api } from '../api/admin';
import { useAdminStore } from '../stores/admin';
import type {
  PortForward,
  PortForwardEntry,
  PortForwardPolicy,
  PortForwardProtocol,
  User,
} from '../types';
import PageHeading from '../components/PageHeading.vue';

type PolicyStatus = 'enabled' | 'disabled' | 'incomplete' | 'unknown';

const store = useAdminStore();
const entries = ref<PortForwardEntry[]>([]);
const allocations = ref<PortForward[]>([]);
const policies = reactive<Record<string, PortForwardPolicy>>({});
const policyLoading = ref(false);
const loading = ref(false);
const saving = ref(false);
const search = ref('');
const statusFilter = ref<'all' | 'enabled' | 'disabled'>('all');
const drawerVisible = ref(false);
const selectedUser = ref<User | null>(null);
const entrySearch = ref('');
const entryFamily = ref<'all' | 'ipv4' | 'ipv6'>('all');
const formRef = ref<FormInstance>();
const targetPortMode = ref<'all' | 'allowlist'>('all');
const targetPortList = ref('');
const form = reactive<PortForwardPolicy>({
  enabled: false,
  max_active_forwards: 0,
  allowed_entry_ids: [],
  allowed_protocols: ['tcp'],
  allowed_target_ports: null,
  allowed_families: ['ipv4', 'ipv6'],
});
const rules: FormRules = {
  max_active_forwards: [
    { required: true, message: '请输入最大同时转发数', trigger: 'change' },
    { type: 'number', min: 0, max: 1024, message: '请输入 0 至 1024 的整数', trigger: 'change' },
  ],
};

const users = computed(() => store.users.filter((user) => user.role === 'user'));
const filteredUsers = computed(() => {
  const query = search.value.trim().toLowerCase();
  return users.value.filter((user) => {
    const status = policyStatus(policies[user.username]);
    return (
      (!query || user.username.toLowerCase().includes(query)) &&
      (statusFilter.value === 'all' ||
        (statusFilter.value === 'enabled' && status === 'enabled') ||
        (statusFilter.value === 'disabled' && status !== 'enabled'))
    );
  });
});
const activePolicies = computed(
  () => users.value.filter((user) => policyStatus(policies[user.username]) === 'enabled').length,
);
const incompletePolicies = computed(
  () => users.value.filter((user) => policyStatus(policies[user.username]) === 'incomplete').length,
);
const enabledEntries = computed(() => entries.value.filter((entry) => entry.enabled).length);
const filteredEntries = computed(() => {
  const query = entrySearch.value.trim().toLowerCase();
  return entries.value.filter(
    (entry) =>
      (!query ||
        `${entry.name} ${entry.entry_id} ${entry.public_address}`.toLowerCase().includes(query)) &&
      (entryFamily.value === 'all' || entry.family === entryFamily.value),
  );
});
const selectedEntryCount = computed(() => form.allowed_entry_ids.length);
const editingTitle = computed(() =>
  selectedUser.value ? `配置 ${selectedUser.value.username} 的转发授权` : '配置转发授权',
);

function policyStatus(policy: PortForwardPolicy | undefined): PolicyStatus {
  if (!policy) return 'unknown';
  if (!policy.enabled || policy.max_active_forwards === 0) return 'disabled';
  if (
    !policy.allowed_entry_ids.length ||
    !policy.allowed_protocols.length ||
    !policy.allowed_families.length
  )
    return 'incomplete';
  return 'enabled';
}
function statusLabel(status: PolicyStatus) {
  return { enabled: '已启用', disabled: '未启用', incomplete: '配置不完整', unknown: '未读取' }[
    status
  ];
}
function statusType(status: PolicyStatus) {
  return status === 'enabled' ? 'success' : status === 'incomplete' ? 'warning' : 'info';
}
function activeCount(username: string) {
  return allocations.value.filter(
    (allocation) =>
      allocation.username === username &&
      ['active', 'applying', 'pending'].includes(allocation.state),
  ).length;
}
function protocolLabel(protocol: PortForwardProtocol) {
  return protocol.toUpperCase();
}
function entryLabel(entryId: string) {
  return entries.value.find((entry) => entry.entry_id === entryId)?.name ?? entryId;
}
function capacityLabel(entry: PortForwardEntry) {
  return `${entry.available_ports} / ${entry.total_ports}`;
}
function targetPortLabel(policy: PortForwardPolicy) {
  return policy.allowed_target_ports
    ? `${policy.allowed_target_ports.length} 个目标端口`
    : '全部目标端口';
}
function resetForm(policy: PortForwardPolicy) {
  Object.assign(form, {
    enabled: policy.enabled,
    max_active_forwards: policy.max_active_forwards,
    allowed_entry_ids: [...policy.allowed_entry_ids],
    allowed_protocols: [...policy.allowed_protocols],
    allowed_target_ports: policy.allowed_target_ports ? [...policy.allowed_target_ports] : null,
    allowed_families: [...policy.allowed_families],
  });
  targetPortMode.value = policy.allowed_target_ports ? 'allowlist' : 'all';
  targetPortList.value = policy.allowed_target_ports?.join(', ') ?? '';
  entrySearch.value = '';
  entryFamily.value = 'all';
}
async function loadPolicies(list: User[]) {
  if (!list.length) return;
  policyLoading.value = true;
  const results = await Promise.allSettled(
    list.map(async (user) => [user.username, await api.portForwardPolicy(user.username)] as const),
  );
  for (const result of results) {
    if (result.status === 'fulfilled') policies[result.value[0]] = result.value[1];
  }
  policyLoading.value = false;
}
async function loadEntries() {
  try {
    entries.value = await api.portForwardEntries();
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '入口读取失败');
  }
}
async function loadAllocations() {
  try {
    allocations.value = await api.portForwardAllocations();
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '转发租约读取失败');
  }
}
async function load() {
  loading.value = true;
  try {
    await Promise.all([store.refresh(), loadEntries(), loadAllocations()]);
    await loadPolicies(users.value);
  } finally {
    loading.value = false;
  }
}
async function openEditor(user: User) {
  selectedUser.value = user;
  drawerVisible.value = true;
  const cached = policies[user.username];
  if (cached) {
    resetForm(cached);
    return;
  }
  policyLoading.value = true;
  try {
    const policy = await api.portForwardPolicy(user.username);
    policies[user.username] = policy;
    resetForm(policy);
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '策略读取失败');
    drawerVisible.value = false;
  } finally {
    policyLoading.value = false;
  }
}
function toggleEntry(entryId: string, checked: boolean) {
  form.allowed_entry_ids = checked
    ? [...new Set([...form.allowed_entry_ids, entryId])]
    : form.allowed_entry_ids.filter((item) => item !== entryId);
}
function toggleProtocol(protocol: PortForwardProtocol, checked: boolean) {
  form.allowed_protocols = checked
    ? [...new Set([...form.allowed_protocols, protocol])]
    : form.allowed_protocols.filter((item) => item !== protocol);
}
function toggleFamily(family: 'ipv4' | 'ipv6', checked: boolean) {
  form.allowed_families = checked
    ? [...new Set([...form.allowed_families, family])]
    : form.allowed_families.filter((item) => item !== family);
}
function entryChecked(entryId: string) {
  return form.allowed_entry_ids.includes(entryId);
}
function selectVisibleEntries(checked: boolean) {
  const visibleIds = filteredEntries.value
    .filter((entry) => entry.enabled)
    .map((entry) => entry.entry_id);
  form.allowed_entry_ids = checked
    ? [...new Set([...form.allowed_entry_ids, ...visibleIds])]
    : form.allowed_entry_ids.filter((id) => !visibleIds.includes(id));
}
function parseTargetPorts() {
  if (targetPortMode.value === 'all') return null;
  const ports = [
    ...new Set(
      targetPortList.value
        .split(/[\s,;]+/)
        .filter(Boolean)
        .map(Number),
    ),
  ];
  if (!ports.length || ports.some((port) => !Number.isInteger(port) || port < 1 || port > 65535))
    throw new Error('目标端口列表无效，请填写 1 至 65535 的端口号');
  return ports.sort((left, right) => left - right);
}
async function save() {
  if (!selectedUser.value) return;
  if (!(await formRef.value?.validate().catch(() => false))) return;
  if (form.enabled && !form.allowed_entry_ids.length) {
    ElMessage.warning('启用转发前至少选择一个可用入口');
    return;
  }
  if (form.enabled && !form.allowed_protocols.length) {
    ElMessage.warning('启用转发前至少允许一种协议');
    return;
  }
  if (form.enabled && !form.allowed_families.length) {
    ElMessage.warning('启用转发前至少允许一个地址族');
    return;
  }
  saving.value = true;
  try {
    const policy = await api.savePortForwardPolicy(selectedUser.value.username, {
      ...form,
      allowed_entry_ids: [...form.allowed_entry_ids],
      allowed_protocols: [...form.allowed_protocols],
      allowed_families: [...form.allowed_families],
      allowed_target_ports: parseTargetPorts(),
    });
    policies[selectedUser.value.username] = policy;
    resetForm(policy);
    ElMessage.success(`已保存 ${selectedUser.value.username} 的转发授权`);
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '策略保存失败');
  } finally {
    saving.value = false;
  }
}

watch(users, (list) => void loadPolicies(list), { immediate: true });
onMounted(() => void Promise.all([loadEntries(), loadAllocations(), loadPolicies(users.value)]));
</script>

<template>
  <PageHeading title="端口转发授权" description="按用户配置入口、协议和并发额度">
    <el-button :icon="Refresh" :loading="loading || policyLoading" @click="load">刷新</el-button>
  </PageHeading>

  <div class="policy-overview">
    <el-card shadow="never" class="metric-card"
      ><div class="metric-label">已启用用户</div>
      <div class="metric-value">{{ activePolicies }}</div>
      <div class="metric-help">拥有可用入口和并发额度</div></el-card
    >
    <el-card shadow="never" class="metric-card"
      ><div class="metric-label">配置待完善</div>
      <div class="metric-value warning-value">{{ incompletePolicies }}</div>
      <div class="metric-help">需要补充入口、协议或地址族</div></el-card
    >
    <el-card shadow="never" class="metric-card"
      ><div class="metric-label">可用入口</div>
      <div class="metric-value">
        {{ enabledEntries }}<span class="metric-unit"> / {{ entries.length }}</span>
      </div>
      <div class="metric-help">仅启用且健康的入口可被授权</div></el-card
    >
    <el-card shadow="never" class="metric-card"
      ><div class="metric-label">在线设备</div>
      <div class="metric-value">
        {{ store.devices.filter((device) => device.status === 'active').length }}
      </div>
      <div class="metric-help">用户需要在线才可创建转发</div></el-card
    >
  </div>

  <el-card shadow="never" class="section-card policy-list-card">
    <template #header
      ><div class="card-heading">
        <div>
          <div class="card-title">用户授权策略</div>
          <div class="card-caption">
            收紧授权会阻止新的申请；不符合新策略的现有租约可能被主控释放。
          </div>
        </div>
        <el-tag type="info" effect="plain">{{ filteredUsers.length }} 个用户</el-tag>
      </div></template
    >
    <div class="filter-bar policy-filter-bar">
      <el-input v-model="search" :prefix-icon="Search" clearable placeholder="搜索用户名" />
      <el-select v-model="statusFilter" aria-label="策略状态" style="width: 150px"
        ><el-option label="全部状态" value="all" /><el-option
          label="已启用"
          value="enabled" /><el-option label="未启用 / 待完善" value="disabled"
      /></el-select>
      <span class="filter-tip">授权入口和端口限制在编辑抽屉中单独配置</span>
    </div>
    <el-table
      v-loading="loading || policyLoading"
      :data="filteredUsers"
      row-key="user_id"
      empty-text="暂无匹配用户"
    >
      <el-table-column label="用户" min-width="180"
        ><template #default="{ row }"
          ><div class="cell-primary">{{ row.username }}</div>
          <div class="cell-secondary">{{ row.enabled ? '账户正常' : '账户已停用' }}</div></template
        ></el-table-column
      >
      <el-table-column label="授权状态" width="140"
        ><template #default="{ row }"
          ><el-tag :type="statusType(policyStatus(policies[row.username]))" effect="light">{{
            statusLabel(policyStatus(policies[row.username]))
          }}</el-tag></template
        ></el-table-column
      >
      <el-table-column label="并发额度" width="150"
        ><template #default="{ row }"
          ><template v-if="policies[row.username]"
            ><div class="quota-text">
              {{ activeCount(row.username) }} / {{ policies[row.username].max_active_forwards }} 个
            </div>
            <el-progress
              :percentage="
                policies[row.username].max_active_forwards
                  ? Math.min(
                      100,
                      (activeCount(row.username) / policies[row.username].max_active_forwards) *
                        100,
                    )
                  : 0
              "
              :show-text="false"
              :status="
                activeCount(row.username) >= policies[row.username].max_active_forwards &&
                policies[row.username].max_active_forwards > 0
                  ? 'warning'
                  : undefined
              " /></template
          ><span v-else class="muted">未读取</span></template
        ></el-table-column
      >
      <el-table-column label="授权范围" min-width="250"
        ><template #default="{ row }"
          ><template v-if="policies[row.username]"
            ><div class="tag-line">
              <el-tag
                v-for="protocol in policies[row.username].allowed_protocols"
                :key="protocol"
                size="small"
                effect="plain"
                >{{ protocolLabel(protocol) }}</el-tag
              ><el-tag
                v-for="family in policies[row.username].allowed_families"
                :key="family"
                size="small"
                type="info"
                effect="plain"
                >{{ family.toUpperCase() }}</el-tag
              >
            </div>
            <div class="cell-secondary">
              {{ policies[row.username].allowed_entry_ids.length }} 个入口 ·
              {{ targetPortLabel(policies[row.username]) }}
            </div></template
          ><span v-else class="muted">未读取</span></template
        ></el-table-column
      >
      <el-table-column label="操作" width="130" fixed="right" align="right"
        ><template #default="{ row }"
          ><el-button link type="primary" :icon="Edit" @click="openEditor(row)"
            >配置</el-button
          ></template
        ></el-table-column
      >
    </el-table>
  </el-card>

  <el-drawer v-model="drawerVisible" :title="editingTitle" size="680px" destroy-on-close>
    <template v-if="selectedUser">
      <el-alert
        title="授权是创建转发的前置条件"
        description="用户只能为在线设备申请转发，公网端口自动分配。"
        type="info"
        show-icon
        :closable="false"
        class="drawer-alert"
      />
      <el-form
        ref="formRef"
        v-loading="policyLoading"
        :model="form"
        :rules="rules"
        label-position="top"
        class="policy-editor"
      >
        <div class="editor-section">
          <div class="editor-section-heading">
            <div>
              <h3>基础权限</h3>
              <p>设置转发开关和并发额度。</p>
            </div>
            <el-switch v-model="form.enabled" active-text="允许使用" inactive-text="已禁用" />
          </div>
          <el-form-item label="最大同时转发数" prop="max_active_forwards"
            ><el-input-number
              v-model="form.max_active_forwards"
              :min="0"
              :max="1024"
              controls-position="right"
            />
            <div class="form-help">
              TCP 和 UDP 使用同一个公网端口时只计一个额度。设置为 0 等同于禁用。
            </div></el-form-item
          >
        </div>
        <el-divider />
        <div class="editor-section">
          <div class="editor-section-heading compact-heading">
            <div>
              <h3>协议与地址族</h3>
              <p>入口必须同时满足协议和地址族授权。</p>
            </div>
          </div>
          <el-form-item label="允许协议"
            ><div class="choice-grid">
              <el-checkbox
                :model-value="form.allowed_protocols.includes('tcp')"
                @change="(value: boolean) => toggleProtocol('tcp', value)"
                >TCP</el-checkbox
              ><el-checkbox
                :model-value="form.allowed_protocols.includes('udp')"
                @change="(value: boolean) => toggleProtocol('udp', value)"
                >UDP</el-checkbox
              >
            </div></el-form-item
          ><el-form-item label="允许入口地址族"
            ><div class="choice-grid">
              <el-checkbox
                :model-value="form.allowed_families.includes('ipv4')"
                @change="(value: boolean) => toggleFamily('ipv4', value)"
                >IPv4</el-checkbox
              ><el-checkbox
                :model-value="form.allowed_families.includes('ipv6')"
                @change="(value: boolean) => toggleFamily('ipv6', value)"
                >IPv6</el-checkbox
              >
            </div></el-form-item
          >
        </div>
        <el-divider />
        <div class="editor-section">
          <div class="editor-section-heading compact-heading">
            <div>
              <h3>
                公网入口范围 <span class="section-count">已选 {{ selectedEntryCount }} 个</span>
              </h3>
              <p>仅授权可用入口。</p>
            </div>
          </div>
          <div class="entry-filter-bar">
            <el-input
              v-model="entrySearch"
              :prefix-icon="Search"
              clearable
              placeholder="搜索入口名称、ID 或地址"
            /><el-select v-model="entryFamily" aria-label="入口地址族" style="width: 120px"
              ><el-option label="全部" value="all" /><el-option
                label="IPv4"
                value="ipv4" /><el-option label="IPv6" value="ipv6"
            /></el-select>
          </div>
          <div class="entry-select-toolbar">
            <el-checkbox
              :model-value="
                filteredEntries.length > 0 &&
                filteredEntries
                  .filter((entry) => entry.enabled)
                  .every((entry) => entryChecked(entry.entry_id))
              "
              :indeterminate="
                filteredEntries.some((entry) => entry.enabled && entryChecked(entry.entry_id)) &&
                !filteredEntries
                  .filter((entry) => entry.enabled)
                  .every((entry) => entryChecked(entry.entry_id))
              "
              @change="(value: boolean) => selectVisibleEntries(value)"
              >选择当前结果中的可用入口</el-checkbox
            ><span class="muted">{{ filteredEntries.length }} 个结果</span>
          </div>
          <el-table
            :data="filteredEntries"
            max-height="300"
            row-key="entry_id"
            class="entry-table"
            empty-text="暂无匹配入口"
            ><el-table-column width="48"
              ><template #default="{ row }"
                ><el-checkbox
                  :model-value="entryChecked(row.entry_id)"
                  :disabled="!row.enabled"
                  @change="
                    (value: boolean) => toggleEntry(row.entry_id, value)
                  " /></template></el-table-column
            ><el-table-column label="入口" min-width="210"
              ><template #default="{ row }"
                ><div class="cell-primary">{{ row.name }}</div>
                <div class="cell-secondary">
                  {{ row.hostname || row.public_address }} · {{ row.family.toUpperCase() }}
                </div></template
              ></el-table-column
            ><el-table-column label="资源" width="125"
              ><template #default="{ row }"
                ><div>{{ capacityLabel(row) }}</div>
                <div class="cell-secondary">可用 / 总量</div></template
              ></el-table-column
            ><el-table-column label="状态" width="85"
              ><template #default="{ row }"
                ><el-tag
                  size="small"
                  :type="row.enabled && row.node_healthy ? 'success' : 'info'"
                  effect="plain"
                  >{{
                    row.enabled && row.node_healthy ? '可用' : row.enabled ? '节点离线' : '已停用'
                  }}</el-tag
                ></template
              ></el-table-column
            ></el-table
          >
          <div
            v-if="
              form.allowed_entry_ids.some((id) => !entries.some((entry) => entry.entry_id === id))
            "
            class="form-help warning-help"
          >
            当前策略包含已不存在的入口，保存前请重新选择授权范围。
          </div>
          <div v-if="form.allowed_entry_ids.length" class="selected-entry-summary">
            <span class="muted">已授权：</span
            ><el-tag
              v-for="entryId in form.allowed_entry_ids"
              :key="entryId"
              size="small"
              closable
              @close="toggleEntry(entryId, false)"
              >{{ entryLabel(entryId) }}</el-tag
            >
          </div>
        </div>
        <el-divider />
        <div class="editor-section">
          <div class="editor-section-heading compact-heading">
            <div>
              <h3>目标端口限制</h3>
              <p>限制客户端目标端口，公网端口不受影响。</p>
            </div>
          </div>
          <el-radio-group v-model="targetPortMode" class="port-mode-group"
            ><el-radio value="all">允许所有目标端口</el-radio
            ><el-radio value="allowlist">仅允许指定目标端口</el-radio></el-radio-group
          ><el-input
            v-if="targetPortMode === 'allowlist'"
            v-model="targetPortList"
            type="textarea"
            :rows="3"
            placeholder="例如：22, 80, 443, 3389"
          />
          <div class="form-help">多个端口用逗号、空格或换行分隔，范围为 1 至 65535。</div>
        </div>
      </el-form>
    </template>
    <template #footer
      ><div class="drawer-footer">
        <el-button @click="drawerVisible = false">取消</el-button
        ><el-button type="primary" :icon="Setting" :loading="saving" @click="save"
          >保存策略</el-button
        >
      </div></template
    >
  </el-drawer>
</template>

<style scoped>
.policy-overview {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;
  margin-bottom: 20px;
}
.metric-card {
  min-height: 118px;
}
.metric-label,
.metric-help {
  color: #65728a;
  font-size: 13px;
}
.metric-value {
  margin: 10px 0 4px;
  color: #1d2638;
  font-size: 28px;
  font-weight: 650;
  line-height: 1;
}
.metric-unit {
  color: #8590a3;
  font-size: 16px;
  font-weight: 400;
}
.warning-value {
  color: #c57b18;
}
.policy-list-card {
  margin-bottom: 24px;
}
.card-title {
  font-size: 15px;
  font-weight: 600;
}
.card-caption {
  margin-top: 6px;
  color: #8590a3;
  font-size: 12px;
  font-weight: 400;
}
.policy-filter-bar {
  margin-bottom: 16px;
}
.policy-filter-bar .el-input {
  width: 280px;
}
.filter-tip {
  color: #8590a3;
  font-size: 12px;
}
.quota-text {
  margin-bottom: 7px;
  color: #4d5b72;
  font-size: 12px;
}
.tag-line,
.selected-entry-summary {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.drawer-alert {
  margin-bottom: 22px;
}
.policy-editor {
  padding: 0 2px 12px;
}
.editor-section-heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 18px;
}
.compact-heading {
  margin-bottom: 14px;
}
.editor-section-heading h3 {
  margin: 0;
  color: #1d2638;
  font-size: 15px;
  font-weight: 600;
}
.editor-section-heading p {
  margin: 6px 0 0;
  color: #8590a3;
  font-size: 12px;
  line-height: 1.6;
}
.section-count {
  margin-left: 8px;
  color: #2563eb;
  font-size: 12px;
  font-weight: 400;
}
.choice-grid {
  display: flex;
  gap: 24px;
}
.entry-filter-bar,
.entry-select-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
}
.entry-filter-bar {
  margin-bottom: 12px;
}
.entry-filter-bar .el-input {
  flex: 1;
}
.entry-select-toolbar {
  justify-content: space-between;
  margin-bottom: 8px;
}
.entry-table {
  border: 1px solid #e8edf3;
  border-radius: 6px;
}
.port-mode-group {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 12px;
  margin-bottom: 14px;
}
.form-help {
  margin-top: 7px;
  color: #8590a3;
  font-size: 12px;
  line-height: 1.6;
}
.warning-help {
  color: #c57b18;
}
.selected-entry-summary {
  margin-top: 12px;
}
.drawer-footer {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}
@media (max-width: 1100px) {
  .policy-overview {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (max-width: 720px) {
  .policy-overview {
    grid-template-columns: 1fr;
  }
  .policy-filter-bar .el-input,
  .filter-tip {
    width: 100%;
  }
  .entry-filter-bar {
    align-items: stretch;
    flex-direction: column;
  }
  .entry-filter-bar .el-select {
    width: 100% !important;
  }
}
</style>
