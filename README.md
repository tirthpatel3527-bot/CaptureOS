# CaptureOS

CaptureOS is a local-first foundation for an intelligent operating system for professional camera media. Its long-term purpose is simple: **your entire shoot, alive, understood, protected.**

Milestone 8 adds **Studio Brain I**: explicit local preference learning for photographer-controlled culling recommendations. It is a compact, offline, explainable advisory layer built on intentional human decisions and representative choices—not passive behavior or AI output. It never replaces Capture Intelligence, Magic Search, Moment Brain, or the photographer's decisions.

## Guarantees

- Original media stays on user-controlled disks and folders.
- No cloud account, paid API, hosted storage, telemetry, or GPU service is required.
- Index Mode and Ingest Mode are separate architectural concepts. Index Mode never copies media; Ingest Mode copies only to explicitly selected destinations and never modifies source media.
- There are no artificial catalog, project, media, or storage size limits.
- `MediaAsset` (a conceptual media item) is distinct from `FileInstance` (a physical copy).
- CaptureOS never deletes, formats, renames, overwrites, or otherwise alters original media. Ingest writes an incomplete destination only as `*.captureos-partial`, then atomically finalizes it after BLAKE3 verification.

## Repository map

```text
apps/desktop/           Tauri desktop shell and React project home
crates/media-model/     Strongly typed CaptureOS domain model
crates/capture-graph/   Extensible relationships and graph utilities
crates/persistence/     SQLite migrations and repository boundary
crates/capture-core/    Golden Shoot loader, projects, and index service
crates/media-index/     Read-only discovery, classification, and fingerprints
crates/media-visual/    Local metadata and thumbnail/poster adapter boundary
crates/capture-intelligence/
                        Local deterministic fingerprints, grouping, technical evidence,
                        face-provider boundary, and conservative recommendations
crates/studio-brain/   Pure-Rust compact preference model, calibration, abstention, and pairwise ranking
crates/storage/         Storage-volume semantics
crates/ingest/          Streaming copy, BLAKE3 verification, pre-flight, safe layout
crates/integrity/       Future integrity extension boundary
packages/contracts/     TypeScript bridge contracts
packages/ui/            Small React UI primitives
packages/design-system/ Design tokens
fixtures/               Deterministic Golden Shoot and filesystem index fixtures
research/capture-intelligence-bench/
                        Generated-fixture benchmark and versioned ground-truth scaffolding
research/magic-search-bench/
                        Local retrieval benchmark foundation and versioned search ground truth
research/moment-brain-bench/
                        Generated structural-timeline benchmark foundation
research/studio-brain-bench/
                        Generated preference-recovery and scale benchmark foundation
docs/                   Product, architecture, ADRs, and security notes
```

## Prerequisites

