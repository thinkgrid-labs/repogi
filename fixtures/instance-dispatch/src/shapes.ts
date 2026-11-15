export abstract class Shape<T> {
  abstract area(): number;
  describe(): string { return `${this.area()}`; }
  protected scale(n: number): number { return n * this.unit(); }
  private unit(): number { return 1; }
  static make(): string { return this.label(); }
  static label(): string { return 'shape'; }
}
export class Square extends Shape<number> {
  area(): number { return this.scale(2); }
}
export class Circle extends Shape<string> {
  area(): number { return super.scale(3); }
  describe(): string { return super.describe(); }
}
