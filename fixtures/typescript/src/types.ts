export interface User {
  name: string;
  active: boolean;
  describe(): string;
}

export type UserId = string;
