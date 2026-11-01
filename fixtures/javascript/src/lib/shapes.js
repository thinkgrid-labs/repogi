export class Shape {
  area() {
    return 0;
  }

  static unit() {
    return new Shape();
  }
}

export class Circle extends Shape {
  constructor(radius) {
    super();
    this.radius = radius;
  }
}
