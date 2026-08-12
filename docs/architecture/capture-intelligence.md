# Capture Intelligence I architecture

Capture Intelligence I is a local, evidence-first layer for eligible still-photo assets. It is intentionally narrower than an "AI photographer": it measures reproducible technical signals, persists their provenance, groups related frames, and offers cautious group-relative suggestions. It does not declare artistic quality, identify people, upload customer media, or act on media without a photographer’s decision.

## Input and provider boundary

`capture-core` orchestrates a durable `capture-intelligence` background job. `capture-intelligence` owns replaceable provider contracts for:

- image decoding from a CaptureOS-managed analysis preview;
- visual fingerprinting and similarity evidence;
- technical-quality evidence;
- face/landmark detection; and
- recommendation generation.

`AnalysisInputResolver` resolves a reusable `ANALYSIS_IMAGE` contract before a job reaches an analyzer. It prefers a valid dedicated `analysis_preview` artifact (2048px target), then a sufficient 1600px visual `PREVIEW`. If neither cache artifact is usable, it deterministically tries each catalog-marked available `FileInstance`, reading a source only to generate a contained `analysis_preview` in the canonical cache root. The analyzer receives only the validated cache path. The adapter preserves orientation/aspect ratio and does not upscale a smaller source. The 768px `MEDIUM` and 256px `SMALL` browsing renditions are not sufficient. The current `ANALYSIS_SETTINGS_VERSION` is `m4.1.analysis-input-resolver.v1`; its changed input contract safely rebuilds current evidence once while retaining old evidence as history. On macOS the fixed `/usr/bin/sips` adapter makes the cache JPEG and the bounded decoder makes an ephemeral BMP. Neither operation modifies a source file or creates a sidecar.

The deterministic baseline is `captureos-deterministic-image` / `m4.det.v1`. It does not download or bundle a machine-learning model. Its compact descriptor is useful for local related-frame candidate generation; it is explicitly **not** a semantic embedding or a claim that CaptureOS understands image meaning.

## Persisted evidence and lifecycle

Each current or historical result is an `AnalysisArtifact` with media-asset ID, type, provider/provider version, optional model version, settings version, input fingerprint, generated timestamp, confidence, status, error, payload, and `Provenance`. Separate tables persist visual fingerprints/compact embeddings, similarity groups and membership, technical-quality evidence, face analyses, recommendations, and append-only human decisions. Migration 008 introduced the M4 records; additive migration 009 converts current-only evidence rows into revision-keyed history while preserving existing IDs and foreign-key-linked human decisions.

| State | Meaning |
| --- | --- |
| `READY` | The relevant local evidence was produced and is usable. An unavailable optional face provider does not invalidate valid deterministic technical evidence. |
| `UNSUPPORTED` | No approved local decoder/provider can make this kind of claim. |
| `CORRUPT` | The cache artifact or available analysis input is malformed. |
| `NEEDS_ORIGINAL` | No suitable cache exists and every known physical `FileInstance` is currently offline or unavailable. |
| `FAILED` | A provider attempted work and failed. The error/provenance remains available. |
| `NOT_APPLICABLE` | The provider intentionally cannot make a valid claim, such as a missing local face capability. |
| `STALE` | An older artifact has a different input, provider version, or settings version and is retained as history rather than shown as current evidence. |

The deterministic analysis cache key is a BLAKE3 digest of the input fingerprint, provider, provider version, and settings version. An analysis-preview identity includes the MediaAsset, FileInstance, source fingerprint, artifact type, generator version, and size class. Face evidence has a separate BLAKE3 face-artifact input identity derived from that preview input plus the face provider’s cache identity and face settings. Re-opening the UI does not recompute work. A source/cache-preview, deterministic-analyzer, face-provider, host-capability, or relevant settings change makes only the corresponding old evidence stale; it does not overwrite it. Corrupt, unsupported, unavailable, and provider-failure outcomes are per-asset terminal states, so one failure cannot prevent the rest of a project’s queue from completing.

## Duplicate, similarity, and grouping strategy

### Exact duplicates

An exact-duplicate group is created only when two assets have the same available **verified full BLAKE3 content hash**. A filename, file size, source path, EXIF date, or M1 bounded fast fingerprint is never sufficient proof. If a full verified hash is unavailable, CaptureOS simply does not make an exact-duplicate claim.

### Near duplicates and related frames

The deterministic descriptor combines:

- a 32×32 sampled, low-frequency 8×8 DCT pHash;
- a 9×8 dHash;
- a 4×4×4 RGB color signature; and
- a mean-centred 8×8 luminance descriptor quantized to 64 signed bytes.

