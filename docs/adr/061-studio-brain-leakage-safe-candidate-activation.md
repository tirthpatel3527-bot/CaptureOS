# ADR 061: Studio Brain leakage-safe evaluation and atomic candidate activation

## Decision

Each explicit training run snapshots eligible source IDs, compact feature snapshots, labels,
feature schema, algorithm/parameters, deterministic grouped split, and a source hash. Evaluation
holds out whole projects when possible; with one project it holds Similar Set, Moment, or
capture-day buckets together. The grouped candidate holdout may select calibration and guard
activation, but is not reported as independent real-world efficacy.

A candidate is checked before storage, while loading, and before activation using its static JSON
schema and checksum. Candidate recommendations may be precomputed while the old model remains
active. Before the final transaction, source hash is checked again and a retained active model is
scored on the exact same current grouped holdout. A trigger-backed source revision is read before
and after snapshot construction, then verified inside that final transaction. Every eligible human
authority write creates an action-scoped durable source guard in the same SQLite transaction as its
append-only history; snapshots and activation refuse while any guard remains. Only a matching
successful live capture may resolve its own guard, while explicit historical reconciliation resolves
durable leftovers under a write reservation. A ready candidate that cannot be safely compared, has a
stale source revision or pending guard, or materially loses macro F1/Brier non-inferiority tolerances
remains `not_activated`; the prior model stays active.

Activation is one immediate SQLite transaction: retire the prior active model to `previous`,
activate the checked candidate, retire older advisory rows, complete the run, update profile
state, and complete the associated background job. One partial unique index permits one active
model/profile and another permits one in-flight run/profile. Reset marks only derived
models/recommendations reset or stale.

## Consequences

Interrupted, malformed, stale-snapshot, failed, or regressed candidates cannot create a half
active profile or destroy a prior valid model. Corrupt active artifacts are invalidated and the UI
falls back to generic M0–M7 evidence. The retained `previous` state is a rollback foundation;
a direct revert control is future work and does not alter human decisions.
