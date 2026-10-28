import { MetricCard } from '@/components/MetricCard';
import { twice } from '../lib/math';

export function Stats({ n }: { n: number }) {
  return (
    <section>
      <MetricCard value={twice(n)} />
    </section>
  );
}
