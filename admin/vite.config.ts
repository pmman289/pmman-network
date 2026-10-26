import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

export default defineConfig({
  base: '/admin/',
  plugins: [vue()],
  build: {
    outDir: '../server/admin-dist',
    emptyOutDir: true,
    rollupOptions: { input: { admin: 'index.html', register: 'register.html' } },
  },
  server: {
    port: 1430,
    strictPort: true,
    proxy: { '/v1': { target: process.env.PMMAN_ADMIN_API ?? 'http://127.0.0.1:18080' } },
  },
});
