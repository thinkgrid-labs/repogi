const { Core } = require('@acme/core');
const extra = require('@acme/core/lib/extra');
const session = require('@acme/session');
const core = new Core();
core.start();
extra.extra();
