import { add } from './arith';

export const sum = (values: number[]): number => values.reduce((total, value) => add(total, value), 0);
