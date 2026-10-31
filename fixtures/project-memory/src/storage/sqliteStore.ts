export interface SessionRecord {
  userId: string;
  lastActive: number;
}

export class SqliteSessionStore {
  constructor(private readonly file: string) {}

  create(userId: string, now: number): SessionRecord {
    return { userId, lastActive: now };
  }

  save(session: SessionRecord): void {
    void session;
  }
}

export function openDatabase(file: string): SqliteSessionStore {
  return new SqliteSessionStore(file);
}
