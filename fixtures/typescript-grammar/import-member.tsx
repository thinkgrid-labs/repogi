import { target as Linked } from './module';
const actual = original<import('./module').T>();
const view = <Linked />;
