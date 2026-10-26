import { get, post } from './client';
import type {
  Invitation,
  InvitationPage,
  InvitationQuery,
  InvitationRegistration,
  InvitationPolicy,
  InvitationPolicyView,
  InvitationGrant,
  CreatedInvitation,
} from '../../../shared/invitations';
function query(value: InvitationQuery) {
  return new URLSearchParams(
    Object.entries(value)
      .filter(([, v]) => v != null && v !== '')
      .map(([k, v]) => [k, String(v)]),
  ).toString();
}
export const invitationApi = {
  list: (q: InvitationQuery) => get<InvitationPage<Invitation>>(`/admin/invitations?${query(q)}`),
  registrations: (q: InvitationQuery) =>
    get<InvitationPage<InvitationRegistration>>(`/admin/invitation-registrations?${query(q)}`),
  create: (data: {
    kind: 'single' | 'unlimited';
    note: string;
    expires_at: string | null;
    grant: InvitationGrant;
  }) => post<CreatedInvitation>('/admin/invitations', data),
  edit: (id: string, data: { note: string; expires_at: string | null; grant?: InvitationGrant }) =>
    post<Invitation>(`/admin/invitations/${encodeURIComponent(id)}`, data),
  revoke: (id: string) => post(`/admin/invitations/${encodeURIComponent(id)}/revoke`),
  policy: (username: string) =>
    get<InvitationPolicyView>(`/admin/users/${encodeURIComponent(username)}/invitation-policy`),
  savePolicy: (username: string, data: InvitationPolicy) =>
    post<InvitationPolicyView>(
      `/admin/users/${encodeURIComponent(username)}/invitation-policy`,
      data,
    ),
};
