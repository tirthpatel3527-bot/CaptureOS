//! Deterministic generated-data benchmark for the product Moment Brain analyzer.
#![forbid(unsafe_code)]

use moment_brain::{
    analyze_append_only_tail, analyze_timeline, AnalysisMode, AppendOnlyTailAnalysisRequest,
    BoundaryCategory, ExistingHumanDecision, HumanPresentationSignals, IncrementalDisposition,
    Orientation, SemanticVector, TimelineAnalysis, TimelineAnalysisConfig, TimelineAnalysisRequest,
    TimelineAssetInput, MOMENT_BRAIN_ALGORITHM_VERSION,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::OpenOptions,
    io::Write,
    mem,
    path::PathBuf,
    time::Instant,
};

const FIXTURE_VERSION: &str = "captureos-moment-timeline.synthetic.v1";
/// Matches the admitted M6 shared image/text embedding width. The values remain generated test
/// data; this makes CPU and allocation scale more representative without claiming model quality.
const SEMANTIC_DIMENSION: usize = 768;
const DEFAULT_SCALES: [usize; 4] = [1_000, 10_000, 50_000, 100_000];
const PAGE_SIZE: usize = 120;
const INCREMENTAL_APPEND_COUNT: usize = 50;
#[cfg(test)]
const FIXTURE_MANIFEST: &str = include_str!("../fixtures/timeline.v1.json");

