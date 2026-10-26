<script setup lang="ts">
import { reactive, ref, watch } from 'vue';
import { ElMessage, type UploadProps, type UploadUserFile } from 'element-plus';
import { api } from '../api/admin';
import type { GeositeInfo } from '../types';
import { confirmAction } from '../composables/useAdmin';

const visible = defineModel<boolean>({ required: true });
const loading = ref(false);
const saving = ref(false);
const error = ref('');
const current = ref<GeositeInfo | null>(null);
const files = ref<UploadUserFile[]>([]);
const form = reactive({ version: '', domestic: 'cn', foreign: 'geolocation-!cn' });
let generation = 0;
watch(visible, async (open) => {
  const request = ++generation;
  if (!open) return;
  files.value = [];
  error.value = '';
  loading.value = true;
  try {
    const info = await api.geosite();
    if (request !== generation) return;
    current.value = info;
    Object.assign(form, info ?? { version: '', domestic: 'cn', foreign: 'geolocation-!cn' });
  } catch (cause) {
    if (request === generation)
      error.value = cause instanceof Error ? cause.message : '读取资源失败';
  } finally {
    if (request === generation) loading.value = false;
  }
});
const selectFile: UploadProps['onChange'] = (file) => {
  if (file.size && file.size > 32 * 1024 * 1024) {
    files.value = [];
    ElMessage.error('文件不能超过 32 MiB');
  }
};
async function save() {
  if (saving.value || loading.value) return;
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(form.version.trim())) {
    ElMessage.error('请输入版本号（最多 64 位字母、数字、点、短横线及下划线）');
    return;
  }
  if (!form.domestic.trim() || !form.foreign.trim()) {
    ElMessage.error('请填写国内和国外分类');
    return;
  }
  if (!current.value && !files.value[0]?.raw) {
    ElMessage.error('请先上传 geosite.dat');
    return;
  }
  if (!(await confirmAction('保存将更新所有已启用 Geosite 分流的节点，确认发布？'))) return;
  saving.value = true;
  error.value = '';
  try {
    const data = new FormData();
    for (const [key, value] of Object.entries(form)) data.append(key, value.trim());
    if (files.value[0]?.raw) data.append('file', files.value[0].raw);
    current.value = await api.saveGeosite(data);
    files.value = [];
    ElMessage.success('资源已发布，节点将在下次心跳加载');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '发布失败';
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <el-dialog
    v-model="visible"
    title="Geosite 资源"
    width="min(640px, 94vw)"
    :close-on-click-modal="!saving"
    :show-close="!saving"
  >
    <el-alert v-if="error" :title="error" type="error" :closable="false" show-icon />
    <el-form v-loading="loading" :model="form" label-position="top" :disabled="saving || loading">
      <el-alert
        title="节点本地分流"
        description="支持 V2Fly / Xray geosite.dat。仅向已启用分流的节点下发，客户端不下载规则文件。"
        type="info"
        :closable="false"
      />
      <el-form-item label="版本号" required
        ><el-input v-model="form.version" maxlength="64" placeholder="例如 2026-10-07"
      /></el-form-item>
      <el-form-item label="规则文件"
        ><el-upload
          v-model:file-list="files"
          :auto-upload="false"
          :limit="1"
          accept=".dat"
          :on-change="selectFile"
          ><el-button>选择 geosite.dat</el-button
          ><template #tip
            ><div class="form-help">最多 32 MiB；留空保留当前文件。</div></template
          ></el-upload
        ></el-form-item
      >
      <el-form-item label="国内分类" required
        ><el-input v-model="form.domestic" placeholder="cn；多分类用逗号分隔，支持 @属性"
      /></el-form-item>
      <el-form-item label="国外分类" required
        ><el-input v-model="form.foreign" placeholder="geolocation-!cn"
      /></el-form-item>
      <el-descriptions v-if="current" :column="1" border>
        <el-descriptions-item label="当前版本">{{ current.version }}</el-descriptions-item>
        <el-descriptions-item label="文件大小"
          >{{ (current.size / 1024 / 1024).toFixed(2) }} MiB</el-descriptions-item
        >
        <el-descriptions-item label="SHA-256"
          ><el-text style="overflow-wrap: anywhere">{{
            current.sha256
          }}</el-text></el-descriptions-item
        >
        <el-descriptions-item label="分类数">{{ current.categories.length }}</el-descriptions-item>
      </el-descriptions>
    </el-form>
    <template #footer
      ><el-button :disabled="saving" @click="visible = false">关闭</el-button
      ><el-button
        type="primary"
        :loading="saving"
        :disabled="loading || (!!error && !current)"
        @click="save"
        >发布资源</el-button
      ></template
    >
  </el-dialog>
</template>
