<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { invitationApi } from '../api/invitations';
import { api } from '../api/admin';
import { confirmAction, formatDate } from '../composables/useAdmin';
import type { PortForwardEntry } from '../types';
import {
  emptyInvitationGrant,
  cloneInvitation,
  grantSummary,
  invitationStates,
  type Invitation,
  type InvitationRegistration,
  type InvitationQuery,
  type CreatedInvitation,
} from '../../../shared/invitations';
import PageHeading from '../components/PageHeading.vue';
import InvitationGrantForm from '../components/InvitationGrantForm.vue';
import InvitationGrantDetails from '../components/InvitationGrantDetails.vue';
const props = defineProps<{ records?: boolean }>();
const query = ref<InvitationQuery>({ page: 1, page_size: 20 });
const rows = ref<(Invitation | InvitationRegistration)[]>([]);
const total = ref(0);
const loading = ref(false);
const saving = ref(false);
const failure = ref('');
const dialog = ref(false);
const editing = ref<Invitation | null>(null);
const grant = ref(emptyInvitationGrant());
const kind = ref<'single' | 'unlimited'>('single');
const note = ref('');
const expiry = ref<string | null>(null);
const range = ref<[string, string] | null>(null);
const entries = ref<PortForwardEntry[]>([]);
const created = ref<CreatedInvitation | null>(null);
const grantForm = ref<InstanceType<typeof InvitationGrantForm>>();
let revision = 0;
async function load() {
  const current = ++revision;
  loading.value = true;
  failure.value = '';
  try {
    const q = { ...query.value, since: range.value?.[0], until: range.value?.[1] };
    const page = props.records ? await invitationApi.registrations(q) : await invitationApi.list(q);
    if (current !== revision) return;
    rows.value = page.items;
    total.value = page.total;
  } catch (e) {
    if (current === revision) failure.value = e instanceof Error ? e.message : '读取失败';
  } finally {
    if (current === revision) loading.value = false;
  }
}
watch(
  () => props.records,
  () => {
    query.value = { page: 1, page_size: 20 };
    range.value = null;
    void load();
  },
);
onMounted(() => {
  void load();
});
function search() {
  query.value.page = 1;
  void load();
}
async function open(row?: Invitation) {
  editing.value = row ?? null;
  grant.value = cloneInvitation(row?.grant ?? emptyInvitationGrant());
  kind.value = row?.kind ?? 'single';
  note.value = row?.note ?? '';
  expiry.value = row?.expires_at ?? null;
  try {
    entries.value = await api.portForwardEntries();
    dialog.value = true;
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : '读取入口失败');
  }
}
async function save() {
  if (grantForm.value && !grantForm.value.valid()) return;
  if (
    editing.value &&
    !(await confirmAction('保存邀请码？尚未使用的邀请码将按新设置注册，已注册用户不受影响。'))
  )
    return;
  saving.value = true;
  try {
    if (editing.value)
      await invitationApi.edit(editing.value.id, {
        note: note.value,
        expires_at: expiry.value,
        grant: editing.value.source === 'admin' ? cloneInvitation(grant.value) : undefined,
      });
    else
      created.value = await invitationApi.create({
        kind: kind.value,
        note: note.value,
        expires_at: expiry.value,
        grant: cloneInvitation(grant.value),
      });
    dialog.value = false;
    await load();
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : '保存失败');
  } finally {
    saving.value = false;
  }
}
async function revoke(row: Invitation) {
  if (!(await confirmAction('撤销此邀请码？此操作不可恢复，不影响已注册用户。'))) return;
  saving.value = true;
  try {
    await invitationApi.revoke(row.id);
    await load();
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : '撤销失败');
  } finally {
    saving.value = false;
  }
}
async function copy(value: string) {
  try {
    await navigator.clipboard.writeText(value);
    ElMessage.success('已复制');
  } catch {
    ElMessage.warning('复制失败，请手动选择文本复制');
  }
}
const url = () =>
  created.value ? new URL(created.value.registration_path, window.location.origin).href : '';