#[derive(Debug, Clone)]
struct SyntheticFixture {
    request: TimelineAnalysisRequest,
    truth_segment_by_asset: BTreeMap<String, usize>,
    truth_boundary_asset_ids: Vec<String>,
    missing_timestamp_asset_ids: BTreeSet<String>,
    previous_latest_captured_at_unix_ms: i64,
    preceding_context_assets: Vec<TimelineAssetInput>,
    appended_assets: Vec<TimelineAssetInput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    suite: String,
    benchmark_kind: &'static str,
    fixture: FixtureReport,
    analyzer: AnalyzerReport,
    scales: Vec<ScaleReport>,
    caveats: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureReport {
    version: &'static str,
    source: &'static str,
    semantic_dimension: usize,
    contains_customer_media: bool,
    contains_model_weights: bool,
    contains_network_calls: bool,
    truth_definition: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalyzerReport {
    algorithm_version: &'static str,
    implementation: &'static str,
    chronological_strategy: &'static str,
    comparison_bound: &'static str,
    missing_timestamp_behavior: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScaleReport {
    record_count: usize,
    generation_elapsed_us: u128,
    estimated_input_lower_bound_bytes: u64,
    timestamped_records: usize,
    intentionally_ungrouped_records: usize,
    truth_segment_count: usize,
    truth_boundary_count: usize,
    modes: Vec<ModeReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModeReport {
    mode: &'static str,
    analysis_elapsed_us: u128,
    incremental: IncrementalReport,
    moment_page_elapsed_ns: u128,
    moment_lookup_elapsed_ns: u128,
    adaptive_gap_seconds: Option<f64>,
    chronological_comparisons: usize,
    semantic_context_vector_reads: usize,
    max_context_assets: usize,
    predicted_segment_count: usize,
    predicted_boundary_count: usize,
    strong_boundary_count: usize,
    moderate_boundary_count: usize,
    boundary_metrics: BoundaryMetrics,
    segment_purity: f64,
    over_segmentation_splits: usize,
    under_segmentation_merges: usize,
    ungrouped_records: usize,
    ungrouped_truth_recall: f64,
    estimated_analysis_lower_bound_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IncrementalReport {
    elapsed_us: u128,
    disposition: &'static str,
    affected_asset_count: usize,
    analyzed_window_asset_count: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BoundaryMetrics {
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    precision: f64,
    recall: f64,
    f1: f64,
}

#[derive(Debug, Default)]
struct SegmentationErrors {
    /// Extra predicted fragments across generated truth segments. A value of two means that
    /// truth segments were split into three additional pieces in total, not merely that counts
    /// differed.
    over_segmentation_splits: usize,
    /// Extra generated truth segments contained inside predicted Moments. This catches a merged
    /// partition even where the total predicted/true segment counts happen to match.
    under_segmentation_merges: usize,
}

fn main() {
    let config = parse_arguments();
    let modes = match config.suite.as_str() {
        "baseline" => vec![
            AnalysisMode::TimeOnly,
            AnalysisMode::SemanticOnly,
            AnalysisMode::Combined,
        ],
        suite => parse_mode(suite).map(|mode| vec![mode]).unwrap_or_else(|| {
            exit_usage("unknown --suite; use baseline, time-only, semantic-only, or combined")
        }),
    };
    let scales = config.scales.unwrap_or_else(|| DEFAULT_SCALES.to_vec());
    if scales.iter().any(|count| *count < 3) {
        exit_usage("--records values must be at least 3");
    }

    let report = BenchmarkReport {
        suite: config.suite,
        benchmark_kind: "deterministic generated timeline mechanics using the product moment-brain crate",
        fixture: FixtureReport {
            version: FIXTURE_VERSION,
            source: "in-memory deterministic generator; no image, catalog, or external dataset",
            semantic_dimension: SEMANTIC_DIMENSION,
            contains_customer_media: false,
            contains_model_weights: false,
            contains_network_calls: false,
            truth_definition: "Synthetic structural segment changes and intentional missing timestamps; not photographic-event truth.",
        },
        analyzer: AnalyzerReport {
            algorithm_version: MOMENT_BRAIN_ALGORITHM_VERSION,
            implementation: "moment-brain::analyze_timeline and moment-brain::analyze_append_only_tail",
            chronological_strategy: "stable capture-time order with asset-id tie-breaker",
            comparison_bound: "product analyzer evaluates each chronological boundary once and bounds semantic context by configuration",
            missing_timestamp_behavior: "product analyzer returns them as explicitly ungrouped; no capture time is guessed",
        },
        scales: scales
            .into_iter()
            .map(|record_count| benchmark_scale(record_count, &modes))
            .collect(),
        caveats: vec![
            "Generated semantic signatures are controlled numeric fixtures, not image embeddings, a captioner, object detector, or a measure of real semantic relevance.",
            "The benchmark does not load a model, tokenizer, preview, original, CaptureOS catalog, customer project, AI Test photo, or cloud service.",
            "Boundary precision/recall/F1 and purity are against synthetic structural labels only; they do not prove that a photographic moment or event is objectively present.",
            "Reported memory is a deterministic lower-bound allocation estimate, not operating-system peak RSS.",
            "Timing is local-machine evidence. Re-run on a target machine and do not present it as a hardware, startup, or semantic-quality guarantee.",
            "Moment labels, human overrides, coverage confirmation, privacy enforcement, SQLite migration behavior, and UI responsiveness require product tests in addition to this benchmark.",
        ],
    };
    let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
    println!("{json}");
    if let Some(output) = config.output {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .unwrap_or_else(|error| {
                eprintln!(
                    "Could not create new benchmark report {}: {error}",
                    output.display()
                );
                std::process::exit(1);
            });
        file.write_all(format!("{json}\n").as_bytes())
            .unwrap_or_else(|error| {
                eprintln!(
                    "Could not write benchmark report {}: {error}",
                    output.display()
                );
                std::process::exit(1);
            });
    }
}

#[derive(Debug)]
struct Arguments {
    suite: String,
    scales: Option<Vec<usize>>,
    output: Option<PathBuf>,
}

fn parse_arguments() -> Arguments {
    let mut arguments = env::args().skip(1);
    let mut suite = "baseline".to_owned();
    let mut scales = None;
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                suite = arguments
                    .next()
                    .unwrap_or_else(|| exit_usage("--suite needs a value"));
            }
            "--records" => {
                let value = arguments
                    .next()
                    .unwrap_or_else(|| exit_usage("--records needs a comma-separated value"));
                let parsed = value
                    .split(',')
                    .map(|item| {
                        item.parse::<usize>().unwrap_or_else(|_| {
                            exit_usage("--records must contain integers of at least 2")
                        })
                    })
                    .collect::<Vec<_>>();
                if parsed.is_empty() {
                    exit_usage("--records must not be empty");
                }
                scales = Some(parsed);
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
                    "Usage: cargo run -p moment-brain-bench -- --suite baseline|time-only|semantic-only|combined [--records 1000,10000] [--output report.json]"
                );
                std::process::exit(0);
            }
            _ => exit_usage("unknown argument"),
        }
    }
    Arguments {
        suite,
        scales,
        output,
    }
}

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}");
    eprintln!(
        "Usage: cargo run -p moment-brain-bench -- --suite baseline|time-only|semantic-only|combined [--records 1000,10000] [--output report.json]"
    );
    std::process::exit(2);
}

