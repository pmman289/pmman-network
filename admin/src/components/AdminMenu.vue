<script setup lang="ts">
import { useRoute } from 'vue-router';
import {
  DataAnalysis,
  User,
  Connection,
  Monitor,
  Setting,
  Box,
  Lock,
  Key,
  Coin,
  Document,
} from '@element-plus/icons-vue';
defineProps<{ collapsed?: boolean }>();
const emit = defineEmits<{ navigate: [] }>();
const route = useRoute();
const groups = [
  {
    title: '工作台',
    items: [{ path: '/overview', title: '运行概览', icon: DataAnalysis }],
  },
  {
    title: '资源管理',
    items: [
      { path: '/nodes', title: '节点', icon: Connection },
      { path: '/devices', title: '设备与地址', icon: Monitor },
      { path: '/address-pools', title: '地址池', icon: Coin },
      { path: '/settings', title: '网络配置', icon: Setting },
      { path: '/port-forward-entries', title: '公网入口', icon: Coin },
    ],
  },
  {
    title: '访问控制',
    items: [
      { path: '/users', title: '用户', icon: User },
      { path: '/invitations', title: '邀请码', icon: Key },
      { path: '/invitation-registrations', title: '邀请记录', icon: Document },
      { path: '/port-forward-policies', title: '转发授权', icon: Key },
    ],
  },
  {
    title: '运维管理',
    items: [
      { path: '/diagnostics', title: '客户端日志', icon: Document },
      { path: '/port-forwards', title: '转发租约', icon: Connection },
      { path: '/client-releases', title: '客户端版本', icon: Box },
    ],
  },
];
</script>
<template>
  <el-menu :default-active="route.path" :collapse="collapsed" router @select="emit('navigate')">
    <el-menu-item-group v-for="group in groups" :key="group.title" :title="group.title">
      <el-menu-item
        v-for="item in group.items"
        :key="item.path"
        :index="item.path"
        :aria-label="item.title"
      >
        <el-icon><component :is="item.icon" /></el-icon><template #title>{{ item.title }}</template>
      </el-menu-item>
    </el-menu-item-group>
    <el-menu-item-group title="账户">
      <el-menu-item index="/account" aria-label="账号安全">
        <el-icon><Lock /></el-icon><template #title>账号安全</template>
      </el-menu-item>
    </el-menu-item-group>
  </el-menu>
</template>
