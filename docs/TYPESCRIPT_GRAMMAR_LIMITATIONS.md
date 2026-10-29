# TypeScript grammar limitations

The parser uses tree-sitter-typescript 0.23.2. Newer TypeScript syntax can produce errors or recovery nodes; accepted parsing alone does not prove correct binding. Type-only imports/exports never establish runtime calls. Recovered syntax and unsupported constructs retain diagnostics and uncertainty. Use current source and compiler tooling when native graph evidence is incomplete. Source regression fixtures live in [fixtures/typescript-grammar](../fixtures/typescript-grammar/).
