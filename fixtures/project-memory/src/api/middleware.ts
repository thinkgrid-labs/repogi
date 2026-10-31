import { isSessionExpired } from '../auth/session';
import { SessionRecord } from '../storage/sqliteStore';

export function requireSession(session: SessionRecord | undefined, now: number): boolean {
  return session !== undefined && !isSessionExpired(session, now);
}
