# Deterministic ingest fixtures

These tiny files model the folder relationships used by Milestone 2 tests and
manual demos. They are deliberately not decodable media: file extensions only
exercise CaptureOS discovery, layout, copying, and BLAKE3 verification without
shipping real shoots or large binaries.

For a manual demo, select `camera-a` and `camera-b` as sources, then select two
new empty folders outside `fixtures/ingest` as the master and backup destinations.
Do not select any fixture source as a destination.

- `same-filenames-different-content` verifies source namespaces prevent a name collision.
- `identical-copy` models a safely reusable existing file.
- `conflict` models an existing destination with different bytes; it must never be overwritten.
- `partial-job` names a partial transfer scenario; tests create the actual partial in a temporary destination.
- `simulated-disconnect` documents a transient unavailable source/destination scenario; automated tests use temporary paths so no fixture is ever mutated.
