import { computed, ref } from 'vue';
import { defineStore } from 'pinia';
import { api } from '../api/admin';
import { readSession, saveSession } from '../auth/session';

export const useAuthStore = defineStore('auth', () => {
  const session = ref(readSession());
  const notice = ref('');
  const authenticated = computed(
    () => !!session.value && Date.parse(session.value.expires_at) > Date.now(),
  );
  async function login(username: string, password: string) {
    const result = await api.login(username, password);
    if (result.role !== 'admin') throw new Error('此账户没有管理后台访问权限');
    session.value = result;
    saveSession(result);
    notice.value = '';
  }
  function logout(message = '') {
    session.value = null;
    saveSession(null);
    notice.value = message;
  }
  async function logoutRemote(message = '') {
    // Revocation is best effort: local logout must still complete if the
    // control plane is unreachable or the token already expired.
    await api.logout().catch(() => undefined);
    logout(message);
  }
  return { session, notice, authenticated, login, logout, logoutRemote };
});
