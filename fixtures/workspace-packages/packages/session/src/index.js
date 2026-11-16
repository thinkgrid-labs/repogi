export class Session {
  validate() { return true; }
}
export function createSession() { return new Session(); }