</script>
<template>
  <PageHeading
    :title="records ? '邀请记录' : '邀请码'"
    :description="records ? '查看邀请注册与实际授予权限' : '管理单次和无限次邀请码'"
    ><el-button v-if="!records" type="primary" :disabled="saving" @click="open()"
      >创建邀请码</el-button
    ></PageHeading
  >
  <el-card shadow="never">
    <el-form inline @submit.prevent="search">
      <el-form-item label="邀请人"
        ><el-input v-model="query.inviter" clearable placeholder="用户名"
      /></el-form-item>
      <el-form-item v-if="records" label="注册用户"
        ><el-input v-model="query.username" clearable
      /></el-form-item>
      <template v-else
        ><el-form-item label="类型"
          ><el-select v-model="query.kind" clearable placeholder="全部" style="width: 120px"
            ><el-option label="单次" value="single" /><el-option
              label="无限次"
              value="unlimited" /></el-select
        ></el-form-item>
        <el-form-item label="状态"
          ><el-select v-model="query.state" clearable placeholder="全部" style="width: 120px"
            ><el-option
              v-for="(label, value) in invitationStates"
              :key="value"
              :label="label"
              :value="value" /></el-select></el-form-item
      ></template>
      <el-form-item label="时间"
        ><el-date-picker
          v-model="range"
          type="datetimerange"
          value-format="YYYY-MM-DDTHH:mm:ssZ"
          start-placeholder="开始时间"
          end-placeholder="结束时间"
      /></el-form-item>
      <el-form-item
        ><el-button native-type="submit" type="primary" :loading="loading">查询</el-button
        ><el-button :disabled="loading" @click="load">刷新</el-button></el-form-item
      >
    </el-form>
    <el-alert v-if="failure" :title="failure" type="error" :closable="false" show-icon />
    <el-table v-loading="loading" :data="rows" row-key="id" empty-text="暂无记录">
      <el-table-column type="expand"
        ><template #default="{ row }"
          ><InvitationGrantDetails :grant="row.grant" :revision="row.policy_revision" /></template
      ></el-table-column>
      <el-table-column prop="prefix" label="邀请码前缀" width="130" /><el-table-column
        prop="inviter_username"
        label="邀请人"
        width="140"
      />
      <el-table-column v-if="records" prop="username" label="注册用户" width="140" />
      <template v-else
        ><el-table-column label="类型" width="90"
          ><template #default="{ row }">{{
            row.kind === 'unlimited' ? '无限次' : '单次'
          }}</template></el-table-column
        >
        <el-table-column label="状态" width="100"
          ><template #default="{ row }"
            ><el-tag :type="row.state === 'available' ? 'success' : 'info'">{{
              invitationStates[(row as Invitation).state]
            }}</el-tag></template
          ></el-table-column
        >
        <el-table-column prop="use_count" label="使用次数" width="100" /><el-table-column
          label="到期时间"
          min-width="175"
          ><template #default="{ row }">{{
            row.expires_at ? formatDate(row.expires_at) : '长期有效'
          }}</template></el-table-column
        ></template
      >
      <el-table-column label="受邀权限" min-width="240" show-overflow-tooltip
        ><template #default="{ row }">{{ grantSummary(row.grant) }}</template></el-table-column
      >
      <el-table-column label="时间" min-width="175"
        ><template #default="{ row }">{{ formatDate(row.created_at) }}</template></el-table-column
      >
      <el-table-column
        v-if="!records"
        prop="note"
        label="备注"
        min-width="140"
        show-overflow-tooltip
      />
      <el-table-column v-if="!records" label="操作" fixed="right" width="130"
        ><template #default="{ row }"
          ><el-button
            link
            type="primary"
            :disabled="saving || row.state === 'used' || row.state === 'revoked'"
            @click="open(row)"
            >编辑</el-button
          ><el-button
            link
            type="danger"
            :disabled="saving || row.state === 'used' || row.state === 'revoked'"
            @click="revoke(row)"
            >撤销</el-button
          ></template
        ></el-table-column
      >
    </el-table>
    <el-pagination
      v-model:current-page="query.page"
      v-model:page-size="query.page_size"
      :total="total"
      :page-sizes="[20, 50, 100]"
      layout="total, sizes, prev, pager, next"
      @current-change="load"
      @size-change="search"
    />
  </el-card>
  <el-dialog
    v-model="dialog"
    class="invitation-editor"
    :title="editing ? '编辑邀请码' : '创建邀请码'"
    width="660px"
    :close-on-click-modal="false"
    :show-close="!saving"
    :close-on-press-escape="!saving"
  >
    <el-form label-width="100px" :disabled="saving" @submit.prevent="save"
      ><el-form-item label="类型"
        ><el-select v-model="kind" :disabled="!!editing"
          ><el-option label="单次" value="single" /><el-option
            label="无限次"
            value="unlimited" /></el-select
      ></el-form-item>
      <el-form-item label="到期时间"
        ><el-date-picker
          v-model="expiry"
          type="datetime"
          value-format="YYYY-MM-DDTHH:mm:ssZ"
          placeholder="留空表示长期有效" /></el-form-item
      ><el-form-item label="备注"
        ><el-input v-model="note" maxlength="200" show-word-limit
      /></el-form-item>
      <InvitationGrantForm
        v-if="!editing || editing.source === 'admin'"
        ref="grantForm"
        v-model="grant"
        :entries="entries"
      /><el-alert
        v-else
        title="受邀权限由邀请人的邀请授权决定"
        type="info"
        :closable="false"
      /> </el-form
    ><template #footer
      ><el-button :disabled="saving" @click="dialog = false">取消</el-button
      ><el-button type="primary" :loading="saving" @click="save">保存</el-button></template
    >
  </el-dialog>
  <el-dialog :model-value="!!created" title="邀请码已创建" width="600px" @close="created = null"
    ><el-alert title="完整邀请码仅显示一次，请复制保存" type="warning" :closable="false" /><el-input
      :model-value="created?.code"
      readonly
      aria-label="完整邀请码"
    /><el-button @click="copy(created?.code ?? '')">复制邀请码</el-button
    ><el-input :model-value="url()" readonly aria-label="注册链接" /><el-button @click="copy(url())"
      >复制链接</el-button
    ></el-dialog
  >
</template>
