Money is stored as integer cents because floating-point dollar arithmetic drifted by a cent across line items and refunds did not reverse charges exactly; the decision record makes every internal amount an integer number of cents and converts only at the edges. `toCents` converts input and rejects more than two decimals, and `formatPrice` converts for display.

CITATIONS: docs/adr/ADR-001-integer-cents.md:5, docs/adr/ADR-001-integer-cents.md:13, src/money/cents.js:3, src/money/format.js:4
