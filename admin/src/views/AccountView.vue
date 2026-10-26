<script setup lang="ts">
import { reactive, ref } from 'vue';
import type { FormInstance, FormRules } from 'element-plus';
import PageHeading from '../components/PageHeading.vue';
import { api } from '../api/admin';
import { useAuthStore } from '../stores/auth';
import { useMutation } from '../composables/useAdmin';
import { saveSession } from '../auth/session';

const auth = useAuthStore();
const formRef = ref<FormInstance>();
const { saving, run } = useMutation();
const form = reactive({ current_password: '', new_password: '', confirm_password: '' });
const rules: FormRules = {
  current_password: [{ required: true, message: '请输入当前密码', trigger: 'blur' }],
  new_password: [
    { required: true, message: '请输入新密码', trigger: 'blur' },
    { min: 6, max: 256, message: '密码长度应为 6 至 256 位', trigger: 'blur' },
  ],
  confirm_password: [
    { required: true, message: '请再次输入新密码', trigger: 'blur' },
    {
      validator: (_rule, value, callback) =>
        value === form.new_password ? callback() : callback(new Error('两次输入的密码不一致')),
      trigger: 'blur',
    },
  ],
};
async function submit() {
  if (!(await formRef.value?.validate().catch(() => false))) return;
  await run(
    () => api.changePassword(form.current_password, form.new_password),
    '密码已修改，后续登录请使用新密码',
    () => {
      form.current_password = '';
      form.new_password = '';
      form.confirm_password = '';
      if (auth.session) {
        auth.session.must_change_password = false;
        saveSession(auth.session);
      }
    },
  );
}
</script>

<template>
  <PageHeading
    title="账号安全"
    :description="
      auth.session?.must_change_password ? '请先设置新的管理员密码' : '修改当前管理员账户密码'
    "
  />
  <el-card shadow="never" class="account-card">
    <el-alert
      v-if="auth.session?.must_change_password"
      title="请先修改初始密码"
      description="完成修改后才能继续使用管理后台。"
      type="warning"
      show-icon
      :closable="false"
    />
    <el-form
      ref="formRef"
      class="password-form"
      :model="form"
      :rules="rules"
      label-position="top"
      @submit.prevent="submit"
    >
      <el-form-item label="当前密码" prop="current_password">
        <el-input
          v-model="form.current_password"
          type="password"
          show-password
          autocomplete="current-password"
        />
      </el-form-item>
      <el-form-item label="新密码" prop="new_password">
        <el-input
          v-model="form.new_password"
          type="password"
          show-password
          autocomplete="new-password"
        />
      </el-form-item>
      <el-form-item label="确认新密码" prop="confirm_password">
        <el-input
          v-model="form.confirm_password"
          type="password"
          show-password
          autocomplete="new-password"
        />
      </el-form-item>
      <el-button type="primary" :loading="saving" @click="submit">修改密码</el-button>
    </el-form>
  </el-card>
</template>

<style scoped>
.account-card {
  max-width: 640px;
}
.password-form {
  max-width: 440px;
  margin-top: 24px;
}
</style>
