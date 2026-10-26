import { get, post, remove } from './client';

export interface FamilyPolicy {
  mode: 'automatic' | 'pool_fixed' | 'manual';
  pool_id: string | null;
  address: string | null;
}
export interface AddressPolicy {
  revision: number;
  ipv4: FamilyPolicy;
  ipv6: FamilyPolicy;
  current_ipv4: string | null;
  current_ipv6: string | null;
}
export interface PoolStats {
  capacity: string;
  allocated: string;
  releasing: string;
  available: string;
}
export interface AddressPool {
  pool_id: string;
  name: string;
  description: string;
  segments: string[];
  exclusions: string[];
  ipv4: PoolStats;
  ipv6: PoolStats;
  warnings: string[];
}
export interface PoolList {
  revision: number;
  ipv4_pool_id: string;
  ipv6_pool_id: string;
  pools: AddressPool[];
}
export interface PoolDraft {
  revision: number;
  name: string;
  description: string;
  segments: string[];
  exclusions: string[];
}
export interface AddressPage {
  revision: number;
  items: { address: string; status: string; user_id: string | null }[];
  next_cursor: string | null;
}
export const addressPoolsApi = {
  list: () => get<PoolList>('/admin/address-pools'),
  save: (id: string | null, draft: PoolDraft) =>
    post<PoolList>(`/admin/address-pools${id ? `/${encodeURIComponent(id)}` : ''}`, draft),
  remove: (id: string, revision: number) =>
    remove(`/admin/address-pools/${encodeURIComponent(id)}?revision=${revision}`),
  defaults: (revision: number, ipv4_pool_id: string, ipv6_pool_id: string) =>
    post<PoolList>('/admin/address-pool-defaults', { revision, ipv4_pool_id, ipv6_pool_id }),
  policy: (username: string) =>
    get<AddressPolicy>(`/admin/users/${encodeURIComponent(username)}/address-policy`),
  savePolicy: (username: string, policy: AddressPolicy) =>
    post<AddressPolicy>(`/admin/users/${encodeURIComponent(username)}/address-policy`, policy),
  addresses: (id: string, query: Record<string, string>) =>
    get<AddressPage>(
      `/admin/address-pools/${encodeURIComponent(id)}/addresses?${new URLSearchParams(query)}`,
    ),
  saveRoutes: (internal_cidrs: string[]) =>
    post('/admin/settings/internal-routes', { internal_cidrs }),
};
