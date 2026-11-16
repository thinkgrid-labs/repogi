const Koa = require('./koa-like');
const app = new Koa();
app.use(() => 1);
app.callback();
