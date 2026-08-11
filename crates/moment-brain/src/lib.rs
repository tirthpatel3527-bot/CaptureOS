//! Deterministic, local structural timeline analysis for CaptureOS Moment Brain.
//!
//! This crate deliberately works on already-authorized, project-scoped metadata and durable
//! derived evidence. It never opens source paths, decodes media, loads a model, contacts a
//! network service, changes Similar Sets, or changes a human decision. The embedding values it
//! receives are opaque vectors from an already-approved local semantic provider.
//!
//! A `MomentSegment` is a useful structural grouping of nearby captures, not a claim about an
//! event, identity, relationship, emotion, or missing shot. In particular, the label policy is
//! intentionally closed: it can compose only the reviewed generic concepts below, or select an
//! exact phrase explicitly supplied by the photographer through project/checklist metadata.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const MOMENT_BRAIN_ALGORITHM_VERSION: &str = "m7.moment-brain.structural.v1";
pub const MOMENT_LABEL_POLICY_VERSION: &str = "m7.moment-label.closed-vocabulary.v1";
pub const UNTITLED_MOMENT_LABEL: &str = "Untitled Moment";

/// A local structural analysis error. Invalid evidence attached to an individual asset is
/// recorded in [`AnalysisDiagnostic`] and does not fail the rest of the project. These errors
/// are reserved for malformed request identity/configuration that makes deterministic analysis
/// impossible.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MomentBrainError {
    #[error("timeline asset id cannot be empty")]
    EmptyAssetId,
    #[error("timeline contains duplicate asset id: {0}")]
    DuplicateAssetId(String),
    #[error("label candidate id cannot be empty")]
    EmptyLabelCandidateId,
    #[error("timeline contains duplicate label candidate id: {0}")]
    DuplicateLabelCandidateId(String),
    #[error("timeline analysis configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("incremental update referred to an unknown asset id: {0}")]
    UnknownChangedAssetId(String),
}

/// The intentionally limited structural modes used by `MomentBrainBench` and product analysis.
/// `Combined` is the normal local M7 mode. These modes only select boundary evidence; they do
/// not alter customer data or make a semantic provider mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMode {
    TimeOnly,
    SemanticOnly,
    #[default]
    Combined,
}

/// A deliberately opaque model/version compatibility key. Vectors are compared only when this
/// key and dimensionality agree. It lets persistence carry M6 provider/model invalidation
/// forward without coupling this pure crate to a particular model or model pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticVector {
    pub compatibility_key: String,
    pub values: Vec<f32>,
}

/// Orientation is optional because a missing or unsupported metadata value is not evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

/// Existing human decisions are presentation inputs only. They never become a culling action
/// and are not created or changed by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingHumanDecision {
    Keep,
    Reject,
    Review,
}

/// Read-only human presentation signals from the existing local catalog. A reject is not a
/// disqualification: it simply receives no positive representative preference.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanPresentationSignals {
    pub decision: Option<ExistingHumanDecision>,
    /// Existing CaptureOS rating, normally in the 0..=5 range. Invalid values are ignored with
    /// a diagnostic rather than forcing a project-wide failure.
    pub rating: Option<u8>,
    pub starred: Option<bool>,
}

/// The only input Moment Brain needs for one still-photo media asset. It intentionally contains
/// no source path, person identity, face crop, geolocation, or editable source metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineAssetInput {
    pub asset_id: String,
    /// UTC Unix milliseconds from a selected local capture-time source. Missing values remain
    /// ungrouped rather than being guessed from filename or import order.
    pub captured_at_unix_ms: Option<i64>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub orientation: Option<Orientation>,
    /// Existing anonymous face-count evidence only. `None` means unavailable/unknown; zero is
    /// a real known count. It is weak boundary evidence and never identity evidence.
    pub anonymous_face_count: Option<u32>,
    /// `Some(vec![])` means Similar Set membership was checked and none exists. An absent value
    /// means the repository could not supply Similar Set evidence.
    pub similar_set_ids: Option<Vec<String>>,
    /// A repository-derived sequence number, if safely available. The engine never parses a
    /// filename itself and treats this as weak evidence only.
    pub filename_sequence: Option<u64>,
    pub semantic_embedding: Option<SemanticVector>,
    /// Existing local technical evidence normalized by the caller into 0.0..=1.0. It is a
    /// transparent presentation factor, not a creative or artistic-quality judgment.
    pub technical_presentation_score: Option<f32>,
    pub human_presentation: HumanPresentationSignals,
}

impl TimelineAssetInput {
    /// Compact fixture constructor. Product callers should populate all locally available
    /// evidence rather than manufacturing values for unavailable analyzers.
    pub fn minimal(asset_id: impl Into<String>, captured_at_unix_ms: Option<i64>) -> Self {
        Self {
            asset_id: asset_id.into(),
            captured_at_unix_ms,
            camera_model: None,
            lens_model: None,
            orientation: None,
            anonymous_face_count: None,
            similar_set_ids: None,
            filename_sequence: None,
            semantic_embedding: None,
            technical_presentation_score: None,
            human_presentation: HumanPresentationSignals::default(),
        }
    }
}

/// The finite generic vocabulary approved for automatic composition. It deliberately excludes
/// people by identity, relationships, wedding stages, emotions, and event names. The engine may
/// output a phrase containing other terminology only if the photographer explicitly supplied
/// that exact phrase as [`LabelCandidateKind::HumanProjectPhrase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenericLabelConcept {
    Portraits,
    Group,
    Indoor,
    Outdoor,
    Water,
    Boat,
    CloseUp,
    WideScene,
    OnePerson,
    MultiplePeople,
}

impl GenericLabelConcept {
    pub const REVIEWED: [Self; 10] = [
        Self::Portraits,
        Self::Group,
        Self::Indoor,
        Self::Outdoor,
        Self::Water,
        Self::Boat,
        Self::CloseUp,
        Self::WideScene,
        Self::OnePerson,
        Self::MultiplePeople,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Portraits => "portraits",
            Self::Group => "group",
            Self::Indoor => "indoor",
            Self::Outdoor => "outdoor",
            Self::Water => "water",
            Self::Boat => "boat",
            Self::CloseUp => "close-up",
            Self::WideScene => "wide scene",
            Self::OnePerson => "one person",
            Self::MultiplePeople => "multiple people",
        }
    }
}

/// A label candidate is either a fixed reviewed generic concept or exact human-provided text.
/// There is intentionally no `FreeForm` variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LabelCandidateKind {
    ReviewedGeneric {
        concept: GenericLabelConcept,
    },
    /// This phrase is not generated or expanded by Moment Brain. It must originate from an
    /// explicit photographer project/checklist field, and is still used only after a compatible
    /// local semantic match clears the conservative threshold.
    HumanProjectPhrase {
        phrase: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCandidate {
    pub candidate_id: String,
    pub kind: LabelCandidateKind,
    pub semantic_embedding: Option<SemanticVector>,
}

/// Boundary signal weights are normalized over evidence that was actually available. This keeps
/// a missing model, camera field, or face analyzer from pretending to be negative evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundarySignalWeights {
    pub time_gap: f32,
    pub semantic_transition: f32,
    pub camera_and_lens: f32,
    pub orientation: f32,
    pub anonymous_face_count: f32,
    pub similar_set_continuity: f32,
    pub filename_adjacency: f32,
}

impl Default for BoundarySignalWeights {
    fn default() -> Self {
        Self {
            time_gap: 0.36,
            semantic_transition: 0.34,
            camera_and_lens: 0.10,
            orientation: 0.06,
            anonymous_face_count: 0.04,
            similar_set_continuity: 0.08,
            filename_adjacency: 0.02,
        }
    }
}

/// Tunable, versioned thresholds. Time is normalized from the current project's measured
/// cadence (`median` and `p90`) instead of a universal "minutes between events" rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryThresholds {
    pub moderate_boundary_score: f32,
    pub strong_boundary_score: f32,
    /// Cosine similarity at or above this is continuity support, never a positive boundary.
    pub semantic_continuity_similarity: f32,
    /// Cosine similarity at or below this is strong semantic transition support.
    pub semantic_boundary_similarity: f32,
    /// A filename gap is deliberately very weak and cannot on its own create a segment.
    pub filename_gap_for_weak_boundary: u64,
}

impl Default for BoundaryThresholds {
    fn default() -> Self {
        Self {
            moderate_boundary_score: 0.32,
            strong_boundary_score: 0.60,
            semantic_continuity_similarity: 0.88,
            semantic_boundary_similarity: 0.58,
            filename_gap_for_weak_boundary: 12,
        }
    }
}

/// Conservative automatic-label policy. Similarity values remain developer evidence; normal UI
/// should only render the resulting concise label or `Untitled Moment`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelPolicy {
    pub minimum_similarity: f32,
    pub minimum_margin: f32,
    pub maximum_generic_concepts: usize,
    pub minimum_supporting_vectors: usize,
}

impl Default for LabelPolicy {
    fn default() -> Self {
        Self {
            minimum_similarity: 0.32,
            minimum_margin: 0.06,
            maximum_generic_concepts: 3,
            minimum_supporting_vectors: 1,
        }
    }
}

/// Weights used solely for the suggested representative. Missing factors are renormalized, and
/// no factor calls a frame artistically "best".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentativePolicy {
    pub semantic_centrality: f32,
    pub technical_evidence: f32,
    pub human_presentation: f32,
}

impl Default for RepresentativePolicy {
    fn default() -> Self {
        Self {
            semantic_centrality: 0.45,
            technical_evidence: 0.30,
            human_presentation: 0.25,
        }
    }
}

/// Configuration is owned by the caller and persisted with its analysis run by higher layers.
/// `max_context_assets` bounds local semantic context; it is not a project/catalog size limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineAnalysisConfig {
    pub mode: AnalysisMode,
    pub max_context_assets: usize,
    pub minimum_cadence_samples: usize,
    pub boundary_weights: BoundarySignalWeights,
    pub boundary_thresholds: BoundaryThresholds,
    pub label_policy: LabelPolicy,
    pub representative_policy: RepresentativePolicy,
}

