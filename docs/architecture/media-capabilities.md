# Media capabilities (Milestone 3)

CaptureOS reports capabilities conservatively. Support means the local adapter can make a browsing artifact or read fields; it does not promise color-critical development, codec playback, or full manufacturer metadata.

| Family | Metadata | Thumbnail/poster | Viewer | Current local adapter |
| --- | --- | --- | --- | --- |
| JPEG / PNG / TIFF | Dimensions, orientation, file and available platform fields | Yes on validated macOS | Yes | `sips` |
| HEIC / HEIF | Platform-dependent | Platform-dependent | Browser/platform-dependent | `sips` when macOS decodes it |
| MOV / MP4 | Available platform fields when indexed by macOS | Quick Look poster when available | Browser/Tauri codec-dependent | `mdls`, `qlmanage` |
| WAV | Container duration, PCM codec, sample rate, bit depth, channels | Audio card | Yes | local RIFF parser |
| MP3 / other audio | Basic file metadata only in this milestone | Audio card | Browser/platform-dependent | platform metadata when available |
| ARW / CR2 / CR3 / NEF / RAF / ORF / RW2 / DNG | Catalog-level details | Explicit unsupported placeholder | No full RAW development | `RawPreviewProvider` boundary only |

The validated macOS implementation uses `/usr/bin/sips` and `/usr/bin/qlmanage` with structured process arguments. No FFmpeg binary is installed or required by this repository at this time. If a provider is absent, source media is never modified; CaptureOS records `unsupported`, `offline`, or `failed` and continues preparing other assets.

All previews are browsing previews in the CaptureOS cache. They are not a substitute for a color-managed editor.

## Capture Intelligence eligibility (Milestone 4)

Capture Intelligence resolves one sufficient local image per asset. It prefers a valid dedicated `ANALYSIS_PREVIEW` (2048px target), then a valid 1600px browsing `PREVIEW`. When no sufficient cache exists, it may read a catalog-marked available original only to generate the dedicated artifact inside the CaptureOS cache; analysis itself receives only that validated cache path. It preserves aspect ratio and orientation through the platform image adapter and does not upscale smaller sources. The smaller `MEDIUM` and `SMALL` browsing artifacts are deliberately excluded from technical, face, and eye claims.

| Family | Deterministic technical / similarity baseline | Optional face/eye provider | Honest fallback |
| --- | --- | --- | --- |
| JPEG / PNG / TIFF | Yes; creates/reuses a local analysis preview when an available source can decode | macOS Vision rectangles first; bundled local UltraFace rectangles if Vision fails | `NEEDS_ORIGINAL` only when no copy/cache is available; `CORRUPT`/`UNSUPPORTED`/`FAILED` as evidenced |
| HEIC / HEIF | Platform-dependent; same cache-resolution rule | Platform-dependent Vision or local UltraFace when a valid analysis preview decodes | `UNSUPPORTED` or `NOT_APPLICABLE` when the local providers cannot make a claim |
| RAW families | Not in M4 unless a future supported preview provider supplies a suitable cache artifact | No M4 RAW-specific face path | Explicit `UNSUPPORTED` or `NEEDS_ORIGINAL` as evidenced; no full RAW development |
| MOV / MP4 / audio / sidecars | Not eligible for M4 still-image intelligence | Not applicable | Existing Milestone 3 behavior remains unchanged |

M4.3 bundles one fixed, MIT-licensed local face-rectangle model: UltraFace RFB-320. It runs through a pure-Rust CPU runtime with no model downloader or network use and is documented in ADR 037. Apple Vision remains a local host-OS rectangle provider, subject to the user’s Apple OS/SDK terms; CaptureOS does not claim it is redistributable or open source. Face landmarks and eye state remain unavailable rather than guessed unless a future approved landmark provider can make that claim.

## Magic Search eligibility (Milestone 6)

Magic Search is still-photo retrieval only. It reuses the same oriented, CaptureOS-managed analysis-preview input boundary; it does not repeatedly decode full originals for search, write beside an original, or use a video/audio frame or waveform as a semantic input. Image embedding is optional and starts only when an admitted local image/text model pack is installed.

