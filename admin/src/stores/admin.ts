import { ref } from 'vue';
import { defineStore } from 'pinia';
import { api } from '../api/admin';
import { addressPoolsApi, type PoolList } from '../api/addressPools';
import { useAuthStore } from './auth';
import type { User, Node, Device, Pool } from '../types';

export const useAdminStore = defineStore('admin', () => {
  const users = ref<User[]>([]);
  const nodes = ref<Node[]>([]);
  const devices = ref<Device[]>([]);
  const pool = ref<Pool | null>(null);
  const addressPools = ref<PoolList | null>(null);
  const loading = ref(false);
  const error = ref('');
  const updatedAt = ref('');
  let generation = 0;
  async function refresh() {
    const request = ++generation;
    const token = useAuthStore().session?.token;
    loading.value = true;
    error.value = '';
    try {
      const result = await Promise.all([
        api.users(),
        api.nodes(),
        api.devices(),
        api.pool(),
        addressPoolsApi.list(),
      ]);
      if (request !== generation || !token || token !== useAuthStore().session?.token) return;
      [users.value, nodes.value, devices.value, pool.value, addressPools.value] = result;
      updatedAt.value = new Date().toISOString();
    } catch (cause) {
      if (request === generation && useAuthStore().authenticated)
        error.value = cause instanceof Error ? cause.message : '数据读取失败';
    } finally {
      if (request === generation) loading.value = false;
    }
  }
  function clear() {
    generation++;
    users.value = [];
    nodes.value = [];
    devices.value = [];
    pool.value = null;
    addressPools.value = null;
    error.value = '';
    updatedAt.value = '';
    loading.value = false;
  }
  return { users, nodes, devices, pool, addressPools, loading, error, updatedAt, refresh, clear };
});
