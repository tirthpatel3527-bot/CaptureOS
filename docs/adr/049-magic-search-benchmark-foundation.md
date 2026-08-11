# ADR 049: Magic Search benchmark foundation

## Decision

Milestone 6 adds `research/magic-search-bench` as a reproducible, no-download benchmark
foundation. Its generated fixture suites measure local vector/index behavior at 1,000, 10,000,
and 50,000 records and record query/index timing plus logical vector payload size. `baseline`
remains 64-dimensional for historical comparison; `provider-768` uses generated vectors at the
candidate SigLIP width. Neither suite loads a model, tokenizer, customer project, private AI Test
photos, or a third-party image dataset.

The benchmark includes versioned schemas for text/image relevance and hybrid-query ground truth.
They support Recall@1/@5/@10, MRR, graded nDCG, query latency, embedding latency, index build
rate, and index memory/disk measurements once a legally usable fixture set and admitted model pack
exist. Synthetic self-retrieval is explicitly a scale/correctness check, not an assertion of real
semantic quality. The persistent-index report also separates approximate candidate self/top-K
coverage and candidate-vector re-rank timing from exact full-corpus evaluation, so it cannot
disguise a full scan as ANN latency.

## Consequences

CaptureOS can compare local semantic-only, deterministic-only, and hybrid retrieval without
turning an unlicensed or private image collection into a repository fixture. Results report the
machine and dataset/model/version used; tiny or empty scaffolds do not justify a quality claim.

Future public evaluation data must record source, license, checksum, derivation, privacy review,
and ground-truth version before it is used in a release benchmark. Benchmark code must never
download a model or call a service as an implicit development dependency.
