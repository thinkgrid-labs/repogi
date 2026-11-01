import { loadSession } from '../lib/session.js';
import type { SessionStore } from '../types';

export function Header({ store, token }: { store: SessionStore; token: string }) {
  const session = loadSession(store, token);
  return <header>{session ? `Signed in as ${session.userId}` : 'Guest'}</header>;
}
