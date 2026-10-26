<script setup lang="ts">
import { reactive, ref } from 'vue';
import type { FormInstance, FormRules } from 'element-plus';
import { registerAccount } from '../api/registration';
// Public registration deliberately has no admin auth store/interceptors.
const form = reactive({
  code: new URLSearchParams(window.location.hash.slice(1)).get('invite') ?? '',
  username: '',
  password: '',
  confirm: '',
});
window.history.replaceState(null, '', window.location.pathname);
const formRef = ref<FormInstance>();
const pending = ref(false);
const error = ref('');
const done = ref(false);
const rules: FormRules = {
  code: [
    { required: true, message: '请输入邀请码', trigger: 'blur' },
    { pattern: /^[a-fA-F0-9]{64}$/, message: '邀请码格式无效', trigger: 'blur' },
  ],
  username: [
    { required: true, message: '请输入用户名', trigger: 'blur' },
    {
      pattern: /^[A-Za-z0-9_.-]{1,64}$/,
      message: '使用字母、数字、点、短横线或下划线，最长 64 位',
      trigger: 'blur',
    },
  ],
  password: [
    { required: true, message: '请输入密码', trigger: 'blur' },
    { min: 6, max: 256, message: '密码长度为 6 至 256 位', trigger: 'blur' },
  ],
  confirm: [
    {
      validator: (_r, _v, cb) =>
        cb(form.confirm === form.password ? undefined : new Error('两次输入的密码不一致')),
      trigger: 'blur',
    },
  ],
};
async function submit() {
  if (pending.value || !(await formRef.value?.validate().catch(() => false))) return;
  pending.value = true;
  error.value = '';
  try {
    await registerAccount({
      code: form.code.trim(),
      username: form.username.trim(),
      password: form.password,
    });
    done.value = true;
    form.code = '';
    form.password = '';
    form.confirm = '';
  } catch (e) {
    error.value = e instanceof Error ? e.message : '注册结果未确认，请在客户端尝试登录';
  } finally {
    pending.value = false;
  }
}
</script>
<template>
  <main class="register-page">
    <section class="register-card">
      <h1>Pmman Network</h1>
      <h2>使用邀请码注册</h2>
      <el-result
        v-if="done"
        icon="success"
        title="注册成功"
        :sub-title="`请在 Pmman Network 客户端登录 ${form.username}`"
      />
      <el-form
        v-else
        ref="formRef"
        :model="form"
        :rules="rules"
        label-position="top"
        :disabled="pending"
        @submit.prevent="submit"
      >
        <el-form-item label="邀请码" prop="code"
          ><el-input v-model="form.code" autocomplete="off" maxlength="64"
        /></el-form-item>
        <el-form-item label="用户名" prop="username"
          ><el-input v-model="form.username" autocomplete="username" maxlength="64"
        /></el-form-item>
        <el-form-item label="密码" prop="password"
          ><el-input
            v-model="form.password"
            type="password"
            show-password
            autocomplete="new-password"
            maxlength="256"
        /></el-form-item>
        <el-form-item label="确认密码" prop="confirm"
          ><el-input
            v-model="form.confirm"
            type="password"
            show-password
            autocomplete="new-password"
            maxlength="256"
        /></el-form-item>
        <el-alert
          v-if="error"
          :title="error"
          type="error"
          :closable="false"
          role="alert"
        /><el-button native-type="submit" type="primary" :loading="pending">{{
          pending ? '正在注册…' : '注册'
        }}</el-button>
      </el-form>
    </section>
  </main>
</template>
<style scoped>
:global(body) {
  margin: 0;
}
.register-page {
  min-height: 100dvh;
  display: grid;
  place-items: center;
  background: #f4f6fa;
  padding: 24px;
  box-sizing: border-box;
  font-family: system-ui, sans-serif;
  color: #253047;
}
.register-card {
  width: min(100%, 420px);
  padding: 32px;
  background: white;
  border: 1px solid #e2e7ef;
  border-radius: 16px;
  box-sizing: border-box;
}
h1 {
  font-size: 22px;
}
h2 {
  font-size: 17px;
  margin-bottom: 24px;
}
.el-button {
  width: 100%;
  margin-top: 16px;
}
@media (prefers-color-scheme: dark) {
  .register-page {
    background: #111827;
  }
  .register-card {
    background: #192230;
    color: #edf2fb;
  }
}
</style>
