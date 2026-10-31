import { isSessionExpired } from './session';

export function expiresAfterTimeout(): boolean {
  return isSessionExpired({ userId: 'a', lastActive: 0 }, 1800);
}
