<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { addressPoolsApi, type AddressPool, type PoolList } from '../api/addressPools';
import { confirmAction } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';

const data = ref<PoolList | null>(null);
const loading = ref(false);
const saving = ref(false);
const error = ref('');
const editing = ref(false);
const draft = reactive({
  id: null as string | null,
  revision: 0,
  name: '',
  description: '',
  segments: '',
  exclusions: '',
});
const defaults = reactive({ ipv4: '', ipv6: '', revision: 0 });
const v4Pools = computed(() => data.value?.pools.filter((p) => p.ipv4.capacity !== '0') ?? []);
const v6Pools = computed(() => data.value?.pools.filter((p) => p.ipv6.capacity !== '0') ?? []);
let generation = 0;
function publish(result: PoolList) {
  data.value = result;
  Object.assign(defaults, {
    ipv4: result.ipv4_pool_id,
    ipv6: result.ipv6_pool_id,
    revision: result.revision,
  });
}
async function refresh() {
  const request = ++generation;
  loading.value = true;
  error.value = '';
  try {
    const result = await addressPoolsApi.list();
    if (request === generation) publish(result);
  } catch (cause) {
    if (request === generation)
      error.value = cause instanceof Error ? cause.message : '读取地址池失败';
  } finally {
    if (request === generation) loading.value = false;
  }
}
function open(pool?: AddressPool) {
  Object.assign(draft, {
    id: pool?.pool_id ?? null,
    revision: data.value?.revision ?? 0,
    name: pool?.name ?? '',
    description: pool?.description ?? '',
    segments: pool?.segments.join('\n') ?? '',
    exclusions: pool?.exclusions.join('\n') ?? '',
  });
  error.value = '';
  editing.value = true;
}
const lines = (value: string) =>
  value
    .split(/[\s,;]+/)
    .map((s) => s.trim())
    .filter(Boolean);
