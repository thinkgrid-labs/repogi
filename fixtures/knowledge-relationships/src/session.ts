export function expireSession(lastActive: number, now: number): boolean {
  return now - lastActive >= 1800;
}
export function refreshSession(now: number): number { return now; }
