const SYMBOLS = { USD: '$', EUR: '€', GBP: '£' };

// Render an integer amount of cents for display.
export function formatPrice(cents, currency = 'USD') {
  const symbol = SYMBOLS[currency] ?? `${currency} `;
  const sign = cents < 0 ? '-' : '';
  const absolute = Math.abs(cents);
  const whole = Math.trunc(absolute / 100);
  const fraction = String(absolute % 100).padStart(2, '0');
  return `${sign}${symbol}${whole}.${fraction}`;
}
