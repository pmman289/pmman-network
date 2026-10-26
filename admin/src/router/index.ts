import { createRouter, createWebHashHistory } from 'vue-router';
import { useAuthStore } from '../stores/auth';
import { useAdminStore } from '../stores/admin';

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    {
      path: '/login',
      component: () => import('../views/LoginView.vue'),
      meta: { title: '登录', public: true },
    },
    {
      path: '/',
      component: () => import('../layouts/AdminLayout.vue'),
      children: [
        { path: '', redirect: '/overview' },
        {
          path: 'overview',
          component: () => import('../views/OverviewView.vue'),
          meta: { title: '运行概览' },
        },
        {
          path: 'users',
          component: () => import('../views/UsersView.vue'),
          meta: { title: '用户管理' },
        },
        {
          path: 'invitations',
          component: () => import('../views/InvitationsView.vue'),
          meta: { title: '邀请码' },
        },
        {
          path: 'invitation-registrations',
          component: () => import('../views/InvitationsView.vue'),
          props: { records: true },
          meta: { title: '邀请记录' },
        },
        {
          path: 'nodes',
          component: () => import('../views/NodesView.vue'),
          meta: { title: '节点管理' },
        },
        {
          path: 'devices',
          component: () => import('../views/DevicesView.vue'),
          meta: { title: '设备与地址' },
        },
        {
          path: 'address-pools',
          component: () => import('../views/AddressPoolsView.vue'),
          meta: { title: '地址池' },
        },
        {
          path: 'settings',
          component: () => import('../views/SettingsView.vue'),
          meta: { title: '网络配置' },
        },
        {
          path: 'client-releases',
          component: () => import('../views/ClientReleasesView.vue'),
          meta: { title: '客户端版本' },
        },
        {
          path: 'port-forwards',
          component: () => import('../views/PortForwardsView.vue'),
          props: { section: 'allocations' },
          meta: { title: '转发租约' },
        },
        {
          path: 'port-forward-entries',
          component: () => import('../views/PortForwardsView.vue'),
          props: { section: 'entries' },
          meta: { title: '公网入口' },
        },
        {
          path: 'port-forward-policies',
          component: () => import('../views/PortForwardPoliciesView.vue'),
          meta: { title: '端口转发授权' },
        },
        {
          path: 'diagnostics',
          component: () => import('../views/DiagnosticsView.vue'),
          meta: { title: '客户端日志' },
        },
        {
          path: 'account',
          component: () => import('../views/AccountView.vue'),
          meta: { title: '账号安全' },
        },
        {
          path: ':pathMatch(.*)*',
          component: () => import('../views/NotFoundView.vue'),
          meta: { title: '页面不存在' },
        },
      ],
    },
  ],
});
router.beforeEach((to) => {
  const auth = useAuthStore();
  if (auth.session && Date.parse(auth.session.expires_at) <= Date.now()) {
    auth.logout('会话已到期，请重新登录');
    useAdminStore().clear();
  }
  if (!to.meta.public && !auth.authenticated) return '/login';
  if (to.path === '/login' && auth.authenticated) return '/overview';
  if (auth.authenticated && auth.session?.must_change_password && to.path !== '/account') {
    return '/account';
  }
});
router.afterEach((to) => {
  document.title = `${to.meta.title ?? '控制台'} · Pmman Network`;
});
