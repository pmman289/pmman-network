<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import type { User } from '../types';
import { addressPoolsApi, type AddressPolicy, type PoolList } from '../api/addressPools';
import { confirmAction } from '../composables/useAdmin';
import { useAdminStore } from '../stores/admin';
import PoolAddressPicker from './PoolAddressPicker.vue';
const props = defineProps<{ user: User | null }>();
const emit = defineEmits<{ close: [] }>();
const store = useAdminStore();
const pools = ref<PoolList | null>(null);
const draft = ref<AddressPolicy | null>(null);
const loading = ref(false);
const saving = ref(false);
const error = ref('');
const primaryPoolValue = '__primary__';
const families = [
  { key: 'ipv4', title: 'IPv4', family: 4 },
  { key: 'ipv6', title: 'IPv6', family: 6 },
] as const;
const valid = computed(
  () =>
    draft.value &&
    families.every(
      ({ key }) => draft.value![key].mode === 'automatic' || draft.value![key].address?.trim(),
    ),
);
let generation = 0;
watch(
  () => props.user,
  async (user) => {
    const request = ++generation;
    draft.value = null;
    pools.value = null;
    error.value = '';
    if (!user) return;
    loading.value = true;
    try {
      const [policy, list] = await Promise.all([
        addressPoolsApi.policy(user.username),
        addressPoolsApi.list(),
      ]);
      if (request !== generation) return;
      if (policy.revision !== list.revision) throw new Error('地址配置已变更，请重新打开');
      draft.value = policy;
      pools.value = list;
    } catch (cause) {
      if (request === generation)
        error.value = cause instanceof Error ? cause.message : '读取配置失败';
    } finally {
      if (request === generation) loading.value = false;
    }
  },
  { immediate: true },
);
function change(key: 'ipv4' | 'ipv6') {
  if (!draft.value) return;
  const p = draft.value[key];
  p.address = null;
  if (p.mode === 'manual') p.pool_id = null;
}
function selectPool(key: 'ipv4' | 'ipv6', value: string) {
  if (!draft.value) return;
  draft.value[key].pool_id = value === primaryPoolValue ? null : value;
  draft.value[key].address = null;
}
async function save() {
  if (saving.value || !draft.value || !props.user || !valid.value) return;
  if (!(await confirmAction('保存地址分配？当前连接保持不变，新配置在下次连接时生效。'))) return;
  saving.value = true;
  error.value = '';
  try {
    await addressPoolsApi.savePolicy(props.user.username, draft.value);
    await store.refresh();
    ElMessage.success('地址分配已保存');
    emit('close');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '保存失败';
  } finally {
    saving.value = false;
  }
}
</script>
<template>
  <el-dialog
    :model-value="!!user"
    :title="`地址分配 · ${user?.username ?? ''}`"
    width="680px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
    @close="emit('close')"
  >
    <div v-loading="loading">
      <el-alert v-if="error" :title="error" type="error" show-icon :closable="false" />
      <el-form
        v-if="draft && pools && user"
        label-position="top"
        :disabled="saving"
        @submit.prevent="save"
      >
        <el-tabs>
          <el-tab-pane v-for="f in families" :key="f.key" :label="f.title">
            <el-descriptions :column="1"
              ><el-descriptions-item label="当前地址">{{
                draft[f.key === 'ipv4' ? 'current_ipv4' : 'current_ipv6'] ?? '尚未分配'
              }}</el-descriptions-item></el-descriptions
            >
            <el-form-item label="分配方式"
              ><el-radio-group v-model="draft[f.key].mode" @change="change(f.key)"
                ><el-radio value="automatic">自动分配</el-radio
                ><el-radio value="pool_fixed">池内固定</el-radio
                ><el-radio value="manual">手动指定</el-radio></el-radio-group
              ></el-form-item
            >
            <el-form-item v-if="draft[f.key].mode !== 'manual'" label="地址池"
              ><el-select
                :model-value="draft[f.key].pool_id ?? primaryPoolValue"
                @change="(value: string) => selectPool(f.key, value)"
                ><el-option :value="primaryPoolValue" label="跟随主地址池" /><el-option
                  v-for="p in pools.pools.filter((p) => p[f.key].capacity !== '0')"
                  :key="p.pool_id"
                  :label="p.name"
                  :value="p.pool_id" /></el-select
            ></el-form-item>
            <el-form-item v-if="draft[f.key].mode === 'pool_fixed'" label="固定地址" required
              ><PoolAddressPicker
                v-model="draft[f.key].address"
                :pool-id="
                  draft[f.key].pool_id ?? pools[f.key === 'ipv4' ? 'ipv4_pool_id' : 'ipv6_pool_id']
                "
                :family="f.family"
                :user-id="user.user_id"
                :disabled="saving"
            /></el-form-item>
            <el-form-item v-if="draft[f.key].mode === 'manual'" label="指定地址" required
              ><el-input
                v-model="draft[f.key].address"
                placeholder="单个 IP 地址，可在地址池之外，不带前缀"
            /></el-form-item>
          </el-tab-pane>
        </el-tabs>
      </el-form>
    </div>
    <template #footer
      ><el-button :disabled="saving" @click="emit('close')">取消</el-button
      ><el-button type="primary" :loading="saving" :disabled="loading || !valid" @click="save"
        >保存</el-button
      ></template
    >
  </el-dialog>
</template>
