# MomentBrainBench

**MomentBrainBench** is the deterministic, no-download benchmark foundation for Milestone 7 structural timeline mechanics. It contains no customer photo, AI Test image, original path, CaptureOS catalog, cached preview, model weight, tokenizer, downloaded asset, cloud API, or identity/event label.

The default suite generates local numeric timeline records at 1,000, 10,000, 50,000, and 100,000 scale and compares:

- **time-only** — adaptive local capture-cadence evidence;
- **semantic-only** — generated compatible-signature continuity; and
- **combined** — bounded time, signature, and supporting local metadata-transition evidence.

Run the full scale suite from the repository root in the release profile:

~~~sh
cargo run --release -p moment-brain-bench -- --suite baseline
~~~

Write a JSON report only to an explicit path:

~~~sh
cargo run --release -p moment-brain-bench -- --suite baseline --output /user/chosen/moment-brain-bench.json
~~~

Run one mechanics mode or a selected scale while investigating a regression:

~~~sh
cargo run --release -p moment-brain-bench -- --suite combined --records 1000,10000
cargo run --release -p moment-brain-bench -- --suite time-only --records 100000
cargo run --release -p moment-brain-bench -- --suite semantic-only --records 50000
~~~

The binary writes JSON to stdout and creates no persistent artifact unless **--output** is supplied. An explicit output uses create-new behavior and fails if that path already exists; it never silently overwrites a report. It does not consult app data, a semantic-model root, an environment variable, a network service, or a fixture download.

## Fixture and ground truth

The checked-in [fixtures manifest](fixtures/timeline.v1.json) describes the deterministic generator. Each record has only:

- a synthetic ordinal;
- an optional capture timestamp;
- a synthetic structural-segment ground-truth ID;
- a generated 768-dimensional numeric signature matching the current M6 embedding width;
- synthetic camera/lens/orientation categories; and
- an optional anonymous-face-count category.

The fixture intentionally includes repeated signatures across a real structural boundary, modest time gaps at some true boundaries, within-segment pauses, and missing timestamps. This prevents any one evidence family from being presented as universal truth. Missing timestamps have no generated structural segment and must stay ungrouped.

The fixture’s “semantic” signatures are **not** SigLIP vectors, photos, model output, captions, object detections, concepts, or label ground truth. The word describes a numeric continuity signal only. Structural ground truth means the generator’s own segment partition; it does not mean that an event, relationship, emotion, identity, wedding stage, or required shot objectively occurred.

## Report fields

Each scale report includes:

| Field | Meaning |
| --- | --- |
| **generationElapsedUs**, **estimatedInputLowerBoundBytes** | Local generated-data setup evidence; byte figure is a deterministic lower-bound allocation estimate, not RSS. |
| **timestampedRecords**, **intentionallyUngroupedRecords** | Explicit missing-time handling. |
| **truthSegmentCount**, **truthBoundaryCount** | Generator-only structural reference. |
| **analysisElapsedUs**, **chronologicalComparisons** | One bounded chronological pass; not startup or full product latency. |
| **incremental.elapsedUs** | The product’s append-only prebounded-tail analysis check; its input window is explicitly reported rather than disguised as a whole-catalog update. |
| **momentPageElapsedNs**, **momentLookupElapsedNs** | In-memory page projection and prepared asset-to-Moment lookup mechanics, not SQLite/UI/preview timing. Lookup-map preparation is excluded from lookup timing; nanoseconds avoid rounding a tiny in-memory lookup to zero. |
| **boundaryMetrics** | Exact-boundary precision/recall/F1 against the generated structural partition. |
| **segmentPurity**, **overSegmentationSplits**, **underSegmentationMerges** | Generated partition fragmentation/merge behavior; these compare actual membership partitions, not merely net counts. |
| **ungroupedTruthRecall** | Confirmation that missing-time fixture records remained ungrouped. |

The report also identifies fixture/analyzer versions and includes caveats in the JSON. All timings are current-machine measurements; re-run them on the target machine rather than copying a historical number into a product claim.

## What it does not measure

MomentBrainBench is a mechanics/scale test. It does not measure:

- real image-text semantic relevance, embedding latency, model admission, model licensing, caption quality, object detection, identity recognition, relationship/event inference, emotion, or creative quality;
- capture-time correctness in a camera file, EXIF correction, multicamera synchronization, or factual missing coverage;
- SQLite migration, foreign-key integrity, app startup, background scheduling, preview/original I/O, UI rendering, paging from a database, lookup-map build time, or real memory peak RSS;
- project isolation, offline cache behavior, human label/merge/split/representative/coverage persistence, Similar Set separation, culling invariants, or source-media immutability.

Those behaviors require focused product tests and manual validation. This benchmark never grants authorization to begin Milestone 8 work.

## Future real-data evaluation boundary

A future real timeline evaluation may be added only after its source, license, checksum, derivation, privacy review, annotation policy, model/provider/revision, and ground-truth limitations are documented. It must never add customer shoots, the private AI Test project, wedding/event labels, identity/demographic data, a model binary, a model downloader, or network access to this repository.

See [Moment Brain architecture](../../docs/architecture/moment-brain.md), [ADR 056](../../docs/adr/056-moment-brain-benchmark-foundation.md), and the repository [research boundary](../README.md).
