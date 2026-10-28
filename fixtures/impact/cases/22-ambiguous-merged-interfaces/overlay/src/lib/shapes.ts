export interface Options {
  width: number;
}

export interface Options {
  hue: string;
}

export function parse(value: string): string;
export function parse(value: number): number;
export function parse(value: string | number): string | number {
  return value;
}

export class Circle {
  area(): number {
    return 1;
  }
}

export class Square {
  area(): number {
    return 2;
  }
}
