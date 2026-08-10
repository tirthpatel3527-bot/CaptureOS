# ADR 002: Tauri + Rust desktop core

## Decision

Use Tauri 2 with a React/TypeScript UI and Rust core.

## Consequences

The UI has a narrow command bridge and Rust owns persistence and filesystem-adjacent work. The architecture is suited to macOS and Windows while avoiding a browser-hosted core.
