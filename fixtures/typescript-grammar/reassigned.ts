import { target as linked } from './module';
let invoke = linked;
vi.mock('./module', async importOriginal => {
  const actual = await importOriginal<typeof import('./module')>();
  return { ...actual };
});
invoke = () => 0;
test('reassigned', () => invoke());
