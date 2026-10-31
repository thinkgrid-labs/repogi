# Architecture

## Session storage
Session records are persisted in an embedded SQLite database file through `SqliteSessionStore`.
No external database server is required.

## Session expiry
Expiry is evaluated on every request by `requireSession`, using the inactivity rule from ADR-0003.

## Token refresh
Tokens are refreshed only for sessions that are still active.
