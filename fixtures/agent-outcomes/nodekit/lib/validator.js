'use strict';

const validator = {
  check(value) {
    return typeof value === 'string' && value.length > 0;
  },
  checkAll(values) {
    return values.every((value) => validator.check(value));
  },
};

module.exports = validator;
