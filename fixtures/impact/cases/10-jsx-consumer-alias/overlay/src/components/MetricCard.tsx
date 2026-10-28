import { formatNumber } from '@/lib';

export function MetricCard({ value, label }: { value: number; label?: string }) {
  return <div className="metric">{formatNumber(value)}</div>;
}
