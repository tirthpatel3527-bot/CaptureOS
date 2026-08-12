//! Deterministic generated-data benchmark for CaptureOS Studio Brain I.
//!
//! This executable intentionally has no filesystem input except an optional new report path. It
//! does not load a catalog, media file, preview, embedding, model weight, tokeniser, or network
//! service. Its labels are controlled synthetic preference rules, not photographic truth.

#![forbid(unsafe_code)]

use serde::Serialize;
use std::{env, fs::OpenOptions, io::Write, mem, path::PathBuf, time::Instant};
use studio_brain::{
    predict, rank_similar_set, train_studio_model, AuxiliaryHumanSignals, ExplicitHumanActionKind,
    GenericRecommendation, HumanDecision, PairwisePreference, SimilarSetCandidate,
    StudioFeatureInput, StudioTrainingConfig, StudioTrainingStatus, TrainingObservation,
    STUDIO_BRAIN_ALGORITHM_VERSION, STUDIO_BRAIN_FEATURE_SCHEMA_VERSION,
};

const FIXTURE_VERSION: &str = "captureos-studio-brain.synthetic.v1";
const DEFAULT_SCALES: [usize; 5] = [100, 1_000, 10_000, 50_000, 100_000];
const INFERENCE_SAMPLE_COUNT: usize = 1_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    suite: String,
    benchmark_kind: &'static str,
    fixture: FixtureReport,
    algorithm: AlgorithmReport,
    scales: Vec<ScaleReport>,
    synthetic_profile_divergence: ProfileDivergenceReport,
    caveats: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureReport {
    version: &'static str,
    source: &'static str,
    contains_customer_media: bool,
    contains_embeddings: bool,
    contains_model_weights: bool,
    contains_network_calls: bool,
    truth_definition: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AlgorithmReport {
    algorithm_version: &'static str,
    feature_schema_version: &'static str,
    classifier: &'static str,
    evaluation: &'static str,
    pairwise_use: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScaleReport {
    record_count: usize,
    generated_pairwise_count: usize,
    generation_elapsed_us: u128,
    training_elapsed_us: u128,
    inference_elapsed_us: u128,
    inference_sample_count: usize,
    estimated_input_lower_bound_bytes: u64,
    serialized_model_bytes: Option<usize>,
    readiness_status: StudioTrainingStatus,
    evaluation: Option<EvaluationSummary>,
    pairwise_ranker_available: bool,
    pairwise_ranking_elapsed_us: Option<u128>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvaluationSummary {
    validation_count: usize,
    personal_accuracy: f32,
    personal_macro_f1: f32,
    personal_brier_score: f32,
    generic_accuracy: f32,
    majority_accuracy: f32,
    holdout_strategy: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileDivergenceReport {
    generated_observation_count_per_profile: usize,
    probe_technical_score: f32,
    studio_a_recommendation: Option<HumanDecision>,
    studio_b_recommendation: Option<HumanDecision>,
    differs: bool,
    statement: &'static str,
}

#[derive(Debug)]
struct Arguments {
    suite: String,
    scales: Option<Vec<usize>>,
    output: Option<PathBuf>,
}

fn main() {
    let arguments = parse_arguments();
    if arguments.suite != "baseline" {
        exit_usage("unknown --suite; use baseline");
    }
    let scales = arguments.scales.unwrap_or_else(|| DEFAULT_SCALES.to_vec());
    if scales.iter().any(|count| *count < 30) {
        exit_usage("--records values must be at least 30");
    }
    let report = BenchmarkReport {
        suite: arguments.suite,
        benchmark_kind: "deterministic generated Studio Brain mechanics using the product studio-brain crate",
        fixture: FixtureReport {
            version: FIXTURE_VERSION,
            source: "in-memory deterministic generator; no catalog, image, customer project, or external dataset",
            contains_customer_media: false,
            contains_embeddings: false,
            contains_model_weights: false,
            contains_network_calls: false,
            truth_definition: "Synthetic explicit human decisions following controlled studio rules; not artistic, photographic, or real-world culling truth.",
        },
        algorithm: AlgorithmReport {
            algorithm_version: STUDIO_BRAIN_ALGORITHM_VERSION,
            feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION,
            classifier: "regularized three-class linear softmax with a static 31-feature schema",
            evaluation: "stable whole-project holdout where generated projects permit it; candidate holdout calibration is disclosed",
            pairwise_use: "separate advisory Similar Set ranker trained from generated human representative comparisons",
        },
        scales: scales.into_iter().map(benchmark_scale).collect(),
        synthetic_profile_divergence: profile_divergence(),
        caveats: vec![
            "All records, labels, group membership, and feature values are generated in memory. This benchmark never opens originals, previews, an AI Test project, a CaptureOS database, or a network connection.",
            "Synthetic accuracy, F1, Brier score, and generic/majority comparisons only measure recovery of the generator's rules. They do not prove creative judgment, artistic quality, real-world recommendation usefulness, or human agreement.",
            "Timing is local-machine evidence. Re-run on target hardware; do not present it as a startup, responsiveness, product-scale, or hardware guarantee.",
            "Estimated input memory is a deterministic lower-bound allocation estimate, not operating-system peak RSS.",
            "The benchmark does not test SQLite migration, provenance materialization, profile inclusion, exclusion, activation/rollback, background-job recovery, UI behavior, or privacy enforcement; product tests cover those boundaries.",
        ],
    };
    let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
    println!("{json}");
    if let Some(output) = arguments.output {
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

fn parse_arguments() -> Arguments {
    let mut values = env::args().skip(1);
    let mut suite = "baseline".to_owned();
    let mut scales = None;
    let mut output = None;
    while let Some(value) = values.next() {
        match value.as_str() {
            "--suite" => {
                suite = values
                    .next()
                    .unwrap_or_else(|| exit_usage("--suite needs a value"));
            }
            "--records" => {
                let list = values
                    .next()
                    .unwrap_or_else(|| exit_usage("--records needs a comma-separated value"));
                let parsed = list
                    .split(',')
                    .map(|item| item.parse::<usize>().unwrap_or_else(|_| exit_usage("--records must contain positive integers")))
                    .collect::<Vec<_>>();
                if parsed.is_empty() {
                    exit_usage("--records must not be empty");
                }
                scales = Some(parsed);
            }
            "--output" => {
                output = Some(PathBuf::from(
                    values.next().unwrap_or_else(|| exit_usage("--output needs a path")),
                ));
            }
            "--help" | "-h" => exit_usage("usage: cargo run -p studio-brain-bench -- [--suite baseline] [--records 100,1000,10000,50000,100000] [--output NEW_REPORT_PATH]"),
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
    std::process::exit(2);
}

fn benchmark_config() -> StudioTrainingConfig {
    StudioTrainingConfig {
        // The bench uses short deterministic iterations to measure scaling. It reports local
        // mechanics, not an approved product activation configuration.
        minimum_eligible_decisions: 30,
        minimum_supported_classes: 2,
        minimum_examples_per_supported_class: 5,
        minimum_distinct_projects: 2,
        minimum_similar_set_decisions: 5,
        minimum_feature_coverage: 0.25,
        minimum_validation_examples: 5,
        minimum_validation_macro_f1: 0.0,
        maximum_validation_brier_score: 1.0,
        holdout_fraction: 0.20,
        training_iterations: 24,
        learning_rate: 0.12,
        l2_regularization: 0.025,
        minimum_pairwise_preferences: 5,
        high_confidence_threshold: 0.70,
        moderate_confidence_threshold: 0.52,
        minimum_confidence_margin: 0.08,
        maximum_explanations: 4,
    }
}

fn benchmark_scale(record_count: usize) -> ScaleReport {
    let generation_start = Instant::now();
    let (observations, pairwise) =
        generated_profile("studio-a", record_count, StudioRule::TechnicalFirst);
    let generation_elapsed_us = generation_start.elapsed().as_micros();
    let estimate = estimate_input_bytes(&observations, &pairwise);
    let config = benchmark_config();
    let training_start = Instant::now();
    let result = train_studio_model(&observations, &pairwise, &config)
        .expect("generated training fixture is valid");
    let training_elapsed_us = training_start.elapsed().as_micros();
    let (inference_elapsed_us, pairwise_ranking_elapsed_us) =
        if let Some(artifact) = &result.artifact {
            let inference_start = Instant::now();
            for index in 0..INFERENCE_SAMPLE_COUNT {
                let input = generated_feature(index, record_count);
                let _ = predict(artifact, &input, &config).expect("generated inference is valid");
            }
            let inference_elapsed_us = inference_start.elapsed().as_micros();
            let ranking_start = Instant::now();
            let candidates = (0..12)
                .map(|index| SimilarSetCandidate {
                    asset_id: format!("bench-rank-{index}"),
                    features: generated_feature(index, 12),
                })
                .collect::<Vec<_>>();
            let _ = rank_similar_set(artifact, &candidates, 4)
                .expect("generated ranker input is valid");
            (
                inference_elapsed_us,
                Some(ranking_start.elapsed().as_micros()),
            )
        } else {
            (0, None)
        };
    let serialized_model_bytes = result.artifact.as_ref().map(|artifact| {
        serde_json::to_vec(artifact)
            .expect("artifact serializes")
            .len()
    });
    let evaluation = result.evaluation.personal_model.as_ref().map(|personal| {
        let generic = result
            .evaluation
            .generic_baseline
            .as_ref()
            .expect("baseline exists with personal metrics");
        let majority = result
            .evaluation
            .majority_baseline
            .as_ref()
            .expect("baseline exists with personal metrics");
        EvaluationSummary {
            validation_count: personal.sample_count,
            personal_accuracy: personal.accuracy,
            personal_macro_f1: personal.macro_f1,
            personal_brier_score: personal.brier_score,
            generic_accuracy: generic.accuracy,
            majority_accuracy: majority.accuracy,
            holdout_strategy: format!("{:?}", result.evaluation.split.strategy),
        }
    });
    ScaleReport {
        record_count,
        generated_pairwise_count: pairwise.len(),
        generation_elapsed_us,
        training_elapsed_us,
        inference_elapsed_us,
        inference_sample_count: INFERENCE_SAMPLE_COUNT,
        estimated_input_lower_bound_bytes: estimate,
        serialized_model_bytes,
        readiness_status: result.readiness.status,
        evaluation,
        pairwise_ranker_available: result.pairwise_summary.model_available,
        pairwise_ranking_elapsed_us,
    }
}

#[derive(Debug, Clone, Copy)]
enum StudioRule {
    TechnicalFirst,
    DetailFirst,
}

fn generated_profile(
    prefix: &str,
    record_count: usize,
    rule: StudioRule,
) -> (Vec<TrainingObservation>, Vec<PairwisePreference>) {
    let observations = (0..record_count)
        .map(|index| {
            let feature = generated_feature(index, record_count);
            let decision = match rule {
                StudioRule::TechnicalFirst => {
                    if feature.technical_score.unwrap_or_default() > 0.68 {
                        HumanDecision::Keep
                    } else if feature.blur_score.unwrap_or_default() > 0.70 {
                        HumanDecision::Reject
                    } else {
                        HumanDecision::Review
                    }
                }
                StudioRule::DetailFirst => {
                    if feature.relative_sharpness_rank.unwrap_or_default() > 0.70 {
                        HumanDecision::Keep
                    } else if feature.timeline_boundary_score.unwrap_or_default() > 0.75 {
                        HumanDecision::Reject
                    } else {
                        HumanDecision::Review
                    }
                }
            };
            TrainingObservation {
                observation_id: format!("{prefix}-observation-{index:06}"),
                project_id: format!("{prefix}-project-{}", index % 5),
                asset_id: format!("{prefix}-asset-{index:06}"),
                decision,
                action_kind: ExplicitHumanActionKind::CullingDecision,
                occurred_at_unix_ms: index as i64 * 1000,
                captured_at_unix_ms: Some(index as i64 * 2_000),
                review_session_id: Some(format!("{prefix}-session-{}", index / 24)),
                similarity_set_id: Some(format!("{prefix}-set-{}", index / 4)),
                moment_id: Some(format!("{prefix}-moment-{}", index / 12)),
                recommendation_was_shown: false,
                generic_recommendation_at_decision: feature.generic_recommendation,
                auxiliary_human_signals: AuxiliaryHumanSignals {
                    rating: Some((index % 6) as u8),
                    starred: Some(index % 11 == 0),
                },
                features: feature,
                training_eligible: true,
            }
        })
        .collect::<Vec<_>>();
    let pairwise = (0..record_count / 4)
        .map(|index| PairwisePreference {
            preference_id: format!("{prefix}-pairwise-{index:06}"),
            project_id: format!("{prefix}-project-{}", index % 5),
            similarity_set_id: format!("{prefix}-set-{index}"),
            chosen_asset_id: format!("{prefix}-chosen-{index}"),
            alternative_asset_id: format!("{prefix}-alternative-{index}"),
            occurred_at_unix_ms: index as i64 * 5_000,
            chosen_features: generated_feature(index * 4 + 3, record_count),
            alternative_features: generated_feature(index * 4, record_count),
            training_eligible: true,
        })
        .collect();
    (observations, pairwise)
}

fn generated_feature(index: usize, record_count: usize) -> StudioFeatureInput {
    let normalized = if record_count <= 1 {
        0.5
    } else {
        (index % record_count) as f32 / (record_count - 1) as f32
    };
    let oscillation = ((index.wrapping_mul(17) % 101) as f32) / 100.0;
    let generic_recommendation = if normalized > 0.72 {
        Some(GenericRecommendation::Keep)
    } else if normalized < 0.18 {
        Some(GenericRecommendation::Reject)
    } else {
        Some(GenericRecommendation::Review)
    };
    StudioFeatureInput {
        technical_score: Some(normalized),
        sharpness_score: Some((0.15 + normalized * 0.80).min(1.0)),
        blur_score: Some(1.0 - normalized),
        exposure_score: Some((normalized * 0.7 + oscillation * 0.3).min(1.0)),
        anonymous_face_count: Some((index % 5) as u32),
        open_eyes_count: Some((index % 3) as u32),
        similar_set_size: Some((2 + index % 9) as u32),
        relative_technical_rank: Some(normalized),
        relative_sharpness_rank: Some((oscillation * 0.55 + normalized * 0.45).min(1.0)),
        moment_size: Some((4 + index % 24) as u32),
        moment_position: Some((index % 12) as f32 / 11.0),
        timeline_boundary_score: Some(oscillation),
        generic_recommendation,
        is_generic_representative: index.is_multiple_of(7),
        is_human_representative: index.is_multiple_of(13),
        semantic_evidence_available: !index.is_multiple_of(10),
    }
}

fn estimate_input_bytes(
    observations: &[TrainingObservation],
    pairwise: &[PairwisePreference],
) -> u64 {
    let observations_bytes = mem::size_of_val(observations);
    let pairwise_bytes = mem::size_of_val(pairwise);
    (observations_bytes + pairwise_bytes) as u64
}

fn profile_divergence() -> ProfileDivergenceReport {
    let config = benchmark_config();
    let (studio_a, pairwise_a) = generated_profile("profile-a", 180, StudioRule::TechnicalFirst);
    let (studio_b, pairwise_b) = generated_profile("profile-b", 180, StudioRule::DetailFirst);
    let a = train_studio_model(&studio_a, &pairwise_a, &config)
        .expect("generated Studio A fixture is valid")
        .artifact
        .expect("generated Studio A fits a candidate");
    let b = train_studio_model(&studio_b, &pairwise_b, &config)
        .expect("generated Studio B fixture is valid")
        .artifact
        .expect("generated Studio B fits a candidate");
    let probe = StudioFeatureInput {
        technical_score: Some(0.93),
        sharpness_score: Some(0.52),
        blur_score: Some(0.07),
        exposure_score: Some(0.62),
        anonymous_face_count: Some(1),
        open_eyes_count: Some(1),
        similar_set_size: Some(4),
        relative_technical_rank: Some(0.92),
        relative_sharpness_rank: Some(0.18),
        moment_size: Some(8),
        moment_position: Some(0.5),
        timeline_boundary_score: Some(0.82),
        generic_recommendation: Some(GenericRecommendation::Keep),
        is_generic_representative: false,
        is_human_representative: false,
        semantic_evidence_available: true,
    };
    let a_recommendation = predict(&a, &probe, &config)
        .expect("generated Studio A probe is valid")
        .recommendation;
    let b_recommendation = predict(&b, &probe, &config)
        .expect("generated Studio B probe is valid")
        .recommendation;
    ProfileDivergenceReport {
        generated_observation_count_per_profile: 180,
        probe_technical_score: 0.93,
        studio_a_recommendation: a_recommendation,
        studio_b_recommendation: b_recommendation,
        differs: a_recommendation != b_recommendation,
        statement: "Two isolated generated studios are trained from different explicit rules to ensure the personal artifact is not a disguised generic classifier.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_profiles_are_local_and_diverge() {
        let report = profile_divergence();
        assert!(report.differs);
    }

    #[test]
    fn generated_scale_builds_an_artifact() {
        let report = benchmark_scale(100);
        assert!(report.serialized_model_bytes.unwrap_or_default() > 100);
        assert!(report.pairwise_ranker_available);
    }
}
