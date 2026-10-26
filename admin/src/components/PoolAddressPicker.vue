<script setup lang="ts">
import { ref, watch } from 'vue';
import { addressPoolsApi, type AddressPage } from '../api/addressPools';
const props = defineProps<{ poolId: string; family: number; userId: string; disabled?: boolean }>();
const model = defineModel<string | null>({ required: true });
const page = ref<AddressPage | null>(null);
const loading = ref(false);
const error = ref('');
const search = ref('');
let generation = 0;
async function load(cursor?: string) {
  const request = ++generation;
  if (!props.poolId) {
    page.value = null;
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    const query: Record<string, string> = { family: String(props.family), user_id: props.userId };
    if (search.value.trim()) query.address = search.value.trim();
    if (cursor) query.cursor = cursor;
    const result = await addressPoolsApi.addresses(props.poolId, query);
    if (request === generation)
      page.value =
        cursor && page.value
          ? { ...result, items: [...page.value.items, ...result.items] }
          : result;
  } catch (cause) {
    if (request === generation)
      error.value = cause instanceof Error ? cause.message : '读取地址失败';
  } finally {
    if (request === generation) loading.value = false;
  }
}
watch(
  () => [props.poolId, props.family],
  () => {
    search.value = '';
    page.value = null;
    void load();
  },
  { immediate: true },
);
</script>
<template>
  <el-space direction="vertical" alignment="stretch" fill>
    <el-input
      v-model="search"
      :disabled="disabled || loading"
      placeholder="输入完整 IP 查找，留空浏览可用地址"
      @keyup.enter="load()"
      ><template #append
        ><el-button :disabled="loading || disabled" @click="load()">查找</el-button></template
      ></el-input
    >
    <el-select
      v-model="model"
      :loading="loading"
      :disabled="disabled || !poolId"
      placeholder="选择具体地址"
      filterable
    >
      <el-option
        v-if="model && !page?.items.some((a) => a.address === model)"
        :value="model"
        :label="model"
      />
      <el-option
        v-for="item in page?.items ?? []"
        :key="item.address"
        :value="item.address"
        :label="`${item.address}${item.status === 'owned' ? '（当前用户）' : ''}`"
      />
    </el-select>
    <el-button
      v-if="page?.next_cursor"
      :loading="loading"
      :disabled="disabled"
      @click="load(page.next_cursor ?? undefined)"
      >更多地址</el-button
    >
    <el-text v-if="error" type="danger" role="alert">{{ error }}</el-text>
    <el-text v-else-if="page && !page.items.length" type="info">未找到可用地址</el-text>
  </el-space>
</template>
