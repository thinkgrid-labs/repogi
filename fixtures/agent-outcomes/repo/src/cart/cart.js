export function createCart() {
  return { items: [] };
}

export function addItem(cart, item) {
  if (!Number.isInteger(item.priceCents) || item.priceCents < 0) {
    throw new RangeError('priceCents must be a non-negative integer');
  }
  const quantity = item.quantity ?? 1;
  return { ...cart, items: [...cart.items, { ...item, quantity }] };
}

export function subtotal(cart) {
  return cart.items.reduce((sum, item) => sum + item.priceCents * item.quantity, 0);
}
