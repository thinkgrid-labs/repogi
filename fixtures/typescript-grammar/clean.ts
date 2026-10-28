import { target as linked } from './module';
const message = "importOriginal<typeof import('./module')>()";
// importOriginal<typeof import('./module')>()
test('binding', () => linked());
