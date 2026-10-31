export function expireSession(lastActivity: number, now: number): boolean {
  return now - lastActivity > 30 * 60 * 1000;
}

export function revokeSession(sessionId: string): string {
  return `revoked:${sessionId}`;
}
