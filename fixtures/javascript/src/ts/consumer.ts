import { add } from '../lib/math';
import type { Point } from '../lib/types.mjs';
import { count } from '../lib/count.cjs';

export const origin: Point = { x: add(0, 0), y: count([]) };
