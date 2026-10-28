import { MetricCard } from './components/MetricCard';

const load = async (loader: any) => {
  const module = await loader<typeof import('./components/MetricCard')>();
  return module;
};

export const view = <MetricCard value={load.length} />;
