# MagicSearchBench

`MagicSearchBench` is a local, no-download benchmark foundation for the Milestone 6 retrieval
architecture. It creates deterministic synthetic vectors in memory and measures both an exact
small-project retrieval baseline and the production `magic_search::PersistentVectorIndex` at
1,000, 10,000, and 50,000 records. Persistent-index artifacts are written only below a temporary
benchmark directory and deleted when the process finishes. The suite does not load an image,
model, tokenizer, customer project, private AI Test photo, cloud API, or hosted vector database.

Run it from the repository root:

```sh
cargo run -p magic-search-bench -- --suite baseline
```

To write a report only to an explicit user-chosen path:

```sh
cargo run -p magic-search-bench -- --suite baseline --output /user/chosen/magic-search-bench.json
```

## What the baseline measures

The output records per-scale synthetic vector generation, actual persistent-index build/load/disk
size/candidate-lookup measurements, exact semantic-only/metadata-only/hybrid query timing,
logical vector payload size, estimated in-memory vector payload, and synthetic self-retrieval
Recall@1/@5/@10, MRR, and nDCG@10. Its synthetic queries are deliberately exact vectors from the
generated corpus, so those retrieval metrics verify only benchmark/index mechanics. They are
**not** a claim about real image-text relevance, photographic concepts, a model’s quality, or
product-scale performance.

The baseline reports actual temporary `PersistentVectorIndex` artifact size, but it does not
report model image/text embedding latency because it intentionally runs no model or tokenizer.
That value is explicitly marked unmeasured rather than invented. An admitted local model pack can
add a separate, versioned model-inference benchmark later.

## Recorded generated-data baseline

The following one-off measurement was recorded on 2026-08-10 with the command above in the Rust
debug profile on macOS 26.3.1 / arm64. It is a regression reference for this repository state, not
a hardware guarantee or semantic-quality result. No model, tokenizer, photograph, query text, or
customer project participated.

| Generated vectors | Persistent build | Persistent artifact | Persistent candidate mode | Median candidate lookup | Mean candidates | Exact full-rank semantic median |
| ---: | ---: | ---: | --- | ---: | ---: | ---: |
| 1,000 | 79 ms | 67,221 B | all stored vectors | 0.001 ms | 1,000.0 | 1.200 ms |
| 10,000 | 744 ms | 655,757 B | approximate asset IDs | 7.318 ms | 17.0 | 13.254 ms |
| 50,000 | 3,686 ms | 2,812,028 B | approximate asset IDs | 7.376 ms | 81.8 | 69.830 ms |

Persistent candidate lookup measures only `PersistentVectorIndex::candidates`; it does not fetch
vectors from SQLite or run hybrid ranking. The exact full-rank figure measures the generated
in-memory baseline. Treat the numbers as separate local mechanics measurements, not directly
comparable end-to-end product latency and never as proof that a real text/image query is correct.

## Ground-truth foundation

`ground-truth/v1/` and `schemas/v1/` define versioned contracts for:

- `TextImageQueryGroundTruth`: query ID/text, dataset/version, relevant MediaAsset references,
  optional graded relevance, and notes;
- `HybridQueryGroundTruth`: query/query-plan reference, deterministic predicates, expected asset
  references, and notes; and
- `BenchmarkDatasetManifest`: source, license, checksum, derivation, and privacy review metadata.

The checked-in fixtures are empty schema scaffolds using generated data only. A public image
fixture or model evaluation may be added only after it documents its source, exact license,
checksum, derivation, annotations, model/provider revision, and privacy review. Never commit a
customer shoot, the local AI Test project, private wedding photographs, a trained model binary, or
identity/demographic data.

## Interpreting future results

For a vetted dataset and admitted model pack, report the dataset/version, model ID/version,
preprocessing/tokenizer version, index implementation/version, platform/hardware, corpus size,
and all terminal-indexing outcomes. Compare semantic-only, deterministic-only, and hybrid
retrieval where meaningful. Measure Recall@1/@5/@10, MRR, graded nDCG, query embedding latency,
image embedding latency, vector lookup latency, hybrid-ranking latency, index build rate, and
actual index memory/disk size. Do not overstate a tiny benchmark or turn a semantic score into an
object-detection or identity claim.
