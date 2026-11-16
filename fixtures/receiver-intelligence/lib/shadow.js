const Symbol = { iterator: 'twist' };
const util = makeUtil();
class Shadowed {
  constructor() { this[Symbol.iterator] = 1; }
  twist() { return 1; }
}
class FakeUtil {
  constructor() { this[util.inspect.custom] = 1; }
  bend() { return 1; }
}
class Registered {
  constructor(name) { this[Symbol.for(name)] = 1; }
  fold() { return 1; }
}
function makeUtil() { return {}; }
const s1 = new Shadowed(); s1.twist();
const s2 = new FakeUtil(); s2.bend();
const s3 = new Registered('x'); s3.fold();
