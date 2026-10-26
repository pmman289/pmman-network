<script setup lang="ts">
import { reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { User, Lock, Connection } from '@element-plus/icons-vue';
import type { FormInstance, FormRules } from 'element-plus';
import { useAuthStore } from '../stores/auth';
const auth = useAuthStore();
const router = useRouter();
const form = reactive({ username: '', password: '' });
const formRef = ref<FormInstance>();
const loading = ref(false);
const error = ref('');
const rules: FormRules = {
  username: [{ required: true, message: '请输入用户名', trigger: 'blur' }],
  password: [{ required: true, message: '请输入密码', trigger: 'blur' }],
};
async function submit() {
  if (loading.value || !(await formRef.value?.validate().catch(() => false))) return;
  loading.value = true;
  error.value = '';
  try {
    await auth.login(form.username.trim(), form.password);
    form.password = '';
    await router.replace('/overview');
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : '登录失败';
  } finally {
    loading.value = false;
  }
}
</script>
<template>
  <div class="login-page">
    <div class="login-intro">
      <div class="brand login-brand">
        <span class="brand-symbol">P</span><span>Pmman Network</span>
      </div>
      <el-icon class="login-network"><Connection /></el-icon>
      <h1>统一管理网络接入</h1>
      <p>管理用户、连接节点与双栈地址资源。</p>
      <div class="login-caption">NETWORK MANAGEMENT CONSOLE</div>
    </div>
    <div class="login-panel">
      <div class="login-form-wrap">
        <el-tag type="info" effect="plain">管理控制台</el-tag>
        <h2>管理员登录</h2>
        <p class="muted">使用管理员账户进入网络控制台</p>
        <el-alert
          v-if="error || auth.notice"
          :title="error || auth.notice"
          type="error"
          show-icon
          :closable="false"
          class="login-alert"
        />
        <el-form
          ref="formRef"
          :model="form"
          :rules="rules"
          label-position="top"
          size="large"
          @submit.prevent="submit"
        >
          <el-form-item label="用户名" prop="username"
            ><el-input
              v-model="form.username"
              :prefix-icon="User"
              placeholder="请输入管理员用户名"
              autocomplete="username"
              :disabled="loading"
          /></el-form-item>
          <el-form-item label="密码" prop="password"
            ><el-input
              v-model="form.password"
              :prefix-icon="Lock"
              type="password"
              show-password
              placeholder="请输入密码"
              autocomplete="current-password"
              :disabled="loading"
          /></el-form-item>
          <el-button type="primary" native-type="submit" :loading="loading" class="login-submit"
            >登录控制台</el-button
          >
        </el-form>
        <div class="login-footer">Pmman Network</div>
      </div>
    </div>
  </div>
</template>
