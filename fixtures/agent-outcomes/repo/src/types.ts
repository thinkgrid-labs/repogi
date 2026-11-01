export interface Session {
  userId: string;
  expiresAt: number;
}

export type SessionStore = Map<string, Session>;