fn parse_mode(value: &str) -> Option<AnalysisMode> {
    match value {
        "time-only" => Some(AnalysisMode::TimeOnly),
        "semantic-only" => Some(AnalysisMode::SemanticOnly),
        "combined" => Some(AnalysisMode::Combined),
        _ => None,
    }
}

fn mode_name(mode: AnalysisMode) -> &'static str {
    match mode {
        AnalysisMode::TimeOnly => "time_only",
        AnalysisMode::SemanticOnly => "semantic_only",
        AnalysisMode::Combined => "combined_multi_signal",
    }
}

fn benchmark_scale(record_count: usize, modes: &[AnalysisMode]) -> ScaleReport {
    let generation_started = Instant::now();
    let mut fixture = generate_fixture(record_count);
    let generation_elapsed = generation_started.elapsed();
    let timestamped_records = fixture
        .request
        .assets
        .iter()
        .filter(|asset| asset.captured_at_unix_ms.is_some())
        .count();
    let truth_segment_count = fixture
        .truth_segment_by_asset
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .len();
    let modes = modes
        .iter()
        .copied()
        .map(|mode| benchmark_mode(&mut fixture, mode))
        .collect();
    ScaleReport {
        record_count,
        generation_elapsed_us: generation_elapsed.as_micros(),
        estimated_input_lower_bound_bytes: estimated_input_bytes(&fixture.request),
        timestamped_records,
        intentionally_ungrouped_records: fixture.missing_timestamp_asset_ids.len(),
        truth_segment_count,
        truth_boundary_count: fixture.truth_boundary_asset_ids.len(),
        modes,
    }
}

fn benchmark_mode(fixture: &mut SyntheticFixture, mode: AnalysisMode) -> ModeReport {
    fixture.request.config.mode = mode;
    let request = &fixture.request;
    let started = Instant::now();
    let analysis = analyze_timeline(request).expect("synthetic request is accepted");
    let analysis_elapsed = started.elapsed();

    let incremental_started = Instant::now();
    let incremental = analyze_append_only_tail(&AppendOnlyTailAnalysisRequest {
        previous_latest_captured_at_unix_ms: fixture.previous_latest_captured_at_unix_ms,
        preceding_context_assets: fixture.preceding_context_assets.clone(),
        appended_assets: fixture.appended_assets.clone(),
        label_candidates: Vec::new(),
        config: request.config.clone(),
    })
    .expect("synthetic append-only update is accepted");
    let incremental_elapsed = incremental_started.elapsed();

    let moment_page_started = Instant::now();
    let _moment_page = timeline_page(&analysis, 0, PAGE_SIZE);
    let moment_page_elapsed = moment_page_started.elapsed();

    let moment_lookup = build_moment_lookup(&analysis);
    let moment_lookup_started = Instant::now();
    let _moment = analysis
        .ordered_asset_ids
        .get(analysis.ordered_asset_ids.len() / 2)
        .and_then(|asset_id| moment_lookup.get(asset_id.as_str()));
    let moment_lookup_elapsed = moment_lookup_started.elapsed();

    let predicted_boundary_asset_ids = analysis
        .moments
        .iter()
        .skip(1)
        .map(|moment| moment.start_asset_id.clone())
        .collect::<Vec<_>>();
    let boundary_metrics = score_boundaries(
        &predicted_boundary_asset_ids,
        &fixture.truth_boundary_asset_ids,
    );
    let ungrouped_records = analysis.ungrouped_assets.len();
    let ungrouped_truth_recall = if fixture.missing_timestamp_asset_ids.is_empty() {
        1.0
    } else {
        analysis
            .ungrouped_assets
            .iter()
            .filter(|asset| {
                fixture
                    .missing_timestamp_asset_ids
                    .contains(&asset.asset_id)
            })
            .count() as f64
            / fixture.missing_timestamp_asset_ids.len() as f64
    };

    let segmentation_errors = segmentation_errors(&analysis, &fixture.truth_segment_by_asset);
    ModeReport {
        mode: mode_name(mode),
        analysis_elapsed_us: analysis_elapsed.as_micros(),
        incremental: IncrementalReport {
            elapsed_us: incremental_elapsed.as_micros(),
            disposition: incremental_disposition_name(&incremental.disposition),
            affected_asset_count: incremental.affected_asset_ids.len(),
            analyzed_window_asset_count: incremental
                .analysis
                .as_ref()
                .map(|analysis| analysis.ordered_asset_ids.len()),
        },
        moment_page_elapsed_ns: moment_page_elapsed.as_nanos(),
        moment_lookup_elapsed_ns: moment_lookup_elapsed.as_nanos(),
        adaptive_gap_seconds: analysis
            .cadence
            .normalizer_gap_ms
            .map(|milliseconds| milliseconds as f64 / 1_000.0),
        chronological_comparisons: analysis.work.boundaries_evaluated,
        semantic_context_vector_reads: analysis.work.semantic_context_vector_reads,
        max_context_assets: analysis.work.max_context_assets,
        predicted_segment_count: analysis.moments.len(),
        predicted_boundary_count: predicted_boundary_asset_ids.len(),
        strong_boundary_count: analysis
            .boundaries
            .iter()
            .filter(|boundary| boundary.category == BoundaryCategory::Strong)
            .count(),
        moderate_boundary_count: analysis
            .boundaries
            .iter()
            .filter(|boundary| boundary.category == BoundaryCategory::Moderate)
            .count(),
        boundary_metrics,
        segment_purity: score_segment_purity(&analysis, &fixture.truth_segment_by_asset),
        over_segmentation_splits: segmentation_errors.over_segmentation_splits,
        under_segmentation_merges: segmentation_errors.under_segmentation_merges,
        ungrouped_records,
        ungrouped_truth_recall,
        estimated_analysis_lower_bound_bytes: estimated_analysis_bytes(&analysis),
    }
}

