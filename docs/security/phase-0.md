# Phase 0 security posture

All media-facing inputs are untrusted: paths, symlinks, volume labels, filenames, extension claims, EXIF/XMP metadata, sidecars, and binary files. Index Mode opens user media read-only. Ingest Mode reads source media read-only and writes only below a canonical user-selected destination root.

- Reject absolute paths, parent traversal (`..`), empty components, and NUL bytes for `FileInstance.relative_path`.
- Treat a mount location as transient UI state, not a volume identity.
- Avoid shelling out with user-provided path strings. Future external tools must receive structured arguments and constrained, canonicalized paths.
- Set SQLite foreign keys for every connection, use migrations transactionally, and return errors rather than silently repairing corrupt catalog state.
- Bound metadata parsing and use isolated decoders in future work; media inspection must never execute media or sidecars.
- No secrets, cloud credentials, network calls, or telemetry are present in Phase 0.

## Milestone 2 ingest policy

- Never follow a source symlink; record it as skipped. Destination parents are checked for symlinks before CaptureOS creates a directory or partial file.
- Canonicalize selected roots, reject source/destination nesting and equal roots, and reject unsafe relative paths before deriving the deterministic destination layout.
- Create destination output with `create_new` using a sibling `*.captureos-partial` name. A partial is never registered as verified and is retained for a safe individual-file retry after interruption.
- Read the source in bounded streaming buffers, hash with BLAKE3 during copy, re-read the partial to compare cryptographic hashes, and check source size/modification time before finalization.
- Finalize with a platform no-replace operation. If another file appears at the final name, retain the partial and report a conflict; never silently overwrite it.
- Treat unavailable paths and filesystem errors as unverified evidence. The app does not shell out to `cp`, `mv`, mount, eject, or format commands.

## Milestone 4 Capture Intelligence security posture

- Analysis opens only a canonical, CaptureOS-managed cached preview beneath the preview-cache root. When required to create that cache representation, the resolver reads only a catalog-marked available source through the fixed local adapter; it never writes a preview, face crop, sidecar, or metadata beside customer originals.
- Analysis-preview paths, image dimensions, decoder output, face boxes, provider responses, model metadata, and persisted descriptors are untrusted. Validate bounds and provider output before persistence; reject paths that escape their owned root.
- The deterministic baseline is local and model-free. It performs no network call, telemetry, cloud inference, hosted GPU use, or automatic model download.
- On macOS, the optional Apple Vision adapter receives a validated cache-preview path through structured process arguments, has a local timeout, and is allowed to return unavailable/failed rather than fabricate evidence. It is a host-OS capability, not a bundled CaptureOS model.
- Face processing is detection/landmark evidence only. CaptureOS stores normalized boxes and technical/eye-state evidence as applicable; it does not upload crops, generate person identities, retain biometric identity embeddings, or compare people across projects.
- Future model files are untrusted external resources. A registry entry must record provider, version, path relative to a controlled model root, checksum, capability, license, source, file size, and hardware needs. A future installer must require explicit user action, verify checksum and canonical containment, and never execute model-provided scripts or arbitrary code.
- Analysis artifacts preserve input fingerprint, provider/model/settings provenance, confidence, status, and error. Human overrides append to that history rather than mutate an AI record. Corrupt, unsupported, offline, and provider-failure cases remain explicit terminal states and cannot stall the remaining queue.
