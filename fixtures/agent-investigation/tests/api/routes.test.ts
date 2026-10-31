import { handleLogin } from '../../src/api/routes';

export function loginReturnsToken(): boolean {
  return handleLogin('alice', 'correct horse battery') !== null;
}
