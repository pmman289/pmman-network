import axios from 'axios';
import { readSession } from '../auth/session';

const client = axios.create({ baseURL: '/v1', timeout: 15_000 });
client.interceptors.request.use((config) => {
  const session = readSession();
  if (session) config.headers.Authorization = `Bearer ${session.token}`;
  return config;
});
client.interceptors.response.use(
  (response) => response,
  async (error: unknown) => {
    if (axios.isAxiosError(error)) {
      const status = error.response?.status;
      if (status === 401 && !error.config?.url?.includes('/auth/login')) {
        const current = readSession();
        if (!current || error.config?.headers.Authorization === `Bearer ${current.token}`) {
          window.dispatchEvent(new Event('pmman:unauthorized'));
        }
      }
      const defaults: Record<number, string> = {
        400: '请求参数无效，请检查表单内容',
        401: '用户名、密码错误或登录已失效',
        403: '没有权限执行此操作',
        404: '目标记录不存在，请刷新后重试',
        409: '数据冲突，请检查用户名或已有租约',
        502: '节点服务暂时不可用',
        503: '服务暂时不可用，请稍后重试',
      };
      let detail = error.response?.data?.error;
      // Downloads use blob responses even for a JSON error (e.g. retention
      // removed the report). Preserve the server's actual error message.
      if (error.response?.data instanceof Blob && error.response.data.size < 64 * 1024) {
        try {
          const body: unknown = JSON.parse(await error.response.data.text());
          if (typeof body === 'object' && body !== null && 'error' in body) detail = body.error;
        } catch {
          // Fall back to the status-specific message for non-JSON responses.
        }
      }
      return Promise.reject(
        new Error(
          typeof detail === 'string'
            ? detail
            : status
              ? (defaults[status] ?? `服务请求失败（${status}）`)
              : error.code === 'ECONNABORTED'
                ? '请求超时，请重试'
                : '无法访问服务，请检查网络连接',
        ),
      );
    }
    return Promise.reject(error);
  },
);
export async function get<T>(url: string): Promise<T> {
  return (await client.get<T>(url)).data;
}
export async function getBlob(url: string): Promise<Blob> {
  return (await client.get<Blob>(url, { responseType: 'blob' })).data;
}
export async function post<T = void>(url: string, data?: unknown): Promise<T> {
  return (await client.post<T>(url, data)).data;
}
export async function remove<T = void>(url: string): Promise<T> {
  return (await client.delete<T>(url)).data;
}
