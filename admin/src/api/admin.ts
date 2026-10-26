import { get, getBlob, post, remove } from './client';
import type {
  Session,
  User,
  CreateUser,
  Capabilities,
  Node,
  Device,
  Pool,
  NodeEnrollment,
  CreateNode,
  NodeSettings,
  NodeRelease,
  NodeBirdConfig,
  ClientRelease,
  GeositeInfo,
  AddressLease,
  PortForwardEntry,
  PortForward,
  PortForwardPolicy,
  DiagnosticReport,
} from '../types';
const userPath = (username: string) => `/admin/users/${encodeURIComponent(username)}`;
export const api = {
  diagnosticReports: () => get<DiagnosticReport[]>('/admin/diagnostics'),
  downloadDiagnostic: (userId: string, reportId: string) =>
    getBlob(`/admin/diagnostics/${encodeURIComponent(userId)}/${encodeURIComponent(reportId)}`),
  geosite: () => get<GeositeInfo | null>('/admin/geosite'),
  saveGeosite: (data: FormData) => post<GeositeInfo>('/admin/geosite', data),
  login: (username: string, password: string) =>
    post<Session>('/auth/login', { username, password }),
  logout: () => post('/auth/logout'),
  changePassword: (current_password: string, new_password: string) =>
    post('/auth/password', { current_password, new_password }),
  users: () => get<User[]>('/admin/users'),
  nodes: () => get<Node[]>('/admin/nodes'),
  enrollNode: (data: CreateNode) => post<NodeEnrollment>('/admin/nodes', data),
  cancelEnrollment: (id: string) => post(`/admin/nodes/${encodeURIComponent(id)}/cancel`),
  devices: () => get<Device[]>('/admin/devices'),
  pool: () => get<Pool>('/admin/settings/address-pool'),
  createUser: (data: CreateUser) => post<User>('/admin/users', data),
  deleteUser: (username: string): Promise<void> => remove<void>(userPath(username)),
  userStatus: (username: string, enabled: boolean) =>
    post(`${userPath(username)}/status`, { enabled }),
  password: (username: string, password: string) =>
    post(`${userPath(username)}/password`, { password }),
  capabilities: (username: string, data: Capabilities) =>
    post(`${userPath(username)}/capabilities`, data),
  addressLease: (username: string, data: AddressLease) =>
    post<User>(`${userPath(username)}/address-lease`, data),
  node: (id: string, data: NodeSettings) =>
    post<Node>(`/admin/nodes/${encodeURIComponent(id)}`, data),
  nodeUpdate: (id: string, version: string | null = null) =>
    post<Node>(`/admin/nodes/${encodeURIComponent(id)}/update`, { version }),
  nodeBirdConfig: (id: string) =>
    get<NodeBirdConfig>(`/admin/nodes/${encodeURIComponent(id)}/bird-config`),
  nodeRelease: () => get<NodeRelease | null>('/admin/node-release'),
  clientReleases: (channel: 'stable' | 'beta') =>
    get<ClientRelease[]>(`/admin/client-releases/${channel}`),
  createClientRelease: (channel: string, data: FormData) =>
    post<ClientRelease>(`/admin/client-releases/${channel}`, data),
  editClientRelease: (channel: string, version: string, data: FormData) =>
    post<ClientRelease>(`/admin/client-releases/${channel}/${encodeURIComponent(version)}`, data),
  deleteClientRelease: (channel: string, version: string) =>
    remove(`/admin/client-releases/${channel}/${encodeURIComponent(version)}`),
  revoke: (id: string) => post(`/admin/devices/${encodeURIComponent(id)}/revoke`),
  savePool: (ipv4_pool: string, ipv6_pool: string, internal_cidrs: string[]) =>
    post<Pool>('/admin/settings/address-pool', { ipv4_pool, ipv6_pool, internal_cidrs }),
  portForwardEntries: () => get<PortForwardEntry[]>('/admin/port-forward/entries'),
  savePortForwardEntry: (data: unknown) =>
    post<PortForwardEntry>('/admin/port-forward/entries', data),
  editPortForwardEntry: (id: string, data: unknown) =>
    post<PortForwardEntry>(`/admin/port-forward/entries/${encodeURIComponent(id)}`, data),
  deletePortForwardEntry: (id: string) =>
    remove(`/admin/port-forward/entries/${encodeURIComponent(id)}`),
  portForwardAllocations: () => get<PortForward[]>('/admin/port-forward/allocations'),
  revokePortForward: (id: string) =>
    post(`/admin/port-forward/allocations/${encodeURIComponent(id)}/revoke`),
  portForwardPolicy: (username: string) =>
    get<PortForwardPolicy>(`${userPath(username)}/port-forward-policy`),
  savePortForwardPolicy: (username: string, data: PortForwardPolicy) =>
    post<PortForwardPolicy>(`${userPath(username)}/port-forward-policy`, data),
};
