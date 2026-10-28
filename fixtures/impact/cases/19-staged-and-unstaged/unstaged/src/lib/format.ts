export function formatNumber(value: number): string {
  return value.toFixed(1);
}

export function formatDate(value: Date): string {
  return value.toISOString();
}

export function unusedHelper(): number {
  return 3;
}
