import chosen, { renamed as local, Base, arrow, anchored } from './barrel';
import * as library from './leaf';
import { original as uncertain } from './ambiguous';
import type { original as onlyType } from './leaf';
export class Derived extends Base {}
export function entry(): string {
  // uncertain() local() phantom() — comments are not references.
  const message = 'uncertain() local() imaginary()';
  return local(message) + chosen() + library.original('ns') + Base.create() + arrow('a') + anchored('cycle');
}
export function shadow(local: () => string): string { return local(); }
export function ambiguous(): string { return uncertain(); }
export function unsupported(): string { return onlyType('no') + new Base().instance(); }
