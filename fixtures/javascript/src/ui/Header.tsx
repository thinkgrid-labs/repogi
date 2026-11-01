import { shout } from '../lib/strings.mjs';

export function Header() {
  return <h1>{shout('hi')}</h1>;
}
