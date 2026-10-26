<script setup lang="ts">
import { ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { invitationApi } from '../api/invitations';
import { api } from '../api/admin';
import { confirmAction, formatDate } from '../composables/useAdmin';
import type { User, PortForwardEntry } from '../types';
import {
  cloneInvitation,
  emptyInvitationPolicy,
  type InvitationPolicyView,
} from '../../../shared/invitations';
import InvitationGrantForm from './InvitationGrantForm.vue';
const props = defineProps<{ user: User | null }>();
const emit = defineEmits<{ close: [] }>();
const draft = ref(emptyInvitationPolicy());
const view = ref<InvitationPolicyView>();
const entries = ref<PortForwardEntry[]>([]);
const loading = ref(false);
const saving = ref(false);
const failure = ref('');
const grantForm = ref<InstanceType<typeof InvitationGrantForm>>();
let revision = 0;
watch(
  () => props.user,
  async (user) => {
    const current = ++revision;
    view.value = undefined;
    failure.value = '';
    if (!user) return;
    loading.value = true;
    try {
      const [v, e] = await Promise.all([
        invitationApi.policy(user.username),
        api.portForwardEntries(),
      ]);
      if (current !== revision) return;
      view.value = v;
      draft.value = cloneInvitation(v.policy);
      entries.value = e;
    } catch (e) {
      if (current === revision) failure.value = e instanceof Error ? e.message : '读取失败';
    } finally {
      if (current === revision) loading.value = false;
    }
  },
  { immediate: true },
);
async function save() {
  if (!props.user || !grantForm.value?.valid()) return;
  if (!(await confirmAction('保存邀请授权？尚未使用的邀请码立即采用新权限，已注册用户不受影响。')))
    return;
  saving.value = true;
  try {
    await invitationApi.savePolicy(props.user.username, cloneInvitation(draft.value));
    ElMessage.success('邀请授权已保存');
    emit('close');
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : '保存失败');
  } finally {
    saving.value = false;
  }
}
</script>
<template>
  <el-dialog
    class="invitation-editor"
    :model-value="!!user"
    :title="`${user?.username ?? ''} · 邀请授权`"
    width="640px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    @close="emit('close')"
  >
    <el-alert v-if="failure" :title="failure" type="error" :closable="false" />
    <el-alert
      v-if="user && !user.enabled"
      title="账户已停用，其邀请码暂不可使用"
      type="warning"
      :closable="false"
    />
    <el-form v-if="view" label-width="110px" :disabled="saving" @submit.prevent="save">
      <el-form-item label="允许邀请"><el-switch v-model="draft.enabled" /></el-form-item>
      <el-form-item label="未使用码上限"
        ><el-input-number v-model="draft.max_unused" :min="1" :max="10000" :precision="0"
      /></el-form-item>
      <el-form-item label="每日生成上限"
        ><el-input-number v-model="draft.daily_limit" :min="1" :max="10000" :precision="0"
      /></el-form-item>
      <el-form-item label="有效天数"
        ><el-input-number v-model="draft.valid_days" :min="1" :max="365" :precision="0"
      /></el-form-item>
      <el-form-item label="当前用量"
        >未使用 {{ view.unused }} 个，今日已生成 {{ view.generated_today }} 个；{{
          formatDate(view.resets_at)
        }}
        重置</el-form-item
      >
      <InvitationGrantForm ref="grantForm" v-model="draft.grant" :entries="entries" />
    </el-form>
    <div v-if="loading" v-loading="loading" style="height: 100px" />
    <template #footer
      ><el-button :disabled="saving" @click="emit('close')">取消</el-button
      ><el-button type="primary" :loading="saving" :disabled="!view || loading" @click="save"
        >保存</el-button
      ></template
    >
  </el-dialog>
</template>
