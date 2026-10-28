import { MetricCard } from '@/components/MetricCard';
import { double } from '../lib/arith';

export function Stats({ n }: { n: number }) {
  return (
    <section>
      <MetricCard value={double(n)} />
    </section>
  );
}
