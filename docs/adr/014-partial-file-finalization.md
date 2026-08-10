# ADR 014: Mark incomplete outputs as partial

## Decision

CaptureOS writes an in-progress transfer as a same-directory `filename.captureos-partial`. It uses create-new semantics and retains an interrupted partial rather than presenting it as a completed file. After verification it requests an atomic no-replace finalization.

## Consequences

An incomplete file is visibly and durably distinguishable from verified output. Retrying a file may restart the partial from zero; CaptureOS does not claim byte-perfect mid-file resume in Milestone 2.
