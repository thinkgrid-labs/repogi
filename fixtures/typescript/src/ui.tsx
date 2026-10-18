import type { User } from "./types";

export function UserBadge({ user }: { user: User }) {
  return <span>{user.name}</span>;
}
