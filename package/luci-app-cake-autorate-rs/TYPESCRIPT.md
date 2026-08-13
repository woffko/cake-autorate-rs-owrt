# JavaScript/TypeScript development support

The LuCI application remains JavaScript executed by OpenWrt. TypeScript is a
development-only language service: it provides semantic navigation, hover
types, references, and a reproducible no-emit compatibility check without
changing the package output.

Install the pinned local dependencies and run the baseline check from this
directory:

```bash
npm ci
npm run check:types
```

`tsconfig.json` covers the browser/LuCI sources and `tests/tsconfig.json`
covers the Node test suite. `types/luci-env.d.ts` supplies the LuCI globals and
the local `cakeUi` contract that standard DOM/ES libraries do not know about.
Keep those declarations synchronized when the application starts using new
LuCI APIs.

The baseline deliberately uses `checkJs: false`. The existing LuCI codebase
contains framework idioms and top-level `return` statements that produce a
large legacy error set under whole-tree `checkJs`; enabling it globally would
hide new regressions in noise. Add `// @ts-check` to files incrementally after
their framework types are accurate, or tighten the configuration in a
dedicated cleanup change.

For Codex, the repository `.lsp-mcp.toml` enables one multi-root MCP session
for this package and the Rust crate. On a cold session the first semantic call
opens the document; retry it briefly while tsserver loads the configured
project. TypeScript diagnostics are published asynchronously, so use
`get_cached_diagnostics` rather than treating a failed pull-diagnostics call
as a type-check result.

LuCI's string directives such as `'require cake-autorate-rs.ui as cakeUi'` are
not ECMAScript imports. The ambient declarations therefore provide the stable
type contract, and go-to-definition for such an alias lands in
`types/luci-env.d.ts`, not in the runtime implementation. A tsserver plugin was
not added: the measured navigation/type goals work without one, while a plugin
would need to emulate LuCI's loader and maintain virtual source mappings. Add
one only if direct alias-to-implementation navigation becomes a required,
tested acceptance criterion.
