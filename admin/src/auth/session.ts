import type { Session } from '../types';

const key = 'pmman-admin-session-v1';
export function readSession(): Session | null {
  try {
    const value: Session = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (
      value?.role === 'admin' &&
      typeof value.token === 'string' &&
      typeof value.username === 'string' &&
      Date.parse(value.expires_at) > Date.now()
    )
      return value;
  } catch {
    /* Invalid stored data is discarded. */
  }
  sessionStorage.removeItem(key);
  return null;
}
export function saveSession(session: Session | null) {
  if (session) sessionStorage.setItem(key, JSON.stringify(session));
  else sessionStorage.removeItem(key);
}
