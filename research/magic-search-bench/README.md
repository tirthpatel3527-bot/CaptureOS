# MagicSearchBench

`MagicSearchBench` is a local, no-download benchmark foundation for the Milestone 6 retrieval
architecture. Its generated-data suites create deterministic vectors in memory and measure both
an exact small-project retrieval baseline and the production `magic_search::PersistentVectorIndex`
at 1,000, 10,000, and 50,000 records. Persistent-index artifacts are written only below a
temporary benchmark directory and deleted when the process finishes. Those generated suites do
not load an image, model, tokenizer, customer project, private AI Test photo, cloud API, or hosted
vector database.

Run it from the repository root:

```sh
cargo run -p magic-search-bench -- --suite baseline
```

`baseline` intentionally stays 64-dimensional so its historical generated-data results remain
comparable. It is not a proxy for the candidate semantic provider's 768-dimensional work.

Run the separate provider-dimension scale suite before making a production-scale claim about the
current candidate embedding space:

```sh
cargo run -p magic-search-bench -- --suite provider-768
```

`provider-768` uses deterministic **generated** 768-dimensional vectors only. It matches the
current candidate SigLIP provider's vector width, but it does not load, validate, or time an ONNX
model pack.

After the explicit local model-pack installation workflow has completed, benchmark the controlled
installed SigLIP pack itself with a supplied model-root path:

```sh
cargo run --release -p magic-search-bench -- --suite local-siglip --model-root /path/to/semantic-models
```

`local-siglip` is deliberately separate from the scale suites. It discovers and fully admits the
fixed, already-installed pack below the supplied CaptureOS-owned model root, then measures that
cold admission/load and ten warm local text and image embedding calls. It uses a generated RGB
calibration pattern and fixed synthetic text, never an AI Test or customer image. The command has
no download, install, catalog, project, preview-cache, cloud, or network path. It fails rather
than reporting a fabricated result if the approved pack is absent or validation fails.

The report identifies the admitted model/runtime, source metadata, and verified 768-dimensional
embedding space. Its cold value includes checksum/manifest/tokenizer admission, ONNX graph load,
and reference inference; it is not merely graph deserialization. Its warm values exclude preview
decode, SQLite, vector-index lookup, hybrid ranking, UI rendering, and retrieval quality. They are
local provider timings on the current machine, not semantic-relevance, object-detection, identity,
or production-scale guarantees.

## Explicit local-project validation

`m6_project_validate` is a separate, deliberately narrow developer command for running the
already-approved M6 production path against an explicitly named local project. It does not
discover a catalog, choose a project by a fuzzy name, download/install a model, call a network
service, or write a report file. Supply every location and every query yourself:

```sh
cargo run --release -p magic-search-bench --bin m6_project_validate -- \
  --catalog "$APP_DATA/captureos.sqlite3" \
  --app-data-root "$APP_DATA" \
  --project "Exact project name" \
  --index \
  --query "<first local query>" \
  --query "<second local query>" \
  --find-similar-asset "<MediaAsset UUID>"
```

Use an actual CaptureOS application-data directory for `$APP_DATA`; the desktop app uses the
children `semantic-models`, `semantic-index`, and `preview-cache` beneath it. Do not run this
command while the desktop application is actively writing or indexing the same catalog.

The command writes a redacted JSON report to stdout. Each result contains only its MediaAsset ID,
filename, optional local semantic ranking signal, explanation, and evidence. It never emits raw
vectors, preview URLs, selected roots, relative paths, or original-media paths. The local ranking
signal is not a confidence, object-detection result, identity claim, or human culling decision.
It also records the actual elapsed time for each explicit index, query, and Find Similar product
operation. A repeated `--index` may reuse compatible durable embeddings, so its elapsed time is
not a substitute for per-image model inference timing.
Its `mutationScope` reports whether the explicit index operation completed and whether local M6
search history/model-admission provenance were written.

