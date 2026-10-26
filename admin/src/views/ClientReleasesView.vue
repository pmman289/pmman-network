<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import type { FormInstance, FormRules, UploadFile, UploadUserFile } from 'element-plus';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Edit, Delete, Refresh, Plus, UploadFilled } from '@element-plus/icons-vue';
import { api } from '../api/admin';
import type { ClientRelease } from '../types';
import { formatDate } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';

const channel = ref<'stable' | 'beta'>('stable');
const releases = ref<ClientRelease[]>([]);
const loading = ref(false);
const dialogVisible = ref(false);
const saving = ref(false);
const editing = ref<ClientRelease | null>(null);
const formRef = ref<FormInstance>();
const selectedFile = ref<File | null>(null);
const fileList = ref<UploadUserFile[]>([]);
const form = reactive({
  version: '',
  minSupportedVersion: '0.0.1-alpha',
  mandatoryAfter: '',
  releaseNotes: '',
});
const isEditing = computed(() => editing.value !== null);
const dialogTitle = computed(() => (isEditing.value ? '编辑客户端版本' : '创建客户端版本'));
const rules: FormRules = {
  version: [
    { required: true, message: '请输入版本号', trigger: 'blur' },
    {
      pattern: /^\d+\.\d+\.\d+([+-][0-9A-Za-z.-]+)?$/,
      message: '请输入 SemVer，例如 0.3.0',
      trigger: 'blur',
    },
  ],
  minSupportedVersion: [{ required: true, message: '请输入最低支持版本', trigger: 'blur' }],
};

