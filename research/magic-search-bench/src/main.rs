use capture_intelligence::AnalysisImage;
use magic_search::{
    discover_siglip_provider, IndexCandidates, PersistentVectorIndex, SemanticEmbeddingProvider,
    SemanticProviderIdentity, ValidatedModelPackMetadata, SIGLIP_EMBEDDING_DIMENSION,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    env, fs, mem,
    path::{Path, PathBuf},
    time::Instant,
};
use tempfile::tempdir;

/// Kept deliberately unchanged so `--suite baseline` remains comparable with the original
/// recorded 64-dimensional generated-vector measurements.
const BASELINE_EMBEDDING_DIMENSION: usize = 64;
/// The current candidate SigLIP provider's shared image/text embedding dimension. This suite
/// still generates vectors: it does not claim to execute, validate, or benchmark a model pack.
const PROVIDER_EMBEDDING_DIMENSION: usize = SIGLIP_EMBEDDING_DIMENSION;
const TOP_K: usize = 10;
const CORPUS_SIZES: [usize; 3] = [1_000, 10_000, 50_000];
const LOCAL_SIGLIP_WARM_RUNS: usize = 10;
const LOCAL_SIGLIP_BENCHMARK_TEXT: &str = "a synthetic color calibration pattern";
const LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH: u32 = 307;
const LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT: u32 = 191;

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
        rank_records(self.records.iter(), query, mode, top_k)
    }
}

