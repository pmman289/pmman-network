<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { addressPoolsApi } from '../api/addressPools';
import { useAdminStore } from '../stores/admin';
import { confirmAction } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';
const store = useAdminStore();
const routes = ref('');
const dirty = ref(false);
const saving = ref(false);
const error = ref('');
const automaticRoutes = computed(() =>
  [...new Set(store.addressPools?.pools.flatMap((pool) => pool.segments) ?? [])].join('\n'),
);
function reset() {
  routes.value = store.pool?.internal_cidrs.join('\n') ?? '';
  dirty.value = false;
  error.value = '';
}
watch(
  () => store.pool,
  () => {
    if (!dirty.value) reset();
  },
  { immediate: true },
);
async function save() {
  const values = routes.value.split(/[\s,;]+/).filter(Boolean);
  if (
    saving.value ||
    !values.length ||
    !(await confirmAction('保存内网路由？新连接将同时使用这些路由和地址池网段。'))
  )
    return;
  saving.value = true;
  error.value = '';
  try {
    await addressPoolsApi.saveRoutes(values);
    dirty.value = false;
    await store.refresh();
    ElMessage.success('内网路由已保存');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '保存失败';
  } finally {
    saving.value = false;
  }
}
</script>
<template>
  <PageHeading title="网络配置" description="配置客户端内网路由" />
  <el-alert v-if="error" :title="error" type="error" show-icon :closable="false" />
  <el-card v-loading="store.loading" shadow="never">
    <template #header>内网路由</template>
    <el-form label-position="top" class="settings-form" :disabled="saving" @submit.prevent="save">
      <el-form-item label="地址池路由（自动）">
        <el-input :model-value="automaticRoutes" type="textarea" :rows="4" readonly />
        <div class="form-help">随地址池自动维护，新连接生效。</div>
      </el-form-item>
      <el-form-item label="附加内网 CIDR" required
        ><el-input
          v-model="routes"
          type="textarea"
          :rows="8"
          placeholder="每行一个 IPv4 或 IPv6 网段"
          @input="dirty = true"
        />
        <div class="form-help">与地址池网段合并下发，重复项自动去除。</div></el-form-item
      >
      <el-form-item
        ><el-button
          type="primary"
          native-type="submit"
          :loading="saving"
          :disabled="!store.pool || !routes.trim()"
          >保存配置</el-button
        ><el-button :disabled="!store.pool" @click="reset">恢复当前配置</el-button
        ><el-button @click="$router.push('/address-pools')">地址池管理</el-button></el-form-item
      >
    </el-form>
  </el-card>
</template>
