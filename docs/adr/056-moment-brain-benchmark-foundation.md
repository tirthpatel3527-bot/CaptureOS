# ADR 056: MomentBrainBench generated-timeline benchmark foundation

## Decision

Milestone 7 adds **research/moment-brain-bench**, a deterministic no-download benchmark foundation. Its generated fixture compares time-only, semantic-only, and combined multi-signal boundary mechanics at 1,000, 10,000, 50,000, and 100,000 synthetic records.

The report records fixture/version provenance, generated-input and output-allocation **lower-bound** memory estimates, bounded chronological comparisons, product prebounded-tail incremental/page/prepared-lookup timing, boundary precision/recall/F1, segment purity, partition-based over-segmentation/under-segmentation, and explicit ungrouped-timestamp behavior. It writes JSON to stdout and creates a report only at an explicit unused output path.

The fixture contains only numeric synthetic records and small generated signatures. It includes no customer project, AI Test media, original path, preview, licensed image corpus, model weight, tokenizer, identity label, event label, or network request.

## Consequences

The benchmark can detect algorithmic scale regressions and demonstrate that a comparison mode is mechanically bounded without misrepresenting synthetic data as real visual understanding. Its semantic-style signatures do not measure an image/text model, label quality, object recognition, identity recognition, or whether a real-world photographic Moment is correct.

Real-world quality/effectiveness claims require a separately reviewed, legally usable, versioned fixture and explicit privacy/model/license review. Migration, foreign-key, project-isolation, offline, human-override, no-mutation, background-startup, and UI tests remain mandatory product validation outside this generated benchmark.
