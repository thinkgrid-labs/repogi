import { subtotal } from '../cart/cart.js';
import { applyTax } from '../cart/tax.js';
import { shippingFor } from './shipping.js';

export function checkout(cart, { taxRate = 0, discountCents = 0 } = {}) {
  if (!Number.isInteger(discountCents) || discountCents < 0) {
    throw new RangeError('discountCents must be a non-negative integer');
  }
  const itemsCents = subtotal(cart);
  const applied = Math.min(discountCents, itemsCents);
  const discounted = itemsCents - applied;
  const taxCents = applyTax(discounted, taxRate);
  const shippingCents = shippingFor(discounted);
  return {
    itemsCents,
    discountCents: applied,
    taxCents,
    shippingCents,
    totalCents: discounted + taxCents + shippingCents,
  };
}
