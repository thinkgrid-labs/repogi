export class Store {
  load() { return this.read(); }
  read() { return 1; }
  save() { return this.write(); }
  write() { return 0; }
  static open() { return this.connect(); }
  static connect() { return new Store(); }
  #token() { return 't'; }
  auth() { return this.#token(); }
  get size() { return 1; }
  count() { return this.size(); }
  static helper() { return this.read(); }
}
