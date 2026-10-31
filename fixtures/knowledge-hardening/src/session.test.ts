import { expireSession } from "./session";
export function sessionExpirationTest(): boolean {
  return expireSession(0, 1_800_001);
}
