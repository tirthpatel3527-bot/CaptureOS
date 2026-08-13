# ADR 069: Edit History and Provenance Review I

## Context

Milestone 10 created the append-only `edit_session_versions` records (ADR 067) and the
Edit Session / derived-output provenance boundary (ADR 066, 068). M10 persists output
versions, their source/match provenance, review state, and availability, but provides no
dedicated, deterministic, read-only way for a photographer to understand how an observed
external edit output evolved across versions, where each version came from, and whether its
file is currently present.

M11 turns that already-persisted data into a safe, local-first, read-only history experience.
It must not edit, render, restore, overwrite, write back, or mutate any file or record. The
existing M10 schema (`edit_session_versions` and related provenance/approval tables) must be
sufficient; no migration or schema change is introduced unless a planning proof shows the
read-only scope cannot be served.

## Decision

M11 adds a read-only history view over existing M10 edit-session/version data:

- A new persistence **read model** queries `edit_session_versions` (and joined
  provenance/approval records) scoped strictly to one `edit_session_id` and one output
  identity. Cross-session leakage is prevented by always filtering on the owning session id and
  the requesting user's project scope.
- Version history is ordered deterministically by the existing persisted `version` integer and
  `created_at` timestamp (tie-broken by `id`). The latest recorded version is designated by
  stable, repeatable comparison, never by mutable state.
- The desktop UI presents a chronological timeline/details surface per output: version list,
  latest marker, provenance (session, source-link evidence state, timestamps, fingerprint
  availability), review/approval state, and online/offline availability derived from the
  existing recorded availability flag — no live filesystem scan required for history.
- Append-only history is preserved when an output file disappears (offline) or reappears
  (online). Availability changes update only the current availability projection; they never
  insert, replace, or delete a historical version. Reappearing files do not synthesize new
  versions.
- Only provenance/metadata already safely persisted by M10 is displayed. No creative edit
  recipe, visual comparison, or media rendering is produced.
- All reads are deterministic and side-effect free. No M0–M10 behavior, schema, or write path
  is altered.

## User workflow

1. Open an Edit Session in the desktop workspace.
2. Select a derived output.
3. Open its Version History.
4. View a deterministic, chronological list: each version with its recorded provenance, match
   state, review/approval state, and online/offline availability.
5. The latest recorded version is clearly marked. Older versions remain visible and immutable.
6. If the file is offline, the history remains intact and the availability state reflects the
   last recorded value; no false "new version" is created on reconnect.

## Data / read model

Uses existing M10 records only:

- `edit_session_versions(id, edit_session_id, version, configuration_json, created_at, ...)`
- Joined output provenance: source-link evidence state (`exact`, `manual`, `strong`,
  `possible`, `unmatched`, `ambiguous`), timestamps, fingerprint availability.
- Joined review/approval records (explicit human approval state per version).
- Availability projection from the existing recorded availability flag.

No new tables, columns, or migrations unless planning proves inadequacy (stop condition below).

## Security and privacy boundaries

- Inherits the M10 security boundary (docs/security/edit-sessions.md) and AGENTS.md M10 rules.
- Read-only: no write, restore, overwrite, delete, or file mutation.
- Queries are scoped to the owning edit session and project; cross-session record leakage is
  impossible by construction (session id is a required filter).
- Absolute paths, output roots, fingerprints, internal IDs, notes, and matching internals are
  redacted/minimized per the existing M10 privacy policy; normal UI surfaces do not expose raw
  fingerprints or full filesystem paths.
- Operations are local-first and offline-capable; no network, cloud, account, or telemetry.

## Acceptance criteria

1. A user can open an output and view its deterministic chronological version history from
   existing M10 data.
2. The latest recorded version is clearly designated and stable across repeated reads.
3. Older versions are preserved and never replaced or deleted.
4. Queries for one session/output never return records from another session or project.
5. Offline outputs retain complete history; reappearing files do not create false versions.
6. Only already-persisted provenance/review/availability is shown; no fabrication.
7. No Rust/TypeScript schema/migration change is required by the approved scope.
8. All reads are side-effect free and deterministic.
9. M0–M10 behavior, including writes, remains unchanged.
10. Focused tests pass for persistence read model, command/API, desktop UI, and security/
    failure cases.

## Test strategy

- Persistence: scoped query returns only owning-session rows; deterministic ordering; latest
  designation stable; no cross-session leakage; offline/reappear does not mutate history.
- Core/command: API returns read-only projections; repeated calls identical; no writes occur.
- Desktop: timeline renders version list, latest marker, provenance, review state, availability;
  offline state handled gracefully.
- Security/failure: redaction of paths/fingerprints/IDs; unavailable data shown honestly;
  malformed/untrusted output metadata never trusted as provenance; no proprietary catalog
  access.

## Performance expectations

- History retrieval uses indexed `edit_session_id` + `version` lookups; bounded to the selected
  output. No full output-root scan, no full catalog load in the frontend. Comparable to existing
  M10 bounded projections; no regression in open/switch latency.

## Explicit exclusions

No creative image/video editing; no rendering or visual media comparison; no restore,
overwrite, write-back, or file mutation; no proprietary Lightroom/Capture One/NLE catalog
integration; no cloud, sync, mobile, teams, billing, or client galleries; no automatic approval
or delivery; no schema change unless planning proves the existing M10 schema cannot support the
approved read-only scope; no work beyond M11.

## Rollback / stop conditions

- If the existing M10 schema provably cannot serve the read-only scope, STOP and report; do not
  silently add a migration. Require explicit re-approval for any schema change.
- If any implementation path would write, mutate, restore, or delete source/output/history
  records, STOP.
- If history reads leak across sessions/projects or expose raw paths/fingerprints/IDs against
  the M10 privacy policy, STOP.
- If scope expands beyond the approved exclusions (editing, cloud, delivery, etc.), STOP.

## Consequences

CaptureOS can show a restrained, deterministic history of how an externally produced output
evolved, with honest provenance, review, and availability — without becoming an editor, a
restore tool, or a cloud service. The model keeps version history append-only and offline-safe,
and leaves all M0–M10 write behavior intact.
