import { createApp } from 'vue';
import { createPinia } from 'pinia';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import './styles/theme.css';
import App from './App.vue';
import { router } from './router';
import { useAuthStore } from './stores/auth';
import { useAdminStore } from './stores/admin';

const app = createApp(App);
app.use(createPinia());
app.use(router);
app.use(ElementPlus);
window.addEventListener('pmman:unauthorized', () => {
  useAuthStore().logout('登录已失效，请重新登录');
  useAdminStore().clear();
  void router.replace('/login');
});
app.mount('#app');
