class Counter {
  count(): number { return 1; }
}
const counter: Counter = new Counter();
counter.count();
let other: Counter = new Counter();
other = makeCounter();
other.count();
function makeCounter(): Counter { return new Counter(); }
