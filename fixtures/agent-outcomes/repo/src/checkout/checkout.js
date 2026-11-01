import { subtotal } from '../cart/cart.js';
import { applyTax } from '../cart/tax.js';
import { shippingFor } from './shipping.js';

export function checkout(cart, { taxRate = 0 } = {}) {
  const itemsCents = subtotal(cart);
  const taxCents = applyTax(itemsCents, taxRate);
  const shippingCents = shippingFor(itemsCents);
  return {
    itemsCents,
    taxCents,
    shippingCents,
    totalCents: itemsCents + taxCents + shippingCents,
  };
}
