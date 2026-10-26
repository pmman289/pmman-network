<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { Fold, Refresh, ArrowDown } from '@element-plus/icons-vue';
import AdminMenu from '../components/AdminMenu.vue';
import { useAuthStore } from '../stores/auth';
import { useAdminStore } from '../stores/admin';
import { formatDate } from '../composables/useAdmin';
const route = useRoute();
const router = useRouter();
const auth = useAuthStore();
const store = useAdminStore();
const collapsed = ref(false);
const mobileMenu = ref(false);
function toggleMenu() {
  if (window.innerWidth <= 768) mobileMenu.value = true;
  else collapsed.value = !collapsed.value;
}
async function logout() {
  await auth.logoutRemote();
  store.clear();
  await router.replace('/login');
}
async function handleAccountCommand(command: string) {
  if (command === 'account') await router.push('/account');
  else await logout();
}
onMounted(() => {
  void store.refresh();
});
</script>
<template>
  <el-container class="admin-shell">
    <el-aside :width="collapsed ? '64px' : '224px'" class="desktop-sidebar">
      <div class="brand">
        <span class="brand-symbol">P</span
        ><span v-if="!collapsed">Pmman <small>NETWORK</small></span>
      </div>
      <AdminMenu :collapsed="collapsed" />
      <div v-if="!collapsed" class="sidebar-footer">网络接入管理</div>
    </el-aside>
    <el-container class="workspace">
      <el-header class="admin-header">
        <div class="header-left">
          <el-button text :icon="Fold" aria-label="展开或收起导航" @click="toggleMenu" />
          <el-breadcrumb separator="/"
            ><el-breadcrumb-item :to="{ path: '/overview' }">工作台</el-breadcrumb-item
            ><el-breadcrumb-item>{{ route.meta.title }}</el-breadcrumb-item></el-breadcrumb
          >
        </div>
        <div class="header-right">
          <el-tag class="admin-role" size="small" type="info" effect="plain">管理员</el-tag>
          <el-dropdown @command="handleAccountCommand">
            <el-button text
              ><el-avatar :size="28">{{
                auth.session?.username.slice(0, 1).toUpperCase()
              }}</el-avatar
              ><span class="identity">{{ auth.session?.username }}</span
              ><el-icon><ArrowDown /></el-icon
            ></el-button>
            <template #dropdown
              ><el-dropdown-menu
                ><el-dropdown-item command="account">账号安全</el-dropdown-item
                ><el-dropdown-item command="logout">退出登录</el-dropdown-item></el-dropdown-menu
              ></template
            >
          </el-dropdown>
        </div>
      </el-header>
      <el-main class="admin-main">
        <div class="update-bar">
          <span>更新于 {{ formatDate(store.updatedAt) }}</span
          ><el-button :icon="Refresh" :loading="store.loading" size="small" @click="store.refresh"
            >刷新</el-button
          >
        </div>
        <el-alert
          v-if="store.error"
          :title="store.error"
          description="数据可能已过期，请刷新重试。"
          type="error"
          show-icon
          :closable="false"
          class="read-error"
        />
        <router-view />
        <footer class="content-footer">Pmman Network · 网络接入管理</footer>
      </el-main>
    </el-container>
    <el-drawer
      v-model="mobileMenu"
      direction="ltr"
      size="240px"
      title="Pmman Network"
      class="navigation-drawer"
      ><AdminMenu @navigate="mobileMenu = false"
    /></el-drawer>
  </el-container>
</template>
