# Magic Search architecture

Magic Search is CaptureOS’s current-project visual retrieval layer. It is local-first, private, non-destructive, and photo-only in Milestone 6. It combines a shared image/text embedding space with existing deterministic evidence; it is not a chatbot, an object detector, an identity system, or a replacement for human culling.

## Query flow

```mermaid
flowchart LR
  Q["Current-project query or Find Similar reference"] --> P["Query planner"]
  P --> M["Metadata / decision predicates"]
  P --> E["Optional local text or image embedding"]
  M --> C["Project-scoped candidate set"]
  E --> V["Local rebuildable vector index"]
  V --> C
  C --> H["Hybrid ranker"]
  H --> X["Evidence-based result explanation"]
  X --> G["Existing paged media grid and Inspector"]
  A["CaptureOS-managed analysis preview"] --> I["Local image embedding provider"]
  I --> D[("SQLite embedding records")]
  D --> V
  R["Local model registry"] --> I
```

`SearchService` owns project isolation, result pagination, history persistence, and the non-destructive search session. `QueryPlanner` recognizes only confident structured terms and leaves the remainder as semantic text. `SemanticEmbeddingProvider`, `VectorIndex`, `MetadataSearchProvider`, `HybridRanker`, and `SearchExplanationProvider` are replaceable adapters. The React UI does not bind directly to a particular model, vector-index implementation, or hard-coded vocabulary.

## Evidence and query planning

The planner can extract supported high-confidence predicates such as face count (`1 face`, `2 faces`), rating (`5 star`, `4+ stars`), human decision (`kept`, `rejected`, `review`), technical evidence (`sharp`, `blurry`, `technical issue`), and camera model (`camera SLT-A58`). The non-structured remainder is eligible for a semantic text embedding when a local provider is available. Unknown language is not assigned a fabricated intent.

The ranker uses only the signals the plan actually selected: vector similarity, deterministic filters, or both. It never presents similarity as a probability or confidence. A semantic-only explanation says it was ranked by **local image/text embedding similarity**; Find Similar says **local visual embedding similarity**. It may additionally list `Faces: 2`, `Rating: 5`, or `Decision: Keep` only when that stored evidence was applied. It must not invent object labels, locations, person identities, or detections from an embedding score.

Search is current-project only. `Find Similar` uses a selected asset’s image embedding as the query vector and returns semantic visual neighbors in the same project. It remains separate from a `SimilarityGroup`: Similar Sets are conservative related-frame/burst groups with their own provenance and are never created, altered, or deleted by semantic retrieval.

## Local model contract

Magic Search’s candidate model is product ID `google-siglip-base-patch16-224`, sourced from [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224), through an opt-in, manually installed static ONNX pack. CaptureOS does not bundle, auto-download, or silently convert model weights. The upstream model card labels its source code/weights Apache-2.0; the exact immutable model revision, source-weight provenance, conversion provenance, per-file checksum, license reference, and tokenizer must still be recorded and checked for every installed pack. See [the local model registry](local-model-registry.md) and ADR 044.

The provider contract expects separate static image and text ONNX encoders plus the matching tokenizer, producing 768-dimensional vectors in one shared space. The managed analysis preview is oriented then resized to the admitted provider’s recorded raster; the current candidate uses 224×224 RGB, `/255`, and mean/std `0.5`. Text is Unicode-normalized/lowercased, tokenized with the admitted pack’s tokenizer, and padded/truncated to 64 tokens. These settings are versioned. Before a provider is marked available, the pack must checksum-verify a fixed non-224 raw RGB24 reference raster and reproduce both reviewed normalized image and text reference vectors within a CaptureOS-controlled tolerance. CPU inference uses the local `tract-onnx` runtime; no browser, Python interpreter, pickle payload, model script, GPU service, or network endpoint is accepted.

When no admitted pack is installed, Magic Search retains deterministic metadata/technical filters and clearly reports **Semantic model not installed**. An invalid or altered pack reports that the semantic model is unavailable rather than pretending it is absent. Neither state substitutes filename matching, tags, or arbitrary vectors as semantic results.