- Node.js 20+ and npm 10+
- Rust 1.78+ via [rustup](https://rustup.rs/)
- Tauri 2 system prerequisites for your OS: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

## Run locally

```sh
npm install
npm run typecheck
npm run lint
npm run test
npm run build

cargo fmt --all --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p capture-intelligence-bench -- --suite baseline
cargo run -p magic-search-bench -- --suite baseline
cargo run -p moment-brain-bench -- --suite baseline
cargo run -p studio-brain-bench -- --suite baseline
# Starts the browser shell for UI-only work:
npm run dev
# Starts the complete desktop app (requires the Rust prerequisite):
npm --workspace @captureos/desktop run tauri:dev
```

The desktop app stores its catalog and generated preview cache at the platform application-data location. It only opens selected source files for read-only metadata, hashing, and on-demand preview preparation; it never writes into selected source folders. Ingest writes only to user-selected destination folders.

## Index Mode behavior

1. CaptureOS opens at **Home / Project Library**. Create a local project or select an existing project; existing catalog data remains available there.
2. Select **Index Folder** and choose a folder using the desktop picker.
3. CaptureOS recursively discovers regular files, skips symlinks, classifies names by extension, makes a bounded content fingerprint, and persists catalog records.
4. The job panel receives Rust-side progress events; the media browser reads the resulting SQLite rows.

The M1 merge heuristic is deliberately conservative: an existing `MediaAsset` is reused only when project, basic media type, normalized extension, exact byte size, and BLAKE3 fast fingerprint all agree. Otherwise CaptureOS creates a separate asset. A matching copy becomes another `FileInstance`.

Fast fingerprints contain a version marker, byte size, extension, and the first/last 64 KiB of content (or all content for smaller files). Files up to 1 MiB also receive a full BLAKE3 hash. This is not perceptual matching, metadata matching, or a guarantee that different files cannot collide; those are later capabilities.

Reindexing a root preserves its `IndexRoot` and matching `FileInstance` IDs. Existing file instances for that root are first marked unavailable, then reactivated when seen. Removed files remain catalogued as unavailable; CaptureOS does not delete source data or catalog history during M1.

Index Mode is intentionally additive within a project: selecting another folder adds another `IndexRoot` to that project. Different roots on the same mounted volume reuse one `StorageVolume`; the media table shows the physical volume/device separately from the selected root. A future remove-root workflow is out of scope for M1.

## Ingest Mode behavior

1. Create or open a local project and choose **Ingest Shoot**.
2. Select one or more source folders, a required master, and zero or more backup folders.
3. Review the pre-flight report. It discovers regular files without following source symlinks, checks destination writability/capacity, rejects unsafe source/destination relationships, and warns when destination copies share one storage volume.
4. CaptureOS copies each source into `Project/01_SOURCES/Source Label/…` below every selected destination. Source namespaces preserve duplicate filenames safely.
5. Each copy streams through a BLAKE3 hash, is re-read and BLAKE3-verified, and remains `*.captureos-partial` until verification and no-overwrite finalization succeed.
6. Verified physical copies become `FileInstance` records linked to the same logical `MediaAsset`, with `COPIED_FROM` and `VERIFIED_COPY_OF` CaptureGraph facts. The persisted per-destination ingest item retains the hash evidence, outcome, and source/destination instance IDs.
7. CaptureGuardian derives the final state from those verified records. Standard protection requires a verified master and verified backup on independent `StorageVolume` identities; Basic requires only a verified master and is presented as reduced protection.

`fixtures/ingest/camera-a` and `fixtures/ingest/camera-b` are safe deterministic demo sources. Select new empty folders outside `fixtures/ingest` for the master and backup; never use a source folder as a destination.

## Visual Media Engine behavior

Index Mode now opens on a logical-media grid: one `MediaAsset` card, even when multiple physical `FileInstance` copies exist. The inspector shows those copies, storage, availability, and prepared metadata. The advanced table keeps the physical-file view available for catalog diagnostics.

The browser pages 120 logical assets at a time, queries deterministic filters/search/sorts in SQLite, and prepares only the requested page in a worker. Photo thumbnails are generated with the macOS `sips` adapter where available; MOV/MP4 poster frames use macOS Quick Look; WAV gets local RIFF metadata and an audio card. The current build does not bundle FFmpeg or a RAW decoder. Unsupported RAW formats use an explicit placeholder rather than a fake image. HEIF support depends on the local macOS provider and browser codec.

Preview artifacts are stored in CaptureOS application data, keyed by asset/file instance/source fingerprint/generator version/size. Clearing preview cache removes only generated artifacts; catalog metadata stays. If a source drive is offline, the grid can still show valid cached thumbnails and metadata and labels the original as offline. Preparation counts every terminal logical asset (ready, unsupported, offline, corrupt, failed, timeout, or cancelled), validates obviously malformed containers before platform decoding, and bounds every platform provider process so one bad file cannot block later real media. The **Retry failed previews** action retries only failed/timed-out work and never regenerates ready artifacts.

See [media capabilities](docs/architecture/media-capabilities.md) for current validated formats and the deliberately unsupported RAW/codec cases.

Keyboard shortcuts in the viewer: Arrow keys move through the loaded filmstrip, Esc or Space closes, +/- changes zoom, I toggles metadata, and G toggles the filmstrip. `Fit` restores the default view.

## Capture Intelligence I behavior

Capture Intelligence is an optional local analysis layer for eligible photo assets. It first reuses a valid CaptureOS-managed `ANALYSIS_PREVIEW` (2048px target) or sufficient 1600px browsing `PREVIEW`. If neither exists but a catalog-marked `FileInstance` is available, it reads that source only to create the contained analysis-preview cache artifact, then gives only the cache path to analysis. It never modifies the source, writes a sidecar, or exposes an original path to an analyzer. `MEDIUM` and `SMALL` browsing renditions are never substituted for technical, face, or eye claims. `NEEDS_ORIGINAL` means every known copy is offline and no usable cache exists; decoder limitations and malformed inputs remain `UNSUPPORTED` or `CORRUPT`. It persists a durable background job and per-asset artifacts, so browsing continues while analysis runs and an interrupted job is recoverable. `ECO`, `BALANCED` (the default), and `FAST` bound local work to one, up to two, or up to four CPU workers respectively (also capped by available CPU capacity); the M4 baseline does not claim GPU acceleration.

The baseline produces inspectable evidence, not an aesthetic verdict:

- exact-duplicate evidence only from available verified BLAKE3 content hashes—never a filename or bounded fast fingerprint;
- pHash, dHash, a compact color signature, and a 64-byte signed luminance descriptor for near-duplicate and related-frame candidates;
- deterministic sharpness, directional low-detail/possible-motion-blur, and exposure/clipping evidence;
- deterministic, reproducible exact/near/similar/burst groups with bounded candidate generation; and
- conservative, group-relative labels such as **Strong technical candidate**, **Alternative**, **Review**, **Probable duplicate**, and **Technical issue**, always accompanied by reasons and confidence.

Technical evidence is not a statement about emotion, composition, storytelling, photographer intent, or whether an image is "beautiful." It never deletes, hides, moves, or culls media. A photographer’s Keep, Review, or Reject decision is stored separately and never erases the recommendation that prompted it.

Face analysis is local-only and optional. On macOS, CaptureOS may use the user’s installed Apple Vision capability for face boxes, landmarks, and conservative eye-state evidence. It is not a bundled or redistributed CaptureOS model; Apple’s host-OS terms govern it. Where that capability is absent or cannot make a reliable claim, CaptureOS records **Face analysis unavailable**, `UNCERTAIN`, or `NOT_ANALYZABLE`—never invented faces or eye states. CaptureOS does not perform identity recognition, person clustering, demographic inference, or cross-project biometric matching.

Analysis is cache-aware. Its key includes the analysis-preview fingerprint, provider/version, and settings version; source or analyzer changes mark earlier evidence stale instead of silently reusing it. Corrupt, unsupported, offline, and provider-failure outcomes are durable terminal states that do not block remaining assets. See [Capture Intelligence architecture](docs/architecture/capture-intelligence.md), [model registry](docs/architecture/local-model-registry.md), and [privacy/security](docs/security/capture-intelligence.md).

Run the generated, no-download baseline suite with:

```sh
cargo run -p capture-intelligence-bench -- --suite baseline
```

It reports measurements from the current machine. It does not claim face or eye accuracy until a properly licensed, test-safe fixture and vetted provider are available.

## Magic Search behavior

Magic Search is scoped to the current project and keeps the existing visual grid, Inspector, pagination, offline labels, and MediaAsset-level cards. A text query can use an installed local image/text embedding provider in a shared vector space; simple high-confidence terms such as `2 faces`, `5 star`, `kept`, `sharp`, or `camera SLT-A58` also become deterministic predicates. The result explanation names only the signals actually used. A semantic-only result is described as a **local semantic match** with a non-confidence local ranking signal, never as an invented object detection.

Eligible still photographs reuse a valid CaptureOS-managed analysis preview whenever possible. A semantic embedding is durable local derived evidence keyed by MediaAsset, input fingerprint, semantic model/version, and preprocessing version. One logical MediaAsset receives one current embedding even if it has several physical FileInstances. Cached READY embeddings remain searchable when originals are offline. Corrupt, unsupported, missing-original, and failed items are terminal per-asset outcomes that do not stop the project queue.

The default provider contract is an opt-in, manually installed SigLIP ONNX model pack; CaptureOS does not bundle or automatically download its model weights. Until the one compiled, checksum-pinned pack completes registry, artifact, tokenizer, ONNX, and reference-vector validation, the UI must report **Semantic model not installed** and retain deterministic metadata and technical filtering. See the [controlled local-model installation guide](docs/architecture/semantic-model-install.md), [Magic Search architecture](docs/architecture/magic-search.md), and [the local model registry](docs/architecture/local-model-registry.md).

**Find Similar** uses the selected image's semantic embedding to retrieve current-project visual neighbors. It is intentionally distinct from **Similar Sets**, which remain conservative related-frame/burst groups. Neither search path modifies human decisions, ratings, notes, representatives, review sessions, source media, or CaptureGraph group membership.

Run the generated, no-download retrieval benchmark foundation with:

```sh
cargo run -p magic-search-bench -- --suite baseline
```

It reports measurements from the current machine for synthetic vector records at 1k, 10k, and 50k scale. It is not a claim of semantic quality; semantic Recall@K/MRR/nDCG require a separately licensed, versioned ground-truth dataset and an admitted local model pack.

## Moment Brain and Shoot Timeline Intelligence I behavior

**Moments** is a project-scoped, optional structural timeline view for still photographs. It uses local capture-time cadence, compatible existing local embeddings, camera/lens/orientation, anonymous face-count availability, Similar Set continuity, and technical evidence to organize a shoot into reviewable sequences. It is not an event detector, person/relationship recognizer, wedding-stage classifier, creative judgement, missing-shot claim, or automatic culling tool. Missing timestamps remain visibly ungrouped rather than being guessed.

Moment analysis is an explicit durable background operation. Opening CaptureOS or a project only reads compact status; it never waits for a model, scan, or rebuild. An **Update** processes bounded local chronology around new records, while **Rebuild** refreshes derived structural records when incompatible/out-of-order evidence requires it. Neither action changes source media, Capture Intelligence artifacts, Similar Set membership, Magic Search history, Keep/Reject/Review state, ratings, notes, review sessions, or existing group representatives.

Moment Brain uses a separate conservative label boundary. The installed M6 shared image/text embedding model is not a captioner. It ranks only a reviewed neutral vocabulary and optional photographer-authored checklist/project phrases; when candidate evidence is weak, ambiguous, conflicting, or unavailable, the UI shows **Untitled Moment**. AI suggestion/evidence and a human title are retained separately; the human title controls presentation. Examples such as **Outdoor portraits**, **Boat portraits**, and **Indoor group photos** illustrate neutral supported combinations only—they are not hard-coded wedding or AI Test labels.

Coverage Map I shows observed local facts such as capture ranges/gaps, asset totals, technical/anonymous-face-count availability, decision summaries, and photographer-owned checklist state. It never claims a required shot was missed or auto-completes a checklist item. A multicamera **Possible camera time offset** is advisory only; CaptureOS does not rewrite EXIF, source metadata, or catalog timestamps.

Run the generated, no-download structural benchmark with:

```sh
cargo run --release -p moment-brain-bench -- --suite baseline
```

It compares time-only, synthetic-signature-only, and combined structural mechanics at 1k, 10k, 50k, and 100k generated records. It contains no AI Test/customer photograph, model weight, model download, cloud request, identity/event label, or real semantic-quality claim. See [Moment Brain architecture](docs/architecture/moment-brain.md) and [MomentBrainBench](research/moment-brain-bench/README.md).

## Studio Brain I behavior

**Studio Brain** is a local preference model that a photographer explicitly trains from intentional Keep/Review/Reject history and approved human representative choices. It starts honestly in **Not ready** or **Learning**; the current AI Test's handful of decisions must not create a ready personalized model. Ratings/stars remain separate auxiliary signals, and private notes are never parsed for training.

The initial compact model is deterministic, regularized three-class linear softmax with calibration, abstention, and a separate pairwise Similar Set ranker when enough human representative choices exist. It uses only bounded local technical, anonymous face/eye, Similar Set, Moment, generic-recommendation, and semantic-availability features; it does not ingest raw embeddings, filenames, paths, notes, identities, demographics, emotion, or client information. Studio Brain works offline and never opens originals merely to train or infer.

Training is an explicit background action. It snapshots eligible immutable human-source rows, holds out whole projects where possible (otherwise whole Similar Set/Moment/capture-day buckets), validates a checksummed static JSON candidate, then atomically activates it only after it satisfies readiness and does not materially regress against the retained active model on the same grouped holdout. If it is not ready, fails, is interrupted, corrupt, or loses that comparison, generic Capture Intelligence remains available and the prior valid model stays active. Reset removes only derived Studio artifacts; human decisions and all M0–M7 evidence remain untouched.

Smart Cull shows **Capture Intelligence** and **Studio Brain** separately. A Similar Set may show a Studio advisory starting point alongside the technical starting point and the photographer's representative; none is auto-selected. See [Studio Brain architecture](docs/architecture/studio-brain.md) and [StudioBrainBench](research/studio-brain-bench/README.md).

Run the generated, no-download preference benchmark with:

```sh
cargo run -p studio-brain-bench -- --suite baseline
```

It reports local generated-data mechanics at 100, 1k, 10k, 50k, and 100k records. It is not real-photographer or artistic-quality accuracy.

## Milestone 8 boundaries

This repository intentionally does **not** perform person identity recognition, cross-project face/person search, People Brain, event/wedding-stage recognition, creative/aesthetic/emotional scoring, client preference modeling, editing/style training, automatic culling/deletion, source write-back, color-critical RAW development, full proxy transcoding, NLE integration, video/audio intelligence, authentication, cloud work, automatic eject, card formatting, collaboration, billing, or Milestone 9 work. See [the product vision](docs/product/vision.md), [architecture](docs/architecture/system.md), and the ADRs in [docs/adr](docs/adr).

## Smart Culling Workspace behavior

Choose **Cull Photos** from a project to open the dedicated local review workspace. It supports All Photos, Similar Sets, and AI Review Queue modes; Focus, Set Grid, Compare, and Face View surfaces; local Keep/Reject/Review decisions; star/favorite and independent 0–5 rating; a local note; optional Auto Advance; Undo/Redo during the current session; and resume position after restart. `REJECT` is catalog metadata only—it never alters originals, source XMP, sidecars, or physical copies.

Keyboard controls are visible in the workspace: `K` Keep, `X` Reject, `R` Review, `S` star, `1`–`5` rate, `0` clear rating, arrow keys navigate, Space switches focus/grid, `C` Compare, `F` Face View, `G` Set Grid, and `U` Undo. They are disabled while typing in a field.

AI technical recommendations, Studio Brain recommendations, and human decisions are stored independently. A Similar Set retains its technical starting point while a photographer may separately choose a human representative; eligible explicit representative choices can become local pairwise Studio evidence only after an explicit training request. A Culling Report is an explicit user-selected new CSV file containing catalog decision data only—never media bytes, face crops, or filesystem paths.

## License

MIT. See [LICENSE](LICENSE).
