function a() { return 1; }
if (process.env.DEBUG) {
  exports.a = a;
}
