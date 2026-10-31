import { expireSession } from './session';
export function testExpiration(): boolean { return expireSession(0, 1800); }
