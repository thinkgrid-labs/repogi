import { formatNumber } from '@/lib';

export function MetricCard({ value }: { value: number }) {
  return <span className="metric">{formatNumber(value)}</span>;
}
