const SYMBOLS = { USD: '$', EUR: '€', GBP: '£' };

// Render an integer amount of cents for display.
export function formatPrice(cents, currency = 'USD') {
  const symbol = SYMBOLS[currency] ?? `${currency} `;
  const whole = Math.trunc(cents / 100);
  const fraction = String(Math.abs(cents % 100)).padStart(2, '0');
  return `${symbol}${whole}.${fraction}`;
}
