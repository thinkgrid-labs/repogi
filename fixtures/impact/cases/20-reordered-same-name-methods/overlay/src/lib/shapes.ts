export interface Options {
  size: number;
}

export interface Options {
  color: string;
}

export function parse(value: string): string;
export function parse(value: number): number;
export function parse(value: string | number): string | number {
  return value;
}

export class Square {
  area(): number {
    return 3;
  }
}

export class Circle {
  area(): number {
    return 1;
  }
}
