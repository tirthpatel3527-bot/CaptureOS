# CaptureIntelligenceBench

Run the deterministic baseline without downloading models or using customer media:

```sh
cargo run -p capture-intelligence-bench -- --suite baseline
```

The suite generates raster patterns in memory, validates the versioned annotation scaffolding, and reports deterministic-analysis throughput, bounded candidate-grouping time, cache-key reuse time, and peak memory when the platform exposes it. On macOS it also generates a 3000×2000 JPEG in a temporary directory and measures first dedicated analysis-preview generation plus cache-hit reuse; the source bytes are checked unchanged. It does not claim face or eye metrics until a properly licensed, test-safe fixture and a vetted local provider are added.

`--output /user/chosen/report.json` writes an optional report only to the explicit path supplied by the user.

## Ground-truth scaffold

`ground-truth/v1/` and `schemas/v1/` define separate, versioned records for duplicate pairs, similarity groups, sharpness preferences, face presence, eye state, candidate preferences, and human-review agreement. The last supports local future measurements such as AI top candidate chosen by a human, AI top-three containing a human representative, technical-issue disagreement rate, and review time per set. It is deliberately scaffolding rather than a performance or accuracy claim.

The baseline is designed to expose naive all-pairs grouping costs: it includes a generated descriptor catalog larger than 1,000 records and measures bounded candidate generation. It does not download datasets, submit images to a service, or make face/eye accuracy claims without licensed fixtures.
