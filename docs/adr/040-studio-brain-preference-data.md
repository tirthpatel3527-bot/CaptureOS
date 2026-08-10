# ADR 040: Studio Brain preference data foundation

## Decision

When a photographer chooses a human representative for a Similar Set, preserve the AI representative and record a bounded `PreferenceExample` with project/group-relative IDs, alternatives, technical/recommendation snapshots, decision context, and time.

## Consequences

CaptureOS gains clean local preference evidence without claiming it has learned the photographer. Examples exclude media bytes, face crops, original paths, identity labels, and remote upload. No training, fine-tuning, or cloud service is introduced.
