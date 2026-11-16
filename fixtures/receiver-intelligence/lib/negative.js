const Session = require('./session');
class Box {
  open() { return 1; }
  close() { return 2; }
  seal() { return 3; }
  peek() { return 4; }
  lift() { return 5; }
  hold() { return 6; }
}
let r = new Box();
r = makeBox();
r.open();
const f = makeBox();
f.open();
const e = new Box();
consume(e);
e.close();
const w = new Box();
w.seal = () => 0;
w.seal();
const q = new Box();
const q2 = q;
consume(q2);
q.peek();
const Pick = Math.random() > 0.5 ? Box : Session;
const p = new Pick();
p.lift();
const out = new Box();
module.exports.out = out;
out.hold();
function makeBox() { return new Box(); }
function consume() { return null; }
