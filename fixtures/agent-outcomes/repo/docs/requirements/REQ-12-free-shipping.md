# REQ-12: Free shipping threshold

Owner: Commerce team

Orders whose subtotal, after discounts and before tax, is at least $50.00 ship
for free. Orders below the threshold pay the flat shipping rate.

Acceptance:

- A subtotal of exactly $50.00 ships free.
- A subtotal of $49.99 pays the flat rate.
- Tax is never charged on shipping.