fn incremental_disposition_name(disposition: &IncrementalDisposition) -> &'static str {
    match disposition {
        IncrementalDisposition::NoChanges => "no_changes",
        IncrementalDisposition::TailReanalyzed => "tail_reanalyzed",
        IncrementalDisposition::UngroupedAssetsUpdated => "ungrouped_assets_updated",
        IncrementalDisposition::FullRebuildRequired => "full_rebuild_required",
    }
}

// Keep `%` arithmetic instead of `usize::is_multiple_of` so the checked-in benchmark honors the
// workspace Rust 1.78 MSRV while still satisfying current Clippy with an explicit local allow.
#[allow(clippy::manual_is_multiple_of)]
fn generate_fixture(record_count: usize) -> SyntheticFixture {
    let mut assets = Vec::with_capacity(record_count);
    let mut truth_segment_by_asset = BTreeMap::new();
    let mut truth_boundary_asset_ids = Vec::new();
    let mut missing_timestamp_asset_ids = BTreeSet::new();
    let mut state = 0xaad3_6b02_0356_71a8_u64;
    let mut captured_at = 1_730_000_000_000_i64;
    let mut segment_id = 0_usize;
    let mut position_in_segment = 0_usize;
    let mut segment_length = next_segment_length(&mut state);
    let mut semantic_group = 0_usize;
    let mut camera = 0_u8;
    let mut lens = 0_u8;
    let mut orientation = 0_u8;

    for ordinal in 0..record_count {
        let asset_id = asset_id(ordinal);
        if ordinal % 997 == 0 {
            missing_timestamp_asset_ids.insert(asset_id.clone());
            assets.push(TimelineAssetInput::minimal(asset_id, None));
            continue;
        }

        let began_new_segment = position_in_segment == segment_length;
        if began_new_segment {
            segment_id += 1;
            position_in_segment = 0;
            segment_length = next_segment_length(&mut state);
            if segment_id % 5 != 0 {
                semantic_group = (semantic_group + 1) % SEMANTIC_DIMENSION;
            }
            let boundary_gap_seconds = if segment_id % 7 == 0 {
                7 + random_bounded(&mut state, 4) as i64
            } else {
                90 + random_bounded(&mut state, 300) as i64
            };
            captured_at += boundary_gap_seconds * 1_000;
            if segment_id % 6 != 0 {
                camera = ((segment_id / 2) % 3) as u8;
                lens = ((segment_id / 3) % 4) as u8;
                orientation = (segment_id % 2) as u8;
            }
            truth_boundary_asset_ids.push(asset_id.clone());
        } else if position_in_segment > 0 {
            let mut cadence_seconds = 1 + random_bounded(&mut state, 3) as i64;
            if position_in_segment == segment_length / 2 && segment_id % 13 == 3 {
                cadence_seconds += 12;
            }
            captured_at += cadence_seconds * 1_000;
        }

        let mut asset = TimelineAssetInput::minimal(asset_id.clone(), Some(captured_at));
        asset.camera_model = Some(format!("bench-camera-{camera}"));
        asset.lens_model = Some(format!("bench-lens-{lens}"));
        asset.orientation = Some(match orientation {
            0 => Orientation::Landscape,
            _ => Orientation::Portrait,
        });
        asset.anonymous_face_count = if ordinal % 29 == 0 {
            None
        } else {
            Some(((segment_id + ordinal / 11) % 4) as u32)
        };
        // Neighbors in the same three-frame synthetic related set share one ID, allowing the
        // product analyzer to exercise Similar Set continuity without changing its membership.
        asset.similar_set_ids = Some(vec![format!(
            "similar-{segment_id}-{}",
            position_in_segment / 3
        )]);
        asset.filename_sequence = Some(ordinal as u64);
        asset.semantic_embedding = Some(SemanticVector {
            compatibility_key: "moment-brain-bench.synthetic-signature.v1".into(),
            values: signature_for(semantic_group, ordinal),
        });
        asset.technical_presentation_score = Some(0.35 + ((ordinal % 61) as f32 / 100.0).min(0.60));
        asset.human_presentation = HumanPresentationSignals {
            decision: match ordinal % 7 {
                0 => Some(ExistingHumanDecision::Keep),
                1 => Some(ExistingHumanDecision::Review),
                2 => Some(ExistingHumanDecision::Reject),
                _ => None,
            },
            rating: (ordinal % 5 == 0).then_some(((ordinal % 5) + 1) as u8),
            starred: (ordinal % 11 == 0).then_some(true),
        };
        truth_segment_by_asset.insert(asset_id, segment_id);
        assets.push(asset);
        position_in_segment += 1;
    }

    let append_count = INCREMENTAL_APPEND_COUNT.min(record_count.saturating_sub(2));
    let changed_start = record_count - append_count;
    let previous_latest_captured_at_unix_ms = assets
        .iter()
        .take(changed_start)
        .filter_map(|asset| asset.captured_at_unix_ms)
        .max()
        .expect("at least one generated timestamp precedes the append-only tail");
    let mut preceding_context_assets = assets
        .iter()
        .take(changed_start)
        .filter(|asset| asset.captured_at_unix_ms.is_some())
        .rev()
        .take(TimelineAnalysisConfig::default().max_context_assets)
        .cloned()
        .collect::<Vec<_>>();
    preceding_context_assets.reverse();
    let appended_assets = assets
        .iter()
        .skip(changed_start)
        .cloned()
        .collect::<Vec<_>>();

    SyntheticFixture {
        request: TimelineAnalysisRequest {
            assets,
            label_candidates: Vec::new(),
            config: TimelineAnalysisConfig::default(),
        },
        truth_segment_by_asset,
        truth_boundary_asset_ids,
        missing_timestamp_asset_ids,
        previous_latest_captured_at_unix_ms,
        preceding_context_assets,
        appended_assets,
    }
}

