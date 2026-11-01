// Tax is rounded once per order, half away from zero (ADR-001).
export function applyTax(cents, rate) {
  const tax = cents * rate;
  return Math.sign(tax) * Math.round(Math.abs(tax));
}
