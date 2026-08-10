# ADR 011: Read-only Index Mode

## Decision

Milestone 1 adds a Rust-owned local indexing pipeline for user-selected folders. It records `IndexRoot`, `StorageVolume`, `MediaAsset`, `FileInstance`, jobs, and non-fatal issues in SQLite. Source media is accessed only through metadata calls and read-only handles.

## Consequences

The frontend uses a native folder chooser and receives job progress through the Tauri bridge; it does not recursively scan folders. `StorageVolume` models a mounted filesystem and `IndexRoot` models a selected project folder, so several project roots can safely share a volume. Symlinks are skipped, file-level errors do not crash a job, and reindexing marks missing physical instances unavailable instead of deleting them. Indexing additional folders is additive within a project. No ingest, copy, backup, AI, cloud, thumbnails, or proxy work is included.