pHash contributes 60% of the visual-similarity score, the color signature 15%, and cosine similarity of the compact luminance descriptor 25%. This is evidence for similar pixels/tones, not face or object identity. Small re-encodes and resizes can remain related; a crop, an unrelated photograph, or a uniform-color frame may not. Confidence and method are persisted with the group.

Candidate generation uses four 16-bit pHash LSH-style bucket keys, plus same-camera 12-second windows to recover certain small-crop cases. A bucket with more than 96 members is skipped instead of generating a catalog-wide all-pairs comparison. For a project with at most 256 eligible analysis inputs, CaptureOS additionally considers its bounded complete candidate set; this recovers valid local related-frame comparisons when all four exact pHash bands differ. It does not relax the visual, pHash, same-camera, or time gates. Larger projects retain only the bounded LSH/time candidate paths. The compact descriptors are SQLite BLOBs and bucket metadata; the provider and candidate/index strategy are separate so a future local vector index can be introduced without coupling it to a model.

Each rebuild writes local developer diagnostics with the descriptor provider/version, successful descriptor count and dimension, candidate strategy and counts, pHash/visual-similarity distributions, threshold values, camera/time acceptance counts, and resulting group IDs/member asset IDs. These diagnostics are not a photographer-facing claim and contain no pixels, source paths, face data, or cloud result.

The grouping pass creates deterministic project-local `SimilarityGroup` records and membership rows:

- `EXACT_DUPLICATE_SET` uses verified content-hash equality.
- `NEAR_DUPLICATE_SET` requires close pHash distance and strong visual evidence.
- `SIMILAR_SET` requires related visual evidence plus same-camera or near-time evidence.
- `BURST` requires three or more same-camera frames in a small time window with strong visual evidence; timestamps alone are insufficient.

Group IDs are derived from project, group kind, baseline version, and sorted member IDs. The representative is the member with the strongest available technical score, with a deterministic ID tie-breaker. Groups are rebuildable and do not create a quadratic `SIMILAR_TO` graph edge for every pair.

## Technical evidence, not creative judgment

The M4 baseline limits technical work to a 512px maximum-edge working raster. It measures Laplacian variance, gradient edge strength, local high-frequency content, directional edge imbalance, luminance mean/median, highlight clipping, shadow clipping, and per-channel clipping. Face-region sharpness is calculated separately for a valid detected face box, so a deliberately soft background does not automatically become a face-quality assertion.

`global_sharpness` is a bounded combination of `sqrt(laplacian variance) × 2.5`, `edge strength × 0.45`, and `local high-frequency content × 1.25`. The first technical score is deliberately documented and versioned:

```text
technical score = clamp(global sharpness × 0.72 + 28
                        − severe clipping penalty − blur penalty, 0, 100)
```

The clipping penalty begins only above 3% highlights, 8% shadows, or 5% clipped channels and is capped. A high directional low-detail signal has a larger blur penalty than moderate evidence. The visible bands are `STRONG`, `GOOD`, `REVIEW`, and `TECHNICAL_ISSUE`; their component measurements and confidence remain visible. These coefficients are M4 provider settings, not a hidden universal aesthetic. They are versioned in the cache key; M4 intentionally does not expose a photographer-facing tuning panel yet.

Motion blur is a conservative foundation rather than a cause classifier. A frame with strong detail reports low blur evidence. A low-detail frame is `HIGH` or `MODERATE` only when directional edge evidence is sufficiently asymmetric. Otherwise it is `UNCERTAIN`, because shallow focus, intentional softness, scene texture, and low light can look similar to motion blur.

Exposure output reports measurements rather than declaring a dark or bright creative frame bad: highlight, shadow, and channel clipping percentages plus luminance mean/median. Severe clipping may influence the technical label, but it never supplies an artistic verdict.

## Face and eye technical evidence

Face detection is rectangle detection only; it never creates a person identity. On macOS, CaptureOS first invokes `VNDetectFaceRectanglesRequest` through a direct Objective-C FFI bridge. The bridge receives a readable file URL for the oriented CaptureOS-managed preview, runs inside an autorelease pool, serializes the service-backed request, and returns only bounded JSON rectangle data. It does not use JXA or parse shell output. Its persisted diagnostic version includes the adapter revision, target architecture, and a validated `sw_vers` product version (for example `m4.apple-vision-adapter.v3;platform=macos;arch=aarch64;os=15.4;vision-request-revision=unavailable`). It retries once only for explicit transient XPC/connection failures.

If Vision returns an error, CaptureOS runs the bundled `UltraFace RFB-320` static ONNX rectangle detector locally through the pure-Rust `tract-onnx` runtime. The model is pinned at `models/ultraface-rfb-320.onnx`, version `version-RFB-320.onnx`, 1,270,727 bytes, SHA-256 `34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017`, under the upstream MIT license; its source, license, and platform scope are recorded in ADR 037. The model is compiled into the app, has no downloader, makes no network request, and outputs only anonymous face rectangles/confidence. Apple Vision remains the preferred macOS provider; a failed Apple attempt is retained only in Developer Details when UltraFace succeeds. A valid fallback result is `READY`, not a failure.