async function save() {
  if (saving.value || !draft.name.trim() || !lines(draft.segments).length) return;
  if (
    !(await confirmAction(
      '保存地址池？当前连接保持不变，不再属于地址池的自动地址将在下次连接时重新分配。',
    ))
  )
    return;
  saving.value = true;
  generation++;
  loading.value = false;
  error.value = '';
  try {
    publish(
      await addressPoolsApi.save(draft.id, {
        revision: draft.revision,
        name: draft.name,
        description: draft.description,
        segments: lines(draft.segments),
        exclusions: lines(draft.exclusions),
      }),
    );
    editing.value = false;
    ElMessage.success('地址池已保存');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '保存失败';
  } finally {
    saving.value = false;
  }
}
async function saveDefaults() {
  if (
    saving.value ||
    !(await confirmAction(
      '修改主地址池？跟随主池的用户将在下次连接时应用新配置，当前连接不受影响。',
    ))
  )
    return;
  saving.value = true;
  generation++;
  loading.value = false;
  error.value = '';
  try {
    publish(await addressPoolsApi.defaults(defaults.revision, defaults.ipv4, defaults.ipv6));
    ElMessage.success('主地址池已保存');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '保存失败';
  } finally {
    saving.value = false;
  }
}
async function remove(pool: AddressPool) {
  if (
    saving.value ||
    !data.value ||
    !(await confirmAction(`删除地址池「${pool.name}」？存在用户或租约引用时不能删除。`))
  )
    return;
  saving.value = true;
  try {
    await addressPoolsApi.remove(pool.pool_id, data.value.revision);
    await refresh();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '删除失败';
  } finally {
    saving.value = false;
  }
}
onMounted(refresh);
</script>
<template>
  <PageHeading title="地址池" description="管理客户端地址资源与默认分配" />
  <el-alert v-if="error && !editing" :title="error" type="error" :closable="false" show-icon />
  <el-card v-loading="loading" shadow="never">
    <template #header><span>主地址池</span></template>
    <el-form label-position="top" :disabled="saving || loading" @submit.prevent="saveDefaults">
      <el-row :gutter="16">
        <el-col :xs="24" :sm="12"
          ><el-form-item label="IPv4 主地址池"
            ><el-select v-model="defaults.ipv4"
              ><el-option
                v-for="p in v4Pools"
                :key="p.pool_id"
                :label="p.name"
                :value="p.pool_id" /></el-select></el-form-item
        ></el-col>
        <el-col :xs="24" :sm="12"
          ><el-form-item label="IPv6 主地址池"
            ><el-select v-model="defaults.ipv6"
              ><el-option
                v-for="p in v6Pools"
                :key="p.pool_id"
                :label="p.name"
                :value="p.pool_id" /></el-select></el-form-item
        ></el-col>
      </el-row>
      <el-button type="primary" native-type="submit" :loading="saving" :disabled="!data"
        >保存主池</el-button
      >
    </el-form>
  </el-card>
  <el-card v-loading="loading" shadow="never">
    <template #header
      ><el-space
        ><span>地址池列表</span
        ><el-button type="primary" :disabled="saving || !data" @click="open()">新增地址池</el-button
        ><el-button :disabled="loading || saving" @click="refresh">刷新</el-button></el-space
      ></template
    >
    <el-table :data="data?.pools ?? []" row-key="pool_id">
      <el-table-column label="名称" prop="name" min-width="160" />
      <el-table-column label="地址段" min-width="240"
        ><template #default="{ row }"
          ><div v-for="s in row.segments.slice(0, 4)" :key="s">{{ s }}</div>
          <span v-if="row.segments.length > 4">共 {{ row.segments.length }} 段</span></template
        ></el-table-column
      >
      <el-table-column
        v-for="family in ['ipv4', 'ipv6'] as const"
        :key="family"
        :label="family === 'ipv4' ? 'IPv4' : 'IPv6'"
        min-width="230"
        ><template #default="{ row }"
          ><div>容量 {{ row[family].capacity }}</div>
          <div>占用 {{ row[family].allocated }} · 待释放 {{ row[family].releasing }}</div>
          <div>剩余 {{ row[family].available }}</div></template
        ></el-table-column
      >
      <el-table-column label="路由" min-width="120"
        ><template #default="{ row }"
          ><el-tooltip v-if="row.warnings.length" :content="row.warnings.join('；')"
            ><el-tag type="warning">检查内网 CIDR</el-tag></el-tooltip
          ><el-tag v-else type="success">自动下发</el-tag></template
        ></el-table-column
      >
      <el-table-column label="操作" width="150" fixed="right"
        ><template #default="{ row }"
          ><el-button link type="primary" :disabled="saving" @click="open(row)">编辑</el-button
          ><el-button link type="danger" :disabled="saving" @click="remove(row)"
            >删除</el-button
          ></template
        ></el-table-column
      >
    </el-table>
  </el-card>
  <el-dialog
    v-model="editing"
    :title="draft.id ? '编辑地址池' : '新增地址池'"
    width="640px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
  >
    <el-alert v-if="error" :title="error" type="error" show-icon :closable="false" />
    <el-form label-position="top" :disabled="saving" @submit.prevent="save">
      <el-form-item label="名称" required
        ><el-input v-model="draft.name" maxlength="128"
      /></el-form-item>
      <el-form-item label="说明"
        ><el-input v-model="draft.description" maxlength="2048"
      /></el-form-item>
      <el-form-item label="地址段" required
        ><el-input
          v-model="draft.segments"
          type="textarea"
          :rows="7"
          placeholder="每行一条 CIDR，支持 IPv4、IPv6、/32 和 /128"
      /></el-form-item>
      <el-form-item label="保留地址"
        ><el-input
          v-model="draft.exclusions"
          type="textarea"
          :rows="4"
          placeholder="每行一条 CIDR，单地址使用 /32 或 /128"
      /></el-form-item>
    </el-form>
    <template #footer
      ><el-button :disabled="saving" @click="editing = false">取消</el-button
      ><el-button
        type="primary"
        :loading="saving"
        :disabled="!draft.name.trim() || !lines(draft.segments).length"
        @click="save"
        >保存</el-button
      ></template
    >
  </el-dialog>
</template>
