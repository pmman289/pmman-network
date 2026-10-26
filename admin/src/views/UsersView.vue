<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { Plus, Search } from '@element-plus/icons-vue';
import { type FormInstance, type FormRules } from 'element-plus';
import { api } from '../api/admin';
import { useAdminStore } from '../stores/admin';
import { useAuthStore } from '../stores/auth';
import {
  capabilityOptions,
  emptyCapabilities,
  type User,
  type CreateUser,
  type Capabilities,
} from '../types';
import { usePagination, useMutation, confirmAction, formatDate } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';
import CapabilityForm from '../components/CapabilityForm.vue';
import InvitationPolicyDialog from '../components/InvitationPolicyDialog.vue';
import UserAddressDialog from '../components/UserAddressDialog.vue';
const addressUser = ref<User | null>(null);
const invitationUser = ref<User | null>(null);
import ListPagination from '../components/ListPagination.vue';
const store = useAdminStore();
const auth = useAuthStore();
const filters = reactive({ query: '', role: '', status: '' });
const filtered = computed(() =>
  store.users.filter(
    (u) =>
      u.username.toLowerCase().includes(filters.query.trim().toLowerCase()) &&
      (!filters.role || u.role === filters.role) &&
      (!filters.status || String(u.enabled) === filters.status),
  ),
);
const { page, size, rows } = usePagination(filtered);
const { saving, run } = useMutation();
const dialog = ref<'create' | 'capabilities' | 'password' | 'delete' | ''>('');
const target = ref<User | null>(null);
const deleteConfirmation = ref('');
const deleteError = ref('');
function isCurrentUser(user: User) {
  return user.user_id === auth.session?.user_id || user.username === auth.session?.username;
}
const canDelete = computed(
  () =>
    auth.authenticated &&
    dialog.value === 'delete' &&
    target.value !== null &&
    !isCurrentUser(target.value) &&
    deleteConfirmation.value === target.value.username,
);
const create = ref<CreateUser>({
  username: '',
  password: '',
  role: 'user',
  ...emptyCapabilities(),
});
const capabilities = ref<Capabilities>(emptyCapabilities());
const passwordForm = reactive({ password: '' });
const formRef = ref<FormInstance>();
const passwordRef = ref<FormInstance>();
const passwordRules: FormRules = {
  password: [
    { required: true, message: '请输入密码', trigger: 'blur' },
    { min: 6, message: '密码至少 6 位', trigger: 'blur' },
  ],
};
const createRules: FormRules = {
  ...passwordRules,
  username: [
    { required: true, message: '请输入用户名', trigger: 'blur' },
    {
      pattern: /^[A-Za-z0-9_.-]{1,64}$/,
      message: '仅支持字母、数字、下划线、点和短横线，最长 64 位',
      trigger: 'blur',
    },
  ],
  role: [{ required: true, message: '请选择角色', trigger: 'change' }],
};
function openCreate() {
  create.value = { username: '', password: '', role: 'user', ...emptyCapabilities() };
  dialog.value = 'create';
}
function openCapabilities(user: User) {
  target.value = user;
  capabilities.value = Object.fromEntries(
    capabilityOptions.map((o) => [o.key, user[o.key]]),
  ) as unknown as Capabilities;
  dialog.value = 'capabilities';
}
function openPassword(user: User) {
  target.value = user;
  passwordForm.password = '';
  dialog.value = 'password';
}
function openAddressLease(user: User) {
  addressUser.value = user;
}
function closeDialog() {
  dialog.value = '';
  create.value.password = '';
  passwordForm.password = '';
  deleteConfirmation.value = '';
  deleteError.value = '';
}
function openDelete(user: User) {
  if (saving.value || !auth.authenticated || isCurrentUser(user)) return;
  target.value = { ...user };
  deleteConfirmation.value = '';
  deleteError.value = '';
  dialog.value = 'delete';
}
async function submitDelete() {
  if (saving.value || !canDelete.value || !target.value) return;
  const username = target.value.username;
  deleteError.value = '';
  await run(
    async () => {
      try {
        await api.deleteUser(username);
      } catch (cause) {
        deleteError.value = cause instanceof Error ? cause.message : '删除账号失败，请重试';
        throw cause;
      }
    },
    `账号 ${username} 已删除`,
    closeDialog,
  );
}
async function submitCreate() {
  if (!(await formRef.value?.validate().catch(() => false))) return;
  await run(() => api.createUser({ ...create.value }), '用户创建成功', closeDialog);
}
async function submitCapabilities() {
  if (
    !target.value ||
    !(await confirmAction(
      `保存 ${target.value.username} 的能力授权？移除正在使用的能力会撤销对应连接。`,
    ))
  )
    return;
  await run(
    () => api.capabilities(target.value!.username, { ...capabilities.value }),
    '能力授权已更新',
    closeDialog,
  );
}
async function submitPassword() {
  if (!target.value || !(await passwordRef.value?.validate().catch(() => false))) return;
  await run(
    () => api.password(target.value!.username, passwordForm.password),
    '密码已重置',
    closeDialog,
  );
}
async function toggleStatus(user: User) {
  if (
    !(await confirmAction(
      user.enabled
        ? `禁用用户 ${user.username}？该用户的活动连接将被撤销。`
        : `启用用户 ${user.username}？`,
    ))
  )
    return;
  await run(() => api.userStatus(user.username, !user.enabled), '用户状态已更新');
}
</script>
<template>
  <PageHeading title="用户管理" description="管理账户、访问权限和地址策略"
    ><el-button type="primary" :icon="Plus" :disabled="saving" @click="openCreate"
      >新建用户</el-button
    ></PageHeading
  >
  <el-card shadow="never">
    <div class="filter-bar">
      <el-input
        v-model="filters.query"
        :prefix-icon="Search"
        placeholder="搜索用户名"
        aria-label="搜索用户名"
        clearable
      /><el-select v-model="filters.role" placeholder="全部角色" aria-label="用户角色" clearable
        ><el-option label="管理员" value="admin" /><el-option
          label="普通用户"
          value="user" /></el-select
      ><el-select v-model="filters.status" placeholder="全部状态" aria-label="用户状态" clearable
        ><el-option label="已启用" value="true" /><el-option
          label="已禁用"
          value="false" /></el-select
      ><el-button @click="Object.assign(filters, { query: '', role: '', status: '' })"
        >重置</el-button
      >
    </div>
    <el-table
      v-loading="store.loading"
      :data="rows"
      row-key="user_id"
      empty-text="暂无符合条件的用户"
    >
      <el-table-column prop="username" label="用户名" min-width="140" /><el-table-column
        label="角色"
        width="120"
        ><template #default="{ row }"
          ><el-tag :type="row.role === 'admin' ? 'primary' : 'info'" effect="plain">{{
            row.role === 'admin' ? '管理员' : '普通用户'
          }}</el-tag></template
        ></el-table-column
      >
      <el-table-column label="状态" width="110"
        ><template #default="{ row }"
          ><el-tag :type="row.enabled ? 'success' : 'info'">{{
            row.enabled ? '已启用' : '已禁用'
          }}</el-tag></template
        ></el-table-column
      >
      <el-table-column label="能力授权" min-width="230"
        ><template #default="{ row }"
          ><div class="tag-group">
            <el-tag
              v-for="option in capabilityOptions.filter((o) => row[o.key])"
              :key="option.key"
              size="small"
              type="info"
              effect="plain"
              >{{ option.label }}</el-tag
            ><span v-if="!capabilityOptions.some((o) => row[o.key])" class="muted">仅内部网络</span>
          </div></template
        ></el-table-column
      >
      <el-table-column label="地址分配" min-width="220"
        ><template #default="{ row }"
          ><template v-if="row.address_policy">
            <div v-for="f in ['ipv4', 'ipv6'] as const" :key="f">
              {{ f.toUpperCase() }} ·
              {{
                { automatic: '自动', pool_fixed: '池内固定', manual: '手动' }[
                  row.address_policy[f].mode as 'automatic' | 'pool_fixed' | 'manual'
                ]
              }}
              <div class="muted">
                {{ row[f === 'ipv4' ? 'leased_ipv4' : 'leased_ipv6'] ?? '尚未分配' }}
              </div>
            </div> </template
          ><template v-else-if="row.address_mode === 'fixed'"
            ><div>{{ row.reserved_ipv4 }}</div>
            <div class="muted">{{ row.reserved_ipv6 }}</div></template
          ><template v-else
            ><div class="muted">自动分配</div>
            <div>{{ row.leased_ipv4 ?? '尚未分配' }}</div>
            <div class="muted">{{ row.leased_ipv6 }}</div></template
          ></template
        ></el-table-column
      >
      <el-table-column label="创建时间" min-width="180"
        ><template #default="{ row }">{{ formatDate(row.created_at) }}</template></el-table-column
      >
      <el-table-column label="操作" fixed="right" width="430"
        ><template #default="{ row }"
          ><el-button link type="primary" :disabled="saving" @click="openCapabilities(row)"
            >授权</el-button
          ><el-button link type="primary" :disabled="saving" @click="openAddressLease(row)"
            >地址租约</el-button
          ><el-button link type="primary" :disabled="saving" @click="invitationUser = row"
            >邀请授权</el-button
          ><el-button link type="primary" :disabled="saving" @click="openPassword(row)"
            >重置密码</el-button
          ><el-button
            link
            :type="row.enabled ? 'danger' : 'primary'"
            :disabled="saving || row.username === auth.session?.username"
            :title="row.username === auth.session?.username ? '不能停用当前登录账户' : ''"
            @click="toggleStatus(row)"
            >{{ row.enabled ? '禁用' : '启用' }}</el-button
          ><el-button
            link
            type="danger"
            :disabled="saving || !auth.authenticated || isCurrentUser(row)"
            :title="isCurrentUser(row) ? '不能删除当前登录账号' : '删除账号'"
            @click="openDelete(row)"
            >删除</el-button
          ></template
        ></el-table-column
      > </el-table
    ><ListPagination v-model:page="page" v-model:size="size" :total="filtered.length" />
  </el-card>
  <el-dialog
    :model-value="dialog === 'create'"
    title="新建用户"
    width="560px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
    @close="closeDialog"
  >
    <el-form
      ref="formRef"
      :model="create"
      :rules="createRules"
      label-position="top"
      @submit.prevent="submitCreate"
      ><el-form-item label="用户名" prop="username"
        ><el-input v-model="create.username" maxlength="64" autocomplete="off" /></el-form-item
      ><el-form-item label="初始密码" prop="password"
        ><el-input
          v-model="create.password"
          type="password"
          show-password
          autocomplete="new-password" /></el-form-item
      ><el-form-item label="角色" prop="role"
        ><el-select v-model="create.role"
          ><el-option label="普通用户" value="user" /><el-option
            label="管理员"
            value="admin" /></el-select></el-form-item
      ><el-form-item label="能力授权"
        ><CapabilityForm v-model="create" />
        <div class="form-help">未授权其他能力时，用户仅可访问内网。</div></el-form-item
      ></el-form
    >
    <template #footer
      ><el-button :disabled="saving" @click="closeDialog">取消</el-button
      ><el-button type="primary" :loading="saving" @click="submitCreate"
        >创建用户</el-button
      ></template
    >
  </el-dialog>
  <el-dialog
    :model-value="dialog === 'capabilities'"
    :title="`能力授权 · ${target?.username ?? ''}`"
    width="520px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    @close="closeDialog"
    ><el-alert title="变更网络权限可能影响当前连接" type="warning" show-icon :closable="false" />
    <div class="capability-editor"><CapabilityForm v-model="capabilities" /></div>
    <template #footer
      ><el-button :disabled="saving" @click="closeDialog">取消</el-button
      ><el-button type="primary" :loading="saving" @click="submitCapabilities"
        >保存授权</el-button
      ></template
    ></el-dialog
  >
  <el-dialog
    :model-value="dialog === 'password'"
    :title="`重置密码 · ${target?.username ?? ''}`"
    width="480px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
    @close="closeDialog"
    ><el-form
      ref="passwordRef"
      :model="passwordForm"
      :rules="passwordRules"
      label-position="top"
      @submit.prevent="submitPassword"
      ><el-form-item label="新密码" prop="password"
        ><el-input
          v-model="passwordForm.password"
          type="password"
          show-password
          autocomplete="new-password" /></el-form-item></el-form
    ><template #footer
      ><el-button :disabled="saving" @click="closeDialog">取消</el-button
      ><el-button type="primary" :loading="saving" @click="submitPassword"
        >确认重置</el-button
      ></template
    ></el-dialog
  >
  <el-dialog
    :model-value="dialog === 'delete'"
    :title="`删除账号 · ${target?.username ?? ''}`"
    width="520px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
    @close="closeDialog"
  >
    <el-alert type="warning" show-icon :closable="false" title="删除账号无法撤销">
      将永久删除账号「{{
        target?.username
      }}」，撤销设备及邀请码，节点确认清理后释放地址租约与端口映射；邀请历史记录保留。
    </el-alert>
    <el-form label-position="top" :disabled="saving" @submit.prevent="submitDelete">
      <el-form-item :label="`请输入用户名 ${target?.username ?? ''} 以确认删除`" required>
        <el-input
          v-model="deleteConfirmation"
          placeholder="请输入完整用户名（区分大小写）"
          autocomplete="off"
          :aria-label="`输入用户名 ${target?.username ?? ''} 确认删除`"
        />
      </el-form-item>
    </el-form>
    <el-alert v-if="deleteError" :title="deleteError" type="error" show-icon :closable="false" />
    <template #footer>
      <el-button :disabled="saving" @click="closeDialog">取消</el-button>
      <el-button
        type="danger"
        :disabled="saving || !canDelete"
        :loading="saving"
        @click="submitDelete"
      >
        确认删除账号
      </el-button>
    </template>
  </el-dialog>
  <InvitationPolicyDialog :user="invitationUser" @close="invitationUser = null" />
  <UserAddressDialog :user="addressUser" @close="addressUser = null" />
</template>
