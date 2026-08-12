# Capture Intelligence privacy and security

Capture Intelligence I is local-only, non-destructive technical analysis. It is designed so an unavailable provider can say "unavailable" without requiring cloud fallback or fictional output.

## Privacy guarantees

- Customer originals, cached previews, metadata, face boxes, compact descriptors, recommendations, and human decisions stay on the local device. M4 makes no inference API, telemetry, analytics, model-download, hosted GPU, or remote facial-service request.
- Analysis receives a CaptureOS-owned cached preview after canonical cache-root validation. `AnalysisInputResolver` may read a catalog-marked available source solely to generate that contained artifact; it does not alter, rename, write beside, embed metadata in, or create a sidecar next to customer originals.
- Face work is detection/landmark technical evidence only: normalized bounding boxes, detection confidence, visibility/pose when supported, face-region sharpness, and conservative eye state. It is not recognition.
- CaptureOS does not create person names, persistent biometric identity embeddings, person clusters, cross-project matches, demographic/sensitive-attribute inferences, or face-crop archives in M4.
- A missing or failed face provider records `NOT_APPLICABLE`, `FAILED`, `UNCERTAIN`, or `NOT_ANALYZABLE` as relevant. It never substitutes a random count, confidence, or eye-state value.
- AI recommendations never delete, hide permanently, trash, move, or mutate media. Human Keep/Review/Reject decisions are append-only and preserve the original recommendation and provenance.

## Trust boundaries

All values crossing into intelligence are untrusted: cache relative paths, image bytes and dimensions, preview metadata, input fingerprints, model metadata, provider stdout, face geometry, timestamps, camera strings, and existing database contents.

1. The desktop/core boundary gives an analyzer only a registered cache artifact, not a general local-file URL. The resolver's read-only source step is before this boundary and cannot pass a source path through it.
2. The cache resolver canonicalizes the artifact below the CaptureOS preview-cache root. Paths outside that root, traversals, missing files, and escaped symlinks are rejected.
3. Decoders impose format/dimension limits and return a terminal status instead of escalating a malformed input into a queue failure.
4. Platform providers use fixed executable paths and structured arguments. The macOS decode adapter invokes `/usr/bin/sips` only for a validated managed-cache preview and parses an ephemeral BMP locally; the Vision rectangle adapter uses a direct Objective-C FFI bridge with a readable managed-cache URL, autorelease pool, bounded retry, and normalized-box/confidence validation before persistence. Its local UltraFace fallback accepts only the compiled-in model bytes and decoded managed preview pixels.
5. SQLite data stays relational and local. Compact visual descriptors use BLOB storage, not large JSON float arrays or original bytes. `AnalysisArtifact` records status/error/provenance instead of losing failure context.
6. Future local model files must remain below a controlled model root, have an expected checksum where available, be explicitly installed, and never supply executable hooks/scripts. See the [model registry](../architecture/local-model-registry.md).

## Data retained by M4

| Data | Local retention purpose | Explicitly absent |
| --- | --- | --- |
| Analysis artifact | Reproducibility, status/error, provider/settings/input provenance | Customer image pixels and a remote inference request |
| Fingerprint / compact descriptor | Bounded related-frame candidate generation | A semantic identity profile or cloud vector database |
| Similarity group/member record | Rebuildable project-local grouping | Millions of pairwise graph edges by default |
| Technical evidence | Explainable sharpness, blur, and exposure components | An artistic-quality assertion |
| Face evidence | Per-asset box/technical/eye state where provider supports it | Person identity, demographics, cross-project biometric link |
| Human decision | Photographer control and future correction research boundary | Replacement or deletion of the AI history |
| Semantic embedding / index | Local current-project visual retrieval and rebuildable index artifact | Original pixels, a cloud vector database, person identity, automatic export |
| Local search history | Recent project-scoped queries and optional saved query definitions | Telemetry, cloud synchronization, cross-project query leakage |

## Platform capability disclosure

On macOS, CaptureOS first uses the user’s installed Apple Vision rectangle capability locally. It neither downloads an Apple model nor calls an Apple network service, and it does not claim Apple Vision is open source or redistributable. If Vision cannot produce a safe result, CaptureOS runs the bundled MIT-licensed UltraFace RFB-320 detector locally through a pure-Rust runtime. The fallback is limited to anonymous face rectangles and confidence; it performs no landmark, eye-state, or identity claim. Both attempts remain component-local and have separately versioned face-artifact provenance; a fallback success is `READY` while the earlier Vision error is developer-only evidence.

## Incident and failure behavior

Analysis failures are scoped to an asset/provider. A corrupt preview, unsupported format, unavailable original, or provider error cannot mark other media failed or leave the durable job indefinitely running. A shutdown-recovery pass records interrupted work as interrupted so a photographer can resume it. Existing `READY` artifacts remain available while originals are offline as long as their cache input remains valid.

## Milestone 5 review data

- Review sessions, current decisions, immutable decision history/events, notes, stars, ratings, flags, group representatives, and preference examples remain in the same local SQLite catalog.
- Preference examples store only relative asset IDs and technical/recommendation snapshots. They do not include original image bytes, face crops, original filesystem paths, identity labels, or a network upload target.
- Face View crops a managed preview only for display. It creates no stored crop and never changes an original or sidecar.
- A culling report is an explicit user-selected new CSV/JSON destination. Export uses create-new semantics and will not overwrite an existing file; it reports catalog metadata only.
- AI/Human Agreement is a local descriptive count. It is not called accuracy and does not trigger personalized training, telemetry, or cloud communication.

