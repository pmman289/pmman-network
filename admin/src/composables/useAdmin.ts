import { computed, ref, watch, type Ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { useAdminStore } from '../stores/admin';
export { formatDate, formatLastActivity } from '../utils/dates';

export function usePagination<T>(items: Ref<T[]>) {
  const page = ref(1);
  const size = ref(20);
  watch(items, () => {
    page.value = 1;
  });
  watch(size, () => {
    page.value = 1;
  });
  const rows = computed(() =>
    items.value.slice((page.value - 1) * size.value, page.value * size.value),
  );
  return { page, size, rows };
}
export function useMutation() {
  const saving = ref(false);
  const store = useAdminStore();
  async function run(task: () => Promise<unknown>, message: string, after?: () => void) {
    if (saving.value) return;
    saving.value = true;
    try {
      await task();
      ElMessage.success(message);
      after?.();
      await store.refresh();
    } catch (cause) {
      ElMessage.error(cause instanceof Error ? cause.message : '操作失败');
    } finally {
      saving.value = false;
    }
  }
  return { saving, run };
}
export async function confirmAction(message: string): Promise<boolean> {
  try {
    await ElMessageBox.confirm(message, '操作确认', {
      confirmButtonText: '确认',
      cancelButtonText: '取消',
      type: 'warning',
    });
    return true;
  } catch {
    return false;
  }
}
export const trafficLabel = (mode: string) =>
  ({ internal: '内部网络', split: '境外分流', global: '全局' })[mode] ?? mode;
