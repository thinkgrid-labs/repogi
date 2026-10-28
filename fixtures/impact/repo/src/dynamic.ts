export function pick(formatter: { formatDate: (value: Date) => string }): string {
  return formatter.formatDate(new Date(0));
}