## Milestone 6 Magic Search data

- Magic Search sends no image, managed preview, text query, embedding, face count, filesystem path, model metadata, search history, or result explanation to a remote service. It has no remote embedding API, hosted vector database, telemetry, analytics, model downloader, or hosted GPU path.
- Semantic inference receives only a canonicalized CaptureOS-managed analysis preview. It does not receive an arbitrary original path; the resolver may read an available original only to create/reuse that contained preview without mutating the source or creating a sidecar.
- Semantic embeddings are potentially sensitive local derived data. They are stored per MediaAsset with model/input/preprocessing provenance, are never automatically exported, and are rebuilt through the local indexing control without affecting source media, catalog records, Capture Intelligence evidence, Similar Sets, or human decisions. Milestone 6 does not expose a clear-index/delete-derived-data control.
- A project-scoped vector index is derived from durable embedding records. Model/index paths are canonicalized below controlled roots, static ONNX/tokenizer files are checksum-checked where configured, and arbitrary Python, pickle, model-provided executable, hook, or traversal path is rejected.
- Search history is local and project-scoped. Clearing it removes only the requested history entries; it does not alter media, culling state, or analytical evidence.
- Semantic similarity is not object detection, object localization, person recognition, a biometric profile, demographic inference, a creative/emotional judgment, or a statement that an image definitely contains a query concept. User-facing explanations name only actual vector/metadata/technical signals.
- Milestone 6 remains photo-only and current-project only. It contains no identity recognition, face clustering, cross-project person search, video/audio semantic search, cloud synchronization, or automatic culling.

## Milestone 7 Moment Brain and timeline data

- Moment Brain sends no original, managed preview, capture timeline, embedding, centroid, label candidate, boundary evidence, checklist text, anonymous face evidence, camera metadata, filesystem path, or human override to a remote service. It has no cloud timeline API, hosted vector database, telemetry, analytics, model downloader, hosted GPU, or paid dependency path.
- Timeline analysis reads project-scoped durable local evidence. It may use compatible existing M6 embeddings and managed-preview-derived evidence while a source volume is offline, but it does not repeatedly decode originals merely to open a project or a timeline. Missing/unavailable input remains an honest partial/ungrouped state and cannot block unrelated work.
- Structural segments, Moments, memberships, local centroids, boundary evidence, label suggestions, checklist rows, camera-clock diagnostics, and human override events are potentially sensitive local derived data. They are foreign-key scoped to one project, rebuildable from durable local evidence plus explicit human events, and are never the sole source of project identity or human decisions.
- Moment labels are local candidate rankings, not captions or detections. The product may score only reviewed neutral vocabulary and photographer-supplied phrases; it abstains as **Untitled Moment** when evidence is weak, ambiguous, conflicting, unavailable, or incompatible. It never invents identity, relationship, emotion, ceremony stage, wedding event, creative intent, or a missing required shot.
- AI suggested labels/evidence and human labels are distinct. Human rename, merge, split, representative, and coverage confirmation actions are append-only local evidence and remain protected during a derived rebuild. Moment analysis cannot change source media, EXIF, sidecars, catalog timestamps, Similar Sets, Magic Search history, or M5 decisions/ratings/notes/history.
- Coverage Map records observed facts only. A photographer owns checklist completion; ranked candidates never auto-complete a checklist. A possible camera-clock offset is advisory and has no timestamp/source write-back path.
- Timeline analysis is an explicit background operation. Startup and project opening query only compact status, and project/asset/Moment ownership is rechecked before every timeline, culling, or search scope. There is no cross-project person, timeline, or biometric correlation.

## Milestone 8 Studio Brain preference data

- Studio Brain is local-only and offline-capable. It does not upload decisions, ratings, stars, representative choices, feature snapshots, model artifacts, metrics, cached previews, embeddings, project names, paths, notes, media, or any preference-derived telemetry.
- Training source records are created only from explicit human actions. AI recommendations, passive browser behavior, search, view time, hover, zoom, indexing, and lack of action are excluded. A Studio recommendation is recorded only as optional decision-time provenance and is never a training label.
- Feature snapshots are bounded to permitted local technical/anonymous face/eye, Similar Set, Moment, generic-recommendation, and semantic-availability summaries. They exclude original bytes, source/cache paths, filenames, notes, raw semantic embeddings, face identity, person profiles, demographics, protected traits, emotion, attractiveness, and client identity.
- Project opt-out blocks both new source materialization and explicit historical backfill while preserving normal culling decisions. Individual training exclusions are separate local records. Neither control deletes or rewrites M5/M7 human history.
- Model artifacts are small structured JSON with a static feature schema, normalizer, calibration, and checksum. Persistence validates schema and checksum before storage/activation and when loading an active model; it does not load pickle, Python, executable hooks, user-supplied paths, or arbitrary code. A corrupt artifact is invalidated and generic M0–M7 evidence remains available.
- Candidate recommendations remain invisible until an atomic model activation transaction succeeds. The previous valid model is retained through candidate fitting, evaluation, persistence, and checksum validation. Training failure/interruption/reset never changes customer media, human decisions, technical evidence, semantic embeddings, Moments, or source metadata.
- Studio model/run/recommendation records are local sensitive derived data and rebuildable from durable explicit source records. They are not a sole source of project identity or authority. Project deletion policy is deferred until deletion itself is designed; CaptureOS does not silently retain preference history from a deleted project.

This policy is additional to the repository-wide posture in [Phase 0 security](phase-0.md) and the non-negotiable rules in [AGENTS.md](../../AGENTS.md).
