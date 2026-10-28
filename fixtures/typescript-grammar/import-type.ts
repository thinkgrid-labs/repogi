import { target as linked } from './module';
vi.mock('./module', async importOriginal => {
  const actual = await importOriginal<typeof import('./module')>();
  return { ...actual };
});
test('binding', () => linked());
