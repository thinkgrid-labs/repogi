const service = {
  validate(session) { return Boolean(session); },
  refresh: function (session) { return session; },
  check: (x) => x > 0,
  get size() { return 1; },
  name: 'svc',
};
service.renew = function renew() { return this.refresh(1); };
service.validate();
service.refresh();
service.check(1);
service.size();
service.name();
service.renew();
service.missing();
module.exports = service;
