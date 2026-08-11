# ADR 057: Provenance-aware local capture-time refresh

## Context

Moment Brain requires a truthful chronological input, but a pre-M7.1 catalog could contain
otherwise-ready still photos whose embedded camera time had never been extracted. Recreating a
project to repair that gap would be unsafe: it risks disrupting independent local records such as
human decisions, ratings, notes, Similar Sets, Capture Intelligence evidence, semantic
embeddings, and Moment events.

Camera EXIF commonly stores a local wall-clock date with no timezone. File creation and modified
times are useful diagnostics/fallbacks, but they are not equivalent to a camera capture time and
may legitimately differ across duplicate physical copies.

## Decision

M7.1 adds a read-only local capture-time resolver at the media-metadata boundary plus an explicit
project-scoped **Refresh metadata** operation. The resolver records normalized local time,
timezone state, source, and provenance confidence. Its deterministic priority is:

1. Standard embedded `DateTimeOriginal`, including valid subsecond and offset fields when present.
2. Supported equivalent embedded original-capture fields.
3. Embedded `DateTimeDigitized` and then EXIF `DateTime`/CreateDate.
4. A locally reported content-creation timestamp from a supported platform adapter.
5. Explicit low-confidence filesystem modified/created fallbacks, only when no stronger value is available.

Direct parsing currently uses the maintained, BSD-2-Clause `kamadak-exif` crate for bounded
JPEG, HEIF, and PNG container reads. TIFF-based RAW is deliberately not sent through that direct
refresh path because a malformed or large TIFF can require an unbounded full-container read.
RAW and video remain eligible only for an existing supported platform metadata path; an
unavailable provider remains unavailable rather than guessed.

An offset-bearing timestamp retains its observed offset. An EXIF timestamp with no offset retains
an ISO local wall-clock string plus `capture_timezone = unknown`; CaptureOS never appends `Z` or
otherwise invents UTC. Moment Brain derives an internal ordering coordinate for a common
unknown-local clock but persists and presents the original local string.

Each refresh stores a per-`FileInstance` observation in `capture_time_observations` and resolves
one logical `media_metadata` value by provenance, local-copy consensus, and a stable ID
tie-breaker. Conflicting embedded values create a local Developer Details diagnostic. Different
filesystem timestamps alone do not create that conflict.

The refresh is an additive migration and a dedicated background worker. It never creates
previews, loads a semantic model, changes an original, rewrites EXIF, modifies a decision,
rebuilds Similar Sets, or clears M0–M6/M7 data. Moment analysis remains nonblocking and is not
started at project open; after a refresh, the photographer explicitly rebuilds the derived Moment
timeline to consume the corrected chronology.

## Consequences

Existing catalogs can gain more accurate chronology without being recreated, while the raw
per-copy evidence remains locally inspectable. The logical result is reproducible from mounted
copies and records exactly why a weaker fallback was used.

The tradeoff is intentionally conservative format coverage. M7.1 does not promise RAW or video
embedded-time parsing on every platform, does not infer a timezone from the computer, GPS, or
folder, and does not use filename sequence/order as a camera-time substitute. A timestamp is
provenance confidence, not a claim that a camera clock was set correctly.
