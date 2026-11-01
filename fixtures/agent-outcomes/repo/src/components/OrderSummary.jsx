import { formatPrice } from '../money/format.js';

export function OrderSummary({ receipt }) {
  return (
    <dl>
      <dt>Items</dt>
      <dd>{formatPrice(receipt.itemsCents)}</dd>
      <dt>Total</dt>
      <dd>{formatPrice(receipt.totalCents)}</dd>
    </dl>
  );
}
