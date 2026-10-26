export interface InvitationForwardGrant {
  enabled: boolean;
  max_active_forwards: number;
  allowed_entry_ids: string[];
  allowed_protocols: ('tcp' | 'udp')[];
  allowed_target_ports: number[] | null;
  allowed_families: string[];
}
export interface InvitationGrant {
  allow_ipv4_global: boolean;
  allow_ipv6_global: boolean;
  allow_ipv4_split: boolean;
  allow_ipv6_split: boolean;
  port_forward: InvitationForwardGrant;
}
export interface InvitationPolicy {
  enabled: boolean;
  max_unused: number;
  daily_limit: number;
  valid_days: number;
  grant: InvitationGrant;
}
export interface InvitationPolicyView {
  account_enabled: boolean;
  policy: InvitationPolicy;
  revision: number;
  unused: number;
  generated_today: number;
  remaining_unused: number;
  remaining_today: number;
  resets_at: string;
}
export interface Invitation {
  id: string;
  prefix: string;
  inviter_username: string;
  creator_username: string;
  source: 'user' | 'admin';
  kind: 'single' | 'unlimited';
  state: 'available' | 'used' | 'expired' | 'revoked' | 'disabled';
  note: string;
  created_at: string;
  expires_at: string | null;
  use_count: number;
  grant: InvitationGrant;
  policy_revision: number;
}
export interface InvitationRegistration {
  id: string;
  invitation_id: string;
  prefix: string;
  inviter_username: string;
  username: string;
  created_at: string;
  grant: InvitationGrant;
  policy_revision: number;
}
export interface InvitationPage<T> {
  items: T[];
  total: number;
  page: number;
  page_size: number;
}
export interface InvitationQuery {
  page?: number;
  page_size?: number;
  inviter?: string;
  username?: string;
  kind?: string;
  state?: string;
  since?: string;
  until?: string;
}
export interface CreatedInvitation {
  invitation: Invitation;
  code: string;
  registration_path: string;
}
export type InvitationRequest =
  | { operation: 'register'; code: string; username: string; password: string }
  | { operation: 'policy' | 'create' }
  | { operation: 'list' | 'registrations'; query: InvitationQuery }
  | { operation: 'revoke'; id: string };
export type InvitationResult =
  | { type: 'registered'; data: { username: string } }
  | { type: 'policy'; data: InvitationPolicyView }
  | { type: 'list'; data: InvitationPage<Invitation> }
  | { type: 'created'; data: CreatedInvitation & { registration_url?: string } }
  | { type: 'revoked' }
  | { type: 'registrations'; data: InvitationPage<InvitationRegistration> };
export const invitationStates: Record<Invitation['state'], string> = {
  available: '可使用', used: '已使用', expired: '已过期', revoked: '已撤销', disabled: '已停用',
};
export function emptyInvitationGrant(): InvitationGrant {
  return { allow_ipv4_global: false, allow_ipv6_global: false, allow_ipv4_split: false, allow_ipv6_split: false,
    port_forward: { enabled: false, max_active_forwards: 0, allowed_entry_ids: [], allowed_protocols: ['tcp'], allowed_families: ['ipv4','ipv6'], allowed_target_ports: null } };
}
export function emptyInvitationPolicy(): InvitationPolicy {
  return { enabled: false, max_unused: 10, daily_limit: 20, valid_days: 7, grant: emptyInvitationGrant() };
}
/** Copies plain API data as well as Vue proxies without retaining live drafts. */
export function cloneInvitation<T>(value: T): T { return JSON.parse(JSON.stringify(value)) as T; }
export function grantSummary(grant: InvitationGrant): string {
  const modes = [grant.allow_ipv4_split && 'IPv4 分流', grant.allow_ipv4_global && 'IPv4 全局', grant.allow_ipv6_split && 'IPv6 分流', grant.allow_ipv6_global && 'IPv6 全局'].filter(Boolean);
  return ['内网接入', ...modes, grant.port_forward.enabled ? `端口转发 ${grant.port_forward.max_active_forwards} 个` : '无端口转发'].join(' · ');
}
