// Conversion at the system edge. See docs/adr/ADR-001-integer-cents.md.

export function toCents(amount) {
  if (typeof amount !== 'number' || !Number.isFinite(amount)) {
    throw new TypeError('amount must be a finite number');
  }
  const scaled = amount * 100;
  const cents = Math.round(scaled);
  if (Math.abs(scaled - cents) > 1e-6) {
    throw new RangeError('amount has more than two decimals');
  }
  return cents;
}

export function fromCents(cents) {
  return cents / 100;
}
