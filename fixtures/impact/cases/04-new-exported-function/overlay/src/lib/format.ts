export function formatNumber(value: number): string {
  return value.toFixed(2);
}

export function formatDate(value: Date): string {
  return value.toISOString();
}

export function unusedHelper(): number {
  return 1;
}

export function formatPercent(value: number): string {
  return `${value}%`;
}
