// Deprecated: dollar-based formatter kept for the v1 export job. Do not use in
// new code; amounts here are dollars, not cents.
export function formatPrice(dollars) {
  return '$' + dollars.toFixed(2);
}
