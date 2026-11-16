const service = require('../lib/service');
function run(service) { return service.validate(); }
run();
module.exports = run;
