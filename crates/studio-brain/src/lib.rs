//! Deterministic, local preference-learning primitives for CaptureOS Studio Brain I.
//!
//! This crate is intentionally narrow. It learns only from explicit, persisted human culling
//! actions and structural representative choices supplied by the application. It does not open
//! a source path, decode media, load model weights, use embeddings, inspect notes, identify a
//! person, make an artistic-quality claim, or contact a network service. The result is an
//! interpretable compact linear model encoded as validated JSON, not executable code.
//!
//! The product layer owns provenance, consent/exclusion state, durable jobs, model activation,
//! and recommendation history. This pure crate owns deterministic feature assembly, candidate
//! training, leakage-aware evaluation, calibration, abstention, and Similar Set ranking support.

#![forbid(unsafe_code)]

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const STUDIO_BRAIN_ALGORITHM_VERSION: &str = "m8.studio-brain.linear-softmax.v1";
pub const STUDIO_BRAIN_FEATURE_SCHEMA_VERSION: &str = "m8.studio-brain.static-features.v1";
pub const STUDIO_BRAIN_ARTIFACT_VERSION: u32 = 1;

/// The fixed, reviewed feature schema. It contains compact numeric evidence and missing-value
/// indicators only. In particular, it deliberately contains no source path, filename, note,
/// person signal, semantic embedding, raw EXIF payload, or opaque high-dimensional vector.
pub const STUDIO_FEATURE_NAMES: [&str; 31] = [
    "technical_score",
    "technical_score_available",
    "sharpness_score",
    "sharpness_score_available",
    "blur_score",
    "blur_score_available",
    "exposure_score",
    "exposure_score_available",
    "anonymous_face_count_capped",
    "anonymous_face_count_available",
    "open_eyes_fraction",
    "open_eyes_fraction_available",
    "similar_set_size_capped",
    "similar_set_size_available",
    "relative_technical_rank",
    "relative_technical_rank_available",
    "relative_sharpness_rank",
    "relative_sharpness_rank_available",
    "moment_size_capped",
    "moment_size_available",
    "moment_position",
    "moment_position_available",
    "timeline_boundary_score",
    "timeline_boundary_score_available",
    "generic_keep",
    "generic_review",
    "generic_reject",
    "generic_recommendation_available",
    "generic_representative",
    "human_representative",
    "semantic_evidence_available",
];

pub const STUDIO_FEATURE_COUNT: usize = STUDIO_FEATURE_NAMES.len();

#[derive(Debug, Error)]
pub enum StudioBrainError {
    #[error("training observation id cannot be empty")]
    EmptyObservationId,
    #[error("training observation has an empty project or asset id: {0}")]
    EmptyObservationScope(String),
    #[error("duplicate training observation id: {0}")]
    DuplicateObservationId(String),
    #[error("pairwise preference id cannot be empty")]
    EmptyPairwisePreferenceId,
    #[error("duplicate pairwise preference id: {0}")]
    DuplicatePairwisePreferenceId(String),
    #[error("pairwise preference must compare two different assets: {0}")]
    InvalidPairwisePreference(String),
    #[error("Studio Brain training configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("Studio Brain model artifact is invalid: {0}")]
    InvalidArtifact(String),
    #[error("Studio Brain model checksum did not match")]
    ChecksumMismatch,
    #[error("Studio Brain artifact serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// The only decision targets Studio Brain I learns. `Reject` remains CaptureOS metadata only;
/// neither this crate nor its caller may use a recommendation to alter source media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanDecision {
    Keep,
    Review,
    Reject,
}

impl HumanDecision {
    pub const ALL: [Self; 3] = [Self::Keep, Self::Review, Self::Reject];

    pub fn index(self) -> usize {
        match self {
            Self::Keep => 0,
            Self::Review => 1,
            Self::Reject => 2,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Keep => "Keep",
            Self::Review => "Review",
            Self::Reject => "Reject",
        }
    }
}

/// A pre-existing generic/technical recommendation. It is a separate input feature, never a
/// target label and never a substitute for a human preference observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenericRecommendation {
    Keep,
    Review,
    Reject,
}

impl GenericRecommendation {
    fn as_human_decision(self) -> HumanDecision {
        match self {
            Self::Keep => HumanDecision::Keep,
            Self::Review => HumanDecision::Review,
            Self::Reject => HumanDecision::Reject,
        }
    }
}

/// Explicit action categories admitted by M8. Passive browsing and notes are intentionally not
/// represented here, so callers cannot accidentally turn them into training data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplicitHumanActionKind {
    CullingDecision,
    Rating,
    Star,
    SimilarSetRepresentative,
    MomentRepresentative,
    BackfilledExplicitDecision,
}

/// Ratings and stars are preserved as auxiliary explicit evidence. The current classifier does
/// not coerce them into Keep/Review/Reject labels; a future explicitly-approved multitask model
/// would need its own versioned schema and evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryHumanSignals {
    pub rating: Option<u8>,
    pub starred: Option<bool>,
}

/// Compact, caller-normalized local evidence available at the time of a human action. Scores
/// are expected in 0.0..=1.0; non-finite values become unavailable rather than fabricated.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioFeatureInput {
    pub technical_score: Option<f32>,
    pub sharpness_score: Option<f32>,
    pub blur_score: Option<f32>,
    pub exposure_score: Option<f32>,
    /// Existing anonymous face-count evidence only. It never encodes a person identity.
    pub anonymous_face_count: Option<u32>,
    pub open_eyes_count: Option<u32>,
    pub similar_set_size: Option<u32>,
    pub relative_technical_rank: Option<f32>,
    pub relative_sharpness_rank: Option<f32>,
    pub moment_size: Option<u32>,
    pub moment_position: Option<f32>,
    pub timeline_boundary_score: Option<f32>,
    pub generic_recommendation: Option<GenericRecommendation>,
    pub is_generic_representative: bool,
    pub is_human_representative: bool,
    /// Presence only: no semantic score or raw embedding is admitted into the M8 feature vector.
    pub semantic_evidence_available: bool,
}

impl StudioFeatureInput {
    pub fn minimal() -> Self {
        Self::default()
    }
}

/// One persisted, explicit decision target. The application should snapshot this structure at
/// the decision time; it must not re-read mutable current recommendation state during training.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainingObservation {
    pub observation_id: String,
    pub project_id: String,
    pub asset_id: String,
    pub decision: HumanDecision,
    pub action_kind: ExplicitHumanActionKind,
    pub occurred_at_unix_ms: i64,
    pub captured_at_unix_ms: Option<i64>,
    pub review_session_id: Option<String>,
    pub similarity_set_id: Option<String>,
    pub moment_id: Option<String>,
    /// Whether an AI recommendation was actually shown before this action. This is provenance
    /// only; it is not used as a model feature.
    pub recommendation_was_shown: bool,
    /// Immutable generic recommendation snapshot, if a valid generic recommendation existed.
    pub generic_recommendation_at_decision: Option<GenericRecommendation>,
    pub auxiliary_human_signals: AuxiliaryHumanSignals,
    pub features: StudioFeatureInput,
    /// Application-enforced project inclusion and decision-level exclusion resolve here before
    /// model assembly. Retaining this flag makes candidate-run evidence auditable.
    pub training_eligible: bool,
}

/// A human representative comparison within a Similar Set. It powers a separate compact
/// pairwise ranker and never creates or changes Similar Set membership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairwisePreference {
    pub preference_id: String,
    pub project_id: String,
    pub similarity_set_id: String,
    pub chosen_asset_id: String,
    pub alternative_asset_id: String,
    pub occurred_at_unix_ms: i64,
    pub chosen_features: StudioFeatureInput,
    pub alternative_features: StudioFeatureInput,
    pub training_eligible: bool,
}

