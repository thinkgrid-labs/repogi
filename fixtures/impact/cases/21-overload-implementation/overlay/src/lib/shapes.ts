export interface Options {
  size: number;
}

export interface Options {
  color: string;
}

export function parse(value: string): string;
export function parse(value: number): number;
export function parse(value: string | number): string | number {
  return typeof value === "string" ? value : value;
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
