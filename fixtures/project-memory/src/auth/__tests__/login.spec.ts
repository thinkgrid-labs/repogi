import { validateLogin } from '../login';

export function rejectsShortPassword(): boolean {
  return !validateLogin('alice', 'short');
}
