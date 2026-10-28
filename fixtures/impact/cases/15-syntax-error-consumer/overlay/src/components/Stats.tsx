import { MetricCard } from '@/components/MetricCard';
import { double } from '../lib/math';

export function Stats({ n }: { n: number }) {
  return (
    <section>
      <MetricCard value={double(n)} />
  );
}
