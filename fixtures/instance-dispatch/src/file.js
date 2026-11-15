import { Store } from './base.js';

export class FileStore extends Store {
  write() { return super.write() + 1; }
  load() { return super.load(); }
  flush() { return this.read() + this.missing(); }
  static open() { return super.open(); }
}
