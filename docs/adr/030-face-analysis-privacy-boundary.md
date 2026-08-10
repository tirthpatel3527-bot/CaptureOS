# ADR 030: Face-analysis privacy boundary

## Decision

Face support is optional, local detection/landmark technical evidence only. M4 stores normalized face boxes, detection confidence, visibility/pose when supported, face-region sharpness, and conservative eye state. `OPEN`, `CLOSED`, `UNCERTAIN`, and `NOT_ANALYZABLE` remain distinct. macOS may use the user-installed Apple Vision capability; it is not bundled or redistributed as a CaptureOS model.

M4 does not create person names, identity embeddings, cross-project matching, people clusters, demographic/sensitive-attribute inference, or face-crop archives. Missing/unsafe provider output is unavailable/not-applicable rather than a guessed face count.

## Consequences

Face evidence remains local and narrowly useful for technical review. The host OS governs Apple Vision availability and terms; it does not satisfy a future redistributable-model admission gate. Any identity or People feature requires explicit later approval and a separate privacy/security architecture decision.
