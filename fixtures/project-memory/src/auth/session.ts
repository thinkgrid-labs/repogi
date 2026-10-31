import { SessionRecord } from '../storage/sqliteStore';

export const SESSION_TIMEOUT_SECONDS = 1800;

export function isSessionExpired(session: SessionRecord, now: number): boolean {
  return now - session.lastActive >= SESSION_TIMEOUT_SECONDS;
}

export function touchSession(session: SessionRecord, now: number): SessionRecord {
  return { ...session, lastActive: now };
}
