use magic_search::{IndexCandidates, PersistentVectorIndex};
use serde::Serialize;
use serde_json::Value;
use std::{env, fs, mem, path::PathBuf, time::Instant};
use tempfile::tempdir;

const EMBEDDING_DIMENSION: usize = 64;
const TOP_K: usize = 10;
const CORPUS_SIZES: [usize; 3] = [1_000, 10_000, 50_000];

#[derive(Debug, Clone)]
struct EmbeddingRecord {
    asset_id: u32,
    vector: Vec<f32>,
    metadata: SyntheticMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SyntheticMetadata {
    face_count: u8,
    rating: u8,
    decision: SyntheticDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyntheticDecision {
    Keep,
    Review,
    Reject,
}

#[derive(Debug, Clone)]
struct BenchmarkQuery {
    vector: Vec<f32>,
    expected_asset_id: u32,
    metadata: SyntheticMetadata,
}

#[derive(Debug, Clone, Copy)]
enum SearchMode {
    SemanticOnly,
    MetadataOnly,
    Hybrid,
}

impl SearchMode {
    fn name(self) -> &'static str {
        match self {
            Self::SemanticOnly => "semantic_only_exact_baseline",
            Self::MetadataOnly => "metadata_only_synthetic_predicates",
            Self::Hybrid => "hybrid_exact_baseline",
        }
    }
}

#[derive(Debug)]
struct SearchHit {
    asset_id: u32,
    score: f32,
}

#[derive(Debug)]
struct ExactVectorIndex {
    records: Vec<EmbeddingRecord>,
}

impl ExactVectorIndex {
    fn build(mut records: Vec<EmbeddingRecord>) -> Self {
        records.sort_by_key(|record| record.asset_id);
        Self { records }
    }

    fn search(&self, query: &BenchmarkQuery, mode: SearchMode, top_k: usize) -> Vec<SearchHit> {
        let mut hits = self
            .records
            .iter()
            .filter(|record| {
                matches!(mode, SearchMode::SemanticOnly)
                    || matches_metadata_predicates(record.metadata, query.metadata)
            })
            .map(|record| SearchHit {
                asset_id: record.asset_id,
                score: score_record(record, query, mode),
            })
            .collect::<Vec<_>>();

        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.asset_id.cmp(&right.asset_id))
        });
        hits.truncate(top_k);
        hits
    }
}

fn matches_metadata_predicates(record: SyntheticMetadata, query: SyntheticMetadata) -> bool {
    record.face_count == query.face_count
        && record.rating >= query.rating
        && record.decision == query.decision
}

fn score_record(record: &EmbeddingRecord, query: &BenchmarkQuery, mode: SearchMode) -> f32 {
    let semantic_score = dot_product(&record.vector, &query.vector);
    match mode {
        SearchMode::SemanticOnly => semantic_score,
        SearchMode::MetadataOnly => metadata_score(record.metadata),
        SearchMode::Hybrid => semantic_score + metadata_score(record.metadata) * 0.001,
    }
}

fn metadata_score(metadata: SyntheticMetadata) -> f32 {
    let decision_score = match metadata.decision {
        SyntheticDecision::Keep => 3.0,
        SyntheticDecision::Review => 2.0,
        SyntheticDecision::Reject => 1.0,
    };
    decision_score + f32::from(metadata.rating) / 10.0 + f32::from(metadata.face_count) / 100.0
}

fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    debug_assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn synthetic_metadata(asset_id: u32) -> SyntheticMetadata {
    let decision = match asset_id % 3 {
        0 => SyntheticDecision::Keep,
        1 => SyntheticDecision::Review,
        _ => SyntheticDecision::Reject,
    };
    SyntheticMetadata {
        face_count: (asset_id % 4) as u8,
        rating: ((asset_id % 5) + 1) as u8,
        decision,
    }
}

fn synthetic_vector(asset_id: u32, dimension: usize) -> Vec<f32> {
    let mut state = u64::from(asset_id)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0xbf58_476d_1ce4_e5b9);
    let mut vector = (0..dimension)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let fraction = ((state >> 40) as f32) / ((1_u32 << 24) as f32);
            fraction * 2.0 - 1.0
        })
        .collect::<Vec<_>>();
    normalize(&mut vector);
    vector
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    assert!(
        norm.is_finite() && norm > 0.0,
        "synthetic vector norm is valid"
    );
    for value in vector {
        *value /= norm;
    }
}

