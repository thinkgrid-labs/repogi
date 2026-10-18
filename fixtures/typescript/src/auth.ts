import type { User } from "./types";
import { audit } from "audit-package";

// function CommentOnlyFake() {} is documentation, not a declaration.
export function authenticate(user: User): boolean {
  audit(user.name);
  return user.active;
}

export class AuthService {
  check(user: User): boolean {
    return authenticate(user);
  }
  ["computed"](): boolean { return true; }
}

export const authVersion = 1;
