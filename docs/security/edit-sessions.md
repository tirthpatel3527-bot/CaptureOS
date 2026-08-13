# Edit Sessions and derived-output security and privacy boundary

Milestone 10 is a local, photographer-controlled handoff and output-observation foundation. It
does not upload originals, derivatives, cached previews, session names, output roots, source or
output paths, file fingerprints, output metadata, matching evidence, approvals, decisions, notes,
Studio data, reports, or proprietary editor data. It requires no account, cloud credential, paid
API, telemetry, hosted storage, or network service.

## Filesystem and handoff safety

- An Edit Session is an explicit local record, not permission to write source media. It never
  changes an original, `FileInstance`, source folder, EXIF, XMP, sidecar, rating, flag, note, or
  CaptureOS decision.
- Each Edit Session begins from one frozen immutable M9 manifest. Any source-copy handoff uses
  that manifest and the existing verified local-copy safeguards. The Edit Session adapter has no
  independent overwrite, move, rename, deletion, or finalization authority.
- A photographer chooses a local output root explicitly. Output observation validates canonical
  containment, rejects absolute/traversal relative paths and unsafe links, and does not scan an
  arbitrary machine location.
- Paths, filenames, extensions, output metadata, editor-generated labels, and file bytes are
  untrusted. They are never interpolated into shell commands or treated as trustworthy provenance
  merely because an external application produced them.

## No proprietary catalog mutation

- The generic M10 adapter must not open, create, edit, migrate, synchronize, or depend on a
  proprietary editor catalog/database. It must not automate an external editor UI or invoke a
  vendor command-line/control surface.
- CaptureOS must not write XMP, sidecars, source metadata, catalog state, flags, ratings, labels,
  or collections into any external application. Missing proprietary catalog data is an unavailable
  capability, not a reason to fabricate a match or integration claim.
- A future editor-specific adapter needs separate explicit approval and a dedicated security,
  privacy, licensing, and mutation review before it can be introduced.

## Conservative output matching and approval

- Matching evidence is local, versioned, and attached to the observed output. A `strong` or
  `possible` candidate remains evidence for review; only documented exact or manual evidence can
  create a final source link. A filename, timestamp, semantic/visual resemblance, face
  information, or an editor assumption alone cannot authoritatively link an output to a source.
- When evidence is incomplete or competing, preserve `unmatched` or `ambiguous` state. Never
  attach an output to a guessed source, invent an edit recipe, or use output matching as identity,
  emotion, aesthetic, or client inference.
- A human must explicitly approve an output version. Observation, matching, source Keep state,
  rating/star, Studio recommendation, or external-editor state never confers approval.
- Approval remains local catalog metadata. It does not auto-deliver, alter culling/Moment/Studio
  records, replace an original, or authorize deletion of any output or source.

## Data minimization and UI boundary

- Normal UI surfaces use bounded session/output pages and concise status. They do not expose raw
  fingerprints, full output-root listings, source paths, proprietary catalog data, or private
  matching internals by default.
- Client-facing reports and future exports omit source/output paths, internal IDs, notes, editor
  metadata, matching evidence, technical/AI scores, Studio recommendations, embeddings, and
  model data unless a separately approved private diagnostic workflow explicitly requires them.
- Derived-output records are local evidence, not a substitute for source identity, verified-copy
  provenance, human culling history, or a future deletion policy.