fn generate_corpus(record_count: usize) -> Vec<EmbeddingRecord> {
    (0..record_count)
        .map(|index| {
            let asset_id = index as u32;
            EmbeddingRecord {
                asset_id,
                vector: synthetic_vector(asset_id, EMBEDDING_DIMENSION),
                metadata: synthetic_metadata(asset_id),
            }
        })
        .collect()
}

fn benchmark_queries(records: &[EmbeddingRecord]) -> Vec<BenchmarkQuery> {
    let last = records.len() - 1;
    let positions = [0, records.len() / 4, records.len() / 2, last * 3 / 4, last];
    positions
        .into_iter()
        .map(|position| {
            let record = &records[position];
            BenchmarkQuery {
                vector: record.vector.clone(),
                expected_asset_id: record.asset_id,
                metadata: record.metadata,
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    suite: String,
    benchmark_kind: &'static str,
    embedding_dimension: usize,
    ground_truth_status: &'static str,
    model_embedding_latency: UnmeasuredMetric,
    scale_runs: Vec<ScaleRun>,
    notes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnmeasuredMetric {
    status: &'static str,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScaleRun {
    record_count: usize,
    embedding_dimension: usize,
    synthetic_vector_generation_ms: u128,
    estimated_vector_payload_bytes: u64,
    estimated_in_memory_payload_bytes: u64,
    exact_baseline: ExactBaselineReport,
    persistent_index: PersistentIndexReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExactBaselineReport {
    implementation: &'static str,
    setup_ms: u128,
    query_modes: Vec<QueryModeReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistentIndexReport {
    implementation: &'static str,
    vector_count: usize,
    approximate: bool,
    build_ms: u128,
    build_records_per_second: f64,
    persisted_index_bytes: u64,
    load_ms: u128,
    candidate_lookup: CandidateLookupReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateLookupReport {
    query_count: usize,
    candidate_mode: &'static str,
    latency: LatencyReport,
    minimum_candidates: usize,
    maximum_candidates: usize,
    mean_candidates: f64,
    includes_vector_fetch_or_hybrid_rank: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryModeReport {
    mode: &'static str,
    query_count: usize,
    top_k: usize,
    latency: LatencyReport,
    synthetic_self_retrieval_metrics: SyntheticRetrievalMetrics,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LatencyReport {
    total_ms: f64,
    median_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyntheticRetrievalMetrics {
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
    mean_reciprocal_rank: f64,
    ndcg_at_10: f64,
    evaluation_scope: &'static str,
}

fn run_baseline() -> BenchmarkReport {
    let scale_runs = CORPUS_SIZES.into_iter().map(run_scale).collect();
    BenchmarkReport {
        suite: "baseline".to_owned(),
        benchmark_kind: "generated local retrieval/index benchmark; not a semantic-quality evaluation",
        embedding_dimension: EMBEDDING_DIMENSION,
        ground_truth_status: "versioned schema scaffold validated; no licensed text/image relevance fixture is bundled",
        model_embedding_latency: UnmeasuredMetric {
            status: "not_measured",
            reason: "The baseline intentionally runs no model or tokenizer; it generates deterministic vectors in memory.",
        },
        scale_runs,
        notes: vec![
            "No customer media, model weights, tokenizer, cloud API, network request, or hosted vector database is used.",
            "Synthetic self-retrieval confirms benchmark mechanics only; it does not measure image-text relevance or object understanding.",
            "PersistentVectorIndex build, disk, and candidate lookup measurements are generated-data mechanics, not a claim of semantic result quality.",
        ],
    }
}

fn run_scale(record_count: usize) -> ScaleRun {
    let generation_started = Instant::now();
    let corpus = generate_corpus(record_count);
    let generation_elapsed = generation_started.elapsed();

    let queries = benchmark_queries(&corpus);
    let persistent_index = measure_persistent_index(&corpus, &queries);

    let index_started = Instant::now();
    let index = ExactVectorIndex::build(corpus);
    let index_elapsed = index_started.elapsed();
    let query_modes = [
        SearchMode::SemanticOnly,
        SearchMode::MetadataOnly,
        SearchMode::Hybrid,
    ]
    .into_iter()
    .map(|mode| benchmark_mode(&index, &queries, mode))
    .collect();

    let vector_payload_bytes = (record_count * EMBEDDING_DIMENSION * mem::size_of::<f32>()) as u64;
    let estimated_in_memory_payload_bytes =
        vector_payload_bytes + (record_count * mem::size_of::<EmbeddingRecord>()) as u64;
    ScaleRun {
        record_count,
        embedding_dimension: EMBEDDING_DIMENSION,
        synthetic_vector_generation_ms: generation_elapsed.as_millis(),
        estimated_vector_payload_bytes: vector_payload_bytes,
        estimated_in_memory_payload_bytes,
        exact_baseline: ExactBaselineReport {
            implementation: "in_memory_exact_scan_baseline",
            setup_ms: index_elapsed.as_millis(),
            query_modes,
        },
        persistent_index,
    }
}

fn measure_persistent_index(
    corpus: &[EmbeddingRecord],
    queries: &[BenchmarkQuery],
) -> PersistentIndexReport {
    let directory = tempdir().expect("create temporary benchmark index root");
    let project_id = format!("magic-search-bench-{}", corpus.len());
    let model_cache_key = "magic-search-bench.synthetic.v1";
    let build_started = Instant::now();
    let build = PersistentVectorIndex::build_and_store(
        directory.path(),
        &project_id,
        model_cache_key,
        EMBEDDING_DIMENSION,
        corpus
            .iter()
            .map(|record| (record.asset_id.to_string(), record.vector.clone())),
    )
    .expect("generated vectors build a persistent local index");
    let build_elapsed = build_started.elapsed();
    let path = directory.path().join(&build.relative_path);
    let persisted_index_bytes = fs::metadata(&path)
        .expect("persistent index was written")
        .len();

    let load_started = Instant::now();
    let index = PersistentVectorIndex::load(
        directory.path(),
        &build.relative_path,
        &project_id,
        model_cache_key,
        EMBEDDING_DIMENSION,
    )
    .expect("generated persistent index is valid")
    .expect("generated persistent index exists");
    let load_elapsed = load_started.elapsed();

    let mut elapsed_ms = Vec::with_capacity(queries.len());
    let mut candidate_counts = Vec::with_capacity(queries.len());
    let mut all_stored_queries = 0_usize;
    let mut approximate_queries = 0_usize;
    for query in queries {
        let started = Instant::now();
        let candidates = index
            .candidates(&query.vector)
            .expect("generated query vector is valid for persistent index");
        elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        match candidates {
            IndexCandidates::AllStoredVectors => {
                all_stored_queries += 1;
                candidate_counts.push(index.vector_count());
            }
            IndexCandidates::AssetIds(asset_ids) => {
                approximate_queries += 1;
                candidate_counts.push(asset_ids.len());
            }
        }
    }
    let candidate_mode = match (all_stored_queries, approximate_queries) {
        (all, 0) if all == queries.len() => "all_stored_vectors",
        (0, approximate) if approximate == queries.len() => "approximate_asset_ids",
        _ => "mixed_candidate_modes",
    };
    let total_candidates = candidate_counts.iter().sum::<usize>();
    PersistentIndexReport {
        implementation: "magic_search::PersistentVectorIndex",
        vector_count: build.vector_count,
        approximate: build.approximate,
        build_ms: build_elapsed.as_millis(),
        build_records_per_second: corpus.len() as f64
            / build_elapsed.as_secs_f64().max(f64::EPSILON),
        persisted_index_bytes,
        load_ms: load_elapsed.as_millis(),
        candidate_lookup: CandidateLookupReport {
            query_count: queries.len(),
            candidate_mode,
            latency: latency_report(&elapsed_ms),
            minimum_candidates: *candidate_counts
                .iter()
                .min()
                .expect("benchmark has candidate counts"),
            maximum_candidates: *candidate_counts
                .iter()
                .max()
                .expect("benchmark has candidate counts"),
            mean_candidates: total_candidates as f64 / candidate_counts.len() as f64,
            includes_vector_fetch_or_hybrid_rank: false,
        },
    }
}

fn benchmark_mode(
    index: &ExactVectorIndex,
    queries: &[BenchmarkQuery],
    mode: SearchMode,
) -> QueryModeReport {
    let mut elapsed_ms = Vec::with_capacity(queries.len());
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        let hits = index.search(query, mode, TOP_K);
        elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        results.push(hits);
    }

    QueryModeReport {
        mode: mode.name(),
        query_count: queries.len(),
        top_k: TOP_K,
        latency: latency_report(&elapsed_ms),
        synthetic_self_retrieval_metrics: synthetic_metrics(queries, &results),
    }
}

fn latency_report(elapsed_ms: &[f64]) -> LatencyReport {
    assert!(!elapsed_ms.is_empty(), "benchmark has at least one query");
    let mut sorted = elapsed_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let p95_index = (sorted.len() - 1) * 95 / 100;
    LatencyReport {
        total_ms: elapsed_ms.iter().sum(),
        median_ms: sorted[sorted.len() / 2],
        p95_ms: sorted[p95_index],
        max_ms: *sorted.last().expect("non-empty query duration list"),
    }
}

fn synthetic_metrics(
    queries: &[BenchmarkQuery],
    results: &[Vec<SearchHit>],
) -> SyntheticRetrievalMetrics {
    assert_eq!(queries.len(), results.len());
    let query_count = queries.len() as f64;
    let mut recall_at_1 = 0.0;
    let mut recall_at_5 = 0.0;
    let mut recall_at_10 = 0.0;
    let mut reciprocal_rank = 0.0;
    let mut ndcg_at_10 = 0.0;

    for (query, hits) in queries.iter().zip(results) {
        let rank = hits
            .iter()
            .position(|hit| hit.asset_id == query.expected_asset_id)
            .map(|position| position + 1);
        if let Some(rank) = rank {
            if rank <= 1 {
                recall_at_1 += 1.0;
            }
            if rank <= 5 {
                recall_at_5 += 1.0;
            }
            if rank <= 10 {
                recall_at_10 += 1.0;
                reciprocal_rank += 1.0 / rank as f64;
                ndcg_at_10 += 1.0 / (rank as f64 + 1.0).log2();
            }
        }
    }

    SyntheticRetrievalMetrics {
        recall_at_1: recall_at_1 / query_count,
        recall_at_5: recall_at_5 / query_count,
        recall_at_10: recall_at_10 / query_count,
        mean_reciprocal_rank: reciprocal_rank / query_count,
        ndcg_at_10: ndcg_at_10 / query_count,
        evaluation_scope:
            "synthetic exact-vector self-retrieval; not text/image semantic relevance",
    }
}

fn validate_ground_truth_scaffolding() -> Result<(), String> {
    let documents = [
        (
            "text-image-queries",
            include_str!("../ground-truth/v1/text-image-queries.json"),
            "TextImageQueryGroundTruth",
        ),
        (
            "hybrid-queries",
            include_str!("../ground-truth/v1/hybrid-queries.json"),
            "HybridQueryGroundTruth",
        ),
        (
            "dataset-manifest",
            include_str!("../ground-truth/v1/dataset-manifest.json"),
            "BenchmarkDatasetManifest",
        ),
    ];
    for (name, source, expected_type) in documents {
        let value: Value = serde_json::from_str(source)
            .map_err(|error| format!("{name} must be valid JSON: {error}"))?;
        if value["formatVersion"].as_u64() != Some(1) {
            return Err(format!("{name} must use formatVersion 1"));
        }
        if value["type"].as_str() != Some(expected_type) {
            return Err(format!("{name} must have type {expected_type}"));
        }
        if name == "dataset-manifest" {
            if !value["datasets"].is_array() {
                return Err("dataset-manifest must contain a datasets array".to_owned());
            }
        } else if !value["queries"].is_array() {
            return Err(format!("{name} must contain a queries array"));
        }
    }
    Ok(())
}

struct Options {
    suite: String,
    output: Option<PathBuf>,
}

fn parse_options() -> Options {
    let mut arguments = env::args().skip(1);
    let mut suite = "baseline".to_owned();
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                suite = arguments
                    .next()
                    .unwrap_or_else(|| exit_usage("--suite needs a value"));
            }
            "--output" => {
                output = Some(PathBuf::from(
                    arguments
                        .next()
                        .unwrap_or_else(|| exit_usage("--output needs a path")),
                ));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run -p magic-search-bench -- --suite baseline [--output report.json]"
                );
                std::process::exit(0);
            }
            _ => exit_usage("unknown argument"),
        }
    }
    if suite != "baseline" {
        exit_usage("only the generated baseline suite is available");
    }
    Options { suite, output }
}

fn exit_usage(message: &str) -> ! {
    eprintln!(
        "{message}. Usage: cargo run -p magic-search-bench -- --suite baseline [--output report.json]"
    );
    std::process::exit(2);
}

fn main() {
    let options = parse_options();
    validate_ground_truth_scaffolding().unwrap_or_else(|error| {
        eprintln!("MagicSearchBench ground-truth scaffold invalid: {error}");
        std::process::exit(1);
    });
    let report = run_baseline();
    debug_assert_eq!(report.suite, options.suite);
    let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
    println!("{json}");
    if let Some(output) = options.output {
        fs::write(&output, format!("{json}\n")).unwrap_or_else(|error| {
            eprintln!(
                "Could not write benchmark report to {}: {error}",
                output.display()
            );
            std::process::exit(1);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_vectors_are_unit_length() {
        let vector = synthetic_vector(42, EMBEDDING_DIMENSION);
        let norm = dot_product(&vector, &vector);
        assert!((norm - 1.0).abs() < 0.000_1);
    }

    #[test]
    fn semantic_exact_search_recovers_the_same_asset() {
        let index = ExactVectorIndex::build(generate_corpus(64));
        let record = &index.records[37];
        let query = BenchmarkQuery {
            vector: record.vector.clone(),
            expected_asset_id: record.asset_id,
            metadata: record.metadata,
        };
        let hits = index.search(&query, SearchMode::SemanticOnly, TOP_K);
        assert_eq!(hits.first().map(|hit| hit.asset_id), Some(record.asset_id));
    }

    #[test]
    fn hybrid_search_preserves_its_deterministic_predicates() {
        let index = ExactVectorIndex::build(generate_corpus(128));
        let record = &index.records[79];
        let query = BenchmarkQuery {
            vector: record.vector.clone(),
            expected_asset_id: record.asset_id,
            metadata: record.metadata,
        };
        let hits = index.search(&query, SearchMode::Hybrid, TOP_K);
        assert!(!hits.is_empty());
        for hit in hits {
            let candidate = index
                .records
                .iter()
                .find(|candidate| candidate.asset_id == hit.asset_id)
                .expect("search hit belongs to index");
            assert!(matches_metadata_predicates(
                candidate.metadata,
                query.metadata
            ));
        }
    }

    #[test]
    fn synthetic_retrieval_metrics_are_bounded() {
        let index = ExactVectorIndex::build(generate_corpus(32));
        let queries = benchmark_queries(&index.records);
        let results = queries
            .iter()
            .map(|query| index.search(query, SearchMode::SemanticOnly, TOP_K))
            .collect::<Vec<_>>();
        let metrics = synthetic_metrics(&queries, &results);
        assert_eq!(metrics.recall_at_1, 1.0);
        assert_eq!(metrics.recall_at_5, 1.0);
        assert_eq!(metrics.recall_at_10, 1.0);
        assert_eq!(metrics.mean_reciprocal_rank, 1.0);
        assert_eq!(metrics.ndcg_at_10, 1.0);
    }

    #[test]
    fn persistent_index_measurement_uses_a_real_rebuildable_artifact() {
        let corpus = generate_corpus(32);
        let queries = benchmark_queries(&corpus);
        let report = measure_persistent_index(&corpus, &queries);
        assert_eq!(report.vector_count, corpus.len());
        assert!(report.persisted_index_bytes > 0);
        assert_eq!(report.candidate_lookup.candidate_mode, "all_stored_vectors");
    }

    #[test]
    fn ground_truth_scaffolding_is_valid_json_with_expected_shapes() {
        validate_ground_truth_scaffolding().expect("ground-truth scaffold is valid");
    }
}
