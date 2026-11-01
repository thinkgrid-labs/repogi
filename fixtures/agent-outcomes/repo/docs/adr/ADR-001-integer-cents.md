# ADR-001: Store money as integer cents

Status: Accepted

## Context

Early prototypes stored prices as JavaScript numbers in dollars. Totals drifted
by a cent when several line items were added (`0.1 + 0.2 !== 0.3`), and refunds
did not always reverse the original charge exactly.

## Decision

Every amount inside the library is an integer number of cents. Values are
converted at the edges only: `toCents` when input enters the system and
`formatPrice` when an amount is shown to a person. Tax is rounded once per
order, half away from zero.

## Consequences

Arithmetic on amounts is exact. Callers must not pass dollar values to cart or
checkout functions; `toCents` rejects values with more than two decimals.