impl Default for TimelineAnalysisConfig {
    fn default() -> Self {
        Self {
            mode: AnalysisMode::Combined,
            max_context_assets: 4,
            minimum_cadence_samples: 3,
            boundary_weights: BoundarySignalWeights::default(),
            boundary_thresholds: BoundaryThresholds::default(),
            label_policy: LabelPolicy::default(),
            representative_policy: RepresentativePolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineAnalysisRequest {
    pub assets: Vec<TimelineAssetInput>,
    pub label_candidates: Vec<LabelCandidate>,
    pub config: TimelineAnalysisConfig,
}

/// A signal is present only when CaptureOS supplied actual local evidence. `Unavailable` is a
/// first-class result so callers can explain an incomplete analysis rather than inventing a
/// boundary signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundarySignal {
    TimeGap,
    SemanticTransition,
    CameraAndLens,
    Orientation,
    AnonymousFaceCount,
    SimilarSetContinuity,
    FilenameAdjacency,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalEvidence {
    pub signal: BoundarySignal,
    pub availability: EvidenceAvailability,
    /// Positive support for separating the adjacent assets, in 0.0..=1.0. This is diagnostic
    /// evidence, not a user-facing probability or object-detection confidence.
    pub boundary_support: Option<f32>,
    /// Positive support for keeping the adjacent assets continuous, in 0.0..=1.0.
    pub continuity_support: Option<f32>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryCategory {
    Continuous,
    Moderate,
    Strong,
    InsufficientEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryEvidence {
    pub left_asset_id: String,
    pub right_asset_id: String,
    pub category: BoundaryCategory,
    /// Kept for persisted developer evidence and benchmarks. Normal UI should use `category`
    /// and the signal explanations instead of presenting this as a precise probability.
    pub combined_boundary_support: Option<f32>,
    pub available_weight: f32,
    pub signals: Vec<SignalEvidence>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CadenceStatus {
    Available,
    InsufficientSamples,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineCadence {
    pub status: CadenceStatus,
    pub positive_gap_samples: usize,
    pub median_gap_ms: Option<u64>,
    pub p90_gap_ms: Option<u64>,
    /// The per-project normalizer used for time-gap support, when available.
    pub normalizer_gap_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    MissingCaptureTimestamp,
    InvalidSemanticEmbedding,
    InvalidTechnicalPresentationScore,
    InvalidRating,
    InvalidLabelCandidateEmbedding,
    EmptyHumanProjectPhrase,
    IncompatibleSemanticEmbedding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisDiagnostic {
    pub code: DiagnosticCode,
    pub asset_id: Option<String>,
    pub candidate_id: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UngroupedReason {
    MissingCaptureTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UngroupedAsset {
    pub asset_id: String,
    pub reason: UngroupedReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSuggestionState {
    Suggested,
    Abstained,
    SemanticEvidenceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSuggestionSource {
    ReviewedGenericConcepts,
    HumanProjectPhrase,
}

/// The AI suggestion is intentionally separate from a human Moment name. Persistence must store
/// a human rename independently and make it authoritative for normal presentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLabelSuggestion {
    pub state: LabelSuggestionState,
    pub display_label: String,
    pub source: Option<LabelSuggestionSource>,
    pub candidate_id: Option<String>,
    pub supporting_generic_concepts: Vec<GenericLabelConcept>,
    pub semantic_similarity: Option<f32>,
    pub competing_similarity: Option<f32>,
    pub compatibility_key: Option<String>,
    pub supporting_vector_count: usize,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentativeFactorKind {
    SemanticCentrality,
    TechnicalEvidence,
    ExistingHumanPresentation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentativeFactor {
    pub kind: RepresentativeFactorKind,
    pub normalized_value: f32,
    pub configured_weight: f32,
    pub contribution: f32,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentativeSelectionMethod {
    EvidenceWeighted,
    StableChronologyFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentativeSuggestion {
    pub asset_id: String,
    pub method: RepresentativeSelectionMethod,
    pub score: Option<f32>,
    pub factors: Vec<RepresentativeFactor>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentSemanticSummary {
    pub compatibility_key: Option<String>,
    pub compatible_vector_count: usize,
    pub unavailable_or_incompatible_asset_count: usize,
}

/// One automatic structural group. The `start_asset_id`/`end_asset_id` pair is a deterministic
/// analysis anchor, not a durable database ID; persistence creates project-scoped record IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentSegment {
    pub ordinal: usize,
    pub start_asset_id: String,
    pub end_asset_id: String,
    pub start_captured_at_unix_ms: i64,
    pub end_captured_at_unix_ms: i64,
    pub asset_ids: Vec<String>,
    pub semantic_summary: SegmentSemanticSummary,
    pub ai_label: AiLabelSuggestion,
    pub ai_representative: RepresentativeSuggestion,
}

/// Counters deliberately expose bounded work to fixtures/benchmarks. `semantic_context_vector`
/// reads are capped by `boundary_count * 2 * max_context_assets`, not catalog size squared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineAnalysisWork {
    pub chronological_asset_count: usize,
    pub boundaries_evaluated: usize,
    pub semantic_context_vector_reads: usize,
    pub max_context_assets: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineAnalysis {
    pub algorithm_version: String,
    pub label_policy_version: String,
    pub mode: AnalysisMode,
    pub ordered_asset_ids: Vec<String>,
    pub cadence: TimelineCadence,
    pub boundaries: Vec<BoundaryEvidence>,
    pub moments: Vec<MomentSegment>,
    pub ungrouped_assets: Vec<UngroupedAsset>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
    pub work: TimelineAnalysisWork,
}

/// The only local signal that may support a camera-clock advisory: two distinct camera-model
/// labels occurred in the same existing Similar Set, and their nearest observed capture times
/// repeatedly disagree by a stable amount. This is not camera identity, a clock correction, or
/// a claim that either timestamp is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockOffsetDiagnosticMethod {
    ExistingSimilarSetNearestCrossCameraCaptureTime,
}

/// A conservative, project-local advisory derived from existing Similar Set co-membership.
///
/// `camera_a` and `camera_b` are the distinct nonempty camera-model labels as supplied by the
/// local catalog, ordered deterministically. A positive `median_offset_ms` means that observed
/// timestamps for `camera_b` were later than those for `camera_a`; it never changes either
/// timestamp. Callers should present this only as a possible camera time offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockOffsetDiagnostic {
    pub camera_a: String,
    pub camera_b: String,
    pub median_offset_ms: i64,
    pub median_absolute_deviation_ms: u64,
    /// Number of independent existing Similar Sets that contributed one nearest cross-camera
    /// comparison each.
    pub comparison_count: usize,
    /// Opaque local Similar Set identifiers retained only as developer/persistence evidence.
    pub supporting_similar_set_ids: Vec<String>,
    pub method: ClockOffsetDiagnosticMethod,
}

/// A basic, conservative incremental boundary. Persistence can replace the returned tail window
/// and then overlay protected human split/merge/representative events. An out-of-order capture
/// timestamp deliberately requests a full rebuild instead of silently moving a protected Moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalAnalysisRequest {
    pub previous_latest_captured_at_unix_ms: Option<i64>,
    pub changed_asset_ids: Vec<String>,
    pub context_assets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncrementalDisposition {
    NoChanges,
    TailReanalyzed,
    UngroupedAssetsUpdated,
    FullRebuildRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalTimelineAnalysis {
    pub disposition: IncrementalDisposition,
    pub reason: String,
    pub affected_asset_ids: Vec<String>,
    pub window_start_asset_id: Option<String>,
    pub analysis: Option<TimelineAnalysis>,
}

/// Bounded append-only input for persistence workers that have already fetched the small
/// preceding context and new chronological tail. Unlike [`analyze_incremental`], this API never
/// scans or sorts the full catalog: its work is bounded by the supplied context plus append
/// batch. The repository remains responsible for deciding that its durable index/view supplied
/// the complete changed tail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendOnlyTailAnalysisRequest {
    pub previous_latest_captured_at_unix_ms: i64,
    pub preceding_context_assets: Vec<TimelineAssetInput>,
    pub appended_assets: Vec<TimelineAssetInput>,
    pub label_candidates: Vec<LabelCandidate>,
    pub config: TimelineAnalysisConfig,
}

/// Analyze a current-project still-photo timeline deterministically.
///
/// The implementation sorts only metadata records (`O(n log n)`) and evaluates each neighboring
/// boundary once. Semantic context is bounded by `config.max_context_assets`, so this routine
/// never makes an all-pairs vector comparison. It is safe for a caller to page/load durable
/// metadata before constructing this request; this crate itself never accesses storage.
pub fn analyze_timeline(
    request: &TimelineAnalysisRequest,
) -> Result<TimelineAnalysis, MomentBrainError> {
    validate_request(request)?;

    let mut diagnostics = collect_input_diagnostics(request);
    let mut chronological: Vec<&TimelineAssetInput> = Vec::with_capacity(request.assets.len());
    let mut ungrouped_assets = Vec::new();

    for asset in &request.assets {
        if asset.captured_at_unix_ms.is_some() {
            chronological.push(asset);
        } else {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::MissingCaptureTimestamp,
                asset_id: Some(asset.asset_id.clone()),
                candidate_id: None,
                detail:
                    "Capture time is unavailable; the asset was left ungrouped rather than guessed."
                        .into(),
            });
            ungrouped_assets.push(UngroupedAsset {
                asset_id: asset.asset_id.clone(),
                reason: UngroupedReason::MissingCaptureTimestamp,
            });
        }
    }

    chronological.sort_by(|left, right| stable_asset_order(left, right));
    ungrouped_assets.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));

    let cadence = calculate_cadence(&chronological, request.config.minimum_cadence_samples);
    let mut boundaries = Vec::with_capacity(chronological.len().saturating_sub(1));
    let mut semantic_context_vector_reads = 0usize;

    for boundary_index in 1..chronological.len() {
        let (boundary, context_reads) =
            analyze_boundary(&chronological, boundary_index, &cadence, &request.config);
        semantic_context_vector_reads = semantic_context_vector_reads.saturating_add(context_reads);
        boundaries.push(boundary);
    }

    let moments = build_segments(
        &chronological,
        &boundaries,
        &request.label_candidates,
        &request.config,
        &mut diagnostics,
    );

    Ok(TimelineAnalysis {
        algorithm_version: MOMENT_BRAIN_ALGORITHM_VERSION.into(),
        label_policy_version: MOMENT_LABEL_POLICY_VERSION.into(),
        mode: request.config.mode,
        ordered_asset_ids: chronological
            .iter()
            .map(|asset| asset.asset_id.clone())
            .collect(),
        cadence,
        boundaries,
        moments,
        ungrouped_assets,
        diagnostics,
        work: TimelineAnalysisWork {
            chronological_asset_count: chronological.len(),
            boundaries_evaluated: chronological.len().saturating_sub(1),
            semantic_context_vector_reads,
            max_context_assets: request.config.max_context_assets,
        },
    })
}

/// Derive only well-supported possible camera time offsets from current-project local evidence.
///
/// Each existing Similar Set contributes at most one observation: its nearest pair of captures
/// with distinct nonempty camera-model labels. A result requires at least three independent
/// Similar Sets, a median disagreement of at least one minute, and no contributing observation
/// more than fifteen seconds from that median. The strict consistency rule deliberately abstains
/// on mixed or ambiguous evidence. This function is read-only: it neither mutates inputs nor
/// corrects catalog/source timestamps, and it does not inspect media, embeddings, or paths.
///
/// Group members are sorted only within their Similar Set, making the work `O(n log n)` over
/// supplied Similar Set membership records rather than an all-pairs comparison across a project.
pub fn derive_clock_offset_diagnostics(
    assets: &[TimelineAssetInput],
) -> Vec<ClockOffsetDiagnostic> {
    const MINIMUM_INDEPENDENT_COMPARISONS: usize = 3;
    const MINIMUM_ABSOLUTE_OFFSET_MS: u64 = 60_000;
    const MAXIMUM_OBSERVATION_DEVIATION_MS: u64 = 15_000;

    let mut members_by_similar_set: BTreeMap<String, Vec<ClockTimestampObservation>> =
        BTreeMap::new();

    for asset in assets {
        let (Some(captured_at_unix_ms), Some(camera_label)) = (
            asset.captured_at_unix_ms,
            normalized_clock_camera_label(asset.camera_model.as_deref()),
        ) else {
            continue;
        };
        let Some(similar_set_ids) = &asset.similar_set_ids else {
            continue;
        };

        // An upstream repository should not supply duplicate group ids for an asset, but a
        // local BTreeSet keeps one malformed record from inflating independent evidence.
        let unique_similar_set_ids: BTreeSet<&str> = similar_set_ids
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|similar_set_id| !similar_set_id.is_empty())
            .collect();
        for similar_set_id in unique_similar_set_ids {
            members_by_similar_set
                .entry(similar_set_id.to_owned())
                .or_default()
                .push(ClockTimestampObservation {
                    camera_label: camera_label.clone(),
                    captured_at_unix_ms,
                });
        }
    }

    let mut observations_by_camera_pair: BTreeMap<(String, String), Vec<ClockOffsetObservation>> =
        BTreeMap::new();
    for (similar_set_id, members) in members_by_similar_set {
        let Some((camera_a, camera_b, captured_at_a, captured_at_b)) =
            nearest_cross_camera_pair(&members)
        else {
            continue;
        };
        let Some(offset_ms) = signed_timestamp_difference(captured_at_b, captured_at_a) else {
            // Only pathological i64-extreme values can overflow the signed representation. An
            // advisory must abstain rather than clamp or manufacture a precise offset.
            continue;
        };
        observations_by_camera_pair
            .entry((camera_a, camera_b))
            .or_default()
            .push(ClockOffsetObservation {
                similar_set_id,
                offset_ms,
            });
    }

    let mut diagnostics = Vec::new();
    for ((camera_a, camera_b), observations) in observations_by_camera_pair {
        if observations.len() < MINIMUM_INDEPENDENT_COMPARISONS {
            continue;
        }
        let mut offsets: Vec<i64> = observations
            .iter()
            .map(|observation| observation.offset_ms)
            .collect();
        let median_offset_ms = median_i64(&mut offsets);
        if median_offset_ms.unsigned_abs() < MINIMUM_ABSOLUTE_OFFSET_MS {
            continue;
        }

        let mut deviations: Vec<u64> = observations
            .iter()
            .map(|observation| observation.offset_ms.abs_diff(median_offset_ms))
            .collect();
        let median_absolute_deviation_ms = median_u64(&mut deviations);
        // A robust MAD alone can hide one materially contradictory group. Require every
        // independent comparison to agree tightly before emitting an advisory.
        if deviations
            .iter()
            .any(|deviation| *deviation > MAXIMUM_OBSERVATION_DEVIATION_MS)
        {
            continue;
        }

        let mut supporting_similar_set_ids: Vec<String> = observations
            .into_iter()
            .map(|observation| observation.similar_set_id)
            .collect();
        supporting_similar_set_ids.sort();
        supporting_similar_set_ids.dedup();
        diagnostics.push(ClockOffsetDiagnostic {
            camera_a,
            camera_b,
            median_offset_ms,
            median_absolute_deviation_ms,
            comparison_count: supporting_similar_set_ids.len(),
            supporting_similar_set_ids,
            method: ClockOffsetDiagnosticMethod::ExistingSimilarSetNearestCrossCameraCaptureTime,
        });
    }
    diagnostics
}

#[derive(Debug, Clone)]
struct ClockTimestampObservation {
    camera_label: String,
    captured_at_unix_ms: i64,
}

#[derive(Debug, Clone)]
struct ClockOffsetObservation {
    similar_set_id: String,
    offset_ms: i64,
}

fn normalized_clock_camera_label(camera_model: Option<&str>) -> Option<String> {
    let camera_model = camera_model?.trim();
    (!camera_model.is_empty()).then(|| camera_model.to_owned())
}

/// Returns the nearest observed pair with distinct camera-model labels. A globally nearest
/// cross-label pair must occur at a transition between adjacent timestamps after sorting.
fn nearest_cross_camera_pair(
    members: &[ClockTimestampObservation],
) -> Option<(String, String, i64, i64)> {
    if members.len() < 2 {
        return None;
    }
    let mut ordered_members = members.to_vec();
    ordered_members.sort_by(|left, right| {
        left.captured_at_unix_ms
            .cmp(&right.captured_at_unix_ms)
            .then_with(|| left.camera_label.cmp(&right.camera_label))
    });

    let mut nearest: Option<(u64, String, String, i64, i64)> = None;
    for pair in ordered_members.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        if left.camera_label == right.camera_label {
            continue;
        }
        let (camera_a, captured_at_a, camera_b, captured_at_b) =
            if left.camera_label < right.camera_label {
                (
                    left.camera_label.clone(),
                    left.captured_at_unix_ms,
                    right.camera_label.clone(),
                    right.captured_at_unix_ms,
                )
            } else {
                (
                    right.camera_label.clone(),
                    right.captured_at_unix_ms,
                    left.camera_label.clone(),
                    left.captured_at_unix_ms,
                )
            };
        let candidate = (
            captured_at_a.abs_diff(captured_at_b),
            camera_a,
            camera_b,
            captured_at_a,
            captured_at_b,
        );
        let is_nearer = match &nearest {
            None => true,
            Some(current) => candidate.cmp(current) == Ordering::Less,
        };
        if is_nearer {
            nearest = Some(candidate);
        }
    }
    nearest.map(|(_, camera_a, camera_b, captured_at_a, captured_at_b)| {
        (camera_a, camera_b, captured_at_a, captured_at_b)
    })
}

fn signed_timestamp_difference(later: i64, earlier: i64) -> Option<i64> {
    i64::try_from(i128::from(later) - i128::from(earlier)).ok()
}

fn median_i64(values: &mut [i64]) -> i64 {
    debug_assert!(!values.is_empty());
    values.sort_unstable();
    let upper = values[values.len() / 2];
    if values.len() % 2 == 1 {
        return upper;
    }
    let lower = values[values.len() / 2 - 1];
    // Averaging in i128 avoids overflow for malformed extreme timestamps.
    i64::try_from((i128::from(lower) + i128::from(upper)) / 2)
        .expect("the average of two i64 values always fits in i64")
}

fn median_u64(values: &mut [u64]) -> u64 {
    debug_assert!(!values.is_empty());
    values.sort_unstable();
    let upper = values[values.len() / 2];
    if values.len() % 2 == 1 {
        return upper;
    }
    let lower = values[values.len() / 2 - 1];
    u64::try_from((u128::from(lower) + u128::from(upper)) / 2)
        .expect("the average of two u64 values always fits in u64")
}

/// Validate and analyze an append-only tail against a full current request. This is a
/// conservative compatibility/fallback API: it sorts metadata for the supplied request to prove
/// that no post-tail asset was omitted. For the actual background incremental worker, prefer
/// [`analyze_append_only_tail`], which accepts a repository-bounded preceding context and does
/// not scan the full catalog.
pub fn analyze_incremental(
    request: &TimelineAnalysisRequest,
    update: &IncrementalAnalysisRequest,
) -> Result<IncrementalTimelineAnalysis, MomentBrainError> {
    validate_request(request)?;

    if update.changed_asset_ids.is_empty() {
        return Ok(IncrementalTimelineAnalysis {
            disposition: IncrementalDisposition::NoChanges,
            reason: "No changed asset ids were supplied.".into(),
            affected_asset_ids: Vec::new(),
            window_start_asset_id: None,
            analysis: None,
        });
    }
    if update.context_assets == 0 {
        return Err(MomentBrainError::InvalidConfiguration(
            "incremental context_assets must be greater than zero".into(),
        ));
    }

    let mut assets_by_id = BTreeMap::new();
    for asset in &request.assets {
        assets_by_id.insert(asset.asset_id.as_str(), asset);
    }

    let mut changed_ids = BTreeSet::new();
    for changed_id in &update.changed_asset_ids {
        if !assets_by_id.contains_key(changed_id.as_str()) {
            return Err(MomentBrainError::UnknownChangedAssetId(changed_id.clone()));
        }
        changed_ids.insert(changed_id.as_str());
    }

    let mut chronological: Vec<&TimelineAssetInput> = request
        .assets
        .iter()
        .filter(|asset| asset.captured_at_unix_ms.is_some())
        .collect();
    chronological.sort_by(|left, right| stable_asset_order(left, right));

    let changed_assets: Vec<&TimelineAssetInput> = changed_ids
        .iter()
        .filter_map(|id| assets_by_id.get(id).copied())
        .collect();
    let changed_chronological: Vec<&TimelineAssetInput> = changed_assets
        .iter()
        .copied()
        .filter(|asset| asset.captured_at_unix_ms.is_some())
        .collect();
    let changed_ungrouped: Vec<&TimelineAssetInput> = changed_assets
        .iter()
        .copied()
        .filter(|asset| asset.captured_at_unix_ms.is_none())
        .collect();

    if changed_chronological.is_empty() {
        let mut partial_request = request.clone();
        partial_request.assets = changed_ungrouped.into_iter().cloned().collect();
        let analysis = analyze_timeline(&partial_request)?;
        let mut affected_asset_ids: Vec<String> =
            changed_ids.into_iter().map(str::to_owned).collect();
        affected_asset_ids.sort();
        return Ok(IncrementalTimelineAnalysis {
            disposition: IncrementalDisposition::UngroupedAssetsUpdated,
            reason: "Only assets without capture timestamps changed; no chronological Moment was inferred."
                .into(),
            affected_asset_ids,
            window_start_asset_id: None,
            analysis: Some(analysis),
        });
    }

    let Some(previous_latest) = update.previous_latest_captured_at_unix_ms else {
        return Ok(full_rebuild_required(
            "No prior latest capture timestamp is available for a safe incremental analysis.",
        ));
    };

    if changed_chronological.iter().any(|asset| {
        asset
            .captured_at_unix_ms
            .is_some_and(|captured_at| captured_at <= previous_latest)
    }) {
        return Ok(full_rebuild_required(
            "A changed capture is at or before the prior timeline tail; a full rebuild is required to avoid silently moving earlier Moments.",
        ));
    }

    // If a caller omitted a post-tail asset from `changed_asset_ids`, a tail-only replacement
    // could leave an inconsistent boundary. Refuse that optimistic path and request a rebuild.
    if chronological.iter().any(|asset| {
        asset
            .captured_at_unix_ms
            .is_some_and(|captured_at| captured_at > previous_latest)
            && !changed_ids.contains(asset.asset_id.as_str())
    }) {
        return Ok(full_rebuild_required(
            "The current request contains post-tail captures that were not declared changed; a full rebuild is required for deterministic coverage.",
        ));
    }

    let first_changed_index = chronological
        .iter()
        .position(|asset| changed_ids.contains(asset.asset_id.as_str()))
        .expect("changed chronological assets were resolved above");
    let context = update.context_assets.max(request.config.max_context_assets);
    let window_start_index = first_changed_index.saturating_sub(context);
    let mut partial_assets: Vec<TimelineAssetInput> = chronological[window_start_index..]
        .iter()
        .map(|asset| (*asset).clone())
        .collect();
    partial_assets.extend(changed_ungrouped.into_iter().cloned());

    let mut partial_request = request.clone();
    partial_request.assets = partial_assets;
    let analysis = analyze_timeline(&partial_request)?;
    let affected_asset_ids = analysis
        .ordered_asset_ids
        .iter()
        .cloned()
        .chain(
            analysis
                .ungrouped_assets
                .iter()
                .map(|asset| asset.asset_id.clone()),
        )
        .collect();

    Ok(IncrementalTimelineAnalysis {
        disposition: IncrementalDisposition::TailReanalyzed,
        reason:
            "Only an append-only chronological tail and a bounded local context were reanalyzed."
                .into(),
        affected_asset_ids,
        window_start_asset_id: chronological
            .get(window_start_index)
            .map(|asset| asset.asset_id.clone()),
        analysis: Some(analysis),
    })
}

/// Analyze an explicitly bounded append-only tail without reading the rest of a project.
///
/// Every chronological appended asset must be later than the prior successful timeline tail;
/// every chronological preceding-context asset must be at or before that tail. Otherwise this
/// returns [`IncrementalDisposition::FullRebuildRequired`] rather than guessing how an inserted
/// capture should affect older structural Moments or protected human edits.
pub fn analyze_append_only_tail(
    request: &AppendOnlyTailAnalysisRequest,
) -> Result<IncrementalTimelineAnalysis, MomentBrainError> {
    let mut assets = request.preceding_context_assets.clone();
    assets.extend(request.appended_assets.iter().cloned());
    let bounded_request = TimelineAnalysisRequest {
        assets,
        label_candidates: request.label_candidates.clone(),
        config: request.config.clone(),
    };
    validate_request(&bounded_request)?;

    let has_invalid_context = request.preceding_context_assets.iter().any(|asset| {
        asset
            .captured_at_unix_ms
            .is_some_and(|captured_at| captured_at > request.previous_latest_captured_at_unix_ms)
    });
    if has_invalid_context {
        return Ok(full_rebuild_required(
            "The supplied preceding context contains a post-tail capture; a full rebuild is required for deterministic ordering.",
        ));
    }
    let has_out_of_order_append = request.appended_assets.iter().any(|asset| {
        asset
            .captured_at_unix_ms
            .is_some_and(|captured_at| captured_at <= request.previous_latest_captured_at_unix_ms)
    });
    if has_out_of_order_append {
        return Ok(full_rebuild_required(
            "An appended capture is at or before the prior timeline tail; a full rebuild is required to preserve older Moment boundaries and human overrides.",
        ));
    }
    if request.appended_assets.is_empty() {
        return Ok(IncrementalTimelineAnalysis {
            disposition: IncrementalDisposition::NoChanges,
            reason: "No appended assets were supplied.".into(),
            affected_asset_ids: Vec::new(),
            window_start_asset_id: None,
            analysis: None,
        });
    }

    let analysis = analyze_timeline(&bounded_request)?;
    let mut affected_asset_ids = analysis.ordered_asset_ids.clone();
    affected_asset_ids.extend(
        analysis
            .ungrouped_assets
            .iter()
            .map(|asset| asset.asset_id.clone()),
    );
    let only_ungrouped = request
        .appended_assets
        .iter()
        .all(|asset| asset.captured_at_unix_ms.is_none());
    let disposition = if only_ungrouped {
        IncrementalDisposition::UngroupedAssetsUpdated
    } else {
        IncrementalDisposition::TailReanalyzed
    };
    let reason = if only_ungrouped {
        "Only appended assets without capture timestamps were supplied; no chronological Moment was inferred."
    } else {
        "A repository-bounded preceding context and append-only chronological tail were analyzed without scanning the full catalog."
    };
    Ok(IncrementalTimelineAnalysis {
        disposition,
        reason: reason.into(),
        affected_asset_ids,
        window_start_asset_id: analysis.ordered_asset_ids.first().cloned(),
        analysis: Some(analysis),
    })
}

fn full_rebuild_required(reason: &str) -> IncrementalTimelineAnalysis {
    IncrementalTimelineAnalysis {
        disposition: IncrementalDisposition::FullRebuildRequired,
        reason: reason.into(),
        affected_asset_ids: Vec::new(),
        window_start_asset_id: None,
        analysis: None,
    }
}

fn validate_request(request: &TimelineAnalysisRequest) -> Result<(), MomentBrainError> {
    validate_config(&request.config)?;

    let mut asset_ids = BTreeSet::new();
    for asset in &request.assets {
        if asset.asset_id.trim().is_empty() {
            return Err(MomentBrainError::EmptyAssetId);
        }
        if !asset_ids.insert(asset.asset_id.as_str()) {
            return Err(MomentBrainError::DuplicateAssetId(asset.asset_id.clone()));
        }
    }

    let mut candidate_ids = BTreeSet::new();
    for candidate in &request.label_candidates {
        if candidate.candidate_id.trim().is_empty() {
            return Err(MomentBrainError::EmptyLabelCandidateId);
        }
        if !candidate_ids.insert(candidate.candidate_id.as_str()) {
            return Err(MomentBrainError::DuplicateLabelCandidateId(
                candidate.candidate_id.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_config(config: &TimelineAnalysisConfig) -> Result<(), MomentBrainError> {
    if config.max_context_assets == 0 {
        return Err(MomentBrainError::InvalidConfiguration(
            "max_context_assets must be greater than zero".into(),
        ));
    }
    if config.minimum_cadence_samples == 0 {
        return Err(MomentBrainError::InvalidConfiguration(
            "minimum_cadence_samples must be greater than zero".into(),
        ));
    }
    if config.label_policy.maximum_generic_concepts == 0 {
        return Err(MomentBrainError::InvalidConfiguration(
            "maximum_generic_concepts must be greater than zero".into(),
        ));
    }
    if config.label_policy.minimum_supporting_vectors == 0 {
        return Err(MomentBrainError::InvalidConfiguration(
            "minimum_supporting_vectors must be greater than zero".into(),
        ));
    }

    let bounded = [
        (
            "moderate_boundary_score",
            config.boundary_thresholds.moderate_boundary_score,
        ),
        (
            "strong_boundary_score",
            config.boundary_thresholds.strong_boundary_score,
        ),
        (
            "semantic_continuity_similarity",
            config.boundary_thresholds.semantic_continuity_similarity,
        ),
        (
            "semantic_boundary_similarity",
            config.boundary_thresholds.semantic_boundary_similarity,
        ),
        (
            "label minimum_similarity",
            config.label_policy.minimum_similarity,
        ),
        ("label minimum_margin", config.label_policy.minimum_margin),
    ];
    for (name, value) in bounded {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(MomentBrainError::InvalidConfiguration(format!(
                "{name} must be finite and within 0.0..=1.0"
            )));
        }
    }
    if config.boundary_thresholds.moderate_boundary_score
        > config.boundary_thresholds.strong_boundary_score
    {
        return Err(MomentBrainError::InvalidConfiguration(
            "moderate_boundary_score cannot exceed strong_boundary_score".into(),
        ));
    }
    if config.boundary_thresholds.semantic_boundary_similarity
        > config.boundary_thresholds.semantic_continuity_similarity
    {
        return Err(MomentBrainError::InvalidConfiguration(
            "semantic_boundary_similarity cannot exceed semantic_continuity_similarity".into(),
        ));
    }
    for (name, weight) in [
        ("time_gap", config.boundary_weights.time_gap),
        (
            "semantic_transition",
            config.boundary_weights.semantic_transition,
        ),
        ("camera_and_lens", config.boundary_weights.camera_and_lens),
        ("orientation", config.boundary_weights.orientation),
        (
            "anonymous_face_count",
            config.boundary_weights.anonymous_face_count,
        ),
        (
            "similar_set_continuity",
            config.boundary_weights.similar_set_continuity,
        ),
        (
            "filename_adjacency",
            config.boundary_weights.filename_adjacency,
        ),
        (
            "representative semantic_centrality",
            config.representative_policy.semantic_centrality,
        ),
        (
            "representative technical_evidence",
            config.representative_policy.technical_evidence,
        ),
        (
            "representative human_presentation",
            config.representative_policy.human_presentation,
        ),
    ] {
        if !weight.is_finite() || weight < 0.0 {
            return Err(MomentBrainError::InvalidConfiguration(format!(
                "{name} must be finite and non-negative"
            )));
        }
    }
    Ok(())
}

fn stable_asset_order(left: &TimelineAssetInput, right: &TimelineAssetInput) -> Ordering {
    left.captured_at_unix_ms
        .cmp(&right.captured_at_unix_ms)
        .then_with(|| left.asset_id.cmp(&right.asset_id))
}

fn collect_input_diagnostics(request: &TimelineAnalysisRequest) -> Vec<AnalysisDiagnostic> {
    let mut diagnostics = Vec::new();
    for asset in &request.assets {
        if asset
            .semantic_embedding
            .as_ref()
            .is_some_and(|embedding| !is_valid_semantic_vector(embedding))
        {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::InvalidSemanticEmbedding,
                asset_id: Some(asset.asset_id.clone()),
                candidate_id: None,
                detail: "The local semantic vector is empty, non-finite, zero-length, or lacks a compatibility key; it was treated as unavailable."
                    .into(),
            });
        }
        if asset
            .technical_presentation_score
            .is_some_and(|score| !is_unit_interval(score))
        {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::InvalidTechnicalPresentationScore,
                asset_id: Some(asset.asset_id.clone()),
                candidate_id: None,
                detail: "Technical presentation evidence was outside 0.0..=1.0 or non-finite and was ignored."
                    .into(),
            });
        }
        if asset
            .human_presentation
            .rating
            .is_some_and(|rating| rating > 5)
        {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::InvalidRating,
                asset_id: Some(asset.asset_id.clone()),
                candidate_id: None,
                detail: "The existing rating was outside 0..=5 and was ignored for representative selection."
                    .into(),
            });
        }
    }
    for candidate in &request.label_candidates {
        if candidate
            .semantic_embedding
            .as_ref()
            .is_some_and(|embedding| !is_valid_semantic_vector(embedding))
        {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::InvalidLabelCandidateEmbedding,
                asset_id: None,
                candidate_id: Some(candidate.candidate_id.clone()),
                detail: "The candidate vector is empty, non-finite, zero-length, or lacks a compatibility key; that candidate was skipped."
                    .into(),
            });
        }
        if matches!(
            &candidate.kind,
            LabelCandidateKind::HumanProjectPhrase { phrase } if phrase.trim().is_empty()
        ) {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::EmptyHumanProjectPhrase,
                asset_id: None,
                candidate_id: Some(candidate.candidate_id.clone()),
                detail:
                    "An empty human project/checklist phrase cannot be matched and was skipped."
                        .into(),
            });
        }
    }
    diagnostics
}

fn is_unit_interval(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn is_valid_semantic_vector(vector: &SemanticVector) -> bool {
    !vector.compatibility_key.trim().is_empty()
        && !vector.values.is_empty()
        && vector.values.iter().all(|value| value.is_finite())
        && squared_norm(&vector.values).is_some_and(|norm| norm > 0.0)
}

fn squared_norm(values: &[f32]) -> Option<f64> {
    let norm = values.iter().fold(0.0f64, |sum, value| {
        sum + f64::from(*value) * f64::from(*value)
    });
    norm.is_finite().then_some(norm)
}

fn calculate_cadence(
    chronological: &[&TimelineAssetInput],
    minimum_samples: usize,
) -> TimelineCadence {
    let mut gaps = Vec::with_capacity(chronological.len().saturating_sub(1));
    for pair in chronological.windows(2) {
        let left = pair[0]
            .captured_at_unix_ms
            .expect("chronological inputs are filtered to timestamped assets");
        let right = pair[1]
            .captured_at_unix_ms
            .expect("chronological inputs are filtered to timestamped assets");
        if let Some(gap) = right
            .checked_sub(left)
            .and_then(|difference| u64::try_from(difference).ok())
        {
            if gap > 0 {
                gaps.push(gap);
            }
        }
    }
    gaps.sort_unstable();
    if gaps.len() < minimum_samples {
        return TimelineCadence {
            status: CadenceStatus::InsufficientSamples,
            positive_gap_samples: gaps.len(),
            median_gap_ms: None,
            p90_gap_ms: None,
            normalizer_gap_ms: None,
        };
    }

    let median = percentile_nearest_rank(&gaps, 0.50);
    let p90 = percentile_nearest_rank(&gaps, 0.90);
    let normalizer = p90.max(median.saturating_mul(2)).max(1);
    TimelineCadence {
        status: CadenceStatus::Available,
        positive_gap_samples: gaps.len(),
        median_gap_ms: Some(median),
        p90_gap_ms: Some(p90),
        normalizer_gap_ms: Some(normalizer),
    }
}

fn percentile_nearest_rank(sorted: &[u64], percentile: f64) -> u64 {
    debug_assert!(!sorted.is_empty());
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn analyze_boundary(
    chronological: &[&TimelineAssetInput],
    boundary_index: usize,
    cadence: &TimelineCadence,
    config: &TimelineAnalysisConfig,
) -> (BoundaryEvidence, usize) {
    let left = chronological[boundary_index - 1];
    let right = chronological[boundary_index];
    let mut context_reads = 0usize;
    let mut signals = Vec::with_capacity(7);

    match config.mode {
        AnalysisMode::TimeOnly => {
            signals.push(time_gap_signal(left, right, cadence));
            signals.push(unavailable_signal(
                BoundarySignal::SemanticTransition,
                "Semantic transition was disabled by the time-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::CameraAndLens,
                "Camera/lens evidence was disabled by the time-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::Orientation,
                "Orientation evidence was disabled by the time-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::AnonymousFaceCount,
                "Anonymous face-count evidence was disabled by the time-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::SimilarSetContinuity,
                "Similar Set continuity was disabled by the time-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::FilenameAdjacency,
                "Filename adjacency was disabled by the time-only analysis mode.",
            ));
        }
        AnalysisMode::SemanticOnly => {
            let (semantic, reads) = semantic_transition_signal(
                chronological,
                boundary_index,
                config.max_context_assets,
                &config.boundary_thresholds,
            );
            context_reads = context_reads.saturating_add(reads);
            signals.push(unavailable_signal(
                BoundarySignal::TimeGap,
                "Time-gap evidence was disabled by the semantic-only analysis mode.",
            ));
            signals.push(semantic);
            signals.push(unavailable_signal(
                BoundarySignal::CameraAndLens,
                "Camera/lens evidence was disabled by the semantic-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::Orientation,
                "Orientation evidence was disabled by the semantic-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::AnonymousFaceCount,
                "Anonymous face-count evidence was disabled by the semantic-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::SimilarSetContinuity,
                "Similar Set continuity was disabled by the semantic-only analysis mode.",
            ));
            signals.push(unavailable_signal(
                BoundarySignal::FilenameAdjacency,
                "Filename adjacency was disabled by the semantic-only analysis mode.",
            ));
        }
        AnalysisMode::Combined => {
            let (semantic, reads) = semantic_transition_signal(
                chronological,
                boundary_index,
                config.max_context_assets,
                &config.boundary_thresholds,
            );
            context_reads = context_reads.saturating_add(reads);
            signals.push(time_gap_signal(left, right, cadence));
            signals.push(semantic);
            signals.push(camera_and_lens_signal(left, right));
            signals.push(orientation_signal(left, right));
            signals.push(anonymous_face_count_signal(left, right));
            signals.push(similar_set_continuity_signal(left, right));
            signals.push(filename_adjacency_signal(
                left,
                right,
                &config.boundary_thresholds,
            ));
        }
    }

    let (combined_boundary_support, available_weight) = combine_signal_evidence(&signals, config);
    let category = match combined_boundary_support {
        None => BoundaryCategory::InsufficientEvidence,
        Some(score) if score >= config.boundary_thresholds.strong_boundary_score => {
            BoundaryCategory::Strong
        }
        Some(score) if score >= config.boundary_thresholds.moderate_boundary_score => {
            BoundaryCategory::Moderate
        }
        Some(_) => BoundaryCategory::Continuous,
    };
    let explanation = boundary_explanation(category, &signals);
    (
        BoundaryEvidence {
            left_asset_id: left.asset_id.clone(),
            right_asset_id: right.asset_id.clone(),
            category,
            combined_boundary_support,
            available_weight,
            signals,
            explanation,
        },
        context_reads,
    )
}

fn unavailable_signal(signal: BoundarySignal, detail: impl Into<String>) -> SignalEvidence {
    SignalEvidence {
        signal,
        availability: EvidenceAvailability::Unavailable,
        boundary_support: None,
        continuity_support: None,
        detail: detail.into(),
    }
}

fn available_signal(
    signal: BoundarySignal,
    boundary_support: f32,
    continuity_support: f32,
    detail: impl Into<String>,
) -> SignalEvidence {
    SignalEvidence {
        signal,
        availability: EvidenceAvailability::Available,
        boundary_support: Some(clamp_unit(boundary_support)),
        continuity_support: Some(clamp_unit(continuity_support)),
        detail: detail.into(),
    }
}

fn time_gap_signal(
    left: &TimelineAssetInput,
    right: &TimelineAssetInput,
    cadence: &TimelineCadence,
) -> SignalEvidence {
    let (Some(left_time), Some(right_time), Some(normalizer)) = (
        left.captured_at_unix_ms,
        right.captured_at_unix_ms,
        cadence.normalizer_gap_ms,
    ) else {
        return unavailable_signal(
            BoundarySignal::TimeGap,
            "Adaptive capture cadence is unavailable because there are too few positive local time gaps.",
        );
    };
    let Some(gap_ms) = right_time
        .checked_sub(left_time)
        .and_then(|difference| u64::try_from(difference).ok())
    else {
        return unavailable_signal(
            BoundarySignal::TimeGap,
            "Chronological timestamps could not produce a non-negative gap.",
        );
    };
    let boundary = (gap_ms as f64 / normalizer as f64).clamp(0.0, 1.0) as f32;
    let continuity = 1.0 - boundary;
    available_signal(
        BoundarySignal::TimeGap,
        boundary,
        continuity,
        format!(
            "Observed local capture gap of {gap_ms} ms against an adaptive {normalizer} ms project cadence normalizer."
        ),
    )
}

fn semantic_transition_signal(
    chronological: &[&TimelineAssetInput],
    boundary_index: usize,
    context_limit: usize,
    thresholds: &BoundaryThresholds,
) -> (SignalEvidence, usize) {
    let left = chronological[boundary_index - 1];
    let right = chronological[boundary_index];
    let (Some(left_vector), Some(right_vector)) = (
        valid_embedding(left.semantic_embedding.as_ref()),
        valid_embedding(right.semantic_embedding.as_ref()),
    ) else {
        return (
            unavailable_signal(
                BoundarySignal::SemanticTransition,
                "A compatible local semantic vector is unavailable on one or both adjacent assets.",
            ),
            0,
        );
    };
    if !vectors_compatible(left_vector, right_vector) {
        return (
            unavailable_signal(
                BoundarySignal::SemanticTransition,
                "Adjacent local semantic vectors use different model/version compatibility keys or dimensions.",
            ),
            0,
        );
    }

    let key = left_vector.compatibility_key.as_str();
    let dimension = left_vector.values.len();
    let (left_centroid, left_count) = bounded_context_centroid(
        chronological,
        boundary_index,
        ContextDirection::Before,
        context_limit,
        key,
        dimension,
    );
    let (right_centroid, right_count) = bounded_context_centroid(
        chronological,
        boundary_index,
        ContextDirection::After,
        context_limit,
        key,
        dimension,
    );
    let context_reads = left_count.saturating_add(right_count);
    let (Some(left_centroid), Some(right_centroid)) = (left_centroid, right_centroid) else {
        return (
            unavailable_signal(
                BoundarySignal::SemanticTransition,
                "No bounded compatible semantic context could be formed around this boundary.",
            ),
            context_reads,
        );
    };
    let Some(similarity) = cosine_similarity(&left_centroid, &right_centroid) else {
        return (
            unavailable_signal(
                BoundarySignal::SemanticTransition,
                "Bounded semantic context had no finite cosine similarity.",
            ),
            context_reads,
        );
    };

    let boundary = if similarity <= thresholds.semantic_boundary_similarity {
        1.0
    } else if similarity >= thresholds.semantic_continuity_similarity {
        0.0
    } else {
        (thresholds.semantic_continuity_similarity - similarity)
            / (thresholds.semantic_continuity_similarity - thresholds.semantic_boundary_similarity)
    };
    let continuity = if similarity >= thresholds.semantic_continuity_similarity {
        (similarity - thresholds.semantic_continuity_similarity)
            / (1.0 - thresholds.semantic_continuity_similarity).max(f32::EPSILON)
    } else {
        0.0
    };
    (
        available_signal(
            BoundarySignal::SemanticTransition,
            boundary,
            continuity,
            format!(
                "Bounded local semantic context compared {left_count} prior and {right_count} following compatible vectors (cosine similarity {similarity:.3})."
            ),
        ),
        context_reads,
    )
}

#[derive(Debug, Clone, Copy)]
enum ContextDirection {
    Before,
    After,
}

fn bounded_context_centroid(
    chronological: &[&TimelineAssetInput],
    boundary_index: usize,
    direction: ContextDirection,
    limit: usize,
    compatibility_key: &str,
    dimension: usize,
) -> (Option<Vec<f32>>, usize) {
    let mut vectors = Vec::with_capacity(limit);
    match direction {
        ContextDirection::Before => {
            for asset in chronological[..boundary_index].iter().rev().take(limit) {
                let Some(vector) = valid_embedding(asset.semantic_embedding.as_ref()) else {
                    break;
                };
                if vector.compatibility_key != compatibility_key || vector.values.len() != dimension
                {
                    break;
                }
                vectors.push(vector.values.as_slice());
            }
        }
        ContextDirection::After => {
            for asset in chronological[boundary_index..].iter().take(limit) {
                let Some(vector) = valid_embedding(asset.semantic_embedding.as_ref()) else {
                    break;
                };
                if vector.compatibility_key != compatibility_key || vector.values.len() != dimension
                {
                    break;
                }
                vectors.push(vector.values.as_slice());
            }
        }
    }
    let count = vectors.len();
    (mean_normalized_vector(&vectors), count)
}

fn camera_and_lens_signal(left: &TimelineAssetInput, right: &TimelineAssetInput) -> SignalEvidence {
    let mut comparable = 0usize;
    let mut changed = 0usize;
    for (left_value, right_value) in [
        (&left.camera_model, &right.camera_model),
        (&left.lens_model, &right.lens_model),
    ] {
        let (Some(left_value), Some(right_value)) = (
            normalized_metadata_value(left_value),
            normalized_metadata_value(right_value),
        ) else {
            continue;
        };
        comparable += 1;
        if left_value != right_value {
            changed += 1;
        }
    }
    if comparable == 0 {
        return unavailable_signal(
            BoundarySignal::CameraAndLens,
            "No comparable local camera or lens metadata was available on both adjacent assets.",
        );
    }
    if changed == 0 {
        return available_signal(
            BoundarySignal::CameraAndLens,
            0.0,
            0.70,
            "Comparable local camera/lens metadata remained consistent across the boundary.",
        );
    }
    let boundary = changed as f32 / comparable as f32;
    available_signal(
        BoundarySignal::CameraAndLens,
        boundary,
        0.0,
        format!(
            "{changed} of {comparable} comparable local camera/lens metadata fields changed across the boundary."
        ),
    )
}

fn orientation_signal(left: &TimelineAssetInput, right: &TimelineAssetInput) -> SignalEvidence {
    match (left.orientation, right.orientation) {
        (Some(left_orientation), Some(right_orientation))
            if left_orientation == right_orientation =>
        {
            available_signal(
                BoundarySignal::Orientation,
                0.0,
                0.60,
                "Local orientation metadata remained consistent across the boundary.",
            )
        }
        (Some(_), Some(_)) => available_signal(
            BoundarySignal::Orientation,
            0.80,
            0.0,
            "Local orientation metadata changed across the boundary.",
        ),
        _ => unavailable_signal(
            BoundarySignal::Orientation,
            "Comparable local orientation metadata was unavailable on one or both adjacent assets.",
        ),
    }
}

fn anonymous_face_count_signal(
    left: &TimelineAssetInput,
    right: &TimelineAssetInput,
) -> SignalEvidence {
    let (Some(left_count), Some(right_count)) =
        (left.anonymous_face_count, right.anonymous_face_count)
    else {
        return unavailable_signal(
            BoundarySignal::AnonymousFaceCount,
            "Anonymous face-count evidence was unavailable on one or both adjacent assets.",
        );
    };
    let difference = left_count.abs_diff(right_count);
    if difference == 0 {
        return available_signal(
            BoundarySignal::AnonymousFaceCount,
            0.0,
            0.45,
            "Anonymous face count remained the same; this is weak continuity evidence only.",
        );
    }
    let boundary = match difference {
        1 => 0.25,
        2 => 0.45,
        _ => 0.60,
    };
    available_signal(
        BoundarySignal::AnonymousFaceCount,
        boundary,
        0.0,
        format!(
            "Anonymous face count changed by {difference}; this is weak structural evidence and never identity evidence."
        ),
    )
}

fn similar_set_continuity_signal(
    left: &TimelineAssetInput,
    right: &TimelineAssetInput,
) -> SignalEvidence {
    let (Some(left_groups), Some(right_groups)) = (&left.similar_set_ids, &right.similar_set_ids)
    else {
        return unavailable_signal(
            BoundarySignal::SimilarSetContinuity,
            "Similar Set membership evidence was unavailable on one or both adjacent assets.",
        );
    };
    let left_groups: BTreeSet<&str> = left_groups
        .iter()
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty())
        .collect();
    let right_groups: BTreeSet<&str> = right_groups
        .iter()
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty())
        .collect();
    if left_groups.intersection(&right_groups).next().is_some() {
        available_signal(
            BoundarySignal::SimilarSetContinuity,
            0.0,
            0.90,
            "Existing Similar Set membership overlaps across the boundary; this supports continuity but does not alter Similar Sets."
        )
    } else {
        // A lack of shared related-frame membership is not proof of an event boundary. Do not
        // turn the separate M4 Similar Set system into a positive Moment classifier.
        unavailable_signal(
            BoundarySignal::SimilarSetContinuity,
            "No shared Similar Set continuity evidence was available; absence is not boundary evidence.",
        )
    }
}

fn filename_adjacency_signal(
    left: &TimelineAssetInput,
    right: &TimelineAssetInput,
    thresholds: &BoundaryThresholds,
) -> SignalEvidence {
    let (Some(left_sequence), Some(right_sequence)) =
        (left.filename_sequence, right.filename_sequence)
    else {
        return unavailable_signal(
            BoundarySignal::FilenameAdjacency,
            "No repository-derived filename sequence evidence was available on both adjacent assets.",
        );
    };
    let gap = left_sequence.abs_diff(right_sequence);
    if gap <= 1 {
        return available_signal(
            BoundarySignal::FilenameAdjacency,
            0.0,
            0.25,
            "Repository-derived filename sequence remained adjacent; this is weak continuity evidence only.",
        );
    }
    if gap >= thresholds.filename_gap_for_weak_boundary {
        return available_signal(
            BoundarySignal::FilenameAdjacency,
            0.10,
            0.0,
            format!(
                "Repository-derived filename sequence gap was {gap}; this is intentionally weak boundary evidence."
            ),
        );
    }
    unavailable_signal(
        BoundarySignal::FilenameAdjacency,
        "Filename sequence was non-adjacent but below the weak-evidence threshold; no inference was made.",
    )
}

fn normalized_metadata_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn combine_signal_evidence(
    signals: &[SignalEvidence],
    config: &TimelineAnalysisConfig,
) -> (Option<f32>, f32) {
    let mut positive = 0.0f32;
    let mut continuity = 0.0f32;
    let mut available_weight = 0.0f32;

    for signal in signals {
        if signal.availability != EvidenceAvailability::Available {
            continue;
        }
        let weight = signal_weight(signal.signal, config);
        if weight <= 0.0 {
            continue;
        }
        available_weight += weight;
        positive += weight * signal.boundary_support.unwrap_or_default();
        continuity += weight * signal.continuity_support.unwrap_or_default();
    }
    if available_weight <= 0.0 {
        return (None, 0.0);
    }
    // Continuity is evidence, rather than merely a missing boundary. It reduces a competing
    // boundary total but never creates a negative score or a claim that frames are the same.
    let combined = ((positive - continuity) / available_weight).max(0.0);
    (Some(clamp_unit(combined)), available_weight)
}

fn signal_weight(signal: BoundarySignal, config: &TimelineAnalysisConfig) -> f32 {
    match signal {
        BoundarySignal::TimeGap => config.boundary_weights.time_gap,
        BoundarySignal::SemanticTransition => config.boundary_weights.semantic_transition,
        BoundarySignal::CameraAndLens => config.boundary_weights.camera_and_lens,
        BoundarySignal::Orientation => config.boundary_weights.orientation,
        BoundarySignal::AnonymousFaceCount => config.boundary_weights.anonymous_face_count,
        BoundarySignal::SimilarSetContinuity => config.boundary_weights.similar_set_continuity,
        BoundarySignal::FilenameAdjacency => config.boundary_weights.filename_adjacency,
    }
}

fn boundary_explanation(category: BoundaryCategory, signals: &[SignalEvidence]) -> String {
    let available: Vec<&str> = signals
        .iter()
        .filter(|signal| signal.availability == EvidenceAvailability::Available)
        .map(|signal| match signal.signal {
            BoundarySignal::TimeGap => "capture timing",
            BoundarySignal::SemanticTransition => "local semantic continuity",
            BoundarySignal::CameraAndLens => "camera/lens metadata",
            BoundarySignal::Orientation => "orientation metadata",
            BoundarySignal::AnonymousFaceCount => "anonymous face-count evidence",
            BoundarySignal::SimilarSetContinuity => "Similar Set continuity",
            BoundarySignal::FilenameAdjacency => "filename sequence",
        })
        .collect();
    match category {
        BoundaryCategory::Strong => format!(
            "Strong structural boundary supported by {}.",
            join_evidence_names(&available)
        ),
        BoundaryCategory::Moderate => format!(
            "Moderate structural boundary supported by {}.",
            join_evidence_names(&available)
        ),
        BoundaryCategory::Continuous => format!(
            "Continuous local sequence based on {}.",
            join_evidence_names(&available)
        ),
        BoundaryCategory::InsufficientEvidence => {
            "Insufficient local evidence to characterize this adjacent pair.".into()
        }
    }
}

fn join_evidence_names(names: &[&str]) -> String {
    match names {
        [] => "no available evidence".into(),
        [one] => (*one).into(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let mut output = names[..names.len() - 1].join(", ");
            output.push_str(", and ");
            output.push_str(names[names.len() - 1]);
            output
        }
    }
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn valid_embedding(embedding: Option<&SemanticVector>) -> Option<&SemanticVector> {
    embedding.filter(|vector| is_valid_semantic_vector(vector))
}

fn vectors_compatible(left: &SemanticVector, right: &SemanticVector) -> bool {
    left.compatibility_key == right.compatibility_key && left.values.len() == right.values.len()
}

fn mean_normalized_vector(vectors: &[&[f32]]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    if first.is_empty() || vectors.iter().any(|vector| vector.len() != first.len()) {
        return None;
    }
    let mut mean = vec![0.0f32; first.len()];
    for vector in vectors {
        for (mean_value, value) in mean.iter_mut().zip(vector.iter()) {
            *mean_value += *value;
        }
    }
    let divisor = vectors.len() as f32;
    for value in &mut mean {
        *value /= divisor;
    }
    normalize_vector(mean)
}

fn normalize_vector(mut vector: Vec<f32>) -> Option<Vec<f32>> {
    let norm = squared_norm(&vector)?.sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        return None;
    }
    for value in &mut vector {
        *value = (*value as f64 / norm) as f32;
    }
    vector
        .iter()
        .all(|value| value.is_finite())
        .then_some(vector)
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let left_norm = squared_norm(left)?.sqrt();
    let right_norm = squared_norm(right)?.sqrt();
    if left_norm <= 0.0 || right_norm <= 0.0 || !left_norm.is_finite() || !right_norm.is_finite() {
        return None;
    }
    let dot = left
        .iter()
        .zip(right.iter())
        .fold(0.0f64, |sum, (a, b)| sum + f64::from(*a) * f64::from(*b));
    (dot / (left_norm * right_norm))
        .is_finite()
        .then_some((dot / (left_norm * right_norm)).clamp(-1.0, 1.0) as f32)
}

fn build_segments(
    chronological: &[&TimelineAssetInput],
    boundaries: &[BoundaryEvidence],
    label_candidates: &[LabelCandidate],
    config: &TimelineAnalysisConfig,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) -> Vec<MomentSegment> {
    if chronological.is_empty() {
        return Vec::new();
    }
    debug_assert_eq!(boundaries.len(), chronological.len().saturating_sub(1));

    let mut segments = Vec::new();
    let mut segment_start = 0usize;
    for (boundary_offset, boundary) in boundaries.iter().enumerate() {
        if matches!(
            boundary.category,
            BoundaryCategory::Moderate | BoundaryCategory::Strong
        ) {
            let segment_end = boundary_offset;
            segments.push(build_segment(
                chronological,
                segment_start,
                segment_end,
                segments.len(),
                label_candidates,
                config,
                diagnostics,
            ));
            segment_start = boundary_offset + 1;
        }
    }
    segments.push(build_segment(
        chronological,
        segment_start,
        chronological.len() - 1,
        segments.len(),
        label_candidates,
        config,
        diagnostics,
    ));
    segments
}

fn build_segment(
    chronological: &[&TimelineAssetInput],
    start: usize,
    end: usize,
    ordinal: usize,
    label_candidates: &[LabelCandidate],
    config: &TimelineAnalysisConfig,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) -> MomentSegment {
    let assets = &chronological[start..=end];
    let semantic_context = derive_segment_semantic_context(assets, diagnostics);
    let ai_label = suggest_ai_label(&semantic_context, label_candidates, config, diagnostics);
    let ai_representative = suggest_representative(assets, &semantic_context, config);
    let first = assets
        .first()
        .expect("non-empty segment is constructed from chronological input");
    let last = assets
        .last()
        .expect("non-empty segment is constructed from chronological input");
    MomentSegment {
        ordinal,
        start_asset_id: first.asset_id.clone(),
        end_asset_id: last.asset_id.clone(),
        start_captured_at_unix_ms: first
            .captured_at_unix_ms
            .expect("chronological input has timestamps"),
        end_captured_at_unix_ms: last
            .captured_at_unix_ms
            .expect("chronological input has timestamps"),
        asset_ids: assets.iter().map(|asset| asset.asset_id.clone()).collect(),
        semantic_summary: semantic_context.summary,
        ai_label,
        ai_representative,
    }
}

#[derive(Debug, Clone)]
struct SegmentSemanticContext {
    summary: SegmentSemanticSummary,
    centroid: Option<SemanticCentroid>,
}

#[derive(Debug, Clone)]
struct SemanticCentroid {
    compatibility_key: String,
    values: Vec<f32>,
    vector_count: usize,
}

fn derive_segment_semantic_context(
    assets: &[&TimelineAssetInput],
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) -> SegmentSemanticContext {
    let mut groups: BTreeMap<(String, usize), Vec<&[f32]>> = BTreeMap::new();
    let mut valid_vector_count = 0usize;
    for asset in assets {
        let Some(vector) = valid_embedding(asset.semantic_embedding.as_ref()) else {
            continue;
        };
        valid_vector_count += 1;
        groups
            .entry((vector.compatibility_key.clone(), vector.values.len()))
            .or_default()
            .push(vector.values.as_slice());
    }

    let chosen =
        groups
            .into_iter()
            .max_by(|(left_key, left_vectors), (right_key, right_vectors)| {
                left_vectors
                    .len()
                    .cmp(&right_vectors.len())
                    // A deterministic lexical compatibility-key tie break prevents map iteration order
                    // from changing a Moment label or representative.
                    .then_with(|| right_key.cmp(left_key))
            });

    let Some(((compatibility_key, _dimension), vectors)) = chosen else {
        return SegmentSemanticContext {
            summary: SegmentSemanticSummary {
                compatibility_key: None,
                compatible_vector_count: 0,
                unavailable_or_incompatible_asset_count: assets.len(),
            },
            centroid: None,
        };
    };
    let vector_count = vectors.len();
    let centroid = mean_normalized_vector(&vectors).map(|values| SemanticCentroid {
        compatibility_key: compatibility_key.clone(),
        values,
        vector_count,
    });
    if valid_vector_count > vector_count {
        diagnostics.push(AnalysisDiagnostic {
            code: DiagnosticCode::IncompatibleSemanticEmbedding,
            asset_id: None,
            candidate_id: None,
            detail: "A structural segment contains multiple valid semantic model/version spaces; only the deterministic dominant compatibility key was used for its local label and representative evidence."
                .into(),
        });
    }
    let compatible_vector_count = centroid
        .as_ref()
        .map_or(0, |centroid| centroid.vector_count);
    SegmentSemanticContext {
        summary: SegmentSemanticSummary {
            compatibility_key: centroid
                .as_ref()
                .map(|centroid| centroid.compatibility_key.clone()),
            compatible_vector_count,
            unavailable_or_incompatible_asset_count: assets
                .len()
                .saturating_sub(compatible_vector_count),
        },
        centroid,
    }
}

#[derive(Debug, Clone)]
struct CandidateScore<'a> {
    candidate: &'a LabelCandidate,
    similarity: f32,
}

