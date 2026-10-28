export function original(value: string): string { return value.trim(); }
export default function fallback(): string { return original('default'); }
export class Base {
  static create(): string { return original('base'); }
  instance(): string { return original('instance'); }
}
export const arrow = (value: string): string => original(value);
