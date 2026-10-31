import { AuthenticationService } from '../auth/service';
import { openDatabase } from '../storage/sqliteStore';
import { SessionRecord } from '../storage/sqliteStore';

const service = new AuthenticationService(openDatabase(':memory:'));

export function handleLogin(user: string, password: string): string | null {
  return service.login(user, password, Date.now());
}

export function handleRefresh(session: SessionRecord): string {
  return service.refresh(session, Date.now());
}