fn suggest_ai_label(
    semantic_context: &SegmentSemanticContext,
    candidates: &[LabelCandidate],
    config: &TimelineAnalysisConfig,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) -> AiLabelSuggestion {
    if config.mode == AnalysisMode::TimeOnly {
        return unavailable_label(
            "Automatic labels were not evaluated because semantic evidence is disabled by the time-only analysis mode.",
        );
    }
    let Some(centroid) = &semantic_context.centroid else {
        return unavailable_label(
            "No compatible local semantic centroid is available for a conservative automatic label.",
        );
    };
    if centroid.vector_count < config.label_policy.minimum_supporting_vectors {
        return abstained_label(
            Some(centroid.compatibility_key.clone()),
            centroid.vector_count,
            "Too few compatible local semantic vectors support a label; Moment Brain abstained.",
        );
    }

    let mut project_phrase_scores = Vec::new();
    let mut generic_scores = Vec::new();
    for candidate in candidates {
        let Some(vector) = valid_embedding(candidate.semantic_embedding.as_ref()) else {
            continue;
        };
        if vector.compatibility_key != centroid.compatibility_key
            || vector.values.len() != centroid.values.len()
        {
            diagnostics.push(AnalysisDiagnostic {
                code: DiagnosticCode::IncompatibleSemanticEmbedding,
                asset_id: None,
                candidate_id: Some(candidate.candidate_id.clone()),
                detail: "The candidate label vector did not match this segment's local semantic model/version space and was skipped."
                    .into(),
            });
            continue;
        }
        let Some(similarity) = cosine_similarity(&centroid.values, &vector.values) else {
            continue;
        };
        match &candidate.kind {
            LabelCandidateKind::ReviewedGeneric { .. } => generic_scores.push(CandidateScore {
                candidate,
                similarity,
            }),
            LabelCandidateKind::HumanProjectPhrase { phrase } if !phrase.trim().is_empty() => {
                project_phrase_scores.push(CandidateScore {
                    candidate,
                    similarity,
                });
            }
            LabelCandidateKind::HumanProjectPhrase { .. } => {}
        }
    }
    sort_candidate_scores(&mut project_phrase_scores);
    sort_candidate_scores(&mut generic_scores);

    // An exact photographer-provided phrase receives priority only after local semantic evidence
    // clears both the strength and ambiguity thresholds. Moment Brain never invents or expands
    // this text, so a user checklist can safely contain photographer-specific terminology.
    if let Some(top) = project_phrase_scores.first() {
        let competitor = project_phrase_scores
            .get(1)
            .map(|candidate| candidate.similarity);
        let has_margin = competitor
            .map(|next| top.similarity - next >= config.label_policy.minimum_margin)
            .unwrap_or(true);
        if top.similarity >= config.label_policy.minimum_similarity && has_margin {
            if let LabelCandidateKind::HumanProjectPhrase { phrase } = &top.candidate.kind {
                return AiLabelSuggestion {
                    state: LabelSuggestionState::Suggested,
                    display_label: phrase.trim().to_owned(),
                    source: Some(LabelSuggestionSource::HumanProjectPhrase),
                    candidate_id: Some(top.candidate.candidate_id.clone()),
                    supporting_generic_concepts: Vec::new(),
                    semantic_similarity: Some(top.similarity),
                    competing_similarity: competitor,
                    compatibility_key: Some(centroid.compatibility_key.clone()),
                    supporting_vector_count: centroid.vector_count,
                    evidence: vec![
                        "Matched an exact photographer-provided project/checklist phrase against compatible local semantic evidence."
                            .into(),
                        "This is a semantic suggestion, not identity, relationship, event, emotion, or missing-coverage evidence."
                            .into(),
                    ],
                };
            }
        }
    }

    let generic = select_generic_label(&generic_scores, &config.label_policy);
    if let Some(generic) = generic {
        return AiLabelSuggestion {
            state: LabelSuggestionState::Suggested,
            display_label: generic.display_label,
            source: Some(LabelSuggestionSource::ReviewedGenericConcepts),
            candidate_id: None,
            supporting_generic_concepts: generic.concepts,
            semantic_similarity: Some(generic.primary_similarity),
            competing_similarity: generic.competing_similarity,
            compatibility_key: Some(centroid.compatibility_key.clone()),
            supporting_vector_count: centroid.vector_count,
            evidence: vec![
                "Composed only from reviewed generic visual concepts with compatible local semantic evidence."
                    .into(),
                "The concise label is a retrieval-oriented semantic suggestion, not object detection or proof that a concept is present."
                    .into(),
            ],
        };
    }

    let strongest_phrase = project_phrase_scores
        .first()
        .map(|candidate| candidate.similarity);
    let strongest_generic = generic_scores.first().map(|candidate| candidate.similarity);
    abstained_label_with_scores(
        Some(centroid.compatibility_key.clone()),
        centroid.vector_count,
        strongest_phrase.or(strongest_generic),
        project_phrase_scores.get(1).map(|candidate| candidate.similarity),
        "Compatible local semantic evidence was weak, ambiguous, or did not support a reviewed candidate; Moment Brain abstained.",
    )
}

