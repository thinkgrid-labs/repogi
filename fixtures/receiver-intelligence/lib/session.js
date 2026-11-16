class Session {
  validate() { return true; }
  static create() { return new Session(); }
  get user() { return null; }
}
module.exports = Session;