fn asset_id(ordinal: usize) -> String {
    format!("asset-{ordinal:08}")
}

fn next_segment_length(state: &mut u64) -> usize {
    31 + random_bounded(state, 67) as usize
}

fn random_bounded(state: &mut u64, bound: u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 32) % bound
}

fn signature_for(group: usize, ordinal: usize) -> Vec<f32> {
    let mut signature = vec![0.0_f32; SEMANTIC_DIMENSION];
    let mut state = (ordinal as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(group as u64);
    for (dimension, value) in signature.iter_mut().enumerate() {
        state = state
            .wrapping_mul(2_862_933_555_777_941_757)
            .wrapping_add(3_037_000_493);
        let noise = ((state >> 40) as f32 / ((1_u32 << 24) as f32) - 0.5) * 0.04;
        *value = if dimension == group {
            1.0 + noise
        } else {
            noise
        };
    }
    normalize(&mut signature);
    signature
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    assert!(
        norm.is_finite() && norm > 0.0,
        "synthetic signature is valid"
    );
    for value in vector {
        *value /= norm;
    }
}

fn score_boundaries(predicted: &[String], truth: &[String]) -> BoundaryMetrics {
    let truth = truth.iter().cloned().collect::<BTreeSet<_>>();
    let predicted = predicted.iter().cloned().collect::<BTreeSet<_>>();
    let true_positives = predicted.intersection(&truth).count();
    let false_positives = predicted.len() - true_positives;
    let false_negatives = truth.len() - true_positives;
    let precision = ratio(true_positives, true_positives + false_positives);
    let recall = ratio(true_positives, true_positives + false_negatives);
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    BoundaryMetrics {
        true_positives,
        false_positives,
        false_negatives,
        precision,
        recall,
        f1,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn score_segment_purity(
    analysis: &TimelineAnalysis,
    truth_segment_by_asset: &BTreeMap<String, usize>,
) -> f64 {
    let mut total = 0_usize;
    let mut pure = 0_usize;
    for moment in &analysis.moments {
        let mut counts = BTreeMap::<usize, usize>::new();
        for asset_id in &moment.asset_ids {
            if let Some(truth_segment) = truth_segment_by_asset.get(asset_id) {
                *counts.entry(*truth_segment).or_default() += 1;
                total += 1;
            }
        }
        pure += counts.values().copied().max().unwrap_or(0);
    }
    ratio(pure, total)
}

fn segmentation_errors(
    analysis: &TimelineAnalysis,
    truth_segment_by_asset: &BTreeMap<String, usize>,
) -> SegmentationErrors {
    let mut predicted_by_truth = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut truth_by_predicted = BTreeMap::<usize, BTreeSet<usize>>::new();
    for (predicted_index, moment) in analysis.moments.iter().enumerate() {
        for asset_id in &moment.asset_ids {
            if let Some(truth_segment) = truth_segment_by_asset.get(asset_id) {
                predicted_by_truth
                    .entry(*truth_segment)
                    .or_default()
                    .insert(predicted_index);
                truth_by_predicted
                    .entry(predicted_index)
                    .or_default()
                    .insert(*truth_segment);
            }
        }
    }
    SegmentationErrors {
        over_segmentation_splits: predicted_by_truth
            .values()
            .map(|predicted| predicted.len().saturating_sub(1))
            .sum(),
        under_segmentation_merges: truth_by_predicted
            .values()
            .map(|truth| truth.len().saturating_sub(1))
            .sum(),
    }
}

fn build_moment_lookup(analysis: &TimelineAnalysis) -> BTreeMap<&str, usize> {
    let mut lookup = BTreeMap::new();
    for (moment_index, moment) in analysis.moments.iter().enumerate() {
        for asset_id in &moment.asset_ids {
            lookup.insert(asset_id.as_str(), moment_index);
        }
    }
    lookup
}

fn timeline_page(
    analysis: &TimelineAnalysis,
    page: usize,
    page_size: usize,
) -> &[moment_brain::MomentSegment] {
    let start = page.saturating_mul(page_size).min(analysis.moments.len());
    let end = start.saturating_add(page_size).min(analysis.moments.len());
    &analysis.moments[start..end]
}

fn estimated_input_bytes(request: &TimelineAnalysisRequest) -> u64 {
    let assets = request
        .assets
        .iter()
        .map(estimated_asset_bytes)
        .sum::<usize>();
    let candidates = request
        .label_candidates
        .iter()
        .map(|candidate| {
            mem::size_of_val(candidate)
                + candidate.candidate_id.capacity()
                + candidate
                    .semantic_embedding
                    .as_ref()
                    .map_or(0, |embedding| {
                        embedding.compatibility_key.capacity()
                            + embedding.values.capacity() * mem::size_of::<f32>()
                    })
        })
        .sum::<usize>();
    (mem::size_of::<TimelineAnalysisRequest>() + assets + candidates) as u64
}

fn estimated_asset_bytes(asset: &TimelineAssetInput) -> usize {
    mem::size_of::<TimelineAssetInput>()
        + asset.asset_id.capacity()
        + asset
            .camera_model
            .as_ref()
            .map_or(0, |value| value.capacity())
        + asset
            .lens_model
            .as_ref()
            .map_or(0, |value| value.capacity())
        + asset
            .similar_set_ids
            .as_ref()
            .map_or(0, |ids| ids.iter().map(|id| id.capacity()).sum())
        + asset.semantic_embedding.as_ref().map_or(0, |embedding| {
            embedding.compatibility_key.capacity()
                + embedding.values.capacity() * mem::size_of::<f32>()
        })
}

fn estimated_analysis_bytes(analysis: &TimelineAnalysis) -> u64 {
    let ordered = analysis
        .ordered_asset_ids
        .iter()
        .map(|id| id.capacity())
        .sum::<usize>();
    let moments = analysis
        .moments
        .iter()
        .map(|moment| {
            mem::size_of_val(moment)
                + moment.start_asset_id.capacity()
                + moment.end_asset_id.capacity()
                + moment
                    .asset_ids
                    .iter()
                    .map(|id| id.capacity())
                    .sum::<usize>()
        })
        .sum::<usize>();
    let boundaries = analysis
        .boundaries
        .iter()
        .map(|boundary| {
            mem::size_of_val(boundary)
                + boundary.left_asset_id.capacity()
                + boundary.right_asset_id.capacity()
                + boundary.explanation.capacity()
                + boundary
                    .signals
                    .iter()
                    .map(|signal| signal.detail.capacity())
                    .sum::<usize>()
        })
        .sum::<usize>();
    (mem::size_of::<TimelineAnalysis>() + ordered + moments + boundaries) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_manifest_matches_the_generated_fixture_contract() {
        let manifest: serde_json::Value =
            serde_json::from_str(FIXTURE_MANIFEST).expect("fixture manifest is valid JSON");
        assert_eq!(
            manifest
                .get("fixtureVersion")
                .and_then(serde_json::Value::as_str),
            Some(FIXTURE_VERSION)
        );
        let cases = manifest
            .get("intentionalCases")
            .and_then(serde_json::Value::as_array)
            .expect("fixture manifest lists intentional cases");
        let identifiers = cases
            .iter()
            .filter_map(|case| case.get("id"))
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            identifiers,
            BTreeSet::from(["sequence-a", "sequence-b", "sequence-c", "sequence-d"])
        );
    }

    #[test]
    fn synthetic_fixture_is_reproducible_and_preserves_missing_times() {
        let first = generate_fixture(2_000);
        let second = generate_fixture(2_000);
        assert_eq!(first.request.assets.len(), second.request.assets.len());
        assert_eq!(
            first.request.assets[1].captured_at_unix_ms,
            second.request.assets[1].captured_at_unix_ms
        );
        assert!(first.request.assets[0].captured_at_unix_ms.is_none());
        assert!(first.missing_timestamp_asset_ids.contains(&asset_id(997)));
        assert!(first
            .truth_segment_by_asset
            .values()
            .any(|segment| *segment == 1));
    }

    #[test]
    fn product_analyzer_uses_bounded_chronological_work() {
        let fixture = generate_fixture(1_000);
        let analysis = analyze_timeline(&fixture.request).expect("fixture is accepted");
        let timestamped = fixture
            .request
            .assets
            .iter()
            .filter(|asset| asset.captured_at_unix_ms.is_some())
            .count();
        assert_eq!(analysis.work.chronological_asset_count, timestamped);
        assert_eq!(analysis.work.boundaries_evaluated, timestamped - 1);
        assert!(
            analysis.work.semantic_context_vector_reads
                <= analysis.work.boundaries_evaluated * 2 * analysis.work.max_context_assets
        );
    }

    #[test]
    fn combined_result_is_scored_against_generated_structure_only() {
        let fixture = generate_fixture(1_000);
        let analysis = analyze_timeline(&fixture.request).expect("fixture is accepted");
        let predicted = analysis
            .moments
            .iter()
            .skip(1)
            .map(|moment| moment.start_asset_id.clone())
            .collect::<Vec<_>>();
        let metrics = score_boundaries(&predicted, &fixture.truth_boundary_asset_ids);
        assert!(!analysis.moments.is_empty());
        assert!(!fixture.truth_boundary_asset_ids.is_empty());
        assert!(metrics.precision.is_finite());
        assert!((0.0..=1.0).contains(&score_segment_purity(
            &analysis,
            &fixture.truth_segment_by_asset
        )));
    }

    #[test]
    fn append_only_tail_uses_the_product_bounded_incremental_path() {
        let fixture = generate_fixture(1_000);
        let update = analyze_append_only_tail(&AppendOnlyTailAnalysisRequest {
            previous_latest_captured_at_unix_ms: fixture.previous_latest_captured_at_unix_ms,
            preceding_context_assets: fixture.preceding_context_assets.clone(),
            appended_assets: fixture.appended_assets.clone(),
            label_candidates: Vec::new(),
            config: fixture.request.config.clone(),
        })
        .expect("fixture update is accepted");
        assert_eq!(update.disposition, IncrementalDisposition::TailReanalyzed);
        assert!(update.analysis.is_some());
    }
}