fn unavailable_label(detail: &str) -> AiLabelSuggestion {
    AiLabelSuggestion {
        state: LabelSuggestionState::SemanticEvidenceUnavailable,
        display_label: UNTITLED_MOMENT_LABEL.into(),
        source: None,
        candidate_id: None,
        supporting_generic_concepts: Vec::new(),
        semantic_similarity: None,
        competing_similarity: None,
        compatibility_key: None,
        supporting_vector_count: 0,
        evidence: vec![detail.into()],
    }
}

fn abstained_label(
    compatibility_key: Option<String>,
    supporting_vector_count: usize,
    detail: &str,
) -> AiLabelSuggestion {
    abstained_label_with_scores(
        compatibility_key,
        supporting_vector_count,
        None,
        None,
        detail,
    )
}

fn abstained_label_with_scores(
    compatibility_key: Option<String>,
    supporting_vector_count: usize,
    semantic_similarity: Option<f32>,
    competing_similarity: Option<f32>,
    detail: &str,
) -> AiLabelSuggestion {
    AiLabelSuggestion {
        state: LabelSuggestionState::Abstained,
        display_label: UNTITLED_MOMENT_LABEL.into(),
        source: None,
        candidate_id: None,
        supporting_generic_concepts: Vec::new(),
        semantic_similarity,
        competing_similarity,
        compatibility_key,
        supporting_vector_count,
        evidence: vec![detail.into()],
    }
}

