// TypeScript output names: core.js does not exist, so it is core.ts; both.js exists.
import { coreValue } from './core.js';
import { pick } from './both.js';

export const used = coreValue() + pick();
