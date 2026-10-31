declare const legacySessions: any;

export function purgeExpired(now: number): void {
  legacySessions.isSessionExpired(now);
}
