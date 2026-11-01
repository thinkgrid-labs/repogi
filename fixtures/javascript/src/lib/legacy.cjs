const { add } = require('./math.js');

function total(items) {
  return items.reduce(add, 0);
}

module.exports = { total };
