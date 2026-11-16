const Session = require('./session');
const S = Session;
class Local { run() { return 1; } }
const a = new Session();
a.validate();
const b = a;
b.validate();
const c = new S();
c.validate();
const d = new Local();
d.run();
d.missing();
c.create();
a.user;
