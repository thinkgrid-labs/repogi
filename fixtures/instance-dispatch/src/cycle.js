export class Ping extends Pong {
  a() { return this.b(); }
}
export class Pong extends Ping {
  c() { return super.a(); }
}
