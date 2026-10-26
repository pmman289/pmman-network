<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref } from 'vue';
import { Download, Refresh, Search } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { api } from '../api/admin';
import type { DiagnosticReport } from '../types';
import { formatDate, usePagination } from '../composables/useAdmin';
import { useAuthStore } from '../stores/auth';
import PageHeading from '../components/PageHeading.vue';
import ListPagination from '../components/ListPagination.vue';

const auth = useAuthStore();
const reports = ref<DiagnosticReport[]>([]);
const query = ref('');
const loading = ref(false);
const error = ref('');
const downloading = ref('');
let alive = true;
let generation = 0;
const filtered = computed(() =>
  reports.value.filter((report) =>
    `${report.username} ${report.user_id} ${report.report_id}`
      .toLowerCase()
      .includes(query.value.trim().toLowerCase()),
  ),
);
const { page, size, rows } = usePagination(filtered);
async function refresh() {
  const request = ++generation;
  const token = auth.session?.token;
  loading.value = true;
  error.value = '';
  try {
    const data = await api.diagnosticReports();
    if (alive && request === generation && token === auth.session?.token) reports.value = data;
  } catch (cause) {
    if (alive && request === generation && token === auth.session?.token)
      error.value = cause instanceof Error ? cause.message : '读取报告失败';
  } finally {
    if (alive && request === generation) loading.value = false;
  }
}
async function download(report: DiagnosticReport) {
  if (downloading.value) return;
  downloading.value = report.report_id;
  const token = auth.session?.token;
  try {
    const blob = await api.downloadDiagnostic(report.user_id, report.report_id);
    if (!alive || token !== auth.session?.token) return;
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${report.report_id}.log`;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch (cause) {
    if (alive && token === auth.session?.token)
      ElMessage.error(cause instanceof Error ? cause.message : '下载失败');
  } finally {
    if (alive) downloading.value = '';
  }
}
onMounted(() => {
  void refresh();
});
onBeforeUnmount(() => {
  alive = false;
  generation++;
});
</script>

<template>
  <PageHeading title="客户端日志" description="查看用户上报的诊断报告，每个账户保留最近 5 份">
    <el-button :icon="Refresh" :loading="loading" @click="refresh">刷新</el-button>
  </PageHeading>
  <el-alert
    v-if="error"
    :title="error"
    type="error"
    show-icon
    :closable="false"
    class="read-error"
  />
  <el-card shadow="never">
    <div class="filter-bar">
      <el-input
        v-model="query"
        :prefix-icon="Search"
        placeholder="搜索用户或报告编号"
        aria-label="搜索诊断报告"
        clearable
      />
    </div>
    <el-table v-loading="loading" :data="rows" row-key="report_id" empty-text="暂无诊断报告">
      <el-table-column prop="username" label="用户" min-width="150" show-overflow-tooltip />
      <el-table-column prop="report_id" label="报告编号" min-width="360" show-overflow-tooltip />
      <el-table-column label="上报时间" width="190">
        <template #default="{ row }">{{ formatDate(row.uploaded_at) }}</template>
      </el-table-column>
      <el-table-column label="大小" width="100">
        <template #default="{ row }">{{ (row.size / 1024).toFixed(1) }} KiB</template>
      </el-table-column>
      <el-table-column label="操作" width="110" fixed="right">
        <template #default="{ row }">
          <el-button
            link
            type="primary"
            :icon="Download"
            :loading="downloading === row.report_id"
            :disabled="!!downloading && downloading !== row.report_id"
            @click="download(row)"
            >下载</el-button
          >
        </template>
      </el-table-column>
    </el-table>
    <ListPagination v-model:page="page" v-model:size="size" :total="filtered.length" />
  </el-card>
</template>
