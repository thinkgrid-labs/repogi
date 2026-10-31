import { validateLogin } from './login';
import { refreshToken, issueToken } from './token';
import { touchSession } from './session';
import { SessionRecord, SqliteSessionStore } from '../storage/sqliteStore';

export class AuthenticationService {
  constructor(private readonly store: SqliteSessionStore) {}

  login(user: string, password: string, now: number): string | null {
    if (!validateLogin(user, password)) {
      return null;
    }
    const session = this.store.create(user, now);
    return issueToken(session.userId, now);
  }

  refresh(session: SessionRecord, now: number): string {
    this.store.save(touchSession(session, now));
    return refreshToken(session, now);
  }
}
