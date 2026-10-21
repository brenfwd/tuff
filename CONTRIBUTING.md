# Contributing to tuff

Thanks for helping.

## Workflow

- Build with `cargo build`.
- Test with `cargo t` (an alias for `cargo test --workspace --lib --bins
  --tests --no-fail-fast`). Do not add `--all-targets` or `--bench`:
  the benchmarks are long load runs, not tests.
- Keep clippy clean: `cargo clippy --workspace --all-targets -- -D warnings`.
- Write commit messages in the
  [Conventional Commits](https://www.conventionalcommits.org/) format.
- Sign the CLA (below) before your first contribution is merged.

## Contributor License Agreement

Every contribution must be covered by the
[Contributor License Agreement](CLA.md) before it can be merged. Companies
whose employees contribute on their behalf sign the
[Corporate CLA](CCLA.md) as well.

The CLA leaves you the copyright in your contribution. It grants the project
owner, Brenden Forward, a broad copyright and patent license so the project
can be distributed under its current licenses (AGPL-3.0-only for the server,
MIT for the plugin SDK, ABI and examples) and under other terms later,
including commercial licenses.

How to agree:

1. On a pull request, the CLA bot posts a comment. Reply with the exact
   sentence it asks for: `I have read the CLA Document and I hereby sign the CLA`.
   You sign once; it covers all later contributions.
2. If you contribute without GitHub pull requests (for example by e-mailed
   patches), add this line to every commit message instead:

   ```
   CLA-Signed-off-by: Your Name <you@example.com>
   ```

   By adding it you state that you have read and agree to `CLA.md` for that
   commit.

## Licensing of your changes

Changes under `crates/tuff-plugin-sdk`, `crates/tuff-plugin-sdk-gen`,
`crates/tuff-plugin-wasm/wit` and `examples/` are MIT licensed; new source
files there start with `// SPDX-License-Identifier: MIT`. Everything else is
AGPL-3.0-only. Do not copy code into the MIT parts from the AGPL parts.