`--index` explicitly invokes the existing M6 local embedding and rebuildable-index pipeline for
the exact project; it can read an available original only when the existing pipeline must create a
CaptureOS-managed preview, and it never writes beside or changes the original. Every supplied
`--query` uses the existing M6 query service and records that query only in the project's local
Magic Search history. A successful local pack admission can also upsert its M6 local-model
provenance record. These are M6 derived-data writes; the tool does not call any M0–M5 decision,
rating, note, representative, Similar Set, or Capture Intelligence mutation API.
`--find-similar-asset` is read-only retrieval and does not create or alter Similar Sets. A missing
or invalid local pack is reported as unavailable; the tool never downloads or pretends semantic
retrieval succeeded.

To write a report only to an explicit user-chosen path:

```sh
cargo run -p magic-search-bench -- --suite provider-768 --output /user/chosen/magic-search-bench.json
```

## What the suites measure

Both suites record per-scale synthetic vector generation, actual persistent-index build/load/disk
size/candidate-lookup measurements, exact semantic-only/metadata-only/hybrid query timing,
logical vector payload size, estimated in-memory vector payload, and synthetic self-retrieval
Recall@1/@5/@10, MRR, and nDCG@10.

For the persistent index, the report additionally measures:

- whether the candidate IDs retain the generated query's own asset;
- candidate-set Recall@1/@5/@10 against the exact generated-vector top K; and
- in-memory vector fetch/re-ranking after candidate selection.

The candidate-retrieval timing deliberately excludes both the separate candidate lookup timing
and SQLite vector fetch. The exact full-corpus rank used to score candidate coverage is evaluation
only and is likewise excluded, so the report never disguises an exact scan as ANN latency.

Synthetic queries are deliberately exact vectors from the generated corpus. These metrics verify
index/candidate mechanics only. They are **not** a claim about real image-text relevance,
photographic concepts, model quality, SQLite-backed end-to-end latency, hybrid product behavior,
or product-scale performance.

The generated suites report actual temporary `PersistentVectorIndex` artifact size, but they do
not report model image/text embedding latency because they intentionally run no model or tokenizer.
That value is explicitly marked unmeasured rather than invented. `local-siglip` is the separate,
versioned installed-model runtime measurement; it does not replace the generated scale/index
measurements or establish retrieval quality.

## Historical generated-data reference

The following one-off 64-dimensional `baseline` measurement was recorded on 2026-08-10 in the
Rust debug profile on macOS 26.3.1 / arm64. It remains a historical regression reference, not a
hardware guarantee, provider-dimension result, or semantic-quality result. No model, tokenizer,
photograph, query text, or customer project participated. Re-run the relevant suite on the target
machine; do not substitute this table for a current measurement.

| Generated vectors | Persistent build | Persistent artifact | Persistent candidate mode | Median candidate lookup | Mean candidates | Exact full-rank semantic median |
| ---: | ---: | ---: | --- | ---: | ---: | ---: |
| 1,000 | 79 ms | 67,221 B | all stored vectors | 0.001 ms | 1,000.0 | 1.200 ms |
| 10,000 | 744 ms | 655,757 B | approximate asset IDs | 7.318 ms | 17.0 | 13.254 ms |
| 50,000 | 3,686 ms | 2,812,028 B | approximate asset IDs | 7.376 ms | 81.8 | 69.830 ms |

Persistent candidate lookup measures only `PersistentVectorIndex::candidates`; it does not fetch
vectors from SQLite or run hybrid ranking. The exact full-rank figure measures the generated
in-memory baseline. Current reports additionally expose candidate-set coverage and an in-memory
candidate-vector re-rank, each clearly labelled with its included work. Treat all figures as
separate local mechanics measurements, not directly comparable end-to-end product latency and
never as proof that a real text/image query is correct.

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
retrieval where meaningful. Measure candidate-set recall, Recall@1/@5/@10, MRR, graded nDCG,
query embedding latency, image embedding latency, vector lookup latency, SQLite vector-fetch
latency, hybrid-ranking latency, index build rate, and actual index memory/disk size. Do not
overstate a generated benchmark or turn a semantic score into an object-detection or identity
claim.
