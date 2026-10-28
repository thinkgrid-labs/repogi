import { target as Linked } from './module';
vi.mock('./module', async importOriginal => {
  const actual = await importOriginal<typeof import('./module')>();
  return { ...actual };
});
test('binding', () => <Linked />);