/// M8 status is persisted and interpreted by the product layer. The pure training result only
/// produces `NotReady`, `Learning`, or `Ready`; a later invalidation/failure maps to `Stale` or
/// `Error` without rewriting an earlier candidate artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudioTrainingStatus {
    NotReady,
    Learning,
    Ready,
    Stale,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutStrategy {
    /// Whole projects are held out, avoiding within-project leakage when enough projects exist.
    ProjectHoldout,
    /// A single-project profile uses stable Similar Set/Moment/time buckets as a weaker fallback.
    GroupedTemporalHoldout,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldoutSplitSummary {
    pub strategy: HoldoutStrategy,
    pub training_count: usize,
    pub validation_count: usize,
    pub held_out_group_count: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassSupport {
    pub decision: HumanDecision,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessCondition {
    pub key: String,
    pub met: bool,
    pub observed: String,
    pub required: String,
    pub message: String,
}

/// A multi-dimensional explanation for why a profile is still learning or may be safely
/// activated. It deliberately has no one-number readiness score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessAssessment {
    pub status: StudioTrainingStatus,
    pub eligible_decision_count: usize,
    pub class_support: Vec<ClassSupport>,
    pub distinct_project_count: usize,
    pub similar_set_decision_count: usize,
    pub represented_feature_ratio: f32,
    pub validation_stability_available: bool,
    pub conditions: Vec<ReadinessCondition>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioTrainingConfig {
    /// Count is only one readiness dimension; class balance, structural coverage, project
    /// diversity, feature availability, and held-out stability are assessed separately.
    pub minimum_eligible_decisions: usize,
    pub minimum_supported_classes: usize,
    pub minimum_examples_per_supported_class: usize,
    pub minimum_distinct_projects: usize,
    pub minimum_similar_set_decisions: usize,
    pub minimum_feature_coverage: f32,
    pub minimum_validation_examples: usize,
    pub minimum_validation_macro_f1: f32,
    pub maximum_validation_brier_score: f32,
    pub holdout_fraction: f32,
    pub training_iterations: usize,
    pub learning_rate: f32,
    pub l2_regularization: f32,
    pub minimum_pairwise_preferences: usize,
    pub high_confidence_threshold: f32,
    pub moderate_confidence_threshold: f32,
    pub minimum_confidence_margin: f32,
    pub maximum_explanations: usize,
}

impl Default for StudioTrainingConfig {
    fn default() -> Self {
        Self {
            minimum_eligible_decisions: 48,
            minimum_supported_classes: 2,
            minimum_examples_per_supported_class: 8,
            minimum_distinct_projects: 2,
            minimum_similar_set_decisions: 8,
            minimum_feature_coverage: 0.55,
            minimum_validation_examples: 12,
            minimum_validation_macro_f1: 0.30,
            maximum_validation_brier_score: 0.66,
            holdout_fraction: 0.20,
            training_iterations: 72,
            learning_rate: 0.12,
            l2_regularization: 0.025,
            minimum_pairwise_preferences: 8,
            high_confidence_threshold: 0.78,
            moderate_confidence_threshold: 0.60,
            minimum_confidence_margin: 0.10,
            maximum_explanations: 5,
        }
    }
}

impl StudioTrainingConfig {
    pub fn validate(&self) -> Result<(), StudioBrainError> {
        if self.minimum_eligible_decisions == 0
            || self.minimum_supported_classes == 0
            || self.minimum_supported_classes > HumanDecision::ALL.len()
            || self.minimum_examples_per_supported_class == 0
            || self.minimum_distinct_projects == 0
            || self.minimum_validation_examples == 0
            || self.training_iterations == 0
            || self.minimum_pairwise_preferences == 0
            || self.maximum_explanations == 0
        {
            return Err(StudioBrainError::InvalidConfiguration(
                "counts and iteration limits must be positive".into(),
            ));
        }
        for (name, value) in [
            ("minimum_feature_coverage", self.minimum_feature_coverage),
            (
                "minimum_validation_macro_f1",
                self.minimum_validation_macro_f1,
            ),
            (
                "maximum_validation_brier_score",
                self.maximum_validation_brier_score,
            ),
            ("holdout_fraction", self.holdout_fraction),
            ("learning_rate", self.learning_rate),
            ("l2_regularization", self.l2_regularization),
            ("high_confidence_threshold", self.high_confidence_threshold),
            (
                "moderate_confidence_threshold",
                self.moderate_confidence_threshold,
            ),
            ("minimum_confidence_margin", self.minimum_confidence_margin),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(StudioBrainError::InvalidConfiguration(format!(
                    "{name} must be finite and non-negative"
                )));
            }
        }
        if !(0.0 < self.holdout_fraction && self.holdout_fraction < 0.5) {
            return Err(StudioBrainError::InvalidConfiguration(
                "holdout_fraction must be greater than 0 and less than 0.5".into(),
            ));
        }
        if self.minimum_feature_coverage > 1.0
            || self.minimum_validation_macro_f1 > 1.0
            || self.maximum_validation_brier_score > 1.0
            || self.high_confidence_threshold > 1.0
            || self.moderate_confidence_threshold > 1.0
            || self.minimum_confidence_margin > 1.0
            || self.moderate_confidence_threshold > self.high_confidence_threshold
        {
            return Err(StudioBrainError::InvalidConfiguration(
                "probability thresholds must be within 0..=1 and moderate cannot exceed high"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Stable train/validation result. The product must only activate a candidate after its own
/// durable transaction validates the checksum and this result is `Ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioTrainingResult {
    pub artifact: Option<StudioModelArtifact>,
    pub readiness: ReadinessAssessment,
    pub evaluation: EvaluationReport,
    pub diagnostics: TrainingDiagnostics,
    pub pairwise_summary: PairwiseTrainingSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainingDiagnostics {
    pub excluded_by_policy_count: usize,
    pub invalid_numeric_values_treated_as_unavailable: usize,
    pub invalid_pairwise_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairwiseTrainingSummary {
    pub eligible_preference_count: usize,
    pub model_available: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationReport {
    pub split: HoldoutSplitSummary,
    pub personal_model: Option<ClassificationMetrics>,
    pub generic_baseline: Option<ClassificationMetrics>,
    pub majority_baseline: Option<ClassificationMetrics>,
    pub calibration: Option<Calibration>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassificationMetrics {
    pub sample_count: usize,
    pub accuracy: f32,
    pub macro_f1: f32,
    pub log_loss: f32,
    pub brier_score: f32,
    pub expected_calibration_error: f32,
    pub confusion: Vec<ConfusionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfusionRow {
    pub actual: HumanDecision,
    pub predicted_keep: usize,
    pub predicted_review: usize,
    pub predicted_reject: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Temperature is model metadata, not a user-facing certainty claim.
    pub temperature: f32,
    pub selected_on_validation_holdout: bool,
    pub method: String,
}

/// A normalizer is fit from training rows only. It is part of the signed/checked artifact so a
/// later caller cannot silently reinterpret model coefficients with a new scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureNormalizer {
    pub means: Vec<f32>,
    pub scales: Vec<f32>,
}

impl FeatureNormalizer {
    pub fn transform(&self, raw: &[f32]) -> Result<Vec<f32>, StudioBrainError> {
        if raw.len() != STUDIO_FEATURE_COUNT
            || self.means.len() != STUDIO_FEATURE_COUNT
            || self.scales.len() != STUDIO_FEATURE_COUNT
        {
            return Err(StudioBrainError::InvalidArtifact(
                "feature normalizer has an unexpected width".into(),
            ));
        }
        let mut values = Vec::with_capacity(STUDIO_FEATURE_COUNT);
        for (index, raw_value) in raw.iter().copied().enumerate() {
            let mean = self.means[index];
            let scale = self.scales[index];
            if !raw_value.is_finite() || !mean.is_finite() || !scale.is_finite() || scale <= 0.0 {
                return Err(StudioBrainError::InvalidArtifact(
                    "feature normalizer contains a non-finite or non-positive value".into(),
                ));
            }
            values.push((raw_value - mean) / scale);
        }
        Ok(values)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearSoftmaxModel {
    /// One row for Keep, Review, Reject respectively.
    pub weights: Vec<Vec<f32>>,
    pub biases: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairwiseRankModel {
    pub feature_schema_version: String,
    pub weights: Vec<f32>,
    pub trained_preference_count: usize,
}

/// Safe, static, JSON-only model material. It has no filesystem path, URL, code hook, plugin,
/// tokenizer, dynamic loader, or serialized executable payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioModelArtifact {
    pub artifact_version: u32,
    pub algorithm_version: String,
    pub feature_schema_version: String,
    pub feature_names: Vec<String>,
    pub normalizer: FeatureNormalizer,
    pub softmax: LinearSoftmaxModel,
    pub pairwise_ranker: Option<PairwiseRankModel>,
    pub calibration: Calibration,
    pub training_summary: ModelTrainingSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTrainingSummary {
    pub eligible_decision_count: usize,
    pub class_support: Vec<ClassSupport>,
    pub distinct_project_count: usize,
    pub feature_coverage: f32,
    pub training_iterations: usize,
}

impl StudioModelArtifact {
    pub fn validate(&self) -> Result<(), StudioBrainError> {
        if self.artifact_version != STUDIO_BRAIN_ARTIFACT_VERSION {
            return Err(StudioBrainError::InvalidArtifact(format!(
                "unsupported artifact version {}",
                self.artifact_version
            )));
        }
        if self.algorithm_version != STUDIO_BRAIN_ALGORITHM_VERSION
            || self.feature_schema_version != STUDIO_BRAIN_FEATURE_SCHEMA_VERSION
        {
            return Err(StudioBrainError::InvalidArtifact(
                "algorithm or feature schema version is unsupported".into(),
            ));
        }
        let expected_names = STUDIO_FEATURE_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        if self.feature_names != expected_names {
            return Err(StudioBrainError::InvalidArtifact(
                "feature names do not match the reviewed static schema".into(),
            ));
        }
        if self.normalizer.means.len() != STUDIO_FEATURE_COUNT
            || self.normalizer.scales.len() != STUDIO_FEATURE_COUNT
            || self.softmax.weights.len() != HumanDecision::ALL.len()
            || self.softmax.biases.len() != HumanDecision::ALL.len()
        {
            return Err(StudioBrainError::InvalidArtifact(
                "model coefficient dimensions are invalid".into(),
            ));
        }
        for row in &self.softmax.weights {
            if row.len() != STUDIO_FEATURE_COUNT || row.iter().any(|value| !value.is_finite()) {
                return Err(StudioBrainError::InvalidArtifact(
                    "softmax weights are non-finite or have an invalid width".into(),
                ));
            }
        }
        if self.softmax.biases.iter().any(|value| !value.is_finite())
            || self.normalizer.means.iter().any(|value| !value.is_finite())
            || self
                .normalizer
                .scales
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
            || !self.calibration.temperature.is_finite()
            || self.calibration.temperature <= 0.0
        {
            return Err(StudioBrainError::InvalidArtifact(
                "model artifact contains a non-finite value".into(),
            ));
        }
        if let Some(pairwise) = &self.pairwise_ranker {
            if pairwise.feature_schema_version != STUDIO_BRAIN_FEATURE_SCHEMA_VERSION
                || pairwise.weights.len() != STUDIO_FEATURE_COUNT
                || pairwise.weights.iter().any(|value| !value.is_finite())
            {
                return Err(StudioBrainError::InvalidArtifact(
                    "pairwise ranker is incompatible with the feature schema".into(),
                ));
            }
        }
        Ok(())
    }
}

/// A checksum over canonical serde serialization of the fixed artifact structure. It is meant
/// for persistence admission and corruption detection, not as a trust boundary against someone
/// who can change both a local database row and its checksum.
pub fn studio_model_checksum(artifact: &StudioModelArtifact) -> Result<String, StudioBrainError> {
    artifact.validate()?;
    let bytes = serde_json::to_vec(artifact)?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedModelArtifact {
    pub artifact_json: String,
    pub checksum: String,
}

pub fn encode_verified_model_artifact(
    artifact: &StudioModelArtifact,
) -> Result<VerifiedModelArtifact, StudioBrainError> {
    let checksum = studio_model_checksum(artifact)?;
    Ok(VerifiedModelArtifact {
        artifact_json: serde_json::to_string(artifact)?,
        checksum,
    })
}

pub fn decode_verified_model_artifact(
    encoded: &VerifiedModelArtifact,
) -> Result<StudioModelArtifact, StudioBrainError> {
    let artifact: StudioModelArtifact = serde_json::from_str(&encoded.artifact_json)?;
    if studio_model_checksum(&artifact)? != encoded.checksum {
        return Err(StudioBrainError::ChecksumMismatch);
    }
    Ok(artifact)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceBand {
    High,
    Moderate,
    Low,
    Abstain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionReason {
    LowTopClassSupport,
    SmallClassMargin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureContribution {
    pub feature: String,
    pub direction: String,
    pub magnitude: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenericAgreement {
    NoGenericRecommendation,
    Agrees,
    Differs,
    Abstained,
}

/// Product UI should present the band, abstention, generic comparison, and feature explanations
/// rather than showing these internal raw probabilities as a claim of objective certainty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioPrediction {
    pub recommendation: Option<HumanDecision>,
    pub confidence_band: ConfidenceBand,
    pub abstention_reason: Option<AbstentionReason>,
    pub probabilities: Vec<ClassProbability>,
    pub generic_agreement: GenericAgreement,
    pub explanations: Vec<FeatureContribution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassProbability {
    pub decision: HumanDecision,
    pub value: f32,
}

/// A Similar Set ranking is strictly advisory and has no culling effect. It is intended to
/// surface a human's own representative preference within a group, not select a winner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSetCandidate {
    pub asset_id: String,
    pub features: StudioFeatureInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedSimilarSetCandidate {
    pub asset_id: String,
    pub relative_rank_score: f32,
    pub explanations: Vec<FeatureContribution>,
}

/// Converts reviewed static evidence to the exact fixed feature vector. This function is public
/// to make persistence snapshots and explanation tests reproducible. It intentionally accepts
/// no vector/embedding argument.
pub fn assemble_feature_vector(input: &StudioFeatureInput) -> (Vec<f32>, usize) {
    let mut invalid_numeric_values = 0;
    let mut vector = Vec::with_capacity(STUDIO_FEATURE_COUNT);
    push_optional_unit(
        &mut vector,
        input.technical_score,
        &mut invalid_numeric_values,
    );
    push_optional_unit(
        &mut vector,
        input.sharpness_score,
        &mut invalid_numeric_values,
    );
    push_optional_unit(&mut vector, input.blur_score, &mut invalid_numeric_values);
    push_optional_unit(
        &mut vector,
        input.exposure_score,
        &mut invalid_numeric_values,
    );
    let face_count = input
        .anonymous_face_count
        .map(|count| (count.min(10) as f32) / 10.0);
    push_optional_unit(&mut vector, face_count, &mut invalid_numeric_values);
    let eye_fraction = match (input.open_eyes_count, input.anonymous_face_count) {
        (Some(open), Some(faces)) if faces > 0 => Some((open.min(faces) as f32) / faces as f32),
        _ => None,
    };
    push_optional_unit(&mut vector, eye_fraction, &mut invalid_numeric_values);
    let similar_set_size = input
        .similar_set_size
        .map(|size| (size.min(64) as f32) / 64.0);
    push_optional_unit(&mut vector, similar_set_size, &mut invalid_numeric_values);
    push_optional_unit(
        &mut vector,
        input.relative_technical_rank,
        &mut invalid_numeric_values,
    );
    push_optional_unit(
        &mut vector,
        input.relative_sharpness_rank,
        &mut invalid_numeric_values,
    );
    let moment_size = input.moment_size.map(|size| (size.min(128) as f32) / 128.0);
    push_optional_unit(&mut vector, moment_size, &mut invalid_numeric_values);
    push_optional_unit(
        &mut vector,
        input.moment_position,
        &mut invalid_numeric_values,
    );
    push_optional_unit(
        &mut vector,
        input.timeline_boundary_score,
        &mut invalid_numeric_values,
    );
    match input.generic_recommendation {
        Some(GenericRecommendation::Keep) => vector.extend([1.0, 0.0, 0.0, 1.0]),
        Some(GenericRecommendation::Review) => vector.extend([0.0, 1.0, 0.0, 1.0]),
        Some(GenericRecommendation::Reject) => vector.extend([0.0, 0.0, 1.0, 1.0]),
        None => vector.extend([0.0, 0.0, 0.0, 0.0]),
    }
    vector.push(if input.is_generic_representative {
        1.0
    } else {
        0.0
    });
    vector.push(if input.is_human_representative {
        1.0
    } else {
        0.0
    });
    vector.push(if input.semantic_evidence_available {
        1.0
    } else {
        0.0
    });
    debug_assert_eq!(vector.len(), STUDIO_FEATURE_COUNT);
    (vector, invalid_numeric_values)
}

fn push_optional_unit(output: &mut Vec<f32>, value: Option<f32>, invalid_count: &mut usize) {
    match value {
        Some(value) if value.is_finite() => {
            output.push(value.clamp(0.0, 1.0));
            output.push(1.0);
        }
        Some(_) => {
            // A corrupt or unsupported analyzer is unknown evidence, not a low score.
            output.extend([0.0, 0.0]);
            *invalid_count += 1;
        }
        None => output.extend([0.0, 0.0]),
    }
}

/// Train a deterministic candidate artifact. It does not write anything, start a background
/// task, activate a model, or issue recommendations. The caller decides whether to persist the
/// candidate after inspecting its multi-dimensional readiness result.
pub fn train_studio_model(
    observations: &[TrainingObservation],
    pairwise_preferences: &[PairwisePreference],
    config: &StudioTrainingConfig,
) -> Result<StudioTrainingResult, StudioBrainError> {
    config.validate()?;
    validate_observations(observations)?;
    validate_pairwise_preferences(pairwise_preferences)?;

    let mut diagnostics = TrainingDiagnostics::default();
    let mut prepared = Vec::new();
    for observation in observations {
        if !observation.training_eligible {
            diagnostics.excluded_by_policy_count += 1;
            continue;
        }
        let (features, invalid) = assemble_feature_vector(&observation.features);
        diagnostics.invalid_numeric_values_treated_as_unavailable += invalid;
        prepared.push(PreparedObservation {
            observation: observation.clone(),
            features,
        });
    }
    prepared.sort_by(|left, right| {
        left.observation
            .observation_id
            .cmp(&right.observation.observation_id)
    });

    let split = leakage_aware_split(&prepared, config);
    let training_rows = split
        .training_indices
        .iter()
        .map(|index| prepared[*index].clone())
        .collect::<Vec<_>>();
    let validation_rows = split
        .validation_indices
        .iter()
        .map(|index| prepared[*index].clone())
        .collect::<Vec<_>>();

    let can_fit = training_rows.len() >= 2 && distinct_class_count(&training_rows) >= 2;
    let (artifact, evaluation, pairwise_summary) = if can_fit {
        let normalizer = fit_normalizer(&training_rows);
        let softmax = fit_softmax(&training_rows, &normalizer, config);
        let calibration = select_temperature(&softmax, &normalizer, &validation_rows);
        let pairwise =
            fit_pairwise_ranker(pairwise_preferences, &normalizer, config, &mut diagnostics);
        let artifact = StudioModelArtifact {
            artifact_version: STUDIO_BRAIN_ARTIFACT_VERSION,
            algorithm_version: STUDIO_BRAIN_ALGORITHM_VERSION.to_owned(),
            feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.to_owned(),
            feature_names: STUDIO_FEATURE_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            normalizer,
            softmax,
            pairwise_ranker: pairwise.0,
            calibration: calibration.clone(),
            training_summary: ModelTrainingSummary {
                eligible_decision_count: prepared.len(),
                class_support: class_support(&prepared),
                distinct_project_count: distinct_projects(&prepared),
                feature_coverage: feature_coverage(&prepared),
                training_iterations: config.training_iterations,
            },
        };
        artifact.validate()?;
        let evaluation = evaluate_candidate(
            &artifact,
            &validation_rows,
            split.summary.clone(),
            calibration,
        );
        (Some(artifact), evaluation, pairwise.1)
    } else {
        let pairwise_summary = PairwiseTrainingSummary {
            eligible_preference_count: pairwise_preferences
                .iter()
                .filter(|preference| preference.training_eligible)
                .count(),
            model_available: false,
            message: "A pairwise ranker is not trained until the decision classifier has at least two represented classes.".into(),
        };
        (
            None,
            EvaluationReport {
                split: split.summary.clone(),
                personal_model: None,
                generic_baseline: None,
                majority_baseline: None,
                calibration: None,
                caveats: vec![
                    "At least two explicitly chosen decision classes are needed before a personal candidate can be fit.".into(),
                ],
            },
            pairwise_summary,
        )
    };

    let readiness = assess_readiness(&prepared, &evaluation, config);
    Ok(StudioTrainingResult {
        artifact,
        readiness,
        evaluation,
        diagnostics,
        pairwise_summary,
    })
}

/// Evaluates an already-validated local artifact against the deterministic leakage-aware
/// holdout formed from these same explicit observations. This is deliberately separate from
/// fitting so orchestration can compare a candidate and a retained previous model on one frozen
/// snapshot before activation. It never writes state or treats a recommendation as a label.
pub fn evaluate_studio_model_on_observations(
    artifact: &StudioModelArtifact,
    observations: &[TrainingObservation],
    config: &StudioTrainingConfig,
) -> Result<EvaluationReport, StudioBrainError> {
    config.validate()?;
    artifact.validate()?;
    validate_observations(observations)?;
    let mut prepared = observations
        .iter()
        .filter(|observation| observation.training_eligible)
        .map(|observation| {
            let (features, _) = assemble_feature_vector(&observation.features);
            PreparedObservation {
                observation: observation.clone(),
                features,
            }
        })
        .collect::<Vec<_>>();
    prepared.sort_by(|left, right| {
        left.observation
            .observation_id
            .cmp(&right.observation.observation_id)
    });
    let split = leakage_aware_split(&prepared, config);
    let validation_rows = split
        .validation_indices
        .iter()
        .map(|index| prepared[*index].clone())
        .collect::<Vec<_>>();
    Ok(evaluate_candidate(
        artifact,
        &validation_rows,
        split.summary,
        artifact.calibration.clone(),
    ))
}

#[derive(Debug, Clone)]
struct PreparedObservation {
    observation: TrainingObservation,
    features: Vec<f32>,
}

fn validate_observations(observations: &[TrainingObservation]) -> Result<(), StudioBrainError> {
    let mut ids = BTreeSet::new();
    for observation in observations {
        if observation.observation_id.trim().is_empty() {
            return Err(StudioBrainError::EmptyObservationId);
        }
        if observation.project_id.trim().is_empty() || observation.asset_id.trim().is_empty() {
            return Err(StudioBrainError::EmptyObservationScope(
                observation.observation_id.clone(),
            ));
        }
        if !ids.insert(observation.observation_id.clone()) {
            return Err(StudioBrainError::DuplicateObservationId(
                observation.observation_id.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_pairwise_preferences(
    preferences: &[PairwisePreference],
) -> Result<(), StudioBrainError> {
    let mut ids = BTreeSet::new();
    for preference in preferences {
        if preference.preference_id.trim().is_empty() {
            return Err(StudioBrainError::EmptyPairwisePreferenceId);
        }
        if !ids.insert(preference.preference_id.clone()) {
            return Err(StudioBrainError::DuplicatePairwisePreferenceId(
                preference.preference_id.clone(),
            ));
        }
        if preference.project_id.trim().is_empty()
            || preference.similarity_set_id.trim().is_empty()
            || preference.chosen_asset_id.trim().is_empty()
            || preference.alternative_asset_id.trim().is_empty()
            || preference.chosen_asset_id == preference.alternative_asset_id
        {
            return Err(StudioBrainError::InvalidPairwisePreference(
                preference.preference_id.clone(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct SplitSelection {
    training_indices: Vec<usize>,
    validation_indices: Vec<usize>,
    summary: HoldoutSplitSummary,
}

/// Selects whole projects where possible. For a single project it keeps Similar Set/Moment/time
/// buckets intact, then uses a stable BLAKE3 ordering. This is deliberately not a random split;
/// it is reproducible across background-job retries and can be audited from source snapshots.
fn leakage_aware_split(
    rows: &[PreparedObservation],
    config: &StudioTrainingConfig,
) -> SplitSelection {
    if rows.len() < 2 {
        return SplitSelection {
            training_indices: (0..rows.len()).collect(),
            validation_indices: Vec::new(),
            summary: HoldoutSplitSummary {
                strategy: HoldoutStrategy::Unavailable,
                training_count: rows.len(),
                validation_count: 0,
                held_out_group_count: 0,
                notes: vec![
                    "At least two eligible observations are required for a holdout split.".into(),
                ],
            },
        };
    }
    let projects = rows
        .iter()
        .map(|row| row.observation.project_id.clone())
        .collect::<BTreeSet<_>>();
    let (strategy, groups, notes) = if projects.len() >= 2 {
        (
            HoldoutStrategy::ProjectHoldout,
            rows.iter()
                .enumerate()
                .map(|(index, row)| (format!("project:{}", row.observation.project_id), index))
                .collect::<Vec<_>>(),
            vec![
                "Validation holds whole projects out of training to reduce project-specific leakage.".into(),
            ],
        )
    } else {
        (
            HoldoutStrategy::GroupedTemporalHoldout,
            rows.iter()
                .enumerate()
                .map(|(index, row)| (fallback_group_key(&row.observation), index))
                .collect::<Vec<_>>(),
            vec![
                "Only one project is available, so validation holds Similar Set, Moment, or capture-day buckets out together. This is weaker than project holdout.".into(),
            ],
        )
    };
    let mut grouped = BTreeMap::<String, Vec<usize>>::new();
    for (group, index) in groups {
        grouped.entry(group).or_default().push(index);
    }
    if grouped.len() < 2 {
        return SplitSelection {
            training_indices: (0..rows.len()).collect(),
            validation_indices: Vec::new(),
            summary: HoldoutSplitSummary {
                strategy: HoldoutStrategy::Unavailable,
                training_count: rows.len(),
                validation_count: 0,
                held_out_group_count: 0,
                notes: vec![
                    "Eligible observations belong to one structural bucket; a leakage-aware holdout is unavailable.".into(),
                ],
            },
        };
    }
    let mut keys = grouped.keys().cloned().collect::<Vec<_>>();
    keys.sort_by(|left, right| {
        stable_group_order(left)
            .cmp(&stable_group_order(right))
            .then_with(|| left.cmp(right))
    });
    let desired_groups =
        ((keys.len() as f32 * config.holdout_fraction).round() as usize).clamp(1, keys.len() - 1);
    let held_out = keys
        .into_iter()
        .take(desired_groups)
        .collect::<BTreeSet<_>>();
    let mut training_indices = Vec::new();
    let mut validation_indices = Vec::new();
    for (group, indexes) in grouped {
        if held_out.contains(&group) {
            validation_indices.extend(indexes);
        } else {
            training_indices.extend(indexes);
        }
    }
    // Keep a fit possible when an unusually large held-out structural bucket would take every
    // row. We do not split that bucket; validation simply remains unavailable in that case.
    if training_indices.is_empty() || validation_indices.is_empty() {
        return SplitSelection {
            training_indices: (0..rows.len()).collect(),
            validation_indices: Vec::new(),
            summary: HoldoutSplitSummary {
                strategy: HoldoutStrategy::Unavailable,
                training_count: rows.len(),
                validation_count: 0,
                held_out_group_count: 0,
                notes: vec![
                    "A non-empty leakage-aware train/validation split could not be formed.".into(),
                ],
            },
        };
    }
    training_indices.sort_unstable();
    validation_indices.sort_unstable();
    SplitSelection {
        summary: HoldoutSplitSummary {
            strategy,
            training_count: training_indices.len(),
            validation_count: validation_indices.len(),
            held_out_group_count: held_out.len(),
            notes,
        },
        training_indices,
        validation_indices,
    }
}

fn fallback_group_key(observation: &TrainingObservation) -> String {
    if let Some(similarity_set_id) = &observation.similarity_set_id {
        return format!("similar:{}", similarity_set_id);
    }
    if let Some(moment_id) = &observation.moment_id {
        return format!("moment:{}", moment_id);
    }
    if let Some(captured_at) = observation.captured_at_unix_ms {
        return format!("day:{}", captured_at.div_euclid(86_400_000));
    }
    // An individual with no structural/timestamp evidence must never be paired with another
    // asset merely to make a prettier metric; its own immutable id is the conservative bucket.
    format!("asset:{}", observation.asset_id)
}

fn stable_group_order(value: &str) -> u64 {
    let hash = blake3::hash(value.as_bytes());
    let bytes = hash.as_bytes();
    u64::from_le_bytes(
        bytes[0..8]
            .try_into()
            .expect("blake3 digest has eight bytes"),
    )
}

fn fit_normalizer(rows: &[PreparedObservation]) -> FeatureNormalizer {
    let count = rows.len().max(1) as f64;
    let mut means = vec![0.0f64; STUDIO_FEATURE_COUNT];
    for row in rows {
        for (index, value) in row.features.iter().enumerate() {
            means[index] += *value as f64;
        }
    }
    for mean in &mut means {
        *mean /= count;
    }
    let mut variances = vec![0.0f64; STUDIO_FEATURE_COUNT];
    for row in rows {
        for (index, value) in row.features.iter().enumerate() {
            let delta = *value as f64 - means[index];
            variances[index] += delta * delta;
        }
    }
    FeatureNormalizer {
        means: means.iter().map(|value| *value as f32).collect(),
        // A fixed floor avoids erasing a sparse-but-real binary feature and prevents division by
        // zero without changing its missing/available interpretation.
        scales: variances
            .iter()
            .map(|value| ((value / count).sqrt().max(0.05)) as f32)
            .collect(),
    }
}

fn fit_softmax(
    rows: &[PreparedObservation],
    normalizer: &FeatureNormalizer,
    config: &StudioTrainingConfig,
) -> LinearSoftmaxModel {
    let class_counts = class_counts(rows);
    let total = rows.len().max(1) as f64;
    let mut weights = vec![vec![0.0f64; STUDIO_FEATURE_COUNT]; HumanDecision::ALL.len()];
    let mut biases = HumanDecision::ALL
        .iter()
        .map(|decision| ((class_counts[decision.index()] as f64 + 1.0) / (total + 3.0)).ln())
        .collect::<Vec<_>>();
    let class_weights = HumanDecision::ALL
        .iter()
        .map(|decision| {
            let count = class_counts[decision.index()];
            if count == 0 {
                0.0
            } else {
                total / (HumanDecision::ALL.len() as f64 * count as f64)
            }
        })
        .collect::<Vec<_>>();
    let transformed = rows
        .iter()
        .map(|row| {
            normalizer
                .transform(&row.features)
                .expect("fitted normalizer is valid")
        })
        .collect::<Vec<_>>();
    for iteration in 0..config.training_iterations {
        let mut weight_gradient =
            vec![vec![0.0f64; STUDIO_FEATURE_COUNT]; HumanDecision::ALL.len()];
        let mut bias_gradient = vec![0.0f64; HumanDecision::ALL.len()];
        for (row_index, row) in rows.iter().enumerate() {
            let x = &transformed[row_index];
            let probabilities = softmax_probabilities_f64(&weights, &biases, x, 1.0);
            let target = row.observation.decision.index();
            let class_weight = class_weights[target];
            for class_index in 0..HumanDecision::ALL.len() {
                let expected = if class_index == target { 1.0 } else { 0.0 };
                let error = (probabilities[class_index] - expected) * class_weight;
                bias_gradient[class_index] += error;
                for feature_index in 0..STUDIO_FEATURE_COUNT {
                    weight_gradient[class_index][feature_index] += error * x[feature_index] as f64;
                }
            }
        }
        // Gentle deterministic decay limits oscillation while retaining an easy-to-audit update.
        let rate = config.learning_rate as f64 / (1.0 + iteration as f64 * 0.015);
        for class_index in 0..HumanDecision::ALL.len() {
            for feature_index in 0..STUDIO_FEATURE_COUNT {
                let gradient = weight_gradient[class_index][feature_index] / total
                    + config.l2_regularization as f64 * weights[class_index][feature_index];
                weights[class_index][feature_index] -= rate * gradient;
            }
            biases[class_index] -= rate * bias_gradient[class_index] / total;
        }
    }
    LinearSoftmaxModel {
        weights: weights
            .into_iter()
            .map(|row| row.into_iter().map(|value| value as f32).collect())
            .collect(),
        biases: biases.into_iter().map(|value| value as f32).collect(),
    }
}

fn softmax_probabilities_f64(
    weights: &[Vec<f64>],
    biases: &[f64],
    features: &[f32],
    temperature: f64,
) -> Vec<f64> {
    let logits = weights
        .iter()
        .zip(biases)
        .map(|(row, bias)| {
            (row.iter()
                .zip(features)
                .map(|(weight, value)| *weight * *value as f64)
                .sum::<f64>()
                + *bias)
                / temperature
        })
        .collect::<Vec<_>>();
    normalize_logits(&logits)
}

fn artifact_probabilities(
    artifact: &StudioModelArtifact,
    features: &[f32],
    temperature: f32,
) -> Result<Vec<f32>, StudioBrainError> {
    let normalized = artifact.normalizer.transform(features)?;
    let logits = artifact
        .softmax
        .weights
        .iter()
        .zip(&artifact.softmax.biases)
        .map(|(weights, bias)| {
            (weights
                .iter()
                .zip(&normalized)
                .map(|(weight, value)| *weight as f64 * *value as f64)
                .sum::<f64>()
                + *bias as f64)
                / temperature as f64
        })
        .collect::<Vec<_>>();
    Ok(normalize_logits(&logits)
        .into_iter()
        .map(|value| value as f32)
        .collect())
}

fn normalize_logits(logits: &[f64]) -> Vec<f64> {
    let maximum = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exponents = logits
        .iter()
        .map(|value| (*value - maximum).exp())
        .collect::<Vec<_>>();
    let sum = exponents.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    exponents.into_iter().map(|value| value / sum).collect()
}

fn select_temperature(
    softmax: &LinearSoftmaxModel,
    normalizer: &FeatureNormalizer,
    validation_rows: &[PreparedObservation],
) -> Calibration {
    if validation_rows.is_empty() {
        return Calibration {
            temperature: 1.0,
            selected_on_validation_holdout: false,
            method:
                "identity temperature because a leakage-aware validation holdout was unavailable"
                    .into(),
        };
    }
    let artifact = StudioModelArtifact {
        artifact_version: STUDIO_BRAIN_ARTIFACT_VERSION,
        algorithm_version: STUDIO_BRAIN_ALGORITHM_VERSION.to_owned(),
        feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.to_owned(),
        feature_names: STUDIO_FEATURE_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        normalizer: normalizer.clone(),
        softmax: softmax.clone(),
        pairwise_ranker: None,
        calibration: Calibration {
            temperature: 1.0,
            selected_on_validation_holdout: false,
            method: "temporary calibration candidate".into(),
        },
        training_summary: ModelTrainingSummary {
            eligible_decision_count: 0,
            class_support: Vec::new(),
            distinct_project_count: 0,
            feature_coverage: 0.0,
            training_iterations: 0,
        },
    };
    let mut best_temperature = 1.0;
    let mut best_loss = f64::INFINITY;
    // A tiny fixed grid makes the calibration safe, reproducible, and inspectable. It does not
    // fit an opaque calibrator or claim independent calibration evidence.
    for temperature in [0.60f32, 0.75, 0.90, 1.00, 1.15, 1.35, 1.60, 1.90] {
        let mut loss = 0.0f64;
        for row in validation_rows {
            let probabilities = artifact_probabilities(&artifact, &row.features, temperature)
                .expect("temporary artifact is valid");
            loss -= probabilities[row.observation.decision.index()]
                .max(1e-7)
                .ln() as f64;
        }
        loss /= validation_rows.len() as f64;
        if loss < best_loss - 1e-12
            || ((loss - best_loss).abs() <= 1e-12 && temperature < best_temperature)
        {
            best_loss = loss;
            best_temperature = temperature;
        }
    }
    Calibration {
        temperature: best_temperature,
        selected_on_validation_holdout: true,
        method: "fixed temperature grid minimizing log loss on the leakage-aware candidate holdout; not independent efficacy evaluation".into(),
    }
}

fn fit_pairwise_ranker(
    preferences: &[PairwisePreference],
    normalizer: &FeatureNormalizer,
    config: &StudioTrainingConfig,
    diagnostics: &mut TrainingDiagnostics,
) -> (Option<PairwiseRankModel>, PairwiseTrainingSummary) {
    let mut deltas = Vec::new();
    for preference in preferences {
        if !preference.training_eligible {
            continue;
        }
        let (chosen, chosen_invalid) = assemble_feature_vector(&preference.chosen_features);
        let (alternative, alternative_invalid) =
            assemble_feature_vector(&preference.alternative_features);
        diagnostics.invalid_numeric_values_treated_as_unavailable +=
            chosen_invalid + alternative_invalid;
        let chosen = match normalizer.transform(&chosen) {
            Ok(values) => values,
            Err(_) => {
                diagnostics.invalid_pairwise_count += 1;
                continue;
            }
        };
        let alternative = match normalizer.transform(&alternative) {
            Ok(values) => values,
            Err(_) => {
                diagnostics.invalid_pairwise_count += 1;
                continue;
            }
        };
        deltas.push(
            chosen
                .iter()
                .zip(alternative)
                .map(|(left, right)| *left as f64 - right as f64)
                .collect::<Vec<_>>(),
        );
    }
    if deltas.len() < config.minimum_pairwise_preferences {
        let count = deltas.len();
        return (
            None,
            PairwiseTrainingSummary {
                eligible_preference_count: count,
                model_available: false,
                message: format!(
                    "{count} eligible Similar Set preferences are available; {} are needed before pairwise ranking is offered.",
                    config.minimum_pairwise_preferences
                ),
            },
        );
    }
    let mut weights = vec![0.0f64; STUDIO_FEATURE_COUNT];
    let count = deltas.len() as f64;
    for iteration in 0..config.training_iterations {
        let mut gradient = vec![0.0f64; STUDIO_FEATURE_COUNT];
        for delta in &deltas {
            let score = dot_f64(&weights, delta);
            let probability = sigmoid(score);
            let error = probability - 1.0;
            for index in 0..STUDIO_FEATURE_COUNT {
                gradient[index] += error * delta[index];
            }
        }
        let rate = config.learning_rate as f64 / (1.0 + iteration as f64 * 0.015);
        for index in 0..STUDIO_FEATURE_COUNT {
            weights[index] -=
                rate * (gradient[index] / count + config.l2_regularization as f64 * weights[index]);
        }
    }
    let count = deltas.len();
    (
        Some(PairwiseRankModel {
            feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.to_owned(),
            weights: weights.into_iter().map(|value| value as f32).collect(),
            trained_preference_count: count,
        }),
        PairwiseTrainingSummary {
            eligible_preference_count: count,
            model_available: true,
            message:
                "A compact pairwise Similar Set ranker is available as advisory ordering only."
                    .into(),
        },
    )
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponent = value.exp();
        exponent / (1.0 + exponent)
    }
}

fn dot_f64(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn evaluate_candidate(
    artifact: &StudioModelArtifact,
    validation_rows: &[PreparedObservation],
    split: HoldoutSplitSummary,
    calibration: Calibration,
) -> EvaluationReport {
    if validation_rows.is_empty() {
        return EvaluationReport {
            split,
            personal_model: None,
            generic_baseline: None,
            majority_baseline: None,
            calibration: None,
            caveats: vec![
                "No leakage-aware validation holdout was available; no stability claim is made."
                    .into(),
            ],
        };
    }
    let personal = evaluate_probabilities(
        validation_rows,
        validation_rows
            .iter()
            .map(|row| {
                artifact_probabilities(artifact, &row.features, artifact.calibration.temperature)
                    .expect("validated artifact")
            })
            .collect(),
    );
    let generic_probabilities = validation_rows
        .iter()
        .map(|row| generic_baseline_probabilities(row.observation.features.generic_recommendation))
        .collect::<Vec<_>>();
    let generic = evaluate_probabilities(validation_rows, generic_probabilities);
    let majority = majority_decision(validation_rows);
    let majority_probabilities = validation_rows
        .iter()
        .map(|_| one_hot_probabilities(majority))
        .collect::<Vec<_>>();
    let majority_metrics = evaluate_probabilities(validation_rows, majority_probabilities);
    EvaluationReport {
        split,
        personal_model: Some(personal),
        generic_baseline: Some(generic),
        majority_baseline: Some(majority_metrics),
        calibration: Some(calibration),
        caveats: vec![
            "Metrics compare generated candidate outputs only against explicit local human decisions; they are not artistic truth or a reason to automatically cull media.".into(),
            "The same leakage-aware holdout selects the small temperature grid and reports candidate metrics, so it is an activation safeguard rather than an independent real-world efficacy claim.".into(),
        ],
    }
}

fn generic_baseline_probabilities(generic: Option<GenericRecommendation>) -> Vec<f32> {
    match generic {
        Some(recommendation) => one_hot_probabilities(recommendation.as_human_decision()),
        // The generic baseline abstains in the application, but metrics need a proper
        // distribution. Uniform is explicit ignorance, rather than copying a personal label.
        None => vec![1.0 / 3.0; HumanDecision::ALL.len()],
    }
}

fn one_hot_probabilities(decision: HumanDecision) -> Vec<f32> {
    HumanDecision::ALL
        .iter()
        .map(|candidate| if *candidate == decision { 1.0 } else { 0.0 })
        .collect()
}

fn majority_decision(rows: &[PreparedObservation]) -> HumanDecision {
    HumanDecision::ALL
        .iter()
        .copied()
        .max_by_key(|decision| {
            (
                class_counts(rows)[decision.index()],
                std::cmp::Reverse(decision.index()),
            )
        })
        .unwrap_or(HumanDecision::Review)
}

fn evaluate_probabilities(
    rows: &[PreparedObservation],
    probabilities: Vec<Vec<f32>>,
) -> ClassificationMetrics {
    let mut confusion = vec![vec![0usize; HumanDecision::ALL.len()]; HumanDecision::ALL.len()];
    let mut log_loss = 0.0f64;
    let mut brier = 0.0f64;
    let mut confidence_rows = Vec::with_capacity(rows.len());
    for (row, distribution) in rows.iter().zip(probabilities) {
        let actual = row.observation.decision.index();
        let predicted = highest_index(&distribution);
        confusion[actual][predicted] += 1;
        log_loss -= distribution[actual].max(1e-7).ln() as f64;
        for (class_index, probability) in distribution.iter().copied().enumerate() {
            let target = if actual == class_index { 1.0 } else { 0.0 };
            let delta = probability as f64 - target;
            brier += delta * delta;
        }
        confidence_rows.push((distribution[predicted], predicted == actual));
    }
    let count = rows.len().max(1) as f64;
    let accuracy = confusion
        .iter()
        .enumerate()
        .map(|(index, row)| row[index])
        .sum::<usize>() as f64
        / count;
    let macro_f1 = (0..HumanDecision::ALL.len())
        .map(|class_index| {
            let true_positive = confusion[class_index][class_index] as f64;
            let false_positive = (0..HumanDecision::ALL.len())
                .filter(|actual| *actual != class_index)
                .map(|actual| confusion[actual][class_index])
                .sum::<usize>() as f64;
            let false_negative = (0..HumanDecision::ALL.len())
                .filter(|predicted| *predicted != class_index)
                .map(|predicted| confusion[class_index][predicted])
                .sum::<usize>() as f64;
            let denominator = 2.0 * true_positive + false_positive + false_negative;
            if denominator == 0.0 {
                0.0
            } else {
                2.0 * true_positive / denominator
            }
        })
        .sum::<f64>()
        / HumanDecision::ALL.len() as f64;
    ClassificationMetrics {
        sample_count: rows.len(),
        accuracy: accuracy as f32,
        macro_f1: macro_f1 as f32,
        log_loss: (log_loss / count) as f32,
        brier_score: (brier / (count * HumanDecision::ALL.len() as f64)) as f32,
        expected_calibration_error: expected_calibration_error(&confidence_rows),
        confusion: HumanDecision::ALL
            .iter()
            .enumerate()
            .map(|(index, decision)| ConfusionRow {
                actual: *decision,
                predicted_keep: confusion[index][HumanDecision::Keep.index()],
                predicted_review: confusion[index][HumanDecision::Review.index()],
                predicted_reject: confusion[index][HumanDecision::Reject.index()],
            })
            .collect(),
    }
}

fn expected_calibration_error(rows: &[(f32, bool)]) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    const BINS: usize = 10;
    let mut bins = vec![Vec::<(f32, bool)>::new(); BINS];
    for row in rows {
        let index = ((row.0.clamp(0.0, 0.999_999) * BINS as f32).floor() as usize).min(BINS - 1);
        bins[index].push(*row);
    }
    bins.iter()
        .filter(|bin| !bin.is_empty())
        .map(|bin| {
            let confidence =
                bin.iter().map(|(value, _)| *value as f64).sum::<f64>() / bin.len() as f64;
            let correctness =
                bin.iter().filter(|(_, correct)| *correct).count() as f64 / bin.len() as f64;
            (confidence - correctness).abs() * bin.len() as f64 / rows.len() as f64
        })
        .sum::<f64>() as f32
}

fn highest_index(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.partial_cmp(right)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right_index.cmp(left_index))
        })
        .map(|(index, _)| index)
        .unwrap_or(HumanDecision::Review.index())
}

fn assess_readiness(
    rows: &[PreparedObservation],
    evaluation: &EvaluationReport,
    config: &StudioTrainingConfig,
) -> ReadinessAssessment {
    let supports = class_support(rows);
    let supported_classes = supports
        .iter()
        .filter(|support| support.count >= config.minimum_examples_per_supported_class)
        .count();
    let project_count = distinct_projects(rows);
    let similar_set_count = rows
        .iter()
        .filter(|row| row.observation.similarity_set_id.is_some())
        .count();
    let coverage = feature_coverage(rows);
    let validation = evaluation.personal_model.as_ref();
    let validation_stable = validation.is_some_and(|metrics| {
        metrics.sample_count >= config.minimum_validation_examples
            && metrics.macro_f1 >= config.minimum_validation_macro_f1
            && metrics.brier_score <= config.maximum_validation_brier_score
    });
    let mut conditions = vec![
        readiness_condition(
            "decision_count",
            rows.len() >= config.minimum_eligible_decisions,
            rows.len().to_string(),
            format!("at least {} eligible explicit decisions", config.minimum_eligible_decisions),
            "Explicit decision count is considered together with all other readiness dimensions.",
        ),
        readiness_condition(
            "decision_class_diversity_and_balance",
            supported_classes >= config.minimum_supported_classes,
            format!("{supported_classes} classes have at least {} examples", config.minimum_examples_per_supported_class),
            format!("at least {} represented classes", config.minimum_supported_classes),
            "A profile should not turn a one-sided history into a broad automatic recommendation.",
        ),
        readiness_condition(
            "project_diversity",
            project_count >= config.minimum_distinct_projects,
            project_count.to_string(),
            format!("at least {} included projects", config.minimum_distinct_projects),
            "Whole-project holdout protects against project-specific preference leakage when possible.",
        ),
        readiness_condition(
            "similar_set_coverage",
            similar_set_count >= config.minimum_similar_set_decisions,
            similar_set_count.to_string(),
            format!("at least {} eligible decisions with Similar Set context", config.minimum_similar_set_decisions),
            "Representative context must be observed before it can contribute to Smart Cull foundations.",
        ),
        readiness_condition(
            "feature_coverage",
            coverage >= config.minimum_feature_coverage,
            format!("{:.0}%", coverage * 100.0),
            format!("at least {:.0}%", config.minimum_feature_coverage * 100.0),
            "Unavailable analyzers are represented as missing, not negative evidence; broad available evidence is still needed.",
        ),
        readiness_condition(
            "leakage_aware_validation_stability",
            validation_stable,
            validation.map_or_else(
                || "no valid holdout".into(),
                |metrics| format!("{} rows, macro F1 {:.2}, Brier {:.2}", metrics.sample_count, metrics.macro_f1, metrics.brier_score),
            ),
            format!(
                "at least {} holdout rows, macro F1 >= {:.2}, Brier <= {:.2}",
                config.minimum_validation_examples,
                config.minimum_validation_macro_f1,
                config.maximum_validation_brier_score
            ),
            "Validation is grouped/project-held-out and is an activation safeguard, not a claim of artistic ground truth.",
        ),
    ];
    let all_met = conditions.iter().all(|condition| condition.met);
    let status = if rows.is_empty() {
        StudioTrainingStatus::NotReady
    } else if all_met {
        StudioTrainingStatus::Ready
    } else {
        StudioTrainingStatus::Learning
    };
    let message = match status {
        StudioTrainingStatus::NotReady => "No eligible explicit human preference data is available yet.".into(),
        StudioTrainingStatus::Learning => "Studio Brain is learning from explicitly included local decisions; it will abstain until every readiness condition is met.".into(),
        StudioTrainingStatus::Ready => "A candidate has met the configured local readiness dimensions and may be considered for explicit activation.".into(),
        StudioTrainingStatus::Stale | StudioTrainingStatus::Error => "Status is assigned by the durable product workflow.".into(),
    };
    // Keep condition order stable for UI snapshots and audit exports.
    conditions.shrink_to_fit();
    ReadinessAssessment {
        status,
        eligible_decision_count: rows.len(),
        class_support: supports,
        distinct_project_count: project_count,
        similar_set_decision_count: similar_set_count,
        represented_feature_ratio: coverage,
        validation_stability_available: validation.is_some(),
        conditions,
        message,
    }
}

fn readiness_condition(
    key: &str,
    met: bool,
    observed: String,
    required: String,
    message: &str,
) -> ReadinessCondition {
    ReadinessCondition {
        key: key.into(),
        met,
        observed,
        required,
        message: message.into(),
    }
}

fn class_counts(rows: &[PreparedObservation]) -> [usize; 3] {
    let mut counts = [0usize; 3];
    for row in rows {
        counts[row.observation.decision.index()] += 1;
    }
    counts
}

fn class_support(rows: &[PreparedObservation]) -> Vec<ClassSupport> {
    let counts = class_counts(rows);
    HumanDecision::ALL
        .iter()
        .map(|decision| ClassSupport {
            decision: *decision,
            count: counts[decision.index()],
        })
        .collect()
}

fn distinct_class_count(rows: &[PreparedObservation]) -> usize {
    class_counts(rows)
        .iter()
        .filter(|count| **count > 0)
        .count()
}

fn distinct_projects(rows: &[PreparedObservation]) -> usize {
    rows.iter()
        .map(|row| &row.observation.project_id)
        .collect::<BTreeSet<_>>()
        .len()
}

fn feature_coverage(rows: &[PreparedObservation]) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let present = rows
        .iter()
        .flat_map(|row| row.features.iter())
        .filter(|value| **value != 0.0)
        .count();
    present as f32 / (rows.len() * STUDIO_FEATURE_COUNT) as f32
}

/// Runs a single, advisory prediction. It never writes a decision or hides media. Callers should
/// additionally require a persisted profile status of `Ready` before showing this as personal
/// guidance; otherwise the artifact remains a candidate for evaluation only.
pub fn predict(
    artifact: &StudioModelArtifact,
    input: &StudioFeatureInput,
    config: &StudioTrainingConfig,
) -> Result<StudioPrediction, StudioBrainError> {
    artifact.validate()?;
    config.validate()?;
    let (features, _) = assemble_feature_vector(input);
    let probabilities =
        artifact_probabilities(artifact, &features, artifact.calibration.temperature)?;
    let ranked = sorted_probability_indexes(&probabilities);
    let top_index = ranked[0];
    let runner_up_index = ranked[1];
    let top = probabilities[top_index];
    let margin = top - probabilities[runner_up_index];
    let (recommendation, confidence_band, abstention_reason) = if top
        >= config.high_confidence_threshold
        && margin >= config.minimum_confidence_margin * 1.5
    {
        (
            Some(HumanDecision::ALL[top_index]),
            ConfidenceBand::High,
            None,
        )
    } else if top >= config.moderate_confidence_threshold
        && margin >= config.minimum_confidence_margin
    {
        (
            Some(HumanDecision::ALL[top_index]),
            ConfidenceBand::Moderate,
            None,
        )
    } else if top >= 0.50 && margin >= config.minimum_confidence_margin * 0.5 {
        (
            Some(HumanDecision::ALL[top_index]),
            ConfidenceBand::Low,
            None,
        )
    } else if top < config.moderate_confidence_threshold {
        (
            None,
            ConfidenceBand::Abstain,
            Some(AbstentionReason::LowTopClassSupport),
        )
    } else {
        (
            None,
            ConfidenceBand::Abstain,
            Some(AbstentionReason::SmallClassMargin),
        )
    };
    let generic_agreement = match (input.generic_recommendation, recommendation) {
        (None, _) => GenericAgreement::NoGenericRecommendation,
        (Some(_), None) => GenericAgreement::Abstained,
        (Some(generic), Some(personal)) if generic.as_human_decision() == personal => {
            GenericAgreement::Agrees
        }
        (Some(_), Some(_)) => GenericAgreement::Differs,
    };
    let explanations = recommendation
        .map(|decision| {
            feature_contributions(
                artifact,
                &features,
                decision.index(),
                config.maximum_explanations,
            )
        })
        .transpose()?
        .unwrap_or_default();
    Ok(StudioPrediction {
        recommendation,
        confidence_band,
        abstention_reason,
        probabilities: HumanDecision::ALL
            .iter()
            .enumerate()
            .map(|(index, decision)| ClassProbability {
                decision: *decision,
                value: probabilities[index],
            })
            .collect(),
        generic_agreement,
        explanations,
    })
}

fn sorted_probability_indexes(probabilities: &[f32]) -> Vec<usize> {
    let mut indexes = (0..probabilities.len()).collect::<Vec<_>>();
    indexes.sort_by(|left, right| {
        probabilities[*right]
            .partial_cmp(&probabilities[*left])
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.cmp(right))
    });
    indexes
}

fn feature_contributions(
    artifact: &StudioModelArtifact,
    raw_features: &[f32],
    class_index: usize,
    maximum: usize,
) -> Result<Vec<FeatureContribution>, StudioBrainError> {
    let normalized = artifact.normalizer.transform(raw_features)?;
    let mut contributions = STUDIO_FEATURE_NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let value = artifact.softmax.weights[class_index][index] * normalized[index];
            FeatureContribution {
                feature: (*name).to_owned(),
                direction: if value >= 0.0 {
                    "supports".into()
                } else {
                    "pushes away".into()
                },
                magnitude: value.abs(),
            }
        })
        .filter(|contribution| contribution.magnitude > 0.000_1)
        .collect::<Vec<_>>();
    contributions.sort_by(|left, right| {
        right
            .magnitude
            .partial_cmp(&left.magnitude)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.feature.cmp(&right.feature))
    });
    contributions.truncate(maximum);
    Ok(contributions)
}

/// Advisory ordering within an already-existing Similar Set. The pairwise model is intentionally
/// separate from the three-way culling classifier and does not return a Keep/Review/Reject label.
pub fn rank_similar_set(
    artifact: &StudioModelArtifact,
    candidates: &[SimilarSetCandidate],
    maximum_explanations: usize,
) -> Result<Vec<RankedSimilarSetCandidate>, StudioBrainError> {
    artifact.validate()?;
    let Some(ranker) = &artifact.pairwise_ranker else {
        return Ok(Vec::new());
    };
    if maximum_explanations == 0 {
        return Err(StudioBrainError::InvalidConfiguration(
            "maximum_explanations must be positive".into(),
        ));
    }
    let mut ranked = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let (raw, _) = assemble_feature_vector(&candidate.features);
        let normalized = artifact.normalizer.transform(&raw)?;
        let score = ranker
            .weights
            .iter()
            .zip(&normalized)
            .map(|(weight, value)| *weight * *value)
            .sum::<f32>();
        let mut explanations = STUDIO_FEATURE_NAMES
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let contribution = ranker.weights[index] * normalized[index];
                FeatureContribution {
                    feature: (*name).to_owned(),
                    direction: if contribution >= 0.0 {
                        "supports relative rank".into()
                    } else {
                        "pushes away from relative rank".into()
                    },
                    magnitude: contribution.abs(),
                }
            })
            .filter(|contribution| contribution.magnitude > 0.000_1)
            .collect::<Vec<_>>();
        explanations.sort_by(|left, right| {
            right
                .magnitude
                .partial_cmp(&left.magnitude)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.feature.cmp(&right.feature))
        });
        explanations.truncate(maximum_explanations);
        ranked.push(RankedSimilarSetCandidate {
            asset_id: candidate.asset_id.clone(),
            relative_rank_score: score,
            explanations,
        });
    }
    ranked.sort_by(|left, right| {
        right
            .relative_rank_score
            .partial_cmp(&left.relative_rank_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.asset_id.cmp(&right.asset_id))
    });
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(technical: f32, generic: Option<GenericRecommendation>) -> StudioFeatureInput {
        StudioFeatureInput {
            technical_score: Some(technical),
            sharpness_score: Some(technical),
            blur_score: Some(1.0 - technical),
            exposure_score: Some(technical),
            anonymous_face_count: Some(1),
            open_eyes_count: Some(1),
            similar_set_size: Some(4),
            relative_technical_rank: Some(technical),
            relative_sharpness_rank: Some(technical),
            moment_size: Some(8),
            moment_position: Some(0.5),
            timeline_boundary_score: Some(0.2),
            generic_recommendation: generic,
            is_generic_representative: technical > 0.7,
            is_human_representative: false,
            semantic_evidence_available: true,
        }
    }

    fn observation(
        studio: &str,
        index: usize,
        decision: HumanDecision,
        technical: f32,
    ) -> TrainingObservation {
        TrainingObservation {
            observation_id: format!("{studio}-observation-{index:03}"),
            project_id: format!("{studio}-project-{}", index % 3),
            asset_id: format!("{studio}-asset-{index:03}"),
            decision,
            action_kind: ExplicitHumanActionKind::CullingDecision,
            occurred_at_unix_ms: index as i64 * 1_000,
            captured_at_unix_ms: Some(index as i64 * 86_400_000),
            review_session_id: Some(format!("{studio}-session-{}", index / 8)),
            similarity_set_id: Some(format!("{studio}-set-{}", index / 4)),
            moment_id: Some(format!("{studio}-moment-{}", index / 10)),
            recommendation_was_shown: false,
            generic_recommendation_at_decision: None,
            auxiliary_human_signals: AuxiliaryHumanSignals::default(),
            features: feature(technical, None),
            training_eligible: true,
        }
    }

    fn test_config() -> StudioTrainingConfig {
        StudioTrainingConfig {
            minimum_eligible_decisions: 18,
            minimum_supported_classes: 2,
            minimum_examples_per_supported_class: 4,
            minimum_distinct_projects: 2,
            minimum_similar_set_decisions: 4,
            minimum_feature_coverage: 0.25,
            minimum_validation_examples: 3,
            minimum_validation_macro_f1: 0.0,
            maximum_validation_brier_score: 1.0,
            holdout_fraction: 0.34,
            training_iterations: 96,
            learning_rate: 0.16,
            l2_regularization: 0.02,
            minimum_pairwise_preferences: 4,
            high_confidence_threshold: 0.60,
            moderate_confidence_threshold: 0.45,
            minimum_confidence_margin: 0.03,
            maximum_explanations: 4,
        }
    }

    #[test]
    fn static_feature_vector_marks_invalid_scores_as_unavailable() {
        let input = StudioFeatureInput {
            technical_score: Some(f32::NAN),
            semantic_evidence_available: true,
            ..StudioFeatureInput::default()
        };
        let (vector, invalid) = assemble_feature_vector(&input);
        assert_eq!(vector.len(), STUDIO_FEATURE_COUNT);
        assert_eq!(invalid, 1);
        assert_eq!(vector[0..2], [0.0, 0.0]);
        assert_eq!(vector[30], 1.0);
    }

    #[test]
    fn checksum_round_trip_rejects_tampering() {
        let observations = (0..30)
            .map(|index| {
                let decision = if index % 4 == 0 {
                    HumanDecision::Keep
                } else {
                    HumanDecision::Review
                };
                observation("checksum", index, decision, index as f32 / 30.0)
            })
            .collect::<Vec<_>>();
        let result = train_studio_model(&observations, &[], &test_config()).unwrap();
        let artifact = result.artifact.unwrap();
        let mut encoded = encode_verified_model_artifact(&artifact).unwrap();
        assert_eq!(decode_verified_model_artifact(&encoded).unwrap(), artifact);
        let mut tampered: serde_json::Value = serde_json::from_str(&encoded.artifact_json).unwrap();
        tampered["trainingSummary"]["trainingIterations"] = serde_json::json!(97);
        encoded.artifact_json = serde_json::to_string(&tampered).unwrap();
        assert_eq!(
            decode_verified_model_artifact(&encoded)
                .unwrap_err()
                .to_string(),
            "Studio Brain model checksum did not match"
        );
    }

    #[test]
    fn retained_artifact_evaluation_reuses_the_candidate_grouped_holdout() {
        let observations = (0..36)
            .map(|index| {
                let technical = index as f32 / 35.0;
                let decision = if technical > 0.70 {
                    HumanDecision::Keep
                } else if technical < 0.20 {
                    HumanDecision::Reject
                } else {
                    HumanDecision::Review
                };
                observation("retained-evaluation", index, decision, technical)
            })
            .collect::<Vec<_>>();
        let config = test_config();
        let trained = train_studio_model(&observations, &[], &config).unwrap();
        let artifact = trained.artifact.as_ref().unwrap();
        let reevaluated =
            evaluate_studio_model_on_observations(artifact, &observations, &config).unwrap();
        assert_eq!(reevaluated.split, trained.evaluation.split);
        assert_eq!(
            reevaluated
                .personal_model
                .as_ref()
                .map(|metric| metric.sample_count),
            trained
                .evaluation
                .personal_model
                .as_ref()
                .map(|metric| metric.sample_count)
        );
    }

    #[test]
    fn project_holdout_never_splits_a_project() {
        let rows = (0..18)
            .map(|index| {
                observation(
                    "split",
                    index,
                    if index % 2 == 0 {
                        HumanDecision::Keep
                    } else {
                        HumanDecision::Review
                    },
                    index as f32 / 18.0,
                )
            })
            .collect::<Vec<_>>();
        let prepared = rows
            .into_iter()
            .map(|observation| PreparedObservation {
                features: assemble_feature_vector(&observation.features).0,
                observation,
            })
            .collect::<Vec<_>>();
        let split = leakage_aware_split(&prepared, &test_config());
        assert_eq!(split.summary.strategy, HoldoutStrategy::ProjectHoldout);
        let train_projects = split
            .training_indices
            .iter()
            .map(|index| prepared[*index].observation.project_id.as_str())
            .collect::<BTreeSet<_>>();
        let validation_projects = split
            .validation_indices
            .iter()
            .map(|index| prepared[*index].observation.project_id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(train_projects.is_disjoint(&validation_projects));
    }

    #[test]
    fn a_handful_of_decisions_remains_learning() {
        let observations = (0..4)
            .map(|index| observation("new", index, HumanDecision::Keep, 0.9))
            .collect::<Vec<_>>();
        let result =
            train_studio_model(&observations, &[], &StudioTrainingConfig::default()).unwrap();
        assert_eq!(result.readiness.status, StudioTrainingStatus::Learning);
        assert!(result.artifact.is_none());
    }

    #[test]
    fn different_studios_learn_different_explicit_preferences() {
        let studio_a = (0..45)
            .map(|index| {
                let technical = index as f32 / 44.0;
                let decision = if technical > 0.58 {
                    HumanDecision::Keep
                } else if technical < 0.20 {
                    HumanDecision::Reject
                } else {
                    HumanDecision::Review
                };
                observation("studio-a", index, decision, technical)
            })
            .collect::<Vec<_>>();
        let studio_b = (0..45)
            .map(|index| {
                let technical = index as f32 / 44.0;
                let decision = if technical < 0.35 {
                    HumanDecision::Keep
                } else if technical > 0.82 {
                    HumanDecision::Reject
                } else {
                    HumanDecision::Review
                };
                observation("studio-b", index, decision, technical)
            })
            .collect::<Vec<_>>();
        let config = test_config();
        let a = train_studio_model(&studio_a, &[], &config)
            .unwrap()
            .artifact
            .unwrap();
        let b = train_studio_model(&studio_b, &[], &config)
            .unwrap()
            .artifact
            .unwrap();
        let a_prediction = predict(&a, &feature(0.92, None), &config).unwrap();
        let b_prediction = predict(&b, &feature(0.92, None), &config).unwrap();
        assert_eq!(a_prediction.recommendation, Some(HumanDecision::Keep));
        assert_ne!(a_prediction.recommendation, b_prediction.recommendation);
    }

    #[test]
    fn pairwise_ranker_orders_advisory_similar_set_candidates() {
        let observations = (0..36)
            .map(|index| {
                let technical = index as f32 / 35.0;
                observation(
                    "pair",
                    index,
                    if technical > 0.5 {
                        HumanDecision::Keep
                    } else {
                        HumanDecision::Review
                    },
                    technical,
                )
            })
            .collect::<Vec<_>>();
        let preferences = (0..12)
            .map(|index| PairwisePreference {
                preference_id: format!("pair-pref-{index}"),
                project_id: format!("pair-project-{}", index % 3),
                similarity_set_id: format!("pair-set-{index}"),
                chosen_asset_id: format!("chosen-{index}"),
                alternative_asset_id: format!("alternative-{index}"),
                occurred_at_unix_ms: index as i64,
                chosen_features: feature(0.92, None),
                alternative_features: feature(0.08, None),
                training_eligible: true,
            })
            .collect::<Vec<_>>();
        let artifact = train_studio_model(&observations, &preferences, &test_config())
            .unwrap()
            .artifact
            .unwrap();
        let ranked = rank_similar_set(
            &artifact,
            &[
                SimilarSetCandidate {
                    asset_id: "low".into(),
                    features: feature(0.1, None),
                },
                SimilarSetCandidate {
                    asset_id: "high".into(),
                    features: feature(0.9, None),
                },
            ],
            3,
        )
        .unwrap();
        assert_eq!(ranked.first().unwrap().asset_id, "high");
    }

    #[test]
    fn prediction_abstains_when_classes_are_too_close() {
        let observations = (0..36)
            .map(|index| {
                observation(
                    "uncertain",
                    index,
                    if index % 2 == 0 {
                        HumanDecision::Keep
                    } else {
                        HumanDecision::Review
                    },
                    0.5,
                )
            })
            .collect::<Vec<_>>();
        let mut config = test_config();
        config.minimum_confidence_margin = 0.40;
        let artifact = train_studio_model(&observations, &[], &config)
            .unwrap()
            .artifact
            .unwrap();
        let prediction = predict(&artifact, &feature(0.5, None), &config).unwrap();
        assert_eq!(prediction.confidence_band, ConfidenceBand::Abstain);
        assert!(prediction.recommendation.is_none());
    }
}
