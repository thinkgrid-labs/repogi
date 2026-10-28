import { a } from './cycle-a';

export function b(n: number): number {
  return a(n);
}
