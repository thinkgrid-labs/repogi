import { isSessionExpired } from './session';
import { SessionRecord } from '../storage/sqliteStore';

export class TokenRefreshError extends Error {}

export function refreshToken(session: SessionRecord, now: number): string {
  if (isSessionExpired(session, now)) {
    throw new TokenRefreshError('session expired');
  }
  return issueToken(session.userId, now);
}

export function issueToken(userId: string, now: number): string {
  return `${userId}:${now}`;
}
