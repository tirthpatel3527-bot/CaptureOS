# ADR 033: Uncertainty is a first-class result

## Decision

M4 records confidence, error, and explicit lifecycle states for every analyzer. It distinguishes `FAILED`, `UNSUPPORTED`, `CORRUPT`, `NEEDS_ORIGINAL`, and `NOT_APPLICABLE`; eye evidence separately distinguishes `UNCERTAIN` and `NOT_ANALYZABLE`. The UI and recommendations must derive from usable current evidence, not from a missing provider or a prior failure.

Technical labels carry visible component reasons and confidence. A human decision is append-only and can override the displayed suggestion without replacing the underlying evidence.

## Consequences

CaptureOS can safely say that it does not know. It avoids fake face counts, eye states, quality scores, and artistic certainty, and it gives future evaluation work a way to measure coverage and accuracy at confidence thresholds instead of forcing every photo into a confident category.
