export const FLAT_RATE_CENTS = 499;
// REQ-12: orders of at least $50.00 (after discounts, before tax) ship free.
export const FREE_SHIPPING_THRESHOLD_CENTS = 5000;

export function shippingFor(subtotalCents) {
  return subtotalCents >= FREE_SHIPPING_THRESHOLD_CENTS ? 0 : FLAT_RATE_CENTS;
}