fn rank_records<'a>(
    records: impl IntoIterator<Item = &'a EmbeddingRecord>,
    query: &BenchmarkQuery,
    mode: SearchMode,
    top_k: usize,
) -> Vec<SearchHit> {
    let mut hits = records
        .into_iter()
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

fn matches_metadata_predicates(record: SyntheticMetadata, query: SyntheticMetadata) -> bool {
    record.face_count == query.face_count
        && record.rating >= query.rating
        && record.decision == query.decision
}

fn score_record(record: &EmbeddingRecord, query: &BenchmarkQuery, mode: SearchMode) -> f32 {
    match mode {
        SearchMode::SemanticOnly => dot_product(&record.vector, &query.vector),
        SearchMode::MetadataOnly => metadata_score(record.metadata),
        SearchMode::Hybrid => {
            dot_product(&record.vector, &query.vector) + metadata_score(record.metadata) * 0.001
        }
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

fn generate_corpus(record_count: usize, embedding_dimension: usize) -> Vec<EmbeddingRecord> {
    (0..record_count)
        .map(|index| {
            let asset_id = index as u32;
            EmbeddingRecord {
                asset_id,
                vector: synthetic_vector(asset_id, embedding_dimension),
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
    embedding_dimension_context: &'static str,
    ground_truth_status: &'static str,
    model_embedding_latency: UnmeasuredMetric,
    scale_runs: Vec<ScaleRun>,
    notes: Vec<&'static str>,
}

/// A deliberately narrow runtime benchmark for the one already-installed, product-approved
/// local SigLIP pack. It never opens a catalog or preview cache, and its inputs are generated
/// RGB bytes rather than photographs.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSiglipBenchmarkReport {
    suite: String,
    benchmark_kind: &'static str,
    local_model: LocalSiglipModelReport,
    cold_admission_load: ColdAdmissionLoadReport,
    warm_text_embedding: WarmEmbeddingReport,
    warm_image_embedding: WarmEmbeddingReport,
    non_photo_inputs: NonPhotoBenchmarkInputs,
    caveats: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSiglipModelReport {
    identity: SemanticProviderIdentity,
    validated_pack: ValidatedModelPackMetadata,
    expected_embedding_dimension: usize,
    embedding_dimension_verified: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ColdAdmissionLoadReport {
    elapsed_ms: f64,
    pack_validated: bool,
    includes: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WarmEmbeddingReport {
    operation: &'static str,
    run_count: usize,
    output_embedding_dimension: usize,
    all_outputs_finite: bool,
    all_outputs_l2_normalized: bool,
    latency: LatencyReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NonPhotoBenchmarkInputs {
    text: &'static str,
    image_width: u32,
    image_height: u32,
    image_description: &'static str,
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
    candidate_retrieval: CandidateRetrievalReport,
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

/// Generated-data mechanics for the approximate candidate stage. The persistent index returns
/// IDs, so this explicitly checks that the candidate set retains the exact generated neighbors
/// before an application would fetch vectors and re-rank them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateRetrievalReport {
    query_count: usize,
    candidate_set_self_recall: f64,
    candidate_set_exact_top_k_recall: CandidateSetRecall,
    reranked_self_retrieval_metrics: SyntheticRetrievalMetrics,
    vector_fetch_and_rerank_latency: LatencyReport,
    includes_candidate_lookup: bool,
    includes_sqlite_vector_fetch: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateSetRecall {
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
    evaluation_scope: &'static str,
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
    run_generated_suite(
        "baseline",
        BASELINE_EMBEDDING_DIMENSION,
        "historical generated-vector baseline (64 dimensions)",
        "magic-search-bench",
        "magic-search-bench.synthetic.v1",
        "generated local retrieval/index benchmark; not a semantic-quality evaluation",
    )
}

fn run_provider_768() -> BenchmarkReport {
    run_generated_suite(
        "provider-768",
        PROVIDER_EMBEDDING_DIMENSION,
        "candidate SigLIP provider-shaped generated vectors (768 dimensions; no model inference)",
        "magic-search-bench-provider-768",
        "magic-search-bench.synthetic.provider-768.v1",
        "generated 768-dimensional provider-scale retrieval/index benchmark; not model inference or a semantic-quality evaluation",
    )
}

/// Runs the installed-pack benchmark only after the provider has completed its normal, closed
/// admission gate. Discovery has no download path: it validates the fixed local pack directory,
/// graphs, tokenizer, and reference vectors before returning a provider.
fn run_local_siglip(model_root: &Path) -> Result<LocalSiglipBenchmarkReport, String> {
    let admission_started = Instant::now();
    let provider = discover_siglip_provider(model_root)
        .map_err(|error| format!("local SigLIP pack admission failed: {error}"))?
        .ok_or_else(|| {
            "no approved local SigLIP pack is installed below the supplied model root".to_owned()
        })?;
    let admission_elapsed = admission_started.elapsed();
    let identity = provider.identity().clone();
    let validated_pack = provider.pack_metadata();
    if identity.dimensions != SIGLIP_EMBEDDING_DIMENSION
        || validated_pack.embedding_dimension != SIGLIP_EMBEDDING_DIMENSION
    {
        return Err(format!(
            "validated local SigLIP pack reported {} identity dimensions and {} manifest dimensions; expected {SIGLIP_EMBEDDING_DIMENSION}",
            identity.dimensions, validated_pack.embedding_dimension
        ));
    }

    let image = deterministic_non_photo_image();
    let warm_text_embedding = measure_warm_embedding_latency("text_embedding", || {
        provider.embed_text(LOCAL_SIGLIP_BENCHMARK_TEXT)
    })?;
    let warm_image_embedding =
        measure_warm_embedding_latency("image_embedding", || provider.embed_image(&image))?;

    Ok(LocalSiglipBenchmarkReport {
        suite: "local-siglip".to_owned(),
        benchmark_kind: "installed local SigLIP pack admission and warm embedding latency; not retrieval evaluation",
        local_model: LocalSiglipModelReport {
            identity,
            validated_pack,
            expected_embedding_dimension: SIGLIP_EMBEDDING_DIMENSION,
            embedding_dimension_verified: true,
        },
        cold_admission_load: ColdAdmissionLoadReport {
            elapsed_ms: admission_elapsed.as_secs_f64() * 1_000.0,
            pack_validated: true,
            includes: "fixed-root pack discovery, manifest and checksum admission, tokenizer checks, ONNX graph loading, and reference image/text inference",
        },
        warm_text_embedding,
        warm_image_embedding,
        non_photo_inputs: NonPhotoBenchmarkInputs {
            text: LOCAL_SIGLIP_BENCHMARK_TEXT,
            image_width: LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH,
            image_height: LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT,
            image_description: "deterministic generated RGB calibration pattern; not decoded media or a photograph",
        },
        caveats: vec![
            "The model pack must already be installed through CaptureOS's controlled local workflow. This command never downloads, installs, or contacts a network service.",
            "No catalog, project, preview cache, AI Test media, customer image, filesystem path, or search result participates in this benchmark.",
            "Cold admission includes full local validation and reference inference, so it is not a pure graph-deserialization measurement.",
            "Warm timings are in-process CPU provider timings only. They exclude preview decoding, database work, vector-index lookup, hybrid ranking, UI rendering, and project-scale concurrency.",
            "Successful local embeddings establish only that this approved pack ran for deterministic non-photo inputs; they do not measure semantic relevance, object detection, person identity, or photographic result quality.",
        ],
    })
}

fn deterministic_non_photo_image() -> AnalysisImage {
    let mut rgb = Vec::with_capacity(
        (LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH * LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT * 3) as usize,
    );
    for y in 0..LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT {
        for x in 0..LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH {
            // The byte pattern is intentionally generated rather than decoded from an image
            // file. It gives preprocessing a non-uniform RGB input without representing media.
            rgb.push(((x.wrapping_mul(17) + y.wrapping_mul(3)) & 0xff) as u8);
            rgb.push(((x.wrapping_mul(5) + y.wrapping_mul(29)) & 0xff) as u8);
            rgb.push((((x ^ y).wrapping_mul(11)) & 0xff) as u8);
        }
    }
    AnalysisImage::new(
        LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH,
        LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT,
        rgb,
    )
    .expect("fixed deterministic benchmark RGB dimensions are valid")
}

fn measure_warm_embedding_latency<F>(
    operation: &'static str,
    mut embed: F,
) -> Result<WarmEmbeddingReport, String>
where
    F: FnMut() -> Result<Vec<f32>, magic_search::MagicSearchError>,
{
    let mut elapsed_ms = Vec::with_capacity(LOCAL_SIGLIP_WARM_RUNS);
    let mut all_outputs_finite = true;
    let mut all_outputs_l2_normalized = true;
    for _ in 0..LOCAL_SIGLIP_WARM_RUNS {
        let started = Instant::now();
        let embedding = embed().map_err(|error| format!("{operation} failed: {error}"))?;
        elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        if embedding.len() != SIGLIP_EMBEDDING_DIMENSION {
            return Err(format!(
                "{operation} returned {} dimensions; expected {SIGLIP_EMBEDDING_DIMENSION}",
                embedding.len()
            ));
        }
        all_outputs_finite &= embedding.iter().all(|value| value.is_finite());
        let norm = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        all_outputs_l2_normalized &= (norm - 1.0).abs() <= 0.000_1;
    }
    Ok(WarmEmbeddingReport {
        operation,
        run_count: LOCAL_SIGLIP_WARM_RUNS,
        output_embedding_dimension: SIGLIP_EMBEDDING_DIMENSION,
        all_outputs_finite,
        all_outputs_l2_normalized,
        latency: latency_report(&elapsed_ms),
    })
}

fn run_generated_suite(
    suite: &str,
    embedding_dimension: usize,
    embedding_dimension_context: &'static str,
    project_prefix: &str,
    model_cache_key: &str,
    benchmark_kind: &'static str,
) -> BenchmarkReport {
    let scale_runs = CORPUS_SIZES
        .into_iter()
        .map(|record_count| {
            run_scale(
                record_count,
                embedding_dimension,
                project_prefix,
                model_cache_key,
            )
        })
        .collect();
    BenchmarkReport {
        suite: suite.to_owned(),
        benchmark_kind,
        embedding_dimension,
        embedding_dimension_context,
        ground_truth_status: "versioned schema scaffold validated; no licensed text/image relevance fixture is bundled",
        model_embedding_latency: UnmeasuredMetric {
            status: "not_measured",
            reason: "This suite intentionally runs no model or tokenizer; it generates deterministic vectors in memory.",
        },
        scale_runs,
        notes: vec![
            "No customer media, model weights, tokenizer, cloud API, network request, or hosted vector database is used.",
            "Synthetic self-retrieval confirms benchmark mechanics only; it does not measure image-text relevance or object understanding.",
            "PersistentVectorIndex build, disk, candidate lookup, and candidate-set recall measurements are generated-data mechanics, not a claim of semantic result quality.",
            "Candidate retrieval measures generated exact-vector neighbor containment and in-memory re-ranking after candidate selection; it does not fetch SQLite rows or execute hybrid product ranking.",
        ],
    }
}

fn run_scale(
    record_count: usize,
    embedding_dimension: usize,
    project_prefix: &str,
    model_cache_key: &str,
) -> ScaleRun {
    let generation_started = Instant::now();
    let corpus = generate_corpus(record_count, embedding_dimension);
    let generation_elapsed = generation_started.elapsed();

    let queries = benchmark_queries(&corpus);
    let persistent_index = measure_persistent_index(
        &corpus,
        &queries,
        embedding_dimension,
        project_prefix,
        model_cache_key,
    );

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

    let vector_payload_bytes = (record_count * embedding_dimension * mem::size_of::<f32>()) as u64;
    let estimated_in_memory_payload_bytes =
        vector_payload_bytes + (record_count * mem::size_of::<EmbeddingRecord>()) as u64;
    ScaleRun {
        record_count,
        embedding_dimension,
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
    embedding_dimension: usize,
    project_prefix: &str,
    model_cache_key: &str,
) -> PersistentIndexReport {
    let directory = tempdir().expect("create temporary benchmark index root");
    let project_id = format!("{project_prefix}-{}", corpus.len());
    let build_started = Instant::now();
    let build = PersistentVectorIndex::build_and_store(
        directory.path(),
        &project_id,
        model_cache_key,
        embedding_dimension,
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
        embedding_dimension,
    )
    .expect("generated persistent index is valid")
    .expect("generated persistent index exists");
    let load_elapsed = load_started.elapsed();

    let mut elapsed_ms = Vec::with_capacity(queries.len());
    let mut candidate_counts = Vec::with_capacity(queries.len());
    let mut candidate_sets = Vec::with_capacity(queries.len());
    let mut all_stored_queries = 0_usize;
    let mut approximate_queries = 0_usize;
    for query in queries {
        let started = Instant::now();
        let candidates = index
            .candidates(&query.vector)
            .expect("generated query vector is valid for persistent index");
        elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        match &candidates {
            IndexCandidates::AllStoredVectors => {
                all_stored_queries += 1;
                candidate_counts.push(index.vector_count());
            }
            IndexCandidates::AssetIds(asset_ids) => {
                approximate_queries += 1;
                candidate_counts.push(asset_ids.len());
            }
        }
        candidate_sets.push(candidates);
    }
    let candidate_mode = match (all_stored_queries, approximate_queries) {
        (all, 0) if all == queries.len() => "all_stored_vectors",
        (0, approximate) if approximate == queries.len() => "approximate_asset_ids",
        _ => "mixed_candidate_modes",
    };
    let total_candidates = candidate_counts.iter().sum::<usize>();
    let candidate_retrieval = measure_candidate_retrieval(corpus, queries, &candidate_sets);
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
        candidate_retrieval,
    }
}

fn measure_candidate_retrieval(
    corpus: &[EmbeddingRecord],
    queries: &[BenchmarkQuery],
    candidate_sets: &[IndexCandidates],
) -> CandidateRetrievalReport {
    assert_eq!(queries.len(), candidate_sets.len());
    let records_by_id = corpus
        .iter()
        .map(|record| (record.asset_id.to_string(), record))
        .collect::<HashMap<_, _>>();
    let mut candidate_self_hits = 0_u64;
    let mut candidate_top_1_recall = 0.0;
    let mut candidate_top_5_recall = 0.0;
    let mut candidate_top_10_recall = 0.0;
    let mut reranked = Vec::with_capacity(queries.len());
    let mut elapsed_ms = Vec::with_capacity(queries.len());

    for (query, candidate_set) in queries.iter().zip(candidate_sets) {
        // This full generated-corpus rank is evaluation-only. Its time is intentionally not
        // included below, so the report does not disguise an exact scan as ANN latency.
        let exact_hits = rank_records(corpus.iter(), query, SearchMode::SemanticOnly, TOP_K);
        let started = Instant::now();
        let candidate_ids = match candidate_set {
            IndexCandidates::AllStoredVectors => None,
            IndexCandidates::AssetIds(asset_ids) => Some(
                asset_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::HashSet<_>>(),
            ),
        };
        let candidate_records = match &candidate_ids {
            None => corpus.iter().collect::<Vec<_>>(),
            Some(asset_ids) => asset_ids
                .iter()
                .filter_map(|asset_id| records_by_id.get(*asset_id).copied())
                .collect::<Vec<_>>(),
        };
        let reranked_hits = rank_records(
            candidate_records.iter().copied(),
            query,
            SearchMode::SemanticOnly,
            TOP_K,
        );
        elapsed_ms.push(started.elapsed().as_secs_f64() * 1_000.0);

        if candidate_set_contains(&candidate_ids, query.expected_asset_id) {
            candidate_self_hits += 1;
        }
        candidate_top_1_recall += candidate_set_recall_at_k(&exact_hits, &candidate_ids, 1);
        candidate_top_5_recall += candidate_set_recall_at_k(&exact_hits, &candidate_ids, 5);
        candidate_top_10_recall += candidate_set_recall_at_k(&exact_hits, &candidate_ids, 10);
        reranked.push(reranked_hits);
    }

    let query_count = queries.len() as f64;
    CandidateRetrievalReport {
        query_count: queries.len(),
        candidate_set_self_recall: candidate_self_hits as f64 / query_count,
        candidate_set_exact_top_k_recall: CandidateSetRecall {
            recall_at_1: candidate_top_1_recall / query_count,
            recall_at_5: candidate_top_5_recall / query_count,
            recall_at_10: candidate_top_10_recall / query_count,
            evaluation_scope:
                "exact top-K generated-vector neighbors retained in PersistentVectorIndex candidate IDs; not text/image semantic relevance",
        },
        reranked_self_retrieval_metrics: synthetic_metrics_with_scope(
            queries,
            &reranked,
            "synthetic exact-vector self-retrieval after PersistentVectorIndex candidate selection and in-memory re-ranking; not text/image semantic relevance",
        ),
        vector_fetch_and_rerank_latency: latency_report(&elapsed_ms),
        includes_candidate_lookup: false,
        includes_sqlite_vector_fetch: false,
    }
}

fn candidate_set_contains(
    candidate_ids: &Option<std::collections::HashSet<&str>>,
    asset_id: u32,
) -> bool {
    match candidate_ids {
        None => true,
        Some(candidate_ids) => {
            let asset_id = asset_id.to_string();
            candidate_ids.contains(asset_id.as_str())
        }
    }
}

fn candidate_set_recall_at_k(
    exact_hits: &[SearchHit],
    candidate_ids: &Option<std::collections::HashSet<&str>>,
    top_k: usize,
) -> f64 {
    let expected = exact_hits.iter().take(top_k).collect::<Vec<_>>();
    if expected.is_empty() {
        return 1.0;
    }
    let recovered = expected
        .iter()
        .filter(|hit| candidate_set_contains(candidate_ids, hit.asset_id))
        .count();
    recovered as f64 / expected.len() as f64
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
    synthetic_metrics_with_scope(
        queries,
        results,
        "synthetic exact-vector self-retrieval; not text/image semantic relevance",
    )
}

fn synthetic_metrics_with_scope(
    queries: &[BenchmarkQuery],
    results: &[Vec<SearchHit>],
    evaluation_scope: &'static str,
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
        evaluation_scope,
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

#[derive(Debug, PartialEq, Eq)]
struct Options {
    suite: String,
    model_root: Option<PathBuf>,
    output: Option<PathBuf>,
}

const USAGE: &str = "Usage: cargo run -p magic-search-bench -- --suite baseline|provider-768 [--output report.json]\n       cargo run -p magic-search-bench -- --suite local-siglip --model-root /path/to/semantic-models [--output report.json]";

fn parse_options() -> Options {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{USAGE}");
        std::process::exit(0);
    }
    parse_options_from(arguments).unwrap_or_else(|message| exit_usage(&message))
}

fn parse_options_from(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut arguments = arguments.into_iter();
    let mut suite = "baseline".to_owned();
    let mut model_root = None;
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                suite = arguments
                    .next()
                    .ok_or_else(|| "--suite needs a value".to_owned())?;
            }
            "--model-root" => {
                if model_root.is_some() {
                    return Err("--model-root may be supplied only once".to_owned());
                }
                model_root = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--model-root needs a path".to_owned())?,
                ));
            }
            "--output" => {
                output = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--output needs a path".to_owned())?,
                ));
            }
            "--help" | "-h" => {
                return Err("help requested".to_owned());
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if !matches!(suite.as_str(), "baseline" | "provider-768" | "local-siglip") {
        return Err("suite must be baseline, provider-768, or local-siglip".to_owned());
    }
    match (suite.as_str(), model_root.is_some()) {
        ("local-siglip", false) => {
            return Err("--model-root is required for --suite local-siglip".to_owned());
        }
        ("baseline" | "provider-768", true) => {
            return Err("--model-root is valid only for --suite local-siglip".to_owned());
        }
        _ => {}
    }
    Ok(Options {
        suite,
        model_root,
        output,
    })
}

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}. {USAGE}");
    std::process::exit(2);
}

fn main() {
    let options = parse_options();
    validate_ground_truth_scaffolding().unwrap_or_else(|error| {
        eprintln!("MagicSearchBench ground-truth scaffold invalid: {error}");
        std::process::exit(1);
    });
    let (report_suite, json) = match options.suite.as_str() {
        "baseline" => {
            let report = run_baseline();
            let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
            (report.suite, json)
        }
        "provider-768" => {
            let report = run_provider_768();
            let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
            (report.suite, json)
        }
        "local-siglip" => {
            let report = run_local_siglip(
                options
                    .model_root
                    .as_deref()
                    .expect("parse_options requires model root for local-siglip"),
            )
            .unwrap_or_else(|error| {
                eprintln!("MagicSearchBench local SigLIP benchmark failed: {error}");
                std::process::exit(1);
            });
            let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
            (report.suite, json)
        }
        _ => unreachable!("parse_options validates the suite"),
    };
    debug_assert_eq!(report_suite, options.suite);
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
        let vector = synthetic_vector(42, BASELINE_EMBEDDING_DIMENSION);
        let norm = dot_product(&vector, &vector);
        assert!((norm - 1.0).abs() < 0.000_1);
    }

    #[test]
    fn semantic_exact_search_recovers_the_same_asset() {
        let index = ExactVectorIndex::build(generate_corpus(64, BASELINE_EMBEDDING_DIMENSION));
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
        let index = ExactVectorIndex::build(generate_corpus(128, BASELINE_EMBEDDING_DIMENSION));
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
        let index = ExactVectorIndex::build(generate_corpus(32, BASELINE_EMBEDDING_DIMENSION));
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
        let corpus = generate_corpus(32, BASELINE_EMBEDDING_DIMENSION);
        let queries = benchmark_queries(&corpus);
        let report = measure_persistent_index(
            &corpus,
            &queries,
            BASELINE_EMBEDDING_DIMENSION,
            "magic-search-bench-test",
            "magic-search-bench.test.v1",
        );
        assert_eq!(report.vector_count, corpus.len());
        assert!(report.persisted_index_bytes > 0);
        assert_eq!(report.candidate_lookup.candidate_mode, "all_stored_vectors");
        assert_eq!(report.candidate_retrieval.candidate_set_self_recall, 1.0);
        assert_eq!(
            report
                .candidate_retrieval
                .reranked_self_retrieval_metrics
                .recall_at_1,
            1.0
        );
    }

    #[test]
    fn approximate_candidate_stage_retains_generated_self_queries() {
        let corpus = generate_corpus(4_097, BASELINE_EMBEDDING_DIMENSION);
        let queries = benchmark_queries(&corpus);
        let report = measure_persistent_index(
            &corpus,
            &queries,
            BASELINE_EMBEDDING_DIMENSION,
            "magic-search-bench-approximate-test",
            "magic-search-bench.approximate-test.v1",
        );
        assert!(report.approximate);
        assert_eq!(
            report.candidate_lookup.candidate_mode,
            "approximate_asset_ids"
        );
        assert_eq!(report.candidate_retrieval.candidate_set_self_recall, 1.0);
        assert_eq!(
            report
                .candidate_retrieval
                .candidate_set_exact_top_k_recall
                .recall_at_1,
            1.0
        );
        assert!(
            report
                .candidate_retrieval
                .candidate_set_exact_top_k_recall
                .recall_at_5
                > 0.0
        );
        assert!(
            report
                .candidate_retrieval
                .candidate_set_exact_top_k_recall
                .recall_at_10
                <= 1.0
        );
        assert_eq!(
            report
                .candidate_retrieval
                .reranked_self_retrieval_metrics
                .recall_at_1,
            1.0
        );
    }

    #[test]
    fn provider_scale_vectors_use_the_candidate_provider_dimension_without_a_model() {
        let corpus = generate_corpus(4, PROVIDER_EMBEDDING_DIMENSION);
        assert!(corpus
            .iter()
            .all(|record| record.vector.len() == PROVIDER_EMBEDDING_DIMENSION));
    }

    #[test]
    fn local_siglip_suite_requires_an_explicit_model_root() {
        let error = parse_options_from(["--suite", "local-siglip"].into_iter().map(str::to_owned))
            .unwrap_err();
        assert_eq!(error, "--model-root is required for --suite local-siglip");
    }

    #[test]
    fn local_siglip_suite_accepts_its_explicit_model_root() {
        let options = parse_options_from(
            [
                "--suite",
                "local-siglip",
                "--model-root",
                "/controlled/semantic-models",
                "--output",
                "/chosen/report.json",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("local installed-pack arguments parse");
        assert_eq!(options.suite, "local-siglip");
        assert_eq!(
            options.model_root,
            Some(PathBuf::from("/controlled/semantic-models"))
        );
        assert_eq!(options.output, Some(PathBuf::from("/chosen/report.json")));
    }

    #[test]
    fn generated_suites_reject_a_model_root() {
        let error = parse_options_from(
            [
                "--suite",
                "baseline",
                "--model-root",
                "/controlled/semantic-models",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap_err();
        assert_eq!(error, "--model-root is valid only for --suite local-siglip");
    }

    #[test]
    fn warm_embedding_metric_validates_outputs_and_records_each_run() {
        let report = measure_warm_embedding_latency("test_embedding", || {
            Ok(vec![
                1.0 / (SIGLIP_EMBEDDING_DIMENSION as f32).sqrt();
                SIGLIP_EMBEDDING_DIMENSION
            ])
        })
        .expect("deterministic normalized test embedding is accepted");
        assert_eq!(report.operation, "test_embedding");
        assert_eq!(report.run_count, LOCAL_SIGLIP_WARM_RUNS);
        assert_eq!(
            report.output_embedding_dimension,
            SIGLIP_EMBEDDING_DIMENSION
        );
        assert!(report.all_outputs_finite);
        assert!(report.all_outputs_l2_normalized);
        assert!(report.latency.total_ms >= 0.0);
    }

    #[test]
    fn deterministic_benchmark_image_is_generated_non_photo_rgb() {
        let image = deterministic_non_photo_image();
        assert_eq!(image.width, LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH);
        assert_eq!(image.height, LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT);
        assert_eq!(
            image.rgb().len(),
            (LOCAL_SIGLIP_BENCHMARK_IMAGE_WIDTH * LOCAL_SIGLIP_BENCHMARK_IMAGE_HEIGHT * 3) as usize
        );
        assert_ne!(image.rgb()[0..3], image.rgb()[3..6]);
    }

    #[test]
    fn ground_truth_scaffolding_is_valid_json_with_expected_shapes() {
        validate_ground_truth_scaffolding().expect("ground-truth scaffold is valid");
    }
}
