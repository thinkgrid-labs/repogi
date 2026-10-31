import { refreshToken } from './token';

export function rejectsExpiredRefresh(): boolean {
  try {
    refreshToken({ userId: 'a', lastActive: 0 }, 1800);
    return false;
  } catch {
    return true;
  }
}
