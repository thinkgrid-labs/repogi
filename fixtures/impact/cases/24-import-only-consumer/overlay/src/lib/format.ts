export function formatNumber(value: number): string {
  return value.toFixed(2);
}

export function formatDate(value: Date): string {
  return value.toUTCString();
}

export function unusedHelper(): number {
  return 1;
}
