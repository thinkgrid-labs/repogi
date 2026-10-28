export function formatNumber(value: number): string {
  return value.toFixed(2);
}

function formatDate(value: Date): string {
  return value.toISOString();
}

export function unusedHelper(): number {
  return 1;
}
