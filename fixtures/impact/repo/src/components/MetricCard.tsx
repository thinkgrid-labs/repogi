import { formatNumber } from '@/lib';

export function MetricCard({ value }: { value: number }) {
  return <div className="metric">{formatNumber(value)}</div>;
}