After its first full admission in a desktop process, CaptureOS may reuse the validated provider for
status, indexing, text search, and Find Similar. Every reuse checks a closed lightweight pack
stamp (manifest digest, expected entries, and filesystem identity metadata); an observed removal,
replacement, unexpected entry, or symlink clears the cache and requires full checksum/tokenizer/
reference-inference admission again. This avoids reloading the complete local pack for every query
without treating a cache hit as a new cryptographic verification of unchanged bytes.

## Embeddings and index lifecycle

An embedding belongs to a logical `MediaAsset`, not to each physical `FileInstance`. Its cache/provenance identity includes MediaAsset ID, managed-preview input fingerprint, model ID, model version, preprocessing/embedding version, generated timestamp, terminal state, and error where applicable. A source or model/preprocessing change makes old records stale rather than silently comparing incompatible vectors. Existing READY embeddings remain useful when a source disk is offline; if no sufficient managed preview or source is available, the item becomes `NEEDS_ORIGINAL` for semantic indexing without blocking other assets.

The durable SQLite embedding record is the source of truth. A project-scoped vector index is a versioned, derived artifact rebuilt from compatible READY embeddings. It is not a source of project identity, Catalog membership, CaptureGraph facts, or human decisions. The current `captureos-m6-lsh.v1` index is exact through 4,096 vectors. Above that threshold it stores four deterministic 16-bit sign-LSH tables, builds the shared hash planes once per rebuild, restores them once when the index loads, probes each exact bucket plus its one-bit neighbors, bounds persistence fetches to 8,192 IDs, then re-ranks only those local candidates. It never substitutes a hidden full-catalog vector scan for large projects.

This is a bounded retrieval accelerator, not a quality guarantee. The generated 768-dimensional benchmark currently shows exact-self retention but low exact-neighbor candidate coverage at larger scales; see MagicSearchBench before treating Find Similar or text retrieval as recall-validated for a real photo corpus.

Embedding work has its own durable Magic Search job while using the same local `eco`, `balanced`, and `fast` worker limits and pause/resume behavior as Capture Intelligence. `READY`, `UNSUPPORTED`, `CORRUPT`, `NEEDS_ORIGINAL`, and `FAILED` are per-asset terminal outcomes, so one bad input cannot hang the queue.

## Data and privacy boundary

Customer originals are read only when no reusable CaptureOS-managed analysis preview exists; semantic inference itself receives only a canonicalized managed preview path. Text queries, embeddings, model metadata, search history, index artifacts, and result explanations stay on the local computer. They are potentially sensitive derived data and are never uploaded, telemetered, exported automatically, or used for training.

Magic Search performs no person recognition, cross-project identity matching, demographic inference, face clustering, creative judgment, emotional scoring, video semantic search, or audio semantic search. Face counts may be used only as already persisted anonymous technical evidence. Rebuilding a semantic index preserves catalog records, preview artifacts, Capture Intelligence evidence, Similar Sets, and human decisions. Milestone 6 exposes rebuild through the local indexing control; it does not expose a clear-index operation.

## Moment Brain interaction

Milestone 7 may consume only a compatible current image embedding as one bounded local continuity, representative, or conservative label-candidate signal. It does not change the M6 provider, embedding identity, index, history, Find Similar behavior, or query planner. A missing/incompatible embedding is an explicit unavailable component in Moment analysis, never a fake semantic label or forced boundary.

Moment structural grouping is distinct from both Magic Search retrieval and M4 Similar Sets. A Moment analysis never writes an M6 search-history row, creates a vector-index neighbor edge, changes a Find Similar result, or creates/rewrites Similar Set membership. Magic Search may be opened in an explicitly validated Moment membership scope, but current-project ownership and normal hybrid-query evidence rules still apply. A photographer-created checklist phrase is only an additional local text candidate for conservative Moment labeling; it is not a detected event or an automatic coverage completion.


## Evaluation

[MagicSearchBench](../../research/magic-search-bench/README.md) holds generated-fixture scale measurements and versioned text/image ground-truth schemas. It measures local query/index behavior at 1k, 10k, and 50k synthetic embedding records without claiming semantic quality. Semantic Recall@K, MRR, nDCG, embedding latency, and hybrid effectiveness require an admitted model pack and a separately licensed, versioned public or generated benchmark dataset.
