export class Shape {
  area() { return 0; }
  name() { return 'shape'; }
}
export class Circle extends Shape {
  area() { return 3; }
}
