# Design notes

- The package entry point (`main` in `package.json`) re-exports the kit factory and its parts.
- Middleware functions take `(ctx, next)` and return what `next()` returns.
- Sessions are plain constructor functions with prototype methods so they can be created without `class` syntax.
