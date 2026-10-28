import { target as linked } from './module';
const actual = original<import('./module').T>();
linked();
