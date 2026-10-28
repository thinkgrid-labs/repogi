export function formatNumber(value: number, _digits?: number): string {
  return value.toFixed(2);
}

export function formatDate(value: Date): string {
  return value.toISOString();
}

export function unusedHelper(): number {
  return 1;
}
