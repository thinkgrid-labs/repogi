import { add } from './math';

export const sum = (values: number[]): number => values.reduce((total, value) => add(total, value), 0);