`FACE DETECTION`, `FACE LANDMARKS`, and `EYE STATE` have separate UI/status values. This repair intentionally stops after reliable rectangles: landmarks are `NOT_APPLICABLE` and eyes are `NOT_ANALYZABLE` unless a future approved local landmark provider is added. A landmark limitation cannot erase a valid face count or face-region sharpness. Small, profile, occluded, sunglasses-covered, or ambiguous faces should remain conservative rather than becoming a confident invented eye statement. CaptureOS does not create person identities, person clusters, cross-project matches, demographic inferences, or persistent biometric identity embeddings.

## Recommendations and photographer control

`captureos-technical-recommendation` / `m4.rules.v1` derives only from visible technical evidence, group relationship, and analyzable eye/face evidence. It can label a frame `STRONG_CANDIDATE`, `STRONG_ALTERNATIVE`, `REVIEW`, `PROBABLE_DUPLICATE`, or `TECHNICAL_ISSUE`. A recommendation carries reasons such as sharpness evidence, severe clipping, directional low-detail evidence, related-frame rank, or possibly closed analyzable eyes.

Recommendations are advisory and normally group-relative. They never mean “best photo,” never judge emotion/composition/storytelling, and never cause deletion, hiding, moving, trashing, renaming, or source mutation. A photographer can record Keep, Review, or Reject as an append-only `HumanDecision`; it overrides the displayed suggestion without changing the recommendation’s history.

## Milestone 5 culling handoff

The Smart Culling Workspace consumes current Capture Intelligence evidence but does not rerun, retune, or overwrite it. For a Similar Set, it presents the existing technical representative as a **suggested starting point** and renders its relative evidence/reasons. When scores are comparable, the UI does not manufacture a winner. A photographer may choose a distinct human representative; the technical representative remains intact and the comparison becomes a local `PreferenceExample` snapshot. M8 can use that explicit human comparison as separate pairwise Studio Brain evidence after an explicit training request; it never changes the technical or human representative.

Human review state is a separate project-scoped model: current decision/rating/star/note/flag state is indexed for queues, while history and review events retain the chronology. `REJECT` remains non-destructive metadata. Face View uses only the already persisted normalized anonymous detector rectangles to crop managed previews in the UI; no crop is stored, written next to an original, or used for identity matching. Compare Mode exposes available technical evidence and marks unavailable/not-analyzable fields honestly. Studio Brain is separate local preference modeling; it does not use note text, identity, creative, or emotion features and never mutates this M5 state.

## Resource behavior and operating limits

The UI records an explicit `ECO`, `BALANCED`, or `FAST` resource mode (`BALANCED` default) with each durable job. The current runner makes those modes real: `ECO` uses one local worker, `BALANCED` uses up to two, and `FAST` uses up to four, each capped by available CPU parallelism. Candidate computation runs in bounded parallel batches, while SQLite persistence and progress events remain serialized and durable. A panic or error in one worker becomes that asset’s `FAILED` result rather than aborting the batch. The M4 baseline is CPU bounded and makes no GPU-acceleration claim. Pause is observed between batches and records a durable job state; resume processes remaining cache misses. Startup recovery marks an interrupted running job rather than leaving it permanently running. This leaves room for future idle/interaction-aware scheduling without changing artifact semantics.

The M4 baseline is photo-only. Milestone 6 adds a separate, optional Magic Search layer for current-project still-photo image/text embeddings and deterministic hybrid filters. Milestone 7 may read existing persisted M4 evidence as weak local input for a separate structural Moment timeline. Milestone 8 may consume bounded current/local snapshots of this evidence for an explicit Studio Brain preference model, but it does not change the deterministic descriptor into a semantic embedding, merge semantic results into Similar Sets, alter generic recommendations, or authorize People Brain, identity recognition, video/audio intelligence, creative ranking, automatic culling, cloud work, model downloads, or a full local model manager. See [Magic Search architecture](magic-search.md), [Moment Brain](moment-brain.md), and [Studio Brain](studio-brain.md).

## Evaluation

`research/capture-intelligence-bench` supplies generated patterns and versioned schemas for duplicate pairs, similarity groups, sharpness preferences, face presence, eye state, and candidate preferences. On macOS it also measures a generated JPEG's first analysis-preview preparation and cache-hit reuse; it reports current-machine analysis throughput, grouping time, cache-key reuse time, and peak memory where measurable. It does not claim face/eye accuracy until licensed, test-safe fixtures and a vetted provider are available. See [the benchmark README](../../research/capture-intelligence-bench/README.md).
