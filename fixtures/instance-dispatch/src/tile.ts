import { Square } from './shapes';
export class Tile extends Square {
  area(): number { return super.area() * 2; }
  static build(): Tile { return new Tile(); }
}
export function factory(): number {
  class Local { a(): number { return this.b(); } b(): number { return 1; } }
  return new Local().a();
}
