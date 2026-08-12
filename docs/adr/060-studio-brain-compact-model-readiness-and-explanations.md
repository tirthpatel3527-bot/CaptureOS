# ADR 060: Studio Brain compact model, readiness, and explanation policy

## Decision

Studio Brain I uses a deterministic regularized three-class linear softmax classifier with a
static 31-value feature schema, train-partition-only normalization, temperature calibration, and
confidence/margin abstention. A separate small pairwise linear ranker is trained only from
explicit Similar Set human representative comparisons. Both are pure Rust, local, fast,
versioned, and serializable as static JSON; no LLM, Python service, model download, or raw
embedding classifier is introduced.

Readiness is multi-factor: eligible decision count, class support/balance, project diversity,
Similar Set evidence, feature coverage, leakage-aware validation size, macro F1, and Brier score.
A small or single-project history may remain `NOT_READY`/`LEARNING`; **Not enough evidence** is a
valid result. v1 feature inputs are limited to real compact technical/anonymous face/eye,
Similar Set, Moment, generic-recommendation, and semantic-availability evidence. Missing values
have explicit availability indicators rather than invented zero values.

Normal UI explanations map only actual available feature contributions to restrained evidence
phrases. They must not reveal internal IDs or private data, describe raw coefficients, claim
object/identity/emotion knowledge, or anthropomorphize the photographer.

## Consequences

The initial model is small enough to retrain explicitly from durable local history and explain
without false precision. It can abstain rather than forcing Keep/Reject. More complex preference,
editing, crop, style, client, or high-dimensional semantic modeling remains out of scope.
