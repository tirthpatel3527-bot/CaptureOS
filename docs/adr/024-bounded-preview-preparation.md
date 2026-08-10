# ADR 024: Bounded preview preparation

## Decision

Every logical-media preparation attempt has a durable terminal outcome: `READY`,
`UNSUPPORTED`, `OFFLINE`, `CORRUPT`, `FAILED`, `TIMEOUT`, or `CANCELLED`. macOS platform
tools run only through structured command arguments with stdout/stderr drained concurrently and
with a local deadline. On expiry CaptureOS terminates the direct child process, cleans the
provider's managed temporary output directory, records `TIMEOUT`, and continues the queue.

Obvious malformed containers are detected read-only before a platform decoder is invoked. Audio
is terminal after metadata preparation and deliberately has no bitmap-thumbnail work. Preparation
progress counts terminal logical assets, not only successfully generated images. A startup repair
marks stale running visual-preparation jobs as interrupted; ready artifacts are never rewritten by
the retry-failed action.

## Consequences

One bad card, fixture, unavailable path, or adapter failure cannot leave the local preview queue
running indefinitely or prevent later real media from receiving previews. Platform adapters remain
replaceable and source files remain read-only. The cache continues to contain generated artifacts
only, keyed to a specific `FileInstance` and source fingerprint.