function formatBytes(size: number) {
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / 1024 / 1024).toFixed(2)} MB`;
}
function notes(release: ClientRelease) {
  return release.releaseNotes.length ? release.releaseNotes.join('；') : '—';
}
async function load() {
  loading.value = true;
  try {
    releases.value = await api.clientReleases(channel.value);
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '版本列表读取失败');
  } finally {
    loading.value = false;
  }
}
function openCreate() {
  editing.value = null;
  Object.assign(form, {
    version: '',
    minSupportedVersion: '0.0.1-alpha',
    mandatoryAfter: '',
    releaseNotes: '',
  });
  selectedFile.value = null;
  fileList.value = [];
  dialogVisible.value = true;
}
function openEdit(release: ClientRelease) {
  editing.value = release;
  Object.assign(form, {
    version: release.version,
    minSupportedVersion: release.minSupportedVersion,
    mandatoryAfter: release.mandatoryAfter ?? '',
    releaseNotes: release.releaseNotes.join('\n'),
  });
  selectedFile.value = null;
  fileList.value = [];
  dialogVisible.value = true;
}
function onFileChange(file: UploadFile) {
  selectedFile.value = file.raw ?? null;
}
function onFileRemove() {
  selectedFile.value = null;
}
function buildForm() {
  const data = new FormData();
  data.append('version', form.version.trim());
  data.append('minSupportedVersion', form.minSupportedVersion.trim());
  data.append('mandatoryAfter', form.mandatoryAfter.trim());
  data.append(
    'releaseNotes',
    JSON.stringify(
      form.releaseNotes
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean),
    ),
  );
  if (selectedFile.value) data.append('package', selectedFile.value, selectedFile.value.name);
  return data;
}
async function save() {
  if (!(await formRef.value?.validate().catch(() => false))) return;
  if (!isEditing.value && !selectedFile.value) {
    ElMessage.warning('创建版本必须上传轻量更新安装包');
    return;
  }
  saving.value = true;
  try {
    const result = isEditing.value
      ? await api.editClientRelease(channel.value, form.version, buildForm())
      : await api.createClientRelease(channel.value, buildForm());
    ElMessage.success(`版本 ${result.version} 已保存`);
    dialogVisible.value = false;
    await load();
  } catch (cause) {
    ElMessage.error(cause instanceof Error ? cause.message : '版本保存失败');
  } finally {
    saving.value = false;
  }
}
async function remove(release: ClientRelease) {
  try {
    await ElMessageBox.confirm(
      `删除 ${release.version} 后，客户端将无法再获取该版本。${release.active ? '当前对外发布版本也会切换到下一个最高版本。' : ''}`,
      '删除客户端版本',
      { type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消' },
    );
    await api.deleteClientRelease(channel.value, release.version);
    ElMessage.success('版本已删除');
    await load();
  } catch (cause) {
    if (cause instanceof Error) ElMessage.error(cause.message);
  }
}
onMounted(() => void load());
</script>

<template>
  <PageHeading title="客户端版本" description="管理版本、更新日志和更新包">
    <el-select v-model="channel" style="width: 120px" @change="load">
      <el-option label="稳定版" value="stable" />
      <el-option label="测试版" value="beta" />
    </el-select>
    <el-button :icon="Refresh" :loading="loading" @click="load">刷新</el-button>
    <el-button type="primary" :icon="Plus" @click="openCreate">创建版本</el-button>
  </PageHeading>

  <el-card shadow="never" class="release-card">
    <el-alert
      title="上传用于客户端自更新的轻量安装包。"
      type="info"
      show-icon
      :closable="false"
      class="release-tip"
    />
    <el-table v-loading="loading" :data="releases" row-key="version" empty-text="暂无已发布版本">
      <el-table-column prop="version" label="版本" width="120" fixed>
        <template #default="{ row }">
          <span class="release-version">v{{ row.version }}</span>
          <el-tag v-if="row.active" type="success" size="small">当前发布</el-tag>
        </template>
      </el-table-column>
      <el-table-column label="更新日志" min-width="300" show-overflow-tooltip>
        <template #default="{ row }">{{ notes(row) }}</template>
      </el-table-column>
      <el-table-column label="最低支持" prop="minSupportedVersion" width="110" />
      <el-table-column label="安装包" width="180">
        <template #default="{ row }"
          >{{ formatBytes(row.size) }} · {{ row.platform }}/{{ row.arch }}</template
        >
      </el-table-column>
      <el-table-column label="SHA-256" min-width="190" show-overflow-tooltip>
        <template #default="{ row }">
          <span class="release-hash" :title="row.sha256">{{ row.sha256 }}</span>
        </template>
      </el-table-column>
      <el-table-column label="发布时间" width="180">
        <template #default="{ row }">{{ formatDate(row.publishedAt) }}</template>
      </el-table-column>
      <el-table-column label="操作" width="150" fixed="right">
        <template #default="{ row }">
          <el-button text type="primary" :icon="Edit" @click="openEdit(row)">编辑</el-button>
          <el-button text type="danger" :icon="Delete" @click="remove(row)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>
  </el-card>

  <el-dialog v-model="dialogVisible" :title="dialogTitle" width="620px" destroy-on-close>
    <el-form ref="formRef" :model="form" :rules="rules" label-position="top">
      <el-row :gutter="16">
        <el-col :span="12">
          <el-form-item label="版本号" prop="version">
            <el-input v-model="form.version" placeholder="0.3.0" :disabled="isEditing || saving" />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item label="最低支持版本" prop="minSupportedVersion">
            <el-input
              v-model="form.minSupportedVersion"
              placeholder="0.0.1-alpha"
              :disabled="saving"
            />
          </el-form-item>
        </el-col>
      </el-row>
      <el-form-item label="强制更新时间（可选）">
        <el-input
          v-model="form.mandatoryAfter"
          placeholder="RFC3339，例如 2026-12-01T00:00:00Z"
          :disabled="saving"
        />
      </el-form-item>
      <el-form-item label="更新日志" help="每行一条，客户端弹窗会原样展示">
        <el-input
          v-model="form.releaseNotes"
          type="textarea"
          :rows="5"
          placeholder="修复连接稳定性问题&#10;优化更新下载体验"
          :disabled="saving"
        />
      </el-form-item>
      <el-form-item :label="isEditing ? '替换轻量安装包（可选）' : '轻量安装包（必选）'">
        <el-upload
          v-model:file-list="fileList"
          drag
          action="#"
          accept=".exe"
          :auto-upload="false"
          :limit="1"
          :disabled="saving"
          :on-change="onFileChange"
          :on-remove="onFileRemove"
        >
          <el-icon class="el-icon--upload"><UploadFilled /></el-icon>
          <div class="el-upload__text">拖入 .exe 或 <em>点击选择</em></div>
          <template #tip
            ><div class="el-upload__tip">
              仅支持不含首次安装依赖的 x64 Windows 更新安装包，最大 64 MiB。
            </div></template
          >
        </el-upload>
      </el-form-item>
    </el-form>
    <template #footer>
      <el-button :disabled="saving" @click="dialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="saving" @click="save">保存并签名</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.release-card {
  min-height: 440px;
}
.release-tip {
  margin-bottom: 16px;
}
.release-version {
  margin-right: 8px;
  font-weight: 650;
}
.release-hash {
  display: block;
  overflow: hidden;
  font-family: var(--el-font-family-monospace, monospace);
  text-overflow: ellipsis;
  white-space: nowrap;
}
.el-upload {
  width: 100%;
}
</style>
