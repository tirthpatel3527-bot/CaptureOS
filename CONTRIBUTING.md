# Contributing to CaptureOS

## Development rules

Read [AGENTS.md](AGENTS.md) first. Contributions must remain zero-cost, local-first, and safe for original media. Keep domain behavior in Rust crates and do not put raw SQL in the UI.

## Checks

```sh
npm install
npm run format:check
npm run lint
npm run typecheck
npm run test
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Please include tests for changed behavior, use deterministic fixtures, and explain architecture-level changes in an ADR.