| Family | Semantic image embedding | Deterministic / hybrid evidence | Honest fallback |
| --- | --- | --- | --- |
| JPEG / PNG / TIFF | Eligible when a valid managed analysis preview and admitted local model pack are available | Face count, technical evidence, camera metadata, human decision/rating where persisted | `NEEDS_ORIGINAL` when no cache/source can create a preview; otherwise `UNSUPPORTED`/`CORRUPT`/`FAILED` as evidenced |
| HEIC / HEIF | Platform-dependent; eligible only when the local preview adapter creates a valid managed analysis preview | Same persisted evidence where available | `UNSUPPORTED`, `NEEDS_ORIGINAL`, or provider failure as evidenced; no fabricated semantic result |
| RAW families | Not eligible unless a future approved preview provider produces a sufficient managed image | Catalog metadata may still filter | Explicit semantic `UNSUPPORTED` or `NEEDS_ORIGINAL`; no implicit RAW decoder or full development claim |
| MOV / MP4 / WAV / MP3 / sidecars | Not eligible in M6 | Existing metadata and visual-engine behavior only | No video/audio semantic search or transcript inference |

The candidate model family is a manual/developer-installed [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) static ONNX pack, not a bundled CaptureOS asset. It must pass the local model registry’s path, checksum, license/provenance, and reference-vector checks before it is marked available. Without it, Magic Search supports only truthful deterministic metadata and technical filters and reports that semantic search is unavailable. See [Magic Search architecture](magic-search.md).

## Capture-time metadata repair (Milestone 7.1)

CaptureOS can explicitly refresh local capture-time metadata for an existing catalog without
recreating the project. For JPEG, HEIF, and PNG it reads standard embedded EXIF fields locally
through a bounded Rust container parser; valid original capture time, subsecond precision, and an
observed offset take precedence over weaker values. An absent EXIF offset is represented as an
unknown camera wall clock, not UTC. Platform metadata may contribute a lower-priority
content-creation value where supported. Filesystem dates are last-resort, low-confidence
provenance only.

The current direct parser intentionally does not promise TIFF-based RAW or video embedded-time
parsing. Those formats remain dependent on the existing local platform metadata adapter. There is
no cloud request, source write, automatic model download, or filename/order inference. Available
duplicate copies are observed separately; disagreement in embedded camera times is surfaced only
as a local developer diagnostic.

## Moment Brain eligibility (Milestone 7)

Moment Brain uses existing local still-photo catalog evidence. It does not introduce another image decoder, require an original merely to open a timeline, or expand M6 support to video/audio. Timestamped eligible photo assets may participate in structural timeline analysis when durable metadata exists; compatible existing M6 embeddings are optional supporting evidence, not a prerequisite for a basic time/evidence projection. Assets without a trustworthy capture timestamp remain explicitly ungrouped/uncertain rather than being placed by filename or a guessed time.

| Family | Structural Moment timeline | Label/representative evidence | Honest fallback |
| --- | --- | --- | --- |
| JPEG / PNG / TIFF | Eligible through local capture metadata and durable evidence | Compatible local embedding when admitted/available; existing technical and anonymous face-count evidence | Structural time-only/partial result or explicit unavailable evidence; never fabricated caption/detection |
| HEIC / HEIF | Platform/catalog-metadata dependent | Same only when an existing managed preview-derived embedding/evidence exists | Ungrouped, partial, or unavailable state as evidenced |
| RAW families | No new RAW decode in M7 | Existing catalog metadata only if already available; no implicit semantic input | No generated Moment visual claim from an unsupported RAW input |
| MOV / MP4 / WAV / MP3 / sidecars | Not eligible for M7 timeline/semantic analysis | Not applicable | Existing Milestone 3 behavior remains unchanged |

Moment Brain uses capture cadence, compatible visual continuity, and weak local metadata transitions to propose structural review units. It is not an event detector, person/relationship recognizer, emotion/creative model, or missing-shot checker. It never rewrites EXIF/source timestamps; a possible camera time offset is advisory local evidence only. See [Moment Brain architecture](moment-brain.md).

## Studio Brain eligibility (Milestone 8)

Studio Brain does not decode media or require an original. It consumes only compact durable evidence already allowed by M4–M7 and can make recommendations while external originals are offline when that evidence remains current. It is still-photo culling advice only; it does not extend any visual decoder or semantic capability.

| Family | Studio advisory feature use | Honest fallback |
| --- | --- | --- |
| JPEG / PNG / TIFF | Existing technical, anonymous face/eye, Similar Set, Moment, generic-recommendation, and semantic-availability summaries where available | Missing evidence is explicitly unavailable; model may abstain |
| HEIC / HEIF | Same only where existing local providers produced durable evidence | No fabricated score or preference if a platform provider is unavailable |
| RAW families | Existing durable catalog/technical context only; no new RAW decode | Missing/unsupported evidence remains unavailable; no full RAW claim |
| MOV / MP4 / WAV / MP3 / sidecars | Not eligible for Studio Brain I | Existing Milestone 3 behavior remains unchanged |

The model receives no original bytes, filenames, paths, notes, raw embeddings, identity data, or semantic object claims. See [Studio Brain architecture](studio-brain.md).
