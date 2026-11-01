import { getUserSession } from '../lib/session.js';
import { checkout } from '../checkout/checkout.js';

export function placeOrder(store, token, cart, options) {
  const session = getUserSession(store, token);
  if (!session) {
    return { status: 401 };
  }
  return { status: 200, userId: session.userId, receipt: checkout(cart, options) };
}
