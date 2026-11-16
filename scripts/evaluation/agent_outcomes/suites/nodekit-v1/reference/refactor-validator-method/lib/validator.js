'use strict';

const validator = {
  isNonEmpty(value) {
    return typeof value === 'string' && value.length > 0;
  },
  checkAll(values) {
    return values.every((value) => validator.isNonEmpty(value));
  },
};

module.exports = validator;