fn sort_candidate_scores(scores: &mut [CandidateScore<'_>]) {
    scores.sort_by(|left, right| {
        right.similarity.total_cmp(&left.similarity).then_with(|| {
            left.candidate
                .candidate_id
                .cmp(&right.candidate.candidate_id)
        })
    });
}

#[derive(Debug, Clone)]
struct GenericLabelSelection {
    display_label: String,
    concepts: Vec<GenericLabelConcept>,
    primary_similarity: f32,
    competing_similarity: Option<f32>,
}

fn select_generic_label(
    scores: &[CandidateScore<'_>],
    policy: &LabelPolicy,
) -> Option<GenericLabelSelection> {
    let mut by_concept: BTreeMap<GenericLabelConcept, &CandidateScore<'_>> = BTreeMap::new();
    for score in scores {
        let LabelCandidateKind::ReviewedGeneric { concept } = score.candidate.kind else {
            continue;
        };
        if score.similarity < policy.minimum_similarity {
            continue;
        }
        by_concept.entry(concept).or_insert(score);
    }
    if by_concept.is_empty() {
        return None;
    }

    let mut selected: Vec<(GenericLabelConcept, &CandidateScore<'_>)> =
        by_concept.into_iter().collect();
    resolve_concept_conflict(
        &mut selected,
        &[GenericLabelConcept::Indoor, GenericLabelConcept::Outdoor],
        policy.minimum_margin,
    );
    resolve_concept_conflict(
        &mut selected,
        &[GenericLabelConcept::CloseUp, GenericLabelConcept::WideScene],
        policy.minimum_margin,
    );
    resolve_concept_conflict(
        &mut selected,
        &[
            GenericLabelConcept::OnePerson,
            GenericLabelConcept::MultiplePeople,
            GenericLabelConcept::Group,
        ],
        policy.minimum_margin,
    );
    if selected.is_empty() {
        return None;
    }

    // Retain the strongest finite number of concepts before composing. This is a label brevity
    // policy, not a catalog limit, and prevents an unchecked bag of semantic words becoming a
    // fabricated description.
    selected.sort_by(|(left_concept, left), (right_concept, right)| {
        right
            .similarity
            .total_cmp(&left.similarity)
            .then_with(|| left_concept.cmp(right_concept))
    });
    let competitor_similarity = selected.get(1).map(|(_, score)| score.similarity);
    selected.truncate(policy.maximum_generic_concepts);
    let primary_similarity = selected.first()?.1.similarity;
    let concepts: Vec<GenericLabelConcept> = selected.iter().map(|(concept, _)| *concept).collect();
    let display_label = compose_reviewed_generic_label(&concepts)?;
    Some(GenericLabelSelection {
        display_label,
        concepts,
        primary_similarity,
        competing_similarity: competitor_similarity,
    })
}

fn resolve_concept_conflict(
    selected: &mut Vec<(GenericLabelConcept, &CandidateScore<'_>)>,
    conflict_group: &[GenericLabelConcept],
    minimum_margin: f32,
) {
    let mut candidates: Vec<(GenericLabelConcept, &CandidateScore<'_>)> = selected
        .iter()
        .copied()
        .filter(|(concept, _)| conflict_group.contains(concept))
        .collect();
    if candidates.len() <= 1 {
        return;
    }
    candidates.sort_by(|(left_concept, left), (right_concept, right)| {
        right
            .similarity
            .total_cmp(&left.similarity)
            .then_with(|| left_concept.cmp(right_concept))
    });
    let winner = candidates[0];
    let runner_up = candidates[1];
    selected.retain(|(concept, _)| !conflict_group.contains(concept));
    if winner.1.similarity - runner_up.1.similarity >= minimum_margin {
        selected.push(winner);
    }
    // Near-tied conflicting concepts are intentionally both removed: abstaining from that part
    // of a label is safer than picking an indoor/outdoor or one/multiple-person claim by chance.
}

fn compose_reviewed_generic_label(concepts: &[GenericLabelConcept]) -> Option<String> {
    if concepts.is_empty() {
        return None;
    }
    let contains = |concept| concepts.contains(&concept);
    let setting = if contains(GenericLabelConcept::Outdoor) {
        Some("Outdoor")
    } else if contains(GenericLabelConcept::Indoor) {
        Some("Indoor")
    } else {
        None
    };
    let framing = if contains(GenericLabelConcept::CloseUp) {
        Some("Close-up")
    } else if contains(GenericLabelConcept::WideScene) {
        Some("Wide")
    } else {
        None
    };
    let subject =
        if contains(GenericLabelConcept::Portraits) && contains(GenericLabelConcept::Group) {
            Some("group portraits")
        } else if contains(GenericLabelConcept::Portraits) {
            Some("portraits")
        } else if contains(GenericLabelConcept::Group) {
            Some("group photos")
        } else if contains(GenericLabelConcept::OnePerson) {
            Some("one person")
        } else if contains(GenericLabelConcept::MultiplePeople) {
            Some("multiple people")
        } else {
            None
        };
    let boat = contains(GenericLabelConcept::Boat);
    let water = contains(GenericLabelConcept::Water);

    if framing == Some("Wide") {
        let scene = if boat {
            "boat scene"
        } else if water {
            "water scene"
        } else {
            "scene"
        };
        let label = match setting {
            Some(setting) => format!("{setting} wide {scene}"),
            None => format!("Wide {scene}"),
        };
        return Some(label);
    }

    let mut parts = Vec::new();
    if let Some(setting) = setting {
        parts.push(setting.to_owned());
    }
    if framing == Some("Close-up") {
        parts.push("Close-up".into());
    }
    if boat {
        parts.push("boat".into());
    }
    if let Some(subject) = subject {
        if water && !boat {
            parts.push(format!("{subject} by water"));
        } else {
            parts.push(subject.into());
        }
    } else if water {
        parts.push("water".into());
    }
    if parts.is_empty() {
        return None;
    }
    Some(capitalize(&parts.join(" ")))
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_uppercase(), chars.as_str())
}

#[derive(Debug)]
struct RawRepresentativeFactor {
    kind: RepresentativeFactorKind,
    normalized_value: f32,
    configured_weight: f32,
    detail: String,
}

fn suggest_representative(
    assets: &[&TimelineAssetInput],
    semantic_context: &SegmentSemanticContext,
    config: &TimelineAnalysisConfig,
) -> RepresentativeSuggestion {
    let mut best: Option<(usize, f32, Vec<RawRepresentativeFactor>)> = None;
    for (index, asset) in assets.iter().enumerate() {
        let mut factors = Vec::new();
        if let Some(centroid) = &semantic_context.centroid {
            if let Some(vector) = valid_embedding(asset.semantic_embedding.as_ref()) {
                if vector.compatibility_key == centroid.compatibility_key
                    && vector.values.len() == centroid.values.len()
                {
                    if let Some(similarity) = cosine_similarity(&centroid.values, &vector.values) {
                        factors.push(RawRepresentativeFactor {
                            kind: RepresentativeFactorKind::SemanticCentrality,
                            normalized_value: clamp_unit((similarity + 1.0) / 2.0),
                            configured_weight: config.representative_policy.semantic_centrality,
                            detail: "Centrality within this local compatible semantic segment."
                                .into(),
                        });
                    }
                }
            }
        }
        if let Some(score) = asset
            .technical_presentation_score
            .filter(|score| is_unit_interval(*score))
        {
            factors.push(RawRepresentativeFactor {
                kind: RepresentativeFactorKind::TechnicalEvidence,
                normalized_value: score,
                configured_weight: config.representative_policy.technical_evidence,
                detail: "Existing local technical presentation evidence supplied by Capture Intelligence."
                    .into(),
            });
        }
        if let Some((value, detail)) = human_presentation_value(&asset.human_presentation) {
            factors.push(RawRepresentativeFactor {
                kind: RepresentativeFactorKind::ExistingHumanPresentation,
                normalized_value: value,
                configured_weight: config.representative_policy.human_presentation,
                detail,
            });
        }
        let total_weight: f32 = factors.iter().map(|factor| factor.configured_weight).sum();
        let score = if total_weight > 0.0 {
            factors
                .iter()
                .map(|factor| factor.configured_weight * factor.normalized_value)
                .sum::<f32>()
                / total_weight
        } else {
            0.0
        };
        let should_replace = match &best {
            None => true,
            Some((best_index, best_score, _)) => {
                score.total_cmp(best_score) == Ordering::Greater
                    || (score.total_cmp(best_score) == Ordering::Equal && index < *best_index)
            }
        };
        if should_replace {
            best = Some((index, score, factors));
        }
    }
    let (index, score, raw_factors) = best.expect("non-empty segment has a representative");
    if raw_factors.is_empty() {
        return RepresentativeSuggestion {
            asset_id: assets[index].asset_id.clone(),
            method: RepresentativeSelectionMethod::StableChronologyFallback,
            score: None,
            factors: Vec::new(),
            explanation: "No compatible local presentation evidence was available; selected the earliest stable chronological asset without calling it the best frame."
                .into(),
        };
    }
    let total_weight: f32 = raw_factors
        .iter()
        .map(|factor| factor.configured_weight)
        .sum();
    let factors = raw_factors
        .into_iter()
        .map(|factor| RepresentativeFactor {
            kind: factor.kind,
            normalized_value: factor.normalized_value,
            configured_weight: factor.configured_weight,
            contribution: if total_weight > 0.0 {
                factor.configured_weight * factor.normalized_value / total_weight
            } else {
                0.0
            },
            detail: factor.detail,
        })
        .collect();
    RepresentativeSuggestion {
        asset_id: assets[index].asset_id.clone(),
        method: RepresentativeSelectionMethod::EvidenceWeighted,
        score: Some(score),
        factors,
        explanation: "Suggested from local semantic centrality, technical evidence, and existing human presentation signals where available; it does not change a human representative."
            .into(),
    }
}

fn human_presentation_value(signals: &HumanPresentationSignals) -> Option<(f32, String)> {
    let mut values = Vec::new();
    let mut names = Vec::new();
    if let Some(decision) = signals.decision {
        let value = match decision {
            ExistingHumanDecision::Keep => 1.0,
            ExistingHumanDecision::Review => 0.45,
            ExistingHumanDecision::Reject => 0.0,
        };
        values.push(value);
        names.push("existing decision");
    }
    if let Some(rating) = signals.rating.filter(|rating| *rating <= 5) {
        values.push(rating as f32 / 5.0);
        names.push("existing rating");
    }
    if let Some(starred) = signals.starred {
        values.push(if starred { 1.0 } else { 0.0 });
        names.push("existing star");
    }
    if values.is_empty() {
        return None;
    }
    let value = values.iter().sum::<f32>() / values.len() as f32;
    Some((
        value,
        format!(
            "Read-only existing human presentation signal from {}.",
            join_evidence_names(&names)
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(values: &[f32]) -> SemanticVector {
        SemanticVector {
            compatibility_key: "fixture-semantic-v1".into(),
            values: values.to_vec(),
        }
    }

    fn asset(id: &str, captured_at: Option<i64>, values: Option<&[f32]>) -> TimelineAssetInput {
        let mut asset = TimelineAssetInput::minimal(id, captured_at);
        asset.semantic_embedding = values.map(vector);
        asset
    }

    fn clock_asset(
        id: &str,
        captured_at: i64,
        camera_model: &str,
        similar_set_id: &str,
    ) -> TimelineAssetInput {
        let mut asset = TimelineAssetInput::minimal(id, Some(captured_at));
        asset.camera_model = Some(camera_model.into());
        asset.similar_set_ids = Some(vec![similar_set_id.into()]);
        asset
    }

    fn request(
        mode: AnalysisMode,
        assets: Vec<TimelineAssetInput>,
        label_candidates: Vec<LabelCandidate>,
    ) -> TimelineAnalysisRequest {
        TimelineAnalysisRequest {
            assets,
            label_candidates,
            config: TimelineAnalysisConfig {
                mode,
                ..TimelineAnalysisConfig::default()
            },
        }
    }

    fn generic_candidate(id: &str, concept: GenericLabelConcept, values: &[f32]) -> LabelCandidate {
        LabelCandidate {
            candidate_id: id.into(),
            kind: LabelCandidateKind::ReviewedGeneric { concept },
            semantic_embedding: Some(vector(values)),
        }
    }

    #[test]
    fn stable_chronology_uses_asset_id_tie_break_and_leaves_missing_time_ungrouped() {
        let analysis = analyze_timeline(&request(
            AnalysisMode::TimeOnly,
            vec![
                asset("c", Some(20), None),
                asset("b", Some(10), None),
                asset("no-time", None, None),
                asset("a", Some(10), None),
            ],
            Vec::new(),
        ))
        .unwrap();

        assert_eq!(analysis.ordered_asset_ids, vec!["a", "b", "c"]);
        assert_eq!(analysis.ungrouped_assets.len(), 1);
        assert_eq!(analysis.ungrouped_assets[0].asset_id, "no-time");
        assert!(analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingCaptureTimestamp));
    }

    #[test]
    fn combined_signals_create_a_structural_boundary_without_event_claims() {
        let mut first = asset("a", Some(0), Some(&[1.0, 0.0]));
        first.camera_model = Some("Camera A".into());
        let mut second = asset("b", Some(1_000), Some(&[1.0, 0.0]));
        second.camera_model = Some("Camera A".into());
        let mut third = asset("c", Some(2_000), Some(&[1.0, 0.0]));
        third.camera_model = Some("Camera A".into());
        let mut fourth = asset("d", Some(60_000), Some(&[0.0, 1.0]));
        fourth.camera_model = Some("Camera B".into());
        let mut fifth = asset("e", Some(61_000), Some(&[0.0, 1.0]));
        fifth.camera_model = Some("Camera B".into());

        let analysis = analyze_timeline(&request(
            AnalysisMode::Combined,
            vec![first, second, third, fourth, fifth],
            Vec::new(),
        ))
        .unwrap();

        let boundary = analysis
            .boundaries
            .iter()
            .find(|boundary| boundary.left_asset_id == "c" && boundary.right_asset_id == "d")
            .unwrap();
        assert_eq!(boundary.category, BoundaryCategory::Strong);
        assert!(boundary.explanation.contains("structural boundary"));
        assert_eq!(analysis.moments.len(), 2);
        assert_eq!(analysis.moments[0].asset_ids, vec!["a", "b", "c"]);
        assert_eq!(analysis.moments[1].asset_ids, vec!["d", "e"]);
    }

    #[test]
    fn unavailable_semantic_evidence_is_explicit_and_does_not_fabricate_a_boundary() {
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![asset("a", Some(0), None), asset("b", Some(1_000), None)],
            Vec::new(),
        ))
        .unwrap();

        assert_eq!(analysis.boundaries.len(), 1);
        assert_eq!(
            analysis.boundaries[0].category,
            BoundaryCategory::InsufficientEvidence
        );
        assert_eq!(analysis.moments.len(), 1);
        let semantic = analysis.boundaries[0]
            .signals
            .iter()
            .find(|signal| signal.signal == BoundarySignal::SemanticTransition)
            .unwrap();
        assert_eq!(semantic.availability, EvidenceAvailability::Unavailable);
    }

    #[test]
    fn semantic_context_reads_are_bounded_per_neighbor_boundary() {
        let assets = (0..100)
            .map(|index| {
                asset(
                    &format!("asset-{index:03}"),
                    Some(index * 1_000),
                    Some(&[1.0, 0.0]),
                )
            })
            .collect();
        let mut analysis_request = request(AnalysisMode::SemanticOnly, assets, Vec::new());
        analysis_request.config.max_context_assets = 3;
        let analysis = analyze_timeline(&analysis_request).unwrap();

        assert_eq!(analysis.work.boundaries_evaluated, 99);
        assert!(
            analysis.work.semantic_context_vector_reads
                <= analysis.work.boundaries_evaluated * 2 * analysis.work.max_context_assets
        );
        assert_eq!(analysis.moments.len(), 1);
    }

    #[test]
    fn reviewed_generic_concepts_compose_a_concise_evidence_grounded_label() {
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![
                asset("a", Some(0), Some(&[1.0, 0.0])),
                asset("b", Some(1_000), Some(&[1.0, 0.0])),
            ],
            vec![
                generic_candidate("outdoor", GenericLabelConcept::Outdoor, &[1.0, 0.0]),
                generic_candidate("portraits", GenericLabelConcept::Portraits, &[0.99, 0.01]),
            ],
        ))
        .unwrap();

        let label = &analysis.moments[0].ai_label;
        assert_eq!(label.state, LabelSuggestionState::Suggested);
        assert_eq!(label.display_label, "Outdoor portraits");
        assert_eq!(
            label.source,
            Some(LabelSuggestionSource::ReviewedGenericConcepts)
        );
        assert_eq!(
            label.supporting_generic_concepts,
            vec![GenericLabelConcept::Outdoor, GenericLabelConcept::Portraits]
        );
    }

    #[test]
    fn human_project_phrase_is_selected_exactly_not_generated() {
        let phrase = "Couple portraits";
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![asset("a", Some(0), Some(&[1.0, 0.0]))],
            vec![LabelCandidate {
                candidate_id: "checklist-couple-portraits".into(),
                kind: LabelCandidateKind::HumanProjectPhrase {
                    phrase: phrase.into(),
                },
                semantic_embedding: Some(vector(&[1.0, 0.0])),
            }],
        ))
        .unwrap();

        let label = &analysis.moments[0].ai_label;
        assert_eq!(label.display_label, phrase);
        assert_eq!(
            label.source,
            Some(LabelSuggestionSource::HumanProjectPhrase)
        );
        assert_eq!(
            label.candidate_id.as_deref(),
            Some("checklist-couple-portraits")
        );
    }

    #[test]
    fn ambiguous_or_weak_phrase_match_abstains_instead_of_fabricating_a_label() {
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![asset("a", Some(0), Some(&[1.0, 0.0]))],
            vec![
                LabelCandidate {
                    candidate_id: "phrase-one".into(),
                    kind: LabelCandidateKind::HumanProjectPhrase {
                        phrase: "Photographer phrase one".into(),
                    },
                    semantic_embedding: Some(vector(&[1.0, 0.0])),
                },
                LabelCandidate {
                    candidate_id: "phrase-two".into(),
                    kind: LabelCandidateKind::HumanProjectPhrase {
                        phrase: "Photographer phrase two".into(),
                    },
                    semantic_embedding: Some(vector(&[0.999, 0.01])),
                },
            ],
        ))
        .unwrap();

        let label = &analysis.moments[0].ai_label;
        assert_eq!(label.state, LabelSuggestionState::Abstained);
        assert_eq!(label.display_label, UNTITLED_MOMENT_LABEL);
    }

    #[test]
    fn conflicting_reviewed_generic_concepts_abstain_instead_of_picking_one_by_chance() {
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![asset("a", Some(0), Some(&[1.0, 0.0]))],
            vec![
                generic_candidate("indoor", GenericLabelConcept::Indoor, &[1.0, 0.0]),
                generic_candidate("outdoor", GenericLabelConcept::Outdoor, &[0.999, 0.01]),
            ],
        ))
        .unwrap();

        let label = &analysis.moments[0].ai_label;
        assert_eq!(label.state, LabelSuggestionState::Abstained);
        assert_eq!(label.display_label, UNTITLED_MOMENT_LABEL);
    }

    #[test]
    fn representative_prefers_transparent_centrality_and_keeps_human_data_read_only() {
        let first = asset("a", Some(0), Some(&[0.8, 0.2]));
        let mut central = asset("b", Some(1_000), Some(&[1.0, 0.0]));
        central.technical_presentation_score = Some(0.8);
        let mut third = asset("c", Some(2_000), Some(&[0.8, -0.2]));
        third.human_presentation.decision = Some(ExistingHumanDecision::Keep);
        third.human_presentation.rating = Some(5);
        let analysis = analyze_timeline(&request(
            AnalysisMode::SemanticOnly,
            vec![first, central, third],
            Vec::new(),
        ))
        .unwrap();

        let representative = &analysis.moments[0].ai_representative;
        assert_eq!(
            representative.method,
            RepresentativeSelectionMethod::EvidenceWeighted
        );
        assert!(!representative.factors.is_empty());
        assert!(representative.explanation.contains("does not change"));
        // The existing Keep/rating data is intentionally only a factor; it cannot make a human
        // decision or representative selection disappear. The exact winner is deterministic.
        assert!(matches!(representative.asset_id.as_str(), "b" | "c"));
    }

    #[test]
    fn bounded_append_only_api_reanalyzes_context_and_tail_without_full_catalog_scan() {
        let tail_request = AppendOnlyTailAnalysisRequest {
            previous_latest_captured_at_unix_ms: 2_000,
            preceding_context_assets: vec![
                asset("a", Some(1_000), Some(&[1.0, 0.0])),
                asset("b", Some(2_000), Some(&[1.0, 0.0])),
            ],
            appended_assets: vec![
                asset("c", Some(3_000), Some(&[1.0, 0.0])),
                asset("d", Some(4_000), Some(&[1.0, 0.0])),
            ],
            label_candidates: Vec::new(),
            config: TimelineAnalysisConfig::default(),
        };
        let incremental = analyze_append_only_tail(&tail_request).unwrap();

        assert_eq!(
            incremental.disposition,
            IncrementalDisposition::TailReanalyzed
        );
        let analysis = incremental.analysis.unwrap();
        assert_eq!(analysis.work.chronological_asset_count, 4);
        assert_eq!(analysis.ordered_asset_ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn out_of_order_append_requires_full_rebuild() {
        let tail_request = AppendOnlyTailAnalysisRequest {
            previous_latest_captured_at_unix_ms: 2_000,
            preceding_context_assets: vec![asset("a", Some(2_000), Some(&[1.0, 0.0]))],
            appended_assets: vec![asset("inserted", Some(1_500), Some(&[1.0, 0.0]))],
            label_candidates: Vec::new(),
            config: TimelineAnalysisConfig::default(),
        };
        let incremental = analyze_append_only_tail(&tail_request).unwrap();

        assert_eq!(
            incremental.disposition,
            IncrementalDisposition::FullRebuildRequired
        );
        assert!(incremental.analysis.is_none());
    }

    #[test]
    fn consistent_cross_camera_similar_sets_produce_a_possible_offset_advisory() {
        let assets = vec![
            clock_asset("a-1", 1_000_000, "Camera A", "set-1"),
            clock_asset("b-1", 1_060_000, "Camera B", "set-1"),
            clock_asset("a-2", 2_000_000, "Camera A", "set-2"),
            clock_asset("b-2", 2_061_000, "Camera B", "set-2"),
            clock_asset("a-3", 3_000_000, "Camera A", "set-3"),
            clock_asset("b-3", 3_059_000, "Camera B", "set-3"),
        ];
        let before = assets.clone();

        let diagnostics = derive_clock_offset_diagnostics(&assets);

        assert_eq!(
            assets, before,
            "the diagnostic must not write back timestamps"
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].method,
            ClockOffsetDiagnosticMethod::ExistingSimilarSetNearestCrossCameraCaptureTime
        );
        assert_eq!(diagnostics[0].camera_a, "Camera A");
        assert_eq!(diagnostics[0].camera_b, "Camera B");
        assert_eq!(diagnostics[0].median_offset_ms, 60_000);
        assert_eq!(diagnostics[0].comparison_count, 3);
        assert_eq!(
            diagnostics[0].supporting_similar_set_ids,
            vec!["set-1", "set-2", "set-3"]
        );
        assert!(diagnostics[0].median_absolute_deviation_ms <= 1_000);
    }

    #[test]
    fn conflicting_cross_camera_similar_sets_abstain() {
        let diagnostics = derive_clock_offset_diagnostics(&[
            clock_asset("a-1", 1_000_000, "Camera A", "set-1"),
            clock_asset("b-1", 1_060_000, "Camera B", "set-1"),
            clock_asset("a-2", 2_000_000, "Camera A", "set-2"),
            clock_asset("b-2", 2_060_000, "Camera B", "set-2"),
            // This independent Similar Set points in the opposite direction. A median alone
            // would hide it, so the conservative all-observation consistency rule abstains.
            clock_asset("a-3", 3_000_000, "Camera A", "set-3"),
            clock_asset("b-3", 2_940_000, "Camera B", "set-3"),
        ]);

        assert!(diagnostics.is_empty());
    }

    #[test]
    fn insufficient_or_missing_cross_camera_similar_set_evidence_abstains_without_mutation() {
        let assets = vec![
            clock_asset("a-1", 1_000_000, "Camera A", "set-1"),
            clock_asset("b-1", 1_060_000, "Camera B", "set-1"),
            clock_asset("a-2", 2_000_000, "Camera A", "set-2"),
            clock_asset("b-2", 2_060_000, "Camera B", "set-2"),
            TimelineAssetInput::minimal("no-similar-set", Some(3_000_000)),
        ];
        let before = assets.clone();

        let diagnostics = derive_clock_offset_diagnostics(&assets);

        assert!(diagnostics.is_empty());
        assert_eq!(
            assets, before,
            "derivation is read-only and has no write-back path"
        );
    }
}
