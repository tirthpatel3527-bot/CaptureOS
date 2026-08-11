# ADR 054: Factual Coverage Map and advisory camera-clock diagnostics

## Decision

Coverage Map I is a factual local review projection, not a shot-list enforcement or event inference engine. It may show observed asset totals, capture-time ranges/gaps, technical-evidence distributions, anonymous face-count availability, decision summaries, and the state of a photographer-created checklist.

A photographer owns checklist text and its confirmation state. Magic Search or Moment Brain may show locally ranked candidate media, but only a human may confirm, mark needs review, or mark not covered. A semantic label, representative, or similarity score can never automatically complete an item or claim a missing required shot.

M7 may calculate a local **Possible camera time offset** diagnostic from timestamp patterns and
existing related-frame evidence. It is advisory, carries evidence/provenance, and has no
automatic correction path. The current conservative rule uses at most one nearest
distinct-camera timestamp pair from each existing Similar Set. It emits only when at least three
independent Similar Sets agree on an absolute median difference of at least one minute and every
contributing observation is within fifteen seconds of that median. Insufficient or conflicting
evidence abstains.

## Consequences

The product can say “No capture activity from A to B” when the recorded timestamps support it, but cannot claim that a person, event, relationship, creative moment, or required image is absent. It never rewrites EXIF, source media, embedded metadata, or catalog timestamps to apply a diagnostic.

Coverage/checklist rows and diagnostics stay project-scoped/local. A diagnostic belongs to the
active completed Moment-analysis run; a bounded tail update abstains rather than displaying a
prior full-run observation as current without rechecking its complete evidence. They do not
create cloud traffic, modify human culling data, imply identity recognition, or turn a
photographer’s checklist into an automatic culling decision.
