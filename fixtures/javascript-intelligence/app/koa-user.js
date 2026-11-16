const Application = require('../lib/application');
const app = new Application();
class Sub extends Application {
  start() { return this.listen(); }
}
module.exports = Sub;
