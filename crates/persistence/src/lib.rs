//! SQLite catalog persistence. SQL stays behind this repository boundary.

use capture_graph::{EntityRef, Relationship, RelationshipKind};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use delivery_brain::{
    DeliveryAssetCandidate, DeliverySourceCandidate, FilenameStrategy, ManifestEntryDraft,
    OrganizationStrategy, PlanOverride, PlanOverrideKind, ProductionPlanStatus, ProductionPlanType,
    SelectionRules, VirtualCollectionKind,
};
use media_model::*;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use studio_brain::{decode_verified_model_artifact, VerifiedModelArtifact};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 20;
// This is a query-page size, never a catalog/result limit. Moment semantic search continues
// until its cursor is exhausted so it cannot silently omit a large project's later Moments.
const MOMENT_SEARCH_ROW_PAGE_SIZE: u32 = 256;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid persisted data: {0}")]
    InvalidData(String),
    #[error("invalid relationship: {0}")]
    Relationship(String),
}

pub type Result<T> = std::result::Result<T, PersistenceError>;

macro_rules! parse_id {
    ($type:ty, $value:expr) => {{
        let raw: &str = $value.as_ref();
        <$type>::try_from(raw).map_err(|error| PersistenceError::InvalidData(error.to_string()))
    }};
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogCounts {
    pub projects: u64,
    pub media_assets: u64,
    pub file_instances: u64,
    pub storage_volumes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedMediaRow {
    pub asset_id: String,
    pub file_instance_id: String,
    pub filename: String,
    pub media_type: String,
    pub extension: Option<String>,
    pub relative_path: String,
    pub selected_root: String,
    pub byte_size: Option<u64>,
    pub modified_at: Option<String>,
    pub storage_volume: String,
    pub storage_volume_id: String,
    pub fingerprint_present: bool,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIndexSummary {
    pub media_assets: u64,
    pub file_instances: u64,
    pub storage_volumes: u64,
    pub files_discovered: u64,
    pub supported_media_count: u64,
    pub unknown_count: u64,
    pub duplicate_fast_fingerprint_count: u64,
    /// Active, project-scoped M7 structural cards only. Reading this count never starts analysis.
    pub moment_count: u64,
    pub last_indexed_folder: Option<String>,
    pub storage_volume_identity: Option<String>,
}

/// Compact, project-scoped information used by the global Project Library. This is deliberately
/// derived from existing catalog records rather than being a second, mutable project summary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLibraryItem {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub media_asset_count: u64,
    pub storage_volume_count: u64,
    /// An honest record of available ingest evidence, not a claim that every asset is protected.
    pub protection_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestJobSummary {
    pub id: String,
    pub state: String,
    pub policy: String,
    pub guardian_state: String,
    pub safe_to_eject: bool,
    pub files_total: u64,
    pub files_verified: u64,
    pub files_failed: u64,
    pub bytes_total: u64,
    pub bytes_verified: u64,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestSourceSummary {
    pub id: String,
    pub label: String,
    pub selected_path: String,
    pub storage_volume_id: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub status: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestDestinationSummary {
    pub id: String,
    pub role: String,
    pub selected_path: String,
    pub storage_volume_id: String,
    pub storage_volume_name: String,
    pub available_bytes: Option<u64>,
    pub required_bytes: u64,
    pub writable: bool,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestItemRecord {
    pub id: IngestItemId,
    pub ingest_job_id: IngestJobId,
    pub ingest_source_id: IngestSourceId,
    pub ingest_destination_id: IngestDestinationId,
    pub media_asset_id: Option<MediaAssetId>,
    pub source_relative_path: String,
    pub destination_relative_path: String,
    pub byte_size: u64,
    pub source_hash: Option<String>,
    pub destination_hash: Option<String>,
    pub state: IngestItemState,
    pub verification_state: VerificationState,
    pub source_file_instance_id: Option<FileInstanceId>,
    pub destination_file_instance_id: Option<FileInstanceId>,
    pub error_message: Option<String>,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestAuditEvent {
    pub id: IngestAuditEventId,
    pub ingest_job_id: IngestJobId,
    pub event_type: String,
    pub detail: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub job: IngestJobSummary,
    pub sources: Vec<IngestSourceSummary>,
    pub destinations: Vec<IngestDestinationSummary>,
    pub recent_errors: Vec<String>,
    pub same_volume_warning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaBrowserFilter {
    All,
    Photos,
    Video,
    Audio,
    Sidecars,
    Unknown,
}

/// Query settings for the visual browser. The browser returns one logical `MediaAsset`
/// per row and selects a preferred `FileInstance` for availability and cache lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualMediaFilter {
    All,
    Photos,
    Raw,
    JpegHeif,
    Video,
    Audio,
    Offline,
    Available,
    StrongCandidates,
    TechnicalIssues,
    ProbableDuplicates,
    SimilarGroups,
    Faces,
    PossibleClosedEyes,
    BlurReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualMediaSort {
    CaptureTime,
    Filename,
    FileSize,
    DateIndexed,
    MediaType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualMediaQuery {
    pub filter: VisualMediaFilter,
    pub sort: VisualMediaSort,
    pub descending: bool,
    pub search: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub captured_from: Option<String>,
    pub captured_to: Option<String>,
    /// An M7 membership scope. It is validated against the selected project server-side; the
    /// renderer never supplies an arbitrary asset-ID list as a substitute for membership.
    pub moment_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for VisualMediaQuery {
    fn default() -> Self {
        Self {
            filter: VisualMediaFilter::All,
            sort: VisualMediaSort::CaptureTime,
            descending: false,
            search: None,
            camera_model: None,
            lens_model: None,
            captured_from: None,
            captured_to: None,
            moment_id: None,
            limit: 120,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualMediaRow {
    pub asset_id: String,
    pub file_instance_id: String,
    pub filename: String,
    pub media_type: String,
    pub extension: Option<String>,
    pub byte_size: Option<u64>,
    pub captured_at: Option<String>,
    pub indexed_at: String,
    pub relative_path: String,
    pub selected_root: Option<String>,
    pub storage_volume: String,
    pub storage_volume_id: String,
    pub is_available: bool,
    /// Internal artifact identity while in persistence; replaced by a runtime-only custom-protocol
    /// URL at the desktop boundary. Browser URLs and absolute cache paths are never persisted.
    pub thumbnail_preview_url: Option<String>,
    pub medium_preview_url: Option<String>,
    pub preview_preview_url: Option<String>,
    pub preview_status: String,
    pub preview_failure_reason: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub codec: Option<String>,
    pub intelligence: IntelligenceSummary,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntelligenceSummary {
    pub status: Option<String>,
    pub technical_quality_band: Option<String>,
    pub technical_quality_score: Option<f64>,
    pub recommendation: Option<String>,
    pub recommendation_confidence: Option<f64>,
    pub similarity_group_id: Option<String>,
    pub similarity_group_kind: Option<String>,
    pub similar_count: u64,
    pub face_count: u64,
    pub open_eyes_count: u64,
    pub possible_closed_eyes_count: u64,
    pub blur_level: Option<String>,
    pub sharpness_band: Option<String>,
    pub confidence: Option<f64>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualMediaPage {
    pub items: Vec<VisualMediaRow>,
    pub has_more: bool,
    pub cache_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCopyView {
    pub file_instance_id: String,
    pub relative_path: String,
    pub selected_root: Option<String>,
    pub storage_volume: String,
    pub is_available: bool,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAssetDetail {
    pub item: VisualMediaRow,
    pub metadata: Option<MediaMetadataRecord>,
    pub copies: Vec<MediaCopyView>,
    pub intelligence: Option<CaptureIntelligenceDetail>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureIntelligenceDetail {
    pub summary: IntelligenceSummary,
    pub input_fingerprint: String,
    pub provider: String,
    pub provider_version: String,
    pub settings_version: String,
    pub generated_at: String,
    pub technical: Option<TechnicalQualityView>,
    /// An empty `faces` collection alone is ambiguous: it may mean no face was detected, or
    /// that the approved local provider was unavailable. Preserve provider state so callers
    /// never turn unavailable analysis into a false zero-face claim.
    pub face_provider: String,
    pub face_provider_version: String,
    pub face_resolved_provider: String,
    pub face_resolved_provider_version: String,
    pub face_analysis_status: String,
    pub face_analysis_error: Option<String>,
    pub face_provider_attempt_error: Option<String>,
    pub face_landmark_status: String,
    pub face_landmark_error: Option<String>,
    pub faces: Vec<FaceAnalysisView>,
    pub recommendation_reasons: Vec<String>,
    pub human_decision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalQualityView {
    pub global_sharpness: Option<f64>,
    pub sharpness_band: String,
    pub directional_blur_ratio: Option<f64>,
    pub blur_level: String,
    pub mean_luminance: Option<f64>,
    pub median_luminance: Option<f64>,
    pub highlight_clipping_percent: Option<f64>,
    pub shadow_clipping_percent: Option<f64>,
    pub channel_clipping_percent: Option<f64>,
    pub technical_quality_score: Option<f64>,
    pub technical_quality_band: String,
    pub confidence: f64,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceAnalysisView {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub detection_confidence: f64,
    pub relative_size: f64,
    pub visibility: Option<String>,
    pub pose: Option<String>,
    pub face_sharpness: Option<f64>,
    pub eye_state: String,
    pub eye_confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityGroupView {
    pub id: String,
    pub kind: String,
    pub representative_asset_id: String,
    pub grouping_method: String,
    pub grouping_version: String,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub visual_similarity: Option<f64>,
    pub members: Vec<SimilarityGroupMemberView>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityGroupMemberView {
    pub asset_id: String,
    pub filename: String,
    pub medium_preview_url: Option<String>,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub is_representative: bool,
    pub intelligence: IntelligenceSummary,
    /// Bounded per-frame technical face boxes for the optional comparison view. These are
    /// detection observations only, never identity matches or persisted face crops.
    pub faces: Vec<FaceAnalysisView>,
}

/// A bounded culling query. It deliberately has no “load all decisions” form: professional
/// projects are expected to be much larger than an in-memory frontend list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CullingQuery {
    pub mode: String,
    pub filter: String,
    pub group_id: Option<String>,
    /// Optional M7 structural scope. It remains a normal MediaAsset decision workflow.
    pub moment_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for CullingQuery {
    fn default() -> Self {
        Self {
            mode: "all_photos".into(),
            filter: "all".into(),
            group_id: None,
            moment_id: None,
            limit: 60,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingDecisionView {
    pub decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub note: Option<String>,
    pub flags: Vec<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingMediaRow {
    pub media: VisualMediaRow,
    pub decision: CullingDecisionView,
    pub faces: Vec<FaceAnalysisView>,
    pub relative_evidence: Vec<String>,
    pub similarity_group_id: Option<String>,
    pub is_ai_representative: bool,
    pub is_human_representative: bool,
    /// Separate, optional local Studio Brain advice. It never replaces `decision` or generic
    /// Capture Intelligence evidence.
    pub studio_brain: Option<StudioRecommendationView>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingProgress {
    pub total: u64,
    pub reviewed: u64,
    pub keep: u64,
    pub reject: u64,
    pub review: u64,
    pub unreviewed: u64,
    pub starred: u64,
    pub five_star: u64,
    pub sets_total: u64,
    pub sets_reviewed: u64,
    /// Local agreement observations, not model-accuracy claims or ground truth.
    pub strong_candidate_kept: u64,
    pub technical_issue_kept: u64,
    pub strong_candidate_rejected: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingGroupSummary {
    pub id: String,
    pub kind: String,
    pub member_count: u64,
    pub ai_representative_asset_id: String,
    pub ai_representative_filename: String,
    pub human_representative_asset_id: Option<String>,
    pub human_representative_filename: Option<String>,
    pub reviewed_count: u64,
    pub completed: bool,
    /// `auto_all_reviewed` is derived from decision state; `explicit_user_completion` is the
    /// durable photographer override stored in `group_review_completion`.
    pub completion_kind: Option<String>,
    /// A non-mutating starting point only when an active local Studio model supplied genuine
    /// comparable-set evidence. It never changes either representative field above.
    pub studio_starting_point_asset_id: Option<String>,
    pub studio_starting_point_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSessionView {
    pub id: String,
    pub project_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub mode: String,
    pub last_asset_id: Option<String>,
    pub last_group_id: Option<String>,
    pub filter_context: Option<String>,
    pub photos_reviewed: u64,
    pub sets_reviewed: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingWorkspaceView {
    pub session: ReviewSessionView,
    pub progress: CullingProgress,
    pub items: Vec<CullingMediaRow>,
    pub groups: Vec<CullingGroupSummary>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CullingDecisionUpdate {
    pub decision: Option<CullingDecisionValue>,
    pub clear_decision: bool,
    pub rating: Option<u8>,
    pub starred: Option<bool>,
    pub note: Option<String>,
    pub flags: Option<Vec<String>>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingReportRow {
    pub filename: String,
    pub media_asset_id: String,
    pub decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub ai_recommendation: Option<String>,
    pub technical_score: Option<f64>,
    pub similarity_group_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferenceExampleView {
    pub id: String,
    pub project_id: String,
    pub similarity_group_id: String,
    pub chosen_asset_id: String,
    pub compared_asset_ids: Vec<String>,
    pub technical_evidence: serde_json::Value,
    pub ai_recommendation: serde_json::Value,
    pub human_decision_context: String,
    pub created_at: String,
}

/// Local-only, profile-scoped Studio Brain state for a single project. This deliberately keeps
/// a project's contribution to learning separate from whether that project may *use* an active
/// local personalization model.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioBrainProjectStatus {
    pub profile_id: String,
    pub profile_name: String,
    pub training_status: String,
    pub personalization_enabled: bool,
    pub project_included: bool,
    pub eligible_decision_count: u64,
    pub keep_count: u64,
    pub review_count: u64,
    pub reject_count: u64,
    pub rating_count: u64,
    pub starred_count: u64,
    pub representative_count: u64,
    pub contributing_project_count: u64,
    pub active_model_version: Option<String>,
    pub last_trained_at: Option<String>,
    pub readiness: serde_json::Value,
    pub last_error: Option<String>,
}

/// An immutable record of an explicit human signal. `feature_snapshot_json` is a compact,
/// engineered local snapshot; it never contains source paths, note text, raw semantic vectors,
/// or biometric identity data.
#[derive(Debug, Clone, PartialEq)]
pub struct StudioTrainingExampleRecord {
    pub id: String,
    pub profile_id: String,
    pub project_id: String,
    pub media_asset_id: Option<String>,
    pub source_kind: String,
    pub source_record_id: String,
    pub decision_type: String,
    pub decision_value: Option<String>,
    pub occurred_at: String,
    pub review_session_id: Option<String>,
    pub similarity_group_id: Option<String>,
    pub moment_id: Option<String>,
    pub generic_recommendation_json: serde_json::Value,
    pub studio_recommendation_id_at_decision: Option<String>,
    pub recommendation_shown: String,
    pub provenance: String,
    pub feature_schema_version: String,
    pub feature_snapshot_json: serde_json::Value,
    pub training_eligible: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudioTrainingRunRecord {
    pub id: String,
    pub profile_id: String,
    pub background_job_id: String,
    pub algorithm: String,
    pub algorithm_version: String,
    pub feature_schema_version: String,
    pub parameters_json: serde_json::Value,
    pub snapshot_hash: String,
    pub snapshot_count: u64,
    pub previous_active_model_id: Option<String>,
    pub state: String,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

/// Activation is distinguishable from an ordinary persistence failure. A source change simply
/// means the candidate's immutable snapshot is no longer current and must stay inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudioModelActivationOutcome {
    Activated,
    SourceSnapshotStale,
}

/// A bounded read projection used to keep a training snapshot out of an in-progress authoritative
/// human-action append. It exposes no source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StudioTrainingSourceState {
    pub revision: u64,
    pub materialization_pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudioModelRecord {
    pub id: String,
    pub profile_id: String,
    pub training_run_id: String,
    pub algorithm: String,
    pub model_version: String,
    pub feature_schema_version: String,
    pub artifact_json: serde_json::Value,
    pub checksum: String,
    pub artifact_size_bytes: u64,
    pub state: String,
    pub metrics_json: serde_json::Value,
    pub created_at: String,
    pub activated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioRecommendationView {
    pub recommendation: String,
    pub confidence_band: String,
    pub model_version: String,
    pub explanation_factors: Vec<String>,
    pub generic_recommendation: Option<String>,
    pub agreement: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StudioRecommendationRecord {
    pub id: String,
    pub profile_id: String,
    pub model_id: String,
    pub project_id: String,
    pub media_asset_id: String,
    pub feature_schema_version: String,
    pub feature_fingerprint: String,
    pub recommendation: String,
    pub confidence_band: String,
    pub explanation_json: serde_json::Value,
    pub generic_recommendation_json: serde_json::Value,
    pub agreement: String,
    pub generated_at: String,
}

/// Current compact, local feature candidates used only by Studio Brain orchestration. These are
/// deliberately engineered summaries rather than semantic embeddings or source-media data.
#[derive(Debug, Clone, PartialEq)]
pub struct StudioFeatureCandidate {
    pub project_id: String,
    pub media_asset_id: String,
    pub feature_snapshot_json: serde_json::Value,
    pub feature_fingerprint: String,
    pub generic_recommendation_json: serde_json::Value,
    pub similarity_group_id: Option<String>,
    pub moment_id: Option<String>,
}

/// An immutable, explicitly human-chosen comparison inside an existing Similar Set. Both
/// snapshots are bounded evidence from the original representative action; the record never
/// changes Similar Set membership or reinterprets a generic recommendation as a label.
#[derive(Debug, Clone, PartialEq)]
pub struct StudioPairwisePreferenceRecord {
    pub id: String,
    pub project_id: String,
    pub similarity_group_id: String,
    pub chosen_asset_id: String,
    pub alternative_asset_id: String,
    pub occurred_at: String,
    pub chosen_feature_snapshot_json: serde_json::Value,
    pub alternative_feature_snapshot_json: serde_json::Value,
    pub training_eligible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisInputCandidate {
    pub asset_id: String,
    pub project_id: String,
    pub file_instance_id: String,
    pub filename: String,
    pub media_type: String,
    pub input_fingerprint: String,
    pub preview_relative_path: Option<String>,
    pub preview_artifact_type: Option<String>,
    pub preview_size_class: Option<String>,
    pub preview_status: String,
    pub preview_failure_reason: Option<String>,
    pub content_hash: Option<String>,
    pub captured_at: Option<String>,
    pub camera_model: Option<String>,
    pub is_available: bool,
    pub perceptual_hash: Option<String>,
    pub difference_hash: Option<String>,
    pub color_signature: Option<Vec<u8>>,
    pub embedding: Option<Vec<i8>>,
    pub bucket_keys: Option<Vec<String>>,
    pub technical_quality_score: Option<f64>,
}

/// Provider tuple used to decide whether local face/eye evidence remains current. It is kept
/// separate from the deterministic image analyzer because an optional platform face capability
/// can change without changing the technical/similarity evidence contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceAnalysisProviderConfig {
    pub provider: String,
    pub provider_version: String,
    pub settings_version: String,
}

impl FaceAnalysisProviderConfig {
    pub fn new(
        provider: impl Into<String>,
        provider_version: impl Into<String>,
        settings_version: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            provider_version: provider_version.into(),
            settings_version: settings_version.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureIntelligenceTerminalCounts {
    pub total: u64,
    pub ready: u64,
    pub unsupported: u64,
    pub corrupt: u64,
    pub needs_original: u64,
    pub failed: u64,
    pub not_applicable: u64,
    pub stale: u64,
}

/// Identity of one compatible image/text embedding space. M6 never compares rows across this
/// tuple: a model, preprocessing, or metric change creates a separately versioned local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticModelConfig {
    pub model_id: String,
    pub provider: String,
    pub model_version: String,
    pub embedding_version: String,
    pub preprocessing_version: String,
    pub metric: String,
    pub dimensions: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticInputCandidate {
    pub asset_id: String,
    pub project_id: String,
    pub file_instance_id: String,
    pub filename: String,
    pub media_type: String,
    pub input_fingerprint: String,
    pub preview_relative_path: Option<String>,
    pub preview_status: String,
    pub preview_failure_reason: Option<String>,
    pub is_available: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticEmbeddingRecord {
    pub media_asset_id: MediaAssetId,
    pub project_id: ProjectId,
    pub input_fingerprint: String,
    pub model: SemanticModelConfig,
    pub embedding: Option<Vec<f32>>,
    pub generated_at: Timestamp,
    pub status: AnalysisStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSemanticVector {
    pub asset_id: String,
    pub vector: Vec<f32>,
}

/// Current workflow evidence joined with a semantic vector only for a bounded set of candidates.
/// The UI receives a separate result projection and never a raw vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchCandidate {
    pub asset_id: String,
    pub vector: Option<Vec<f32>>,
    pub face_count: Option<u64>,
    pub rating: u8,
    pub decision: Option<String>,
    pub sharpness_band: Option<String>,
    pub blur_level: Option<String>,
    pub technical_quality_band: Option<String>,
    pub technical_quality_score: Option<f64>,
    pub camera_model: Option<String>,
    pub captured_at: Option<String>,
}

/// A compact, project-owned row for local Moment Brain analysis. It deliberately contains only
/// durable catalog/analysis evidence; neither original paths nor previews are exposed here.
/// `face_count: None` means the existing anonymous-face evidence is unavailable, not zero.
#[derive(Debug, Clone, PartialEq)]
pub struct MomentAnalysisInput {
    pub asset_id: String,
    pub captured_at: Option<String>,
    pub capture_time_source: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub orientation: Option<String>,
    pub face_count: Option<u64>,
    pub technical_quality_band: Option<String>,
    pub technical_quality_score: Option<f64>,
    pub rating: u8,
    pub starred: bool,
    pub decision: Option<String>,
    pub similar_set_ids: Vec<String>,
    pub embedding: Option<Vec<f32>>,
}

/// A bounded, project-scoped handoff for an append-only Moment Brain update.
///
/// It contains complete memberships from only the two latest active structural Moments plus
/// media that is not currently an active member (including durable `ungrouped` rows). It never
/// materializes the rest of the catalog or its semantic vectors. A caller must request a full
/// rebuild when this is absent or when a new input is chronologically out of order.
#[derive(Debug, Clone, PartialEq)]
pub struct MomentIncrementalAnalysisWindow {
    pub timeline_id: String,
    pub active_run_id: String,
    pub active_semantic_model_key: Option<String>,
    /// Latest known timestamp in the active membership projection, if one exists.
    pub previous_latest_captured_at: Option<String>,
    /// One greater than the current maximum active membership ordinal.
    pub global_ordinal_base: u64,
    /// Earliest active membership ordinal represented by `preceding_context`.
    pub affected_tail_start_ordinal: u64,
    /// One greater than the maximum active Moment-record ordinal preserved before the tail.
    /// Core offsets new tail Moment/segment ordinals from this value to avoid UI ordering
    /// collisions with the earlier active projection.
    pub moment_ordinal_base: u64,
    /// Whole members of the latest two active Moments, never a partial Moment.
    pub preceding_context: Vec<MomentAnalysisInput>,
    /// Active `ungrouped` and newly catalogued still photos that are not active Moment members.
    pub pending_inputs: Vec<MomentAnalysisInput>,
}

/// Stored output from one local, versioned structural-analysis run. This is intentionally a
/// data record rather than a UI view: raw centroids and score components never cross the desktop
/// boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct MomentRecord {
    pub id: String,
    pub project_id: String,
    pub timeline_id: String,
    pub run_id: String,
    pub segment_id: String,
    pub anchor_asset_id: String,
    pub ordinal: u64,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub asset_count: u64,
    pub ai_representative_asset_id: Option<String>,
    pub centroid: Option<Vec<f32>>,
    pub centroid_dimensions: Option<usize>,
    pub suggested_label: Option<String>,
    pub label_confidence: Option<f64>,
    pub label_evidence: serde_json::Value,
    pub label_state: String,
    pub created_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MomentMembershipRecord {
    pub id: String,
    pub project_id: String,
    pub run_id: String,
    pub moment_id: Option<String>,
    pub media_asset_id: String,
    pub ordinal: u64,
    /// `member` or the explicit `ungrouped` state. An unavailable timestamp/evidence never
    /// becomes a fabricated membership.
    pub membership_state: String,
    pub created_at: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MomentBoundaryEvidenceRecord {
    pub id: String,
    pub project_id: String,
    pub run_id: String,
    pub left_asset_id: String,
    pub right_asset_id: String,
    pub ordinal: u64,
    pub category: String,
    pub components: serde_json::Value,
    pub explanation: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentTimelineStatusRecord {
    pub timeline_id: String,
    pub project_id: String,
    pub state: String,
    pub analyzer_id: String,
    pub analyzer_version: String,
    pub boundary_algorithm_version: String,
    pub semantic_model_key: Option<String>,
    pub input_catalog_version: String,
    pub active_run_id: Option<String>,
    pub moment_count: u64,
    pub eligible_count: u64,
    pub ungrouped_count: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentTimelineRow {
    pub id: String,
    /// Immutable local-analysis provenance. This stays behind the core boundary and is used
    /// only to determine whether a structurally safe adjacent merge can be offered.
    pub run_id: String,
    pub ordinal: u64,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub asset_count: u64,
    pub ai_representative_asset_id: Option<String>,
    pub human_representative_asset_id: Option<String>,
    pub suggested_label: Option<String>,
    pub human_label: Option<String>,
    pub display_label: String,
    pub label_state: String,
    pub similar_set_count: u64,
    pub keep_count: u64,
    pub reject_count: u64,
    pub review_count: u64,
    pub unreviewed_count: u64,
    pub starred_count: u64,
    pub technical_issue_count: u64,
    pub boundary_category: Option<String>,
    pub boundary_explanation: Option<String>,
    pub human_override_present: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineGapView {
    pub started_at: String,
    pub ended_at: String,
    pub duration_seconds: u64,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentTimelinePage {
    pub timeline: Option<MomentTimelineStatusRecord>,
    pub moments: Vec<MomentTimelineRow>,
    pub gaps: Vec<TimelineGapView>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentDetailRecord {
    pub moment: MomentTimelineRow,
    pub label_evidence: Vec<String>,
    pub membership_count: u64,
    pub has_human_label: bool,
    pub has_human_representative: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageChecklistItemView {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub confirmation_state: Option<String>,
    pub confirmed_at: Option<String>,
    pub moment_id: Option<String>,
    pub media_asset_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoverageChecklistItemRecord {
    pub id: String,
    pub project_id: String,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentAnalysisRunRecord {
    pub id: String,
    pub timeline_id: String,
    pub project_id: String,
    pub state: String,
    pub analyzer_id: String,
    pub analyzer_version: String,
    pub boundary_algorithm_version: String,
    pub semantic_model_key: Option<String>,
    pub input_catalog_version: String,
    pub items_total: u64,
    pub items_completed: u64,
    pub error_count: u64,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Advisory, local-only evidence that two actual camera clock streams may be offset. A missing
/// `possible_offset_seconds` is deliberately distinct from an offset of zero: callers must not
/// turn an absent or inconclusive diagnostic into a claim that no offset exists.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraClockOffsetDiagnosticRecord {
    pub id: String,
    pub project_id: String,
    pub run_id: String,
    pub camera_a: String,
    pub camera_b: String,
    pub possible_offset_seconds: Option<i64>,
    pub evidence_json: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineSegmentRecord {
    pub id: String,
    pub project_id: String,
    pub run_id: String,
    pub ordinal: u64,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub asset_count: u64,
    pub boundary_category: Option<String>,
    pub boundary_evidence: serde_json::Value,
    pub created_at: String,
    pub stale: bool,
}

/// A human structural constraint anchored to adjacent actual assets, never a fragile display
/// ordinal. `split` forces a boundary; `merge` prevents one when both assets remain eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentOverrideOperation {
    pub id: String,
    pub project_id: String,
    pub operation: String,
    pub left_asset_id: String,
    pub right_asset_id: String,
    pub created_at: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MomentSearchCandidate {
    pub moment_id: String,
    pub centroid: Vec<f32>,
    pub row: MomentTimelineRow,
}

/// Bounded, project-scoped deterministic predicates used by Magic Search when an image/text
/// model is unavailable or when a hybrid query needs metadata evidence. This deliberately
/// contains no embedding vector, filesystem path, or human-decision mutation capability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticMetadataQuery {
    pub face_count: Option<u64>,
    pub rating_exact: Option<u8>,
    pub rating_minimum: Option<u8>,
    pub decision: Option<String>,
    pub require_sharp: bool,
    pub require_blurry: bool,
    pub require_technical_issue: bool,
    pub camera_model: Option<String>,
    /// Optional active M7 Moment membership scope. This is an internal, project-validated
    /// predicate and never accepts caller-provided MediaAsset collections.
    pub moment_id: Option<String>,
    pub sort: SemanticMetadataSort,
    pub descending: bool,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SemanticMetadataSort {
    #[default]
    CaptureTime,
    TechnicalQuality,
    Rating,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticIndexVersion {
    pub id: SemanticIndexVersionId,
    pub project_id: ProjectId,
    pub model: SemanticModelConfig,
    pub index_format: String,
    pub index_relative_path: String,
    pub index_checksum: String,
    pub embedding_count: u64,
    pub status: String,
    pub stale: bool,
    pub created_at: Timestamp,
    pub rebuilt_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchHistoryEntry {
    pub id: String,
    pub project_id: String,
    pub query_text: String,
    pub normalized_query: String,
    pub plan: serde_json::Value,
    pub used_at: String,
    pub use_count: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticIndexTerminalCounts {
    pub total: u64,
    pub ready: u64,
    pub unsupported: u64,
    pub corrupt: u64,
    pub needs_original: u64,
    pub failed: u64,
    pub stale: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadataRecord {
    pub media_asset_id: String,
    pub source_file_instance_id: String,
    pub source_fingerprint: String,
    pub extractor: String,
    pub extractor_version: String,
    pub status: String,
    pub failure_reason: Option<String>,
    pub extracted_at: String,
    pub mime_type: Option<String>,
    pub byte_size: Option<u64>,
    pub captured_at_raw: Option<String>,
    pub captured_at_local: Option<String>,
    pub capture_timezone: Option<String>,
    pub capture_time_source: Option<String>,
    pub capture_time_confidence: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub focal_length_equivalent_mm: Option<f64>,
    pub aperture: Option<f64>,
    pub shutter_speed: Option<String>,
    pub iso: Option<u32>,
    pub exposure_compensation: Option<String>,
    pub flash: Option<String>,
    pub white_balance: Option<String>,
    pub color_space: Option<String>,
    pub gps_present: Option<bool>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub codec: Option<String>,
    pub pixel_format: Option<String>,
    pub bitrate: Option<u64>,
    pub audio_streams: Option<u32>,
    pub video_streams: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub raw_metadata: serde_json::Value,
}

/// One read-only metadata observation from a specific physical copy. The logical
/// `media_metadata` row holds the deterministic resolved value; these rows preserve the
/// per-copy evidence needed to diagnose a disagreement without treating filesystem times as
/// equivalent to camera capture metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTimeObservationRecord {
    pub media_asset_id: String,
    pub source_file_instance_id: String,
    pub source_fingerprint: String,
    pub extractor: String,
    pub extractor_version: String,
    pub status: String,
    pub failure_reason: Option<String>,
    pub extracted_at: String,
    pub captured_at_raw: Option<String>,
    pub captured_at_local: Option<String>,
    pub capture_timezone: Option<String>,
    pub capture_time_source: Option<String>,
    pub capture_time_confidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewArtifactRecord {
    pub id: String,
    pub media_asset_id: String,
    pub source_file_instance_id: String,
    pub artifact_type: String,
    pub size_class: String,
    pub cache_relative_path: String,
    pub provider: String,
    pub generator_version: String,
    pub source_fingerprint: String,
    pub created_at: String,
    pub status: String,
    pub failure_reason: Option<String>,
}

/// Minimal record used by the desktop-only rendering bridge. The database retains the relative
/// cache path; no browser URL or absolute filesystem path is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRenderArtifact {
    pub id: String,
    pub cache_relative_path: String,
}

/// Actual terminal evidence for one logical-media preparation result. These counts are derived
/// from the selected `FileInstance`'s preview artifacts or metadata status, not from a project
/// media-asset total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualPreparationTerminalCounts {
    pub processed: u64,
    pub ready: u64,
    pub unsupported: u64,
    pub corrupt: u64,
    pub offline: u64,
    pub failed: u64,
    pub timeout: u64,
    pub cancelled: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPreparationCandidate {
    pub asset_id: String,
    pub file_instance_id: String,
    pub filename: String,
    pub media_type: String,
    pub selected_root: Option<String>,
    pub relative_path: String,
    pub source_fingerprint: String,
    pub is_available: bool,
}

/// The human-configured intention. It deliberately has no export execution state: an immutable
/// Manifest captures a point-in-time plan, and an Export Job records one actual local execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPlanRecord {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub plan_type: ProductionPlanType,
    pub status: ProductionPlanStatus,
    pub selection_rules: SelectionRules,
    pub organization: OrganizationStrategy,
    pub filename_strategy: FilenameStrategy,
    pub destination_path: Option<String>,
    pub destination_reserve_bytes: u64,
    pub estimated_file_count: u64,
    pub estimated_bytes: u64,
    pub current_manifest_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPlanInput {
    pub name: String,
    pub plan_type: ProductionPlanType,
    pub selection_rules: SelectionRules,
    pub organization: OrganizationStrategy,
    pub filename_strategy: FilenameStrategy,
}

/// Explicit planning inputs read atomically from the local catalog for one dry-run/manifest
/// attempt. The source revision is the transactionally rechecked anti-race token.
pub type ProductionManifestBuildInput = (
    ProductionPlanRecord,
    Vec<PlanOverride>,
    Vec<DeliveryAssetCandidate>,
    BTreeMap<String, String>,
    u64,
    Option<Vec<String>>,
);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualCollectionRecord {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub kind: VirtualCollectionKind,
    pub rules: SelectionRules,
    pub created_at: String,
    pub updated_at: String,
    pub asset_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualCollectionInput {
    pub name: String,
    pub kind: VirtualCollectionKind,
    pub rules: SelectionRules,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionWorkspaceView {
    pub plans: Vec<ProductionPlanRecord>,
    pub collections: Vec<VirtualCollectionRecord>,
    /// Bounded presentation history only; complete durable job history remains queryable in the
    /// catalog and is never deleted by this workspace projection.
    pub recent_exports: Vec<ExportJobRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportManifestRecord {
    pub id: String,
    pub plan_id: String,
    pub project_id: String,
    pub manifest_version: u64,
    pub source_revision: u64,
    pub status: String,
    pub selection_snapshot: serde_json::Value,
    pub organization_snapshot: serde_json::Value,
    pub filename_strategy_snapshot: serde_json::Value,
    pub destination_path: String,
    pub selected_file_count: u64,
    pub estimated_bytes: u64,
    pub checksum: String,
    pub validation: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportManifestEntryRecord {
    pub id: String,
    pub manifest_id: String,
    pub ordinal: u64,
    pub media_asset_id: String,
    pub selected_file_instance_id: Option<String>,
    pub original_filename: String,
    pub destination_relative_path: String,
    pub destination_filename: String,
    pub expected_byte_size: u64,
    pub source_checksum: Option<String>,
    pub human_decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub moment_id: Option<String>,
    pub moment_label: Option<String>,
    pub status: String,
    pub issue: Option<String>,
}

/// Private execution projection. This remains inside the backend because it includes local root
/// resolution metadata; normal plan/manifest views never expose source filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportManifestExecutionEntry {
    pub entry: ExportManifestEntryRecord,
    pub source_root_path: Option<String>,
    pub source_relative_path: Option<String>,
    pub source_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJobRecord {
    pub id: String,
    pub plan_id: String,
    pub manifest_id: String,
    pub background_job_id: String,
    pub state: String,
    pub destination_path: String,
    pub items_total: u64,
    pub items_completed: u64,
    pub verified_count: u64,
    pub skipped_identical_count: u64,
    pub failed_count: u64,
    pub verified_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJobEntryUpdate {
    pub manifest_entry_id: String,
    pub state: String,
    pub copied_bytes: u64,
    pub source_checksum: Option<String>,
    pub destination_checksum: Option<String>,
    pub error_message: Option<String>,
}

/// Immutable, local handoff summary for one completed or partially completed export. The
/// serialized content intentionally excludes source locations, internal asset identities,
/// notes, and any AI or Studio Brain evidence.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReportRecord {
    pub id: String,
    pub export_job_id: String,
    pub manifest_checksum: String,
    pub report_json: serde_json::Value,
    pub report_text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPreflight {
    pub manifest: ExportManifestRecord,
    /// Execution-only entry details stay inside the backend. Serializing every entry here would
    /// make a 100k-file preflight load a full manifest into the desktop webview.
    #[serde(skip_serializing)]
    pub entries: Vec<ExportManifestEntryRecord>,
    pub destination_writable: bool,
    pub available_bytes: Option<u64>,
    pub required_bytes: u64,
    pub reserve_bytes: u64,
    pub headroom_bytes: Option<u64>,
    pub available_source_count: u64,
    pub offline_source_count: u64,
    pub existing_identical_count: u64,
    pub collision_count: u64,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl ProductionPreflight {
    pub fn can_start(&self) -> bool {
        self.blockers.is_empty()
    }
}

pub trait CatalogRepository {
    fn create_project(&self, name: &str) -> Result<Project>;
    fn projects(&self) -> Result<Vec<Project>>;
    fn insert_project(&self, project: &Project) -> Result<()>;
    fn get_project(&self, id: &ProjectId) -> Result<Option<Project>>;
    fn insert_shoot(&self, shoot: &Shoot) -> Result<()>;
    fn insert_moment(&self, moment: &Moment) -> Result<()>;
    fn insert_person_cluster(&self, person: &PersonCluster) -> Result<()>;
    fn insert_capture_device(&self, device: &CaptureDevice) -> Result<()>;
    fn insert_storage_volume(&self, volume: &StorageVolume) -> Result<()>;
    fn update_volume_state(
        &self,
        id: &StorageVolumeId,
        state: MountState,
        mount: Option<&str>,
    ) -> Result<()>;
    fn get_storage_volume(&self, id: &StorageVolumeId) -> Result<Option<StorageVolume>>;
    fn storage_volume_by_filesystem_identity(
        &self,
        identity: &str,
    ) -> Result<Option<StorageVolume>>;
    fn update_storage_volume(&self, volume: &StorageVolume) -> Result<()>;
    fn register_index_root(&self, root: &IndexRoot) -> Result<IndexRoot>;
    fn index_roots_for_project(&self, project_id: &ProjectId) -> Result<Vec<IndexRoot>>;
    fn update_index_root(&self, root: &IndexRoot) -> Result<()>;
    fn insert_media_asset(&self, asset: &MediaAsset) -> Result<()>;
    fn media_asset_by_fingerprint(
        &self,
        project_id: &ProjectId,
        media_type: &MediaType,
        extension: Option<&str>,
        fingerprint: &MediaFingerprint,
    ) -> Result<Option<MediaAsset>>;
    fn media_asset_by_content_hash(
        &self,
        project_id: &ProjectId,
        media_type: &MediaType,
        extension: Option<&str>,
        byte_size: u64,
        content_hash: &str,
    ) -> Result<Option<MediaAsset>>;
    fn insert_file_instance(&self, instance: &FileInstance) -> Result<()>;
    fn upsert_file_instance(&self, instance: &FileInstance) -> Result<FileInstance>;
    fn mark_root_instances_unavailable(&self, index_root_id: &IndexRootId) -> Result<()>;
    fn associate_sidecars_for_root(&self, index_root_id: &IndexRootId) -> Result<u64>;
    fn file_instances_for_asset(&self, asset_id: &MediaAssetId) -> Result<Vec<FileInstance>>;
    fn insert_relationship(&self, relationship: &Relationship) -> Result<()>;
    fn relationships(&self) -> Result<Vec<Relationship>>;
    fn insert_derivative(&self, derivative: &Derivative) -> Result<()>;
    fn derivatives_from(&self, source_asset_id: &MediaAssetId) -> Result<Vec<Derivative>>;
    fn insert_backup_copy(&self, backup: &BackupCopy) -> Result<()>;
    fn insert_correction(&self, correction: &Correction) -> Result<()>;
    fn corrections_for(&self, kind: &str, id: &str) -> Result<Vec<Correction>>;
    fn insert_background_job(&self, job: &BackgroundJob) -> Result<()>;
    fn update_background_job(&self, job: &BackgroundJob) -> Result<()>;
    fn get_background_job(&self, id: &JobId) -> Result<Option<BackgroundJob>>;
    fn recent_background_job(&self, project_id: &ProjectId) -> Result<Option<BackgroundJob>>;
    fn insert_index_issue(&self, issue: &IndexIssue) -> Result<()>;
    fn indexed_media(
        &self,
        project_id: &ProjectId,
        filter: MediaBrowserFilter,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<IndexedMediaRow>>;
    fn visual_media_page(
        &self,
        project_id: &ProjectId,
        query: &VisualMediaQuery,
    ) -> Result<VisualMediaPage>;
    fn media_asset_detail(&self, asset_id: &MediaAssetId) -> Result<Option<MediaAssetDetail>>;
    fn media_asset_belongs_to_project(
        &self,
        asset_id: &MediaAssetId,
        project_id: &ProjectId,
    ) -> Result<bool>;
    fn visual_preparation_candidates(
        &self,
        project_id: &ProjectId,
        query: &VisualMediaQuery,
    ) -> Result<Vec<MediaPreparationCandidate>>;
    fn failed_visual_preparation_candidates(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<MediaPreparationCandidate>>;
    fn preparation_file_instance_candidates(
        &self,
        asset_id: &str,
    ) -> Result<Vec<MediaPreparationCandidate>>;
    /// Returns whole logical assets in a stable bounded batch, with every currently available
    /// physical copy for each selected asset. It is used only by the explicit metadata refresh
    /// worker, never by frontend pagination.
    fn capture_time_refresh_candidates(
        &self,
        project_id: &ProjectId,
        after_asset_id: Option<&str>,
        asset_limit: u32,
    ) -> Result<Vec<MediaPreparationCandidate>>;
    fn capture_time_refresh_asset_count(&self, project_id: &ProjectId) -> Result<u64>;
    fn upsert_capture_time_observation(
        &self,
        observation: &CaptureTimeObservationRecord,
    ) -> Result<()>;
    fn upsert_media_metadata(&self, metadata: &MediaMetadataRecord) -> Result<()>;
    fn upsert_preview_artifact(&self, artifact: &PreviewArtifactRecord) -> Result<()>;
    fn clear_preview_artifacts(&self, project_id: &ProjectId) -> Result<()>;
    fn preview_render_artifact(&self, artifact_id: &str) -> Result<Option<PreviewRenderArtifact>>;
    fn latest_visual_preparation_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>>;
    fn visual_preparation_terminal_counts(
        &self,
        project_id: &ProjectId,
    ) -> Result<VisualPreparationTerminalCounts>;
    fn analysis_input_candidates(
        &self,
        project_id: &ProjectId,
        provider: &str,
        provider_version: &str,
        settings_version: &str,
        face_provider: &FaceAnalysisProviderConfig,
    ) -> Result<Vec<AnalysisInputCandidate>>;
    /// Returns only current READY primary analyses whose separate face artifact is missing or
    /// stale. This permits a face-provider repair without re-running technical evidence,
    /// embeddings, grouping, or preview generation.
    fn face_analysis_input_candidates(
        &self,
        project_id: &ProjectId,
        provider: &str,
        provider_version: &str,
        settings_version: &str,
        face_provider: &FaceAnalysisProviderConfig,
    ) -> Result<Vec<AnalysisInputCandidate>>;
    fn upsert_analysis_artifact(&self, artifact: &AnalysisArtifact) -> Result<()>;
    fn upsert_visual_fingerprint(&self, fingerprint: &VisualFingerprint) -> Result<()>;
    fn upsert_technical_quality(&self, evidence: &TechnicalQualityEvidence) -> Result<()>;
    fn replace_face_analyses(
        &self,
        asset_id: &MediaAssetId,
        input_fingerprint: &str,
        faces: &[FaceAnalysis],
    ) -> Result<()>;
    fn replace_similarity_groups(
        &self,
        project_id: &ProjectId,
        groups: &[SimilarityGroup],
        members: &[SimilarityGroupMember],
    ) -> Result<()>;
    fn upsert_ai_recommendation(&self, recommendation: &AiRecommendation) -> Result<()>;
    fn upsert_human_decision(&self, decision: &HumanDecision) -> Result<()>;
    fn grouping_inputs(&self, project_id: &ProjectId) -> Result<Vec<AnalysisInputCandidate>>;
    fn capture_intelligence_detail(
        &self,
        asset_id: &MediaAssetId,
    ) -> Result<Option<CaptureIntelligenceDetail>>;
    fn similarity_group_for_asset(
        &self,
        asset_id: &MediaAssetId,
        limit: u32,
        offset: u32,
    ) -> Result<Option<SimilarityGroupView>>;
    fn start_or_resume_review_session(
        &self,
        project_id: &ProjectId,
        mode: &str,
        filter_context: Option<&str>,
    ) -> Result<ReviewSessionView>;
    fn culling_workspace(
        &self,
        project_id: &ProjectId,
        query: &CullingQuery,
    ) -> Result<CullingWorkspaceView>;
    fn culling_progress(&self, project_id: &ProjectId) -> Result<CullingProgress>;
    fn update_culling_decision(
        &self,
        project_id: &ProjectId,
        asset_id: &MediaAssetId,
        update: &CullingDecisionUpdate,
    ) -> Result<CullingDecisionView>;
    fn update_review_position(
        &self,
        project_id: &ProjectId,
        session_id: &str,
        asset_id: Option<&MediaAssetId>,
        group_id: Option<&str>,
        mode: &str,
        filter_context: Option<&str>,
    ) -> Result<()>;
    fn set_group_human_representative(
        &self,
        project_id: &ProjectId,
        group_id: &str,
        asset_id: &MediaAssetId,
        session_id: Option<&str>,
    ) -> Result<()>;
    fn mark_group_review_complete(
        &self,
        project_id: &ProjectId,
        group_id: &str,
        session_id: Option<&str>,
    ) -> Result<()>;
    fn finish_review_session(
        &self,
        project_id: &ProjectId,
        session_id: &str,
    ) -> Result<ReviewSessionView>;
    fn culling_report(&self, project_id: &ProjectId) -> Result<Vec<CullingReportRow>>;
    fn preference_examples(&self, project_id: &ProjectId) -> Result<Vec<PreferenceExampleView>>;
    /// Creates/returns the one local default profile. This is identity-only and does not scan
    /// historical decisions or train a model.
    fn ensure_default_studio_profile(&self) -> Result<String>;
    /// Cheap status projection used by project opening. It must never enqueue training, load a
    /// model artifact, or backfill an unbounded catalog.
    fn studio_brain_project_status(
        &self,
        project_id: &ProjectId,
    ) -> Result<StudioBrainProjectStatus>;
    fn set_project_training_included(
        &self,
        profile_id: &str,
        project_id: &ProjectId,
        included: bool,
    ) -> Result<()>;
    fn set_studio_personalization_enabled(&self, profile_id: &str, enabled: bool) -> Result<()>;
    fn update_studio_profile_training_state(
        &self,
        profile_id: &str,
        status: &str,
        readiness: &serde_json::Value,
        error_message: Option<&str>,
    ) -> Result<()>;
    /// Explicit-training backfill from immutable existing human history. It is idempotent and
    /// never runs while merely opening a project.
    fn materialize_historical_studio_training_examples(&self, profile_id: &str) -> Result<u64>;
    fn studio_training_examples(
        &self,
        profile_id: &str,
    ) -> Result<Vec<StudioTrainingExampleRecord>>;
    /// A monotonic local token for changes to the explicit eligible-source set. A candidate
    /// captures it with its snapshot and activation checks it inside the write transaction.
    fn studio_training_source_revision(&self, profile_id: &str) -> Result<u64>;
    fn studio_training_source_state(&self, profile_id: &str) -> Result<StudioTrainingSourceState>;
    fn studio_feature_candidates(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<StudioFeatureCandidate>>;
    /// Returns only explicit Similar Set representative comparisons whose profile/project/
    /// decision-level eligibility is currently active. The underlying source snapshots are
    /// immutable and contain no path, filename, note, raw embedding, or identity data.
    fn studio_pairwise_preferences(
        &self,
        profile_id: &str,
    ) -> Result<Vec<StudioPairwisePreferenceRecord>>;
    fn set_studio_training_example_excluded(
        &self,
        profile_id: &str,
        example_id: &str,
        excluded: bool,
    ) -> Result<()>;
    fn create_studio_training_run(&self, record: &StudioTrainingRunRecord) -> Result<()>;
    fn update_studio_training_run(&self, record: &StudioTrainingRunRecord) -> Result<()>;
    fn store_studio_training_snapshot(
        &self,
        run_id: &str,
        example_id: &str,
        split: &str,
        feature_snapshot_json: &serde_json::Value,
        label: Option<&str>,
    ) -> Result<()>;
    fn store_studio_model(&self, record: &StudioModelRecord) -> Result<()>;
    /// Validates persisted candidate identity, activates it, and completes its durable
    /// background job in one transaction. The caller must have already validated the
    /// structured artifact and checksum.
    fn activate_studio_model(
        &self,
        profile_id: &str,
        model_id: &str,
        expected_source_revision: u64,
        completed_job: &BackgroundJob,
    ) -> Result<StudioModelActivationOutcome>;
    fn active_studio_model(&self, profile_id: &str) -> Result<Option<StudioModelRecord>>;
    fn latest_studio_training_run(
        &self,
        profile_id: &str,
    ) -> Result<Option<StudioTrainingRunRecord>>;
    fn replace_studio_recommendations(
        &self,
        profile_id: &str,
        model_id: &str,
        project_id: &ProjectId,
        recommendations: &[StudioRecommendationRecord],
    ) -> Result<()>;
    fn reset_studio_personalization(&self, profile_id: &str) -> Result<()>;
    fn recover_interrupted_studio_training(&self) -> Result<u64>;
    /// Production reads are cheap project-opening projections. They never build a manifest,
    /// inspect a destination, or start a copy job.
    fn production_workspace(&self, project_id: &ProjectId) -> Result<ProductionWorkspaceView>;
    fn create_production_plan(
        &self,
        project_id: &ProjectId,
        input: &ProductionPlanInput,
    ) -> Result<ProductionPlanRecord>;
    fn update_production_plan_destination(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        destination_path: Option<&str>,
    ) -> Result<ProductionPlanRecord>;
    fn update_production_plan_destination_reserve(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        reserve_bytes: u64,
    ) -> Result<ProductionPlanRecord>;
    fn update_production_plan_configuration(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        input: &ProductionPlanInput,
    ) -> Result<ProductionPlanRecord>;
    fn production_plan(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
    ) -> Result<Option<ProductionPlanRecord>>;
    fn set_production_plan_override(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        media_asset_id: &MediaAssetId,
        kind: Option<PlanOverrideKind>,
    ) -> Result<()>;
    fn create_virtual_collection(
        &self,
        project_id: &ProjectId,
        input: &VirtualCollectionInput,
    ) -> Result<VirtualCollectionRecord>;
    fn set_static_virtual_collection_members(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
        media_asset_ids: &[MediaAssetId],
    ) -> Result<()>;
    /// Adds or removes one explicit static member without loading the collection into a desktop
    /// webview. This preserves the 100k-project memory boundary.
    fn set_static_virtual_collection_member(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
        media_asset_id: &MediaAssetId,
        included: bool,
    ) -> Result<()>;
    fn virtual_collection_assets(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
    ) -> Result<Vec<String>>;
    /// Bounded only by actual project selection; it returns storage-safe source summaries and no
    /// absolute paths. Core invokes it only for an explicit Preview/Manifest action.
    fn production_manifest_build_input(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
    ) -> Result<ProductionManifestBuildInput>;
    /// Persists a full manifest and every entry in one transaction. Failed creation leaves a
    /// previous valid manifest active and never creates a partial exportable snapshot.
    fn create_export_manifest(
        &self,
        plan: &ProductionPlanRecord,
        source_revision: u64,
        destination_path: &str,
        validation: &serde_json::Value,
        entries: &[ManifestEntryDraft],
        checksum: &str,
    ) -> Result<ExportManifestRecord>;
    fn export_manifest(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Option<ExportManifestRecord>>;
    fn export_manifest_entries(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Vec<ExportManifestEntryRecord>>;
    fn export_manifest_execution_entries(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Vec<ExportManifestExecutionEntry>>;
    fn create_export_job(&self, record: &ExportJobRecord, job: &BackgroundJob) -> Result<()>;
    fn export_job(&self, project_id: &ProjectId, job_id: &str) -> Result<Option<ExportJobRecord>>;
    fn latest_export_job(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Option<ExportJobRecord>>;
    fn update_export_job_entry(
        &self,
        export_job_id: &str,
        update: &ExportJobEntryUpdate,
    ) -> Result<()>;
    fn cancel_pending_export_job_entries(&self, export_job_id: &str, message: &str) -> Result<()>;
    fn update_export_job(
        &self,
        record: &ExportJobRecord,
        background_job: &BackgroundJob,
    ) -> Result<()>;
    /// Persists the same privacy-preserving report emitted to the selected local destination.
    /// One immutable report is allowed per execution; retries must create a new Export Job.
    fn store_delivery_report(&self, report: &DeliveryReportRecord) -> Result<()>;
    fn recover_interrupted_production_exports(&self) -> Result<u64>;
    fn latest_capture_intelligence_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>>;
    fn capture_intelligence_terminal_counts(
        &self,
        project_id: &ProjectId,
    ) -> Result<CaptureIntelligenceTerminalCounts>;
    fn recover_interrupted_capture_intelligence(&self) -> Result<u64>;
    fn upsert_local_model(&self, record: &LocalModelRecord) -> Result<()>;
    fn local_model(
        &self,
        model_id: &str,
        provider: &str,
        version: &str,
    ) -> Result<Option<LocalModelRecord>>;
    /// Dedicated M6 candidate discovery. Unlike `analysis_input_candidates`, this never stales
    /// or otherwise mutates Capture Intelligence evidence.
    fn semantic_embedding_candidates(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<SemanticInputCandidate>>;
    fn upsert_semantic_embedding(&self, embedding: &SemanticEmbeddingRecord) -> Result<()>;
    /// A model, preprocessing, or metric replacement never erases prior local evidence. It
    /// marks it stale so the active index cannot silently compare incompatible vectors.
    fn mark_other_semantic_embeddings_stale(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<u64>;
    fn semantic_embeddings_for_index(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<StoredSemanticVector>>;
    fn semantic_search_candidates_for_assets(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
        asset_ids: &[String],
    ) -> Result<Vec<SemanticSearchCandidate>>;
    fn semantic_metadata_candidates(
        &self,
        project_id: &ProjectId,
        query: &SemanticMetadataQuery,
    ) -> Result<Vec<SemanticSearchCandidate>>;
    fn upsert_semantic_index_version(&self, version: &SemanticIndexVersion) -> Result<()>;
    fn active_semantic_index_version(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Option<SemanticIndexVersion>>;
    fn mark_other_semantic_index_versions_stale(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<u64>;
    fn record_magic_search_history(
        &self,
        project_id: &ProjectId,
        query_text: &str,
        normalized_query: &str,
        plan: &serde_json::Value,
    ) -> Result<()>;
    fn magic_search_history(
        &self,
        project_id: &ProjectId,
        limit: u32,
    ) -> Result<Vec<MagicSearchHistoryEntry>>;
    fn clear_magic_search_history(&self, project_id: &ProjectId) -> Result<u64>;
    fn latest_semantic_indexing_job(&self, project_id: &ProjectId)
        -> Result<Option<BackgroundJob>>;
    fn semantic_index_terminal_counts(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<SemanticIndexTerminalCounts>;
    fn recover_interrupted_semantic_indexing(&self) -> Result<u64>;
    /// Reads only local, durable still-photo evidence for Moment Brain. A model tuple filters
    /// incompatible M6 vectors rather than ever mixing semantic spaces.
    fn moment_analysis_inputs(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
    ) -> Result<Vec<MomentAnalysisInput>>;
    /// Returns a small append-only handoff when a project has an active Moment timeline. This
    /// does not replace active rows; core must preserve/reapply human overrides around any later
    /// projection write. `None` deliberately asks the caller to use a first/full analysis.
    fn moment_incremental_analysis_window(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
    ) -> Result<Option<MomentIncrementalAnalysisWindow>>;
    fn active_moment_override_operations(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<MomentOverrideOperation>>;
    /// Replaces only the active AI-derived projection. Historical runs, human events, labels,
    /// representatives, and override operations remain durable and untouched.
    fn replace_active_moment_analysis(
        &self,
        timeline: &MomentTimelineStatusRecord,
        run: &MomentAnalysisRunRecord,
        segments: &[TimelineSegmentRecord],
        moments: &[MomentRecord],
        memberships: &[MomentMembershipRecord],
        boundaries: &[MomentBoundaryEvidenceRecord],
    ) -> Result<()>;
    /// Replaces a structurally whole active tail only. Earlier active records/memberships remain
    /// visible, while tail history is marked stale/inactive. The caller must use the bounded
    /// window's `affected_tail_start_ordinal` and preserve human constraints in the replacement.
    // The replacement is intentionally kept as distinct durable projections so callers cannot
    // accidentally combine a tail write with an unrelated full projection.
    #[allow(clippy::too_many_arguments)]
    fn replace_active_moment_analysis_tail(
        &self,
        timeline: &MomentTimelineStatusRecord,
        run: &MomentAnalysisRunRecord,
        affected_tail_start_ordinal: u64,
        segments: &[TimelineSegmentRecord],
        moments: &[MomentRecord],
        memberships: &[MomentMembershipRecord],
        boundaries: &[MomentBoundaryEvidenceRecord],
    ) -> Result<()>;
    fn moment_timeline_status(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<MomentTimelineStatusRecord>>;
    fn moment_timeline_page(
        &self,
        project_id: &ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<MomentTimelinePage>;
    fn moment_detail(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
    ) -> Result<Option<MomentDetailRecord>>;
    fn moment_belongs_to_project(&self, project_id: &ProjectId, moment_id: &str) -> Result<bool>;
    fn moment_contains_asset(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_id: &str,
    ) -> Result<bool>;
    fn filter_active_moment_assets(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<String>>;
    fn rename_moment(&self, project_id: &ProjectId, moment_id: &str, label: &str) -> Result<()>;
    fn set_moment_human_representative(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_id: &str,
    ) -> Result<()>;
    fn merge_adjacent_moments(
        &self,
        project_id: &ProjectId,
        left_moment_id: &str,
        right_moment_id: &str,
    ) -> Result<()>;
    fn split_moment(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        after_asset_id: &str,
    ) -> Result<()>;
    fn coverage_checklist_items(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<CoverageChecklistItemView>>;
    fn create_coverage_checklist_item(&self, item: &CoverageChecklistItemRecord) -> Result<()>;
    fn update_coverage_confirmation(
        &self,
        project_id: &ProjectId,
        checklist_item_id: &str,
        state: &str,
        moment_id: Option<&str>,
        media_asset_id: Option<&str>,
    ) -> Result<()>;
    /// Appends advisory local clock evidence for one completed Moment-analysis run. This never
    /// writes capture timestamps or attempts a clock correction.
    fn record_camera_clock_offset_diagnostics(
        &self,
        diagnostics: &[CameraClockOffsetDiagnosticRecord],
    ) -> Result<()>;
    /// Reads diagnostics attached to the project's current active Moment-analysis run. An empty
    /// result means no current diagnostic is available; it must never be presented as evidence
    /// that all camera clocks are aligned.
    fn latest_camera_clock_offset_diagnostics(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<CameraClockOffsetDiagnosticRecord>>;
    fn moment_search_candidates(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<MomentSearchCandidate>>;
    fn latest_moment_analysis_job(&self, project_id: &ProjectId) -> Result<Option<BackgroundJob>>;
    fn recover_interrupted_moment_analysis(&self) -> Result<u64>;
    fn project_index_summary(&self, project_id: &ProjectId) -> Result<ProjectIndexSummary>;
    fn project_library(&self) -> Result<Vec<ProjectLibraryItem>>;
    fn insert_ingest_job(&self, job: &IngestJob) -> Result<()>;
    fn update_ingest_job(&self, job: &IngestJob) -> Result<()>;
    fn ingest_job(&self, id: &IngestJobId) -> Result<Option<IngestJob>>;
    fn ingest_job_by_start_request_id(&self, start_request_id: &str) -> Result<Option<IngestJob>>;
    fn ingest_jobs_for_project(&self, project_id: &ProjectId) -> Result<Vec<IngestJobSummary>>;
    fn insert_ingest_source(&self, source: &IngestSource, warnings: &[String]) -> Result<()>;
    fn update_ingest_source_status(
        &self,
        id: &IngestSourceId,
        status: IngestItemState,
    ) -> Result<()>;
    fn ingest_sources(&self, job_id: &IngestJobId) -> Result<Vec<IngestSourceSummary>>;
    fn insert_ingest_destination(&self, destination: &IngestDestination) -> Result<()>;
    fn update_ingest_destination_status(
        &self,
        id: &IngestDestinationId,
        status: IngestItemState,
    ) -> Result<()>;
    fn ingest_destinations(&self, job_id: &IngestJobId) -> Result<Vec<IngestDestinationSummary>>;
    fn upsert_ingest_item(&self, item: &IngestItemRecord) -> Result<IngestItemRecord>;
    fn ingest_items(&self, job_id: &IngestJobId) -> Result<Vec<IngestItemRecord>>;
    fn insert_ingest_audit_event(&self, event: &IngestAuditEvent) -> Result<()>;
    fn ingest_audit_events(&self, job_id: &IngestJobId) -> Result<Vec<IngestAuditEvent>>;
    fn ingest_report(&self, job_id: &IngestJobId) -> Result<Option<IngestReport>>;
    fn recover_interrupted_ingests(&self) -> Result<u64>;
    fn recover_interrupted_visual_preparations(&self) -> Result<u64>;
    fn counts(&self) -> Result<CatalogCounts>;
}

pub struct SqliteRepository {
    connection: Connection,
}

impl SqliteRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let repository = Self { connection };
        repository.migrate()?;
        Ok(repository)
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let repository = Self { connection };
        repository.migrate()?;
        Ok(repository)
    }

    pub fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(PersistenceError::InvalidData(format!(
                "catalog schema version {version} is newer than supported version {SCHEMA_VERSION}"
            )));
        }
        if version == 0 {
            self.connection.execute_batch(MIGRATION_001)?;
            self.connection.pragma_update(None, "user_version", 1_i64)?;
        }
        if version < 2 {
            self.connection.execute_batch(MIGRATION_002)?;
            self.connection.pragma_update(None, "user_version", 2_i64)?;
        }
        if version < 3 {
            self.connection.execute_batch(MIGRATION_003)?;
            self.connection.pragma_update(None, "user_version", 3_i64)?;
        }
        if version < 4 {
            self.connection.execute_batch(MIGRATION_004)?;
            self.connection.pragma_update(None, "user_version", 4_i64)?;
        }
        if version < 5 {
            self.connection.execute_batch(MIGRATION_005)?;
            self.connection.pragma_update(None, "user_version", 5_i64)?;
        }
        if version < 6 {
            self.connection.execute_batch(MIGRATION_006)?;
            self.connection.pragma_update(None, "user_version", 6_i64)?;
        }
        if version < 7 {
            self.connection.execute_batch(MIGRATION_007)?;
            self.connection.pragma_update(None, "user_version", 7_i64)?;
        }
        if version < 8 {
            self.connection.execute_batch(MIGRATION_008)?;
            self.connection.pragma_update(None, "user_version", 8_i64)?;
        }
        if version < 9 {
            self.migrate_m4_evidence_history()?;
            self.connection.pragma_update(None, "user_version", 9_i64)?;
        }
        if version < 10 {
            self.connection.execute_batch(MIGRATION_010)?;
            self.connection
                .pragma_update(None, "user_version", 10_i64)?;
        }
        if version < 11 {
            self.connection.execute_batch(MIGRATION_011)?;
            self.connection
                .pragma_update(None, "user_version", 11_i64)?;
        }
        if version < 12 {
            self.connection.execute_batch(MIGRATION_012)?;
            self.connection
                .pragma_update(None, "user_version", 12_i64)?;
        }
        if version < 13 {
            self.connection.execute_batch(MIGRATION_013)?;
            self.connection
                .pragma_update(None, "user_version", 13_i64)?;
        }
        if version < 14 {
            self.connection.execute_batch(MIGRATION_014)?;
            self.connection
                .pragma_update(None, "user_version", 14_i64)?;
        }
        if version < 15 {
            self.connection.execute_batch(MIGRATION_015)?;
            self.connection
                .pragma_update(None, "user_version", 15_i64)?;
        }
        if version < 16 {
            self.connection.execute_batch(MIGRATION_016)?;
            self.connection
                .pragma_update(None, "user_version", 16_i64)?;
        }
        if version < 17 {
            self.connection.execute_batch(MIGRATION_017)?;
            self.connection
                .pragma_update(None, "user_version", 17_i64)?;
        }
        if version < 18 {
            self.connection.execute_batch(MIGRATION_018)?;
            self.connection
                .pragma_update(None, "user_version", 18_i64)?;
        }
        if version < 19 {
            self.connection.execute_batch(MIGRATION_019)?;
            self.connection
                .pragma_update(None, "user_version", 19_i64)?;
        }
        if version < 20 {
            self.connection.execute_batch(MIGRATION_020)?;
            self.connection
                .pragma_update(None, "user_version", 20_i64)?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Version 8 stored only one visual/technical row per asset. Version 9 keeps every
    /// analyzer/input revision so a later preview or provider change cannot rewrite the
    /// evidence that motivated a prior human choice. `analysis_recommendations` is referenced
    /// by append-only human decisions, so its table is rebuilt with foreign-key enforcement
    /// temporarily disabled while the original IDs are copied verbatim. The final
    /// `foreign_key_check` makes the migration fail closed if that preservation ever breaks.
    fn migrate_m4_evidence_history(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")?;
        let migration = self.connection.execute_batch(MIGRATION_009);
        if let Err(error) = migration {
            let _ = self.connection.execute_batch("ROLLBACK;");
            let _ = self
                .connection
                .execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;");
            return Err(error.into());
        }
        self.connection
            .execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")?;
        let foreign_key_violation = self
            .connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()?;
        if foreign_key_violation.is_some() {
            return Err(PersistenceError::InvalidData(
                "M4 evidence-history migration produced a foreign-key violation".into(),
            ));
        }
        Ok(())
    }

    /// Resolve the current app-owned analysis preview before queueing work and immediately make
    /// incompatible evidence historical. This deliberately happens during candidate discovery:
    /// the browser must never continue to present a READY result for an earlier preview,
    /// analyzer version, or settings version while the replacement is merely waiting in queue.
    ///
    /// The tuple is intentionally limited to the primary analyzer contract. Optional face
    /// providers retain their own artifacts/provenance and can evolve independently; a stale
    /// primary artifact hides their old observations from current detail views until a compatible
    /// result is persisted.
    fn refresh_capture_intelligence_staleness(
        &self,
        project_id: &ProjectId,
        provider: &str,
        provider_version: &str,
        settings_version: &str,
    ) -> Result<u64> {
        let invalidated = self.connection.execute(
            r#"WITH ranked_instances AS (
                SELECT fi.id, fi.media_asset_id, fi.is_available, fi.observed_at,
                  ROW_NUMBER() OVER (
                    PARTITION BY fi.media_asset_id
                    ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC
                  ) AS instance_rank
                FROM file_instances fi
              ), primary_instances AS (
                SELECT * FROM ranked_instances WHERE instance_rank = 1
              ), current_inputs AS (
                SELECT a.id AS media_asset_id,
                  COALESCE(
                    (
                      SELECT artifact.source_fingerprint || CASE artifact.artifact_type
                        WHEN 'thumbnail' THEN '|preview:'
                        ELSE '|analysis-preview:'
                      END || artifact.generator_version || ':' || artifact.size_class
                      FROM preview_artifacts artifact
                      WHERE artifact.media_asset_id = a.id
                        AND artifact.source_fingerprint = COALESCE(a.content_hash, a.fast_fingerprint, a.id)
                        AND (
                          (artifact.artifact_type = 'analysis_preview' AND artifact.size_class = 'analysis')
                          OR (artifact.artifact_type = 'thumbnail' AND artifact.size_class = 'preview')
                        )
                      ORDER BY CASE
                        WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status = 'ready' THEN 0
                        WHEN artifact.artifact_type = 'thumbnail' AND artifact.status = 'ready' THEN 1
                        WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status = 'corrupt' THEN 2
                        WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status = 'unsupported' THEN 3
                        ELSE 4 END,
                        artifact.created_at DESC,
                        artifact.id DESC
                      LIMIT 1
                    ),
                    a.content_hash,
                    a.fast_fingerprint,
                    a.id
                  ) AS input_fingerprint
                FROM media_assets a
                JOIN primary_instances pi ON pi.media_asset_id = a.id
                WHERE a.project_id = ?1
                  AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
              )
              UPDATE analysis_artifacts AS artifact
              SET stale = 1
              WHERE artifact.stale = 0
                AND artifact.artifact_type = 'capture_intelligence'
                AND artifact.media_asset_id IN (SELECT media_asset_id FROM current_inputs)
                AND NOT EXISTS (
                  SELECT 1 FROM current_inputs current
                  WHERE current.media_asset_id = artifact.media_asset_id
                    AND artifact.input_fingerprint = current.input_fingerprint
                    AND artifact.provider = ?2
                    AND artifact.provider_version = ?3
                    AND artifact.settings_version = ?4
                )"#,
            params![
                project_id.to_string(),
                provider,
                provider_version,
                settings_version
            ],
        )? as u64;

        // Keep the historical values, but make all queryable evidence follow the current
        // primary artifact. These updates are idempotent and also repair a partially completed
        // v8-era analysis after the v9 migration.
        self.connection.execute(
            "UPDATE visual_fingerprints AS fingerprint
             SET stale = 1
             WHERE fingerprint.stale = 0
               AND fingerprint.media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)
               AND NOT EXISTS (
                 SELECT 1 FROM analysis_artifacts artifact
                 WHERE artifact.media_asset_id = fingerprint.media_asset_id
                   AND artifact.artifact_type = 'capture_intelligence'
                   AND artifact.stale = 0
                   AND artifact.input_fingerprint = fingerprint.input_fingerprint
                   AND artifact.provider = fingerprint.provider
                   AND artifact.provider_version = fingerprint.provider_version
               )",
            params![project_id.to_string()],
        )?;
        self.connection.execute(
            "UPDATE visual_embeddings AS embedding
             SET stale = 1
             WHERE embedding.stale = 0
               AND embedding.media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)
               AND NOT EXISTS (
                 SELECT 1 FROM analysis_artifacts artifact
                 WHERE artifact.media_asset_id = embedding.media_asset_id
                   AND artifact.artifact_type = 'capture_intelligence'
                   AND artifact.stale = 0
                   AND artifact.input_fingerprint = embedding.input_fingerprint
                   AND artifact.provider = embedding.provider
                   AND artifact.provider_version = embedding.provider_version
               )",
            params![project_id.to_string()],
        )?;
        self.connection.execute(
            "UPDATE technical_quality AS quality
             SET stale = 1
             WHERE quality.stale = 0
               AND quality.media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)
               AND NOT EXISTS (
                 SELECT 1 FROM analysis_artifacts artifact
                 WHERE artifact.media_asset_id = quality.media_asset_id
                   AND artifact.artifact_type = 'capture_intelligence'
                   AND artifact.stale = 0
                   AND artifact.input_fingerprint = quality.input_fingerprint
                   AND artifact.provider = quality.provider
                   AND artifact.provider_version = quality.provider_version
               )",
            params![project_id.to_string()],
        )?;
        self.connection.execute(
            "UPDATE analysis_recommendations AS recommendation
             SET stale = 1
             WHERE recommendation.stale = 0
               AND recommendation.media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)
               AND NOT EXISTS (
                 SELECT 1 FROM analysis_artifacts artifact
                 WHERE artifact.media_asset_id = recommendation.media_asset_id
                   AND artifact.artifact_type = 'capture_intelligence'
                   AND artifact.stale = 0
                   AND artifact.input_fingerprint = recommendation.input_fingerprint
               )",
            params![project_id.to_string()],
        )?;
        if invalidated > 0 {
            // A grouping is a project-level derived view. Once any of its source evidence has
            // changed, retain the old group for audit/recommendation references but hide it from
            // current browsing until the durable rebuild completes.
            self.connection.execute(
                "UPDATE similarity_groups SET stale = 1 WHERE project_id = ?1 AND stale = 0",
                params![project_id.to_string()],
            )?;
        }
        Ok(invalidated)
    }

    /// Face evidence owns a separate provider/settings lifecycle from deterministic image
    /// evidence. Its payload records the preview input it consumed, while its artifact input is
    /// an opaque cache key that also contains the face-provider identity. This lets a macOS or
    /// local-provider revision stale only face/eye claims without pretending the technical
    /// analyzer itself changed.
    fn refresh_face_analysis_staleness(
        &self,
        project_id: &ProjectId,
        face_provider: &str,
        face_provider_version: &str,
        face_settings_version: &str,
    ) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE analysis_artifacts AS face_artifact
             SET stale = 1
             WHERE face_artifact.stale = 0
               AND face_artifact.artifact_type = 'face_detection'
               AND face_artifact.media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)
               AND (
                 face_artifact.provider <> ?2
                 OR face_artifact.provider_version <> ?3
                 OR face_artifact.settings_version <> ?4
                 OR NOT EXISTS (
                   SELECT 1
                   FROM analysis_artifacts primary_artifact
                   WHERE primary_artifact.media_asset_id = face_artifact.media_asset_id
                     AND primary_artifact.artifact_type = 'capture_intelligence'
                     AND primary_artifact.stale = 0
                     AND primary_artifact.input_fingerprint = COALESCE(
                       json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'),
                       face_artifact.input_fingerprint
                     )
                 )
               )",
            params![
                project_id.to_string(),
                face_provider,
                face_provider_version,
                face_settings_version,
            ],
        )?;
        Ok(changed as u64)
    }

    /// Fetches a deliberately bounded set of locally durable Moment Brain inputs. `predicate`
    /// and `order_by` are private, fixed SQL fragments assembled below; customer text and IDs
    /// remain bound values in `extra_parameters` and are never interpolated into SQL.
    fn moment_analysis_inputs_for_predicate(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
        predicate: &str,
        mut extra_parameters: Vec<rusqlite::types::Value>,
        order_by: &str,
    ) -> Result<Vec<MomentAnalysisInput>> {
        let select_vector = if model.is_some() {
            "stored.embedding_blob, stored.dimensions"
        } else {
            "NULL AS embedding_blob, NULL AS dimensions"
        };
        let join_vector = if model.is_some() {
            "LEFT JOIN semantic_embeddings stored ON stored.media_asset_id = a.id AND stored.project_id = a.project_id AND stored.stale = 0 AND stored.status = 'ready' AND stored.model_id = ?2 AND stored.provider = ?3 AND stored.model_version = ?4 AND stored.embedding_version = ?5 AND stored.preprocessing_version = ?6 AND stored.metric = ?7 AND stored.dimensions = ?8"
        } else {
            ""
        };
        let sql = format!(
            "SELECT a.id,
                COALESCE(metadata.captured_at_local, a.captured_at), metadata.capture_time_source,
                metadata.camera_model, metadata.lens_model, metadata.orientation,
                CASE WHEN EXISTS (SELECT 1 FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready') THEN (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = a.id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready' ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)) ELSE NULL END,
                (SELECT quality.technical_quality_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.technical_quality_score FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                COALESCE(decision.rating, 0), COALESCE(decision.starred, 0), decision.decision,
                COALESCE((SELECT group_concat(g.id, char(31)) FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = a.id AND g.project_id = a.project_id AND g.stale = 0), ''),
                {select_vector}
             FROM media_assets a
             LEFT JOIN media_metadata metadata ON metadata.media_asset_id = a.id
             LEFT JOIN media_decisions decision ON decision.media_asset_id = a.id AND decision.project_id = a.project_id
             {join_vector}
             WHERE a.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff') {predicate}
             ORDER BY {order_by}"
        );
        let mut bound = vec![rusqlite::types::Value::from(project_id.to_string())];
        if let Some(model) = model {
            bound.extend([
                model.model_id.clone().into(),
                model.provider.clone().into(),
                model.model_version.clone().into(),
                model.embedding_version.clone().into(),
                model.preprocessing_version.clone().into(),
                model.metric.clone().into(),
                (model.dimensions as i64).into(),
            ]);
        }
        bound.append(&mut extra_parameters);
        self.read_moment_analysis_inputs(&sql, bound)
    }

    fn read_moment_analysis_inputs(
        &self,
        sql: &str,
        bound: Vec<rusqlite::types::Value>,
    ) -> Result<Vec<MomentAnalysisInput>> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(bound), |row| {
            let dimensions = row.get::<_, Option<i64>>(14)?;
            let embedding = match (row.get::<_, Option<Vec<u8>>>(13)?, dimensions) {
                (Some(blob), Some(dimensions)) => {
                    Some(decode_semantic_vector(&blob, dimensions as usize).map_err(to_sql_error)?)
                }
                (None, None) => None,
                _ => {
                    return Err(rusqlite::Error::InvalidColumnType(
                        13,
                        "embedding_blob".into(),
                        rusqlite::types::Type::Blob,
                    ))
                }
            };
            let groups = row.get::<_, String>(12)?;
            Ok(MomentAnalysisInput {
                asset_id: row.get(0)?,
                captured_at: row.get(1)?,
                capture_time_source: row.get(2)?,
                camera_model: row.get(3)?,
                lens_model: row.get(4)?,
                orientation: row.get(5)?,
                face_count: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
                technical_quality_band: row.get(7)?,
                technical_quality_score: row.get(8)?,
                rating: row.get::<_, i64>(9)? as u8,
                starred: row.get(10)?,
                decision: row.get(11)?,
                similar_set_ids: if groups.is_empty() {
                    Vec::new()
                } else {
                    groups.split('\u{1f}').map(str::to_owned).collect()
                },
                embedding,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn count(&self, table: &str) -> Result<u64> {
        // Only literal internal table names are used by `counts`; never accept UI input here.
        self.connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_moment_projection_payload(
    timeline: &MomentTimelineStatusRecord,
    run: &MomentAnalysisRunRecord,
    moment_ordinal_start: Option<u64>,
    membership_ordinal_start: u64,
    segments: &[TimelineSegmentRecord],
    moments: &[MomentRecord],
    memberships: &[MomentMembershipRecord],
    boundaries: &[MomentBoundaryEvidenceRecord],
) -> Result<BTreeSet<String>> {
    if timeline.project_id != run.project_id || timeline.timeline_id != run.timeline_id {
        return Err(PersistenceError::InvalidData(
            "timeline and analysis run identities differ".into(),
        ));
    }
    let mut segment_ids = BTreeSet::new();
    let mut segment_ordinals = BTreeSet::new();
    let mut segment_ordinal_by_id = BTreeMap::new();
    for segment in segments {
        if segment.project_id != timeline.project_id || segment.run_id != run.id {
            return Err(PersistenceError::InvalidData(
                "Moment segment does not belong to the selected timeline run/project".into(),
            ));
        }
        if segment.stale {
            return Err(PersistenceError::InvalidData(
                "Moment replacement cannot insert a stale timeline segment".into(),
            ));
        }
        if !segment_ids.insert(segment.id.clone()) || !segment_ordinals.insert(segment.ordinal) {
            return Err(PersistenceError::InvalidData(
                "Moment analysis contains duplicate timeline segment IDs or ordinals".into(),
            ));
        }
        segment_ordinal_by_id.insert(segment.id.clone(), segment.ordinal);
    }
    validate_contiguous_moment_ordinals(
        "Timeline segment",
        &segment_ordinals,
        moment_ordinal_start,
    )?;

    let mut moment_ids = BTreeSet::new();
    let mut moment_segment_ids = BTreeSet::new();
    let mut moment_ordinals = BTreeSet::new();
    let mut new_asset_ids = BTreeSet::new();
    for moment in moments {
        if moment.project_id != timeline.project_id
            || moment.timeline_id != timeline.timeline_id
            || moment.run_id != run.id
            || !segment_ids.contains(&moment.segment_id)
        {
            return Err(PersistenceError::InvalidData(
                "Moment record does not belong to the supplied project, timeline, run, and segment"
                    .into(),
            ));
        }
        if moment.stale {
            return Err(PersistenceError::InvalidData(
                "Moment replacement cannot insert a stale Moment record".into(),
            ));
        }
        if !moment_ids.insert(moment.id.as_str()) || !moment_ordinals.insert(moment.ordinal) {
            return Err(PersistenceError::InvalidData(
                "Moment analysis contains duplicate Moment IDs or ordinals".into(),
            ));
        }
        let segment_ordinal = segment_ordinal_by_id
            .get(&moment.segment_id)
            .expect("validated Moment segment must have an ordinal");
        if *segment_ordinal != moment.ordinal {
            return Err(PersistenceError::InvalidData(
                "Moment and timeline segment ordinals must agree within a generated run".into(),
            ));
        }
        if !moment_segment_ids.insert(moment.segment_id.as_str()) {
            return Err(PersistenceError::InvalidData(
                "Moment analysis contains multiple Moment records for one timeline segment".into(),
            ));
        }
        new_asset_ids.insert(moment.anchor_asset_id.clone());
        if let Some(representative) = &moment.ai_representative_asset_id {
            new_asset_ids.insert(representative.clone());
        }
    }
    validate_contiguous_moment_ordinals("Moment", &moment_ordinals, moment_ordinal_start)?;
    if moment_segment_ids.len() != segment_ids.len() {
        return Err(PersistenceError::InvalidData(
            "every supplied timeline segment must materialize exactly one Moment record".into(),
        ));
    }

    let mut membership_asset_ids = BTreeSet::new();
    let mut membership_ordinals = BTreeSet::new();
    let mut member_assets_by_moment = BTreeMap::<String, BTreeSet<String>>::new();
    for membership in memberships {
        if membership.project_id != timeline.project_id
            || membership.run_id != run.id
            || !membership.active
            || membership.ordinal < membership_ordinal_start
        {
            return Err(PersistenceError::InvalidData(
                "Moment membership is outside the supplied active projection or belongs to another project/run"
                    .into(),
            ));
        }
        if !membership_asset_ids.insert(membership.media_asset_id.as_str())
            || !membership_ordinals.insert(membership.ordinal)
        {
            return Err(PersistenceError::InvalidData(
                "Moment analysis contains duplicate membership media assets or ordinals".into(),
            ));
        }
        match membership.membership_state.as_str() {
            "member" => {
                let Some(moment_id) = membership.moment_id.as_deref() else {
                    return Err(PersistenceError::InvalidData(
                        "Moment member membership requires a supplied Moment ID".into(),
                    ));
                };
                if !moment_ids.contains(moment_id) {
                    return Err(PersistenceError::InvalidData(
                        "Moment membership refers to a Moment outside the supplied projection"
                            .into(),
                    ));
                }
                member_assets_by_moment
                    .entry(moment_id.to_owned())
                    .or_default()
                    .insert(membership.media_asset_id.clone());
            }
            "ungrouped" if membership.moment_id.is_none() => {}
            "ungrouped" => {
                return Err(PersistenceError::InvalidData(
                    "Moment ungrouped membership cannot point to a Moment".into(),
                ))
            }
            _ => {
                return Err(PersistenceError::InvalidData(
                    "Moment membership has an unsupported state".into(),
                ))
            }
        }
        new_asset_ids.insert(membership.media_asset_id.clone());
    }
    validate_contiguous_moment_ordinals(
        "Moment membership",
        &membership_ordinals,
        Some(membership_ordinal_start),
    )?;
    for moment in moments {
        let member_assets = member_assets_by_moment.get(&moment.id).ok_or_else(|| {
            PersistenceError::InvalidData(
                "every supplied Moment record must have at least one member membership".into(),
            )
        })?;
        if !member_assets.contains(&moment.anchor_asset_id) {
            return Err(PersistenceError::InvalidData(
                "Moment anchor asset must be a member of its supplied Moment record".into(),
            ));
        }
        if let Some(representative) = &moment.ai_representative_asset_id {
            if !member_assets.contains(representative) {
                return Err(PersistenceError::InvalidData(
                    "AI Moment representative must be a member of its supplied Moment record"
                        .into(),
                ));
            }
        }
        if moment.asset_count != member_assets.len() as u64 {
            return Err(PersistenceError::InvalidData(
                "Moment asset count must match its supplied member memberships".into(),
            ));
        }
    }
    let mut boundary_ids = BTreeSet::new();
    let mut boundary_ordinals = BTreeSet::new();
    for boundary in boundaries {
        if boundary.project_id != timeline.project_id || boundary.run_id != run.id {
            return Err(PersistenceError::InvalidData(
                "Moment boundary evidence belongs to another project or run".into(),
            ));
        }
        if !boundary_ids.insert(boundary.id.as_str()) || !boundary_ordinals.insert(boundary.ordinal)
        {
            return Err(PersistenceError::InvalidData(
                "Moment analysis contains duplicate boundary evidence IDs or ordinals".into(),
            ));
        }
        new_asset_ids.insert(boundary.left_asset_id.clone());
        new_asset_ids.insert(boundary.right_asset_id.clone());
    }
    validate_contiguous_moment_ordinals("Moment boundary evidence", &boundary_ordinals, Some(0))?;
    Ok(new_asset_ids)
}

fn validate_contiguous_moment_ordinals(
    kind: &str,
    ordinals: &BTreeSet<u64>,
    expected_start: Option<u64>,
) -> Result<()> {
    let Some(first) = ordinals.first().copied() else {
        return Ok(());
    };
    let start = expected_start.unwrap_or(first);
    for (index, ordinal) in ordinals.iter().enumerate() {
        let index = u64::try_from(index).map_err(|_| {
            PersistenceError::InvalidData("Moment ordinal count exceeds u64 range".into())
        })?;
        let expected = start.checked_add(index).ok_or_else(|| {
            PersistenceError::InvalidData("Moment ordinal exceeds u64 range".into())
        })?;
        if *ordinal != expected {
            return Err(PersistenceError::InvalidData(format!(
                "{kind} ordinals must be contiguous from {start}"
            )));
        }
    }
    Ok(())
}

fn assert_moment_assets_belong_to_project(
    connection: &Connection,
    project_id: &ProjectId,
    asset_ids: &BTreeSet<String>,
) -> Result<()> {
    // SQLite's host-parameter cap must not become an artificial catalog/tail limit. Chunking
    // bound IDs preserves the local project check for arbitrarily large append batches.
    for chunk in asset_ids.iter().collect::<Vec<_>>().chunks(500) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(DISTINCT id) FROM media_assets WHERE project_id = ? AND id IN ({placeholders})"
        );
        let mut parameters = vec![rusqlite::types::Value::from(project_id.to_string())];
        parameters.extend(chunk.iter().map(|asset_id| (*asset_id).to_owned().into()));
        let count: i64 =
            connection.query_row(&sql, rusqlite::params_from_iter(parameters), |row| {
                row.get(0)
            })?;
        if count != chunk.len() as i64 {
            return Err(PersistenceError::InvalidData(
                "Moment analysis referenced a media asset outside the selected project".into(),
            ));
        }
    }
    Ok(())
}

/// Check the active projection that will exist after a replacement but before the new rows are
/// inserted. This protects the persistence boundary from malformed direct callers as well as
/// core-generated payloads: global display and membership ordinals are always deterministic,
/// gap-free `0..n` sequences.
fn assert_active_moment_projection_contiguous(
    transaction: &Transaction<'_>,
    project_id: &str,
    incoming_moments: &[MomentRecord],
    incoming_memberships: &[MomentMembershipRecord],
) -> Result<()> {
    let stored_moment_ordinals = transaction
        .prepare(
            "SELECT ordinal FROM moment_records
             WHERE project_id = ?1 AND stale = 0",
        )?
        .query_map(params![project_id], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut moment_ordinals = BTreeSet::new();
    for ordinal in stored_moment_ordinals {
        let ordinal = u64::try_from(ordinal).map_err(|_| {
            PersistenceError::InvalidData("active Moment ordinal must not be negative".into())
        })?;
        if !moment_ordinals.insert(ordinal) {
            return Err(PersistenceError::InvalidData(
                "active Moment projection contains duplicate display ordinals".into(),
            ));
        }
    }
    for moment in incoming_moments {
        if !moment_ordinals.insert(moment.ordinal) {
            return Err(PersistenceError::InvalidData(
                "replacement Moment ordinal collides with the active projection".into(),
            ));
        }
    }
    validate_contiguous_moment_ordinals("active Moment", &moment_ordinals, Some(0))?;

    let stored_memberships = transaction
        .prepare(
            "SELECT ordinal, media_asset_id FROM moment_memberships
             WHERE project_id = ?1 AND active = 1",
        )?
        .query_map(params![project_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut membership_ordinals = BTreeSet::new();
    let mut membership_asset_ids = BTreeSet::new();
    for (ordinal, asset_id) in stored_memberships {
        let ordinal = u64::try_from(ordinal).map_err(|_| {
            PersistenceError::InvalidData(
                "active Moment membership ordinal must not be negative".into(),
            )
        })?;
        if !membership_ordinals.insert(ordinal) {
            return Err(PersistenceError::InvalidData(
                "active Moment projection contains duplicate membership ordinals".into(),
            ));
        }
        if !membership_asset_ids.insert(asset_id) {
            return Err(PersistenceError::InvalidData(
                "active Moment projection contains duplicate membership media assets".into(),
            ));
        }
    }
    for membership in incoming_memberships {
        if !membership_ordinals.insert(membership.ordinal) {
            return Err(PersistenceError::InvalidData(
                "replacement Moment membership ordinal collides with the active projection".into(),
            ));
        }
        if !membership_asset_ids.insert(membership.media_asset_id.clone()) {
            return Err(PersistenceError::InvalidData(
                "replacement Moment membership repeats a preserved active media asset".into(),
            ));
        }
    }
    validate_contiguous_moment_ordinals("active Moment membership", &membership_ordinals, Some(0))?;
    Ok(())
}

impl CatalogRepository for SqliteRepository {
    fn create_project(&self, name: &str) -> Result<Project> {
        let project = Project {
            id: ProjectId::new(),
            name: name.trim().to_owned(),
            created_at: Utc::now(),
        };
        if project.name.is_empty() {
            return Err(PersistenceError::InvalidData(
                "project name may not be empty".into(),
            ));
        }
        self.insert_project(&project)?;
        Ok(project)
    }

    fn projects(&self) -> Result<Vec<Project>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, created_at FROM projects ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, name, created_at) = row?;
            Ok(Project {
                id: parse_id!(ProjectId, &id)?,
                name,
                created_at: parse_timestamp(&created_at)?,
            })
        })
        .collect()
    }

    fn insert_project(&self, project: &Project) -> Result<()> {
        self.connection.execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![
                project.id.to_string(),
                project.name,
                timestamp(&project.created_at)
            ],
        )?;
        Ok(())
    }

    fn get_project(&self, id: &ProjectId) -> Result<Option<Project>> {
        self.connection
            .query_row(
                "SELECT id, name, created_at FROM projects WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|(id, name, created_at)| {
                Ok(Project {
                    id: parse_id!(ProjectId, &id)?,
                    name,
                    created_at: parse_timestamp(&created_at)?,
                })
            })
            .transpose()
    }

    fn insert_shoot(&self, shoot: &Shoot) -> Result<()> {
        self.connection.execute(
            "INSERT INTO shoots (id, project_id, name, captured_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                shoot.id.to_string(),
                shoot.project_id.to_string(),
                shoot.name,
                optional_timestamp(&shoot.captured_at)
            ],
        )?;
        Ok(())
    }

    fn insert_moment(&self, moment: &Moment) -> Result<()> {
        self.connection.execute(
            "INSERT INTO moments (id, shoot_id, name, started_at, ended_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![moment.id.to_string(), moment.shoot_id.to_string(), moment.name, optional_timestamp(&moment.started_at), optional_timestamp(&moment.ended_at)],
        )?;
        Ok(())
    }

    fn insert_person_cluster(&self, person: &PersonCluster) -> Result<()> {
        self.connection.execute(
            "INSERT INTO person_clusters (id, project_id, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![person.id.to_string(), person.project_id.to_string(), person.display_name, timestamp(&person.created_at)],
        )?;
        Ok(())
    }

    fn insert_capture_device(&self, device: &CaptureDevice) -> Result<()> {
        self.connection.execute(
            "INSERT INTO capture_devices (id, project_id, display_name, device_type) VALUES (?1, ?2, ?3, ?4)",
            params![device.id.to_string(), device.project_id.to_string(), device.display_name, device.device_type],
        )?;
        Ok(())
    }

    fn insert_storage_volume(&self, volume: &StorageVolume) -> Result<()> {
        self.connection.execute(
            "INSERT INTO storage_volumes (id, display_name, filesystem_identity, capacity_bytes, mount_state, mount_location, last_seen_at, filesystem_type, classification) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![volume.id.to_string(), volume.display_name, volume.filesystem_identity, volume.capacity_bytes.map(|v| v as i64), json(&volume.mount_state)?, volume.mount_location, optional_timestamp(&volume.last_seen_at), volume.filesystem_type, json(&volume.classification)?],
        )?;
        Ok(())
    }

    fn update_volume_state(
        &self,
        id: &StorageVolumeId,
        state: MountState,
        mount: Option<&str>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE storage_volumes SET mount_state = ?2, mount_location = ?3 WHERE id = ?1",
            params![id.to_string(), json(&state)?, mount],
        )?;
        Ok(())
    }

    fn get_storage_volume(&self, id: &StorageVolumeId) -> Result<Option<StorageVolume>> {
        self.connection.query_row(
            "SELECT id, display_name, filesystem_identity, capacity_bytes, mount_state, mount_location, last_seen_at, filesystem_type, classification FROM storage_volumes WHERE id = ?1",
            params![id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get::<_, Option<i64>>(3)?, row.get::<_, String>(4)?, row.get(5)?, row.get::<_, Option<String>>(6)?, row.get(7)?, row.get::<_, String>(8)?)),
        ).optional()?.map(|(id, display_name, filesystem_identity, capacity_bytes, mount_state, mount_location, last_seen_at, filesystem_type, classification)| Ok(StorageVolume {
            id: parse_id!(StorageVolumeId, &id)?, display_name, filesystem_identity,
            capacity_bytes: capacity_bytes.map(|value| value as u64), mount_state: from_json(&mount_state)?, mount_location,
            last_seen_at: last_seen_at.map(|value| parse_timestamp(&value)).transpose()?, filesystem_type, classification: from_json(&classification)?,
        })).transpose()
    }

    fn storage_volume_by_filesystem_identity(
        &self,
        identity: &str,
    ) -> Result<Option<StorageVolume>> {
        self.connection.query_row(
            "SELECT id, display_name, filesystem_identity, capacity_bytes, mount_state, mount_location, last_seen_at, filesystem_type, classification FROM storage_volumes WHERE filesystem_identity = ?1",
            params![identity],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get::<_, Option<i64>>(3)?, row.get::<_, String>(4)?, row.get(5)?, row.get::<_, Option<String>>(6)?, row.get(7)?, row.get::<_, String>(8)?)),
        ).optional()?.map(|(id, display_name, filesystem_identity, capacity_bytes, mount_state, mount_location, last_seen_at, filesystem_type, classification)| Ok(StorageVolume {
            id: parse_id!(StorageVolumeId, &id)?, display_name, filesystem_identity,
            capacity_bytes: capacity_bytes.map(|value| value as u64), mount_state: from_json(&mount_state)?, mount_location,
            last_seen_at: last_seen_at.map(|value| parse_timestamp(&value)).transpose()?, filesystem_type, classification: from_json(&classification)?,
        })).transpose()
    }

    fn update_storage_volume(&self, volume: &StorageVolume) -> Result<()> {
        self.connection.execute(
            "UPDATE storage_volumes SET display_name = ?2, filesystem_identity = ?3, capacity_bytes = ?4, mount_state = ?5, mount_location = ?6, last_seen_at = ?7, filesystem_type = ?8, classification = ?9 WHERE id = ?1",
            params![volume.id.to_string(), volume.display_name, volume.filesystem_identity, volume.capacity_bytes.map(|value| value as i64), json(&volume.mount_state)?, volume.mount_location, optional_timestamp(&volume.last_seen_at), volume.filesystem_type, json(&volume.classification)?],
        )?;
        Ok(())
    }

    fn register_index_root(&self, root: &IndexRoot) -> Result<IndexRoot> {
        let existing = self.connection.query_row(
            "SELECT id, project_id, selected_path, storage_volume_id, added_at, last_indexed_at, status_json FROM index_roots WHERE project_id = ?1 AND selected_path = ?2",
            params![root.project_id.to_string(), root.selected_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, String>(6)?)),
        ).optional()?.map(index_root_from_tuple).transpose()?;
        if let Some(existing) = existing {
            self.connection.execute(
                "UPDATE index_roots SET storage_volume_id = ?3, status_json = ?4 WHERE id = ?1 AND project_id = ?2",
                params![existing.id.to_string(), root.project_id.to_string(), root.storage_volume_id.to_string(), json(&root.status)?],
            )?;
            return Ok(IndexRoot {
                storage_volume_id: root.storage_volume_id.clone(),
                status: root.status.clone(),
                ..existing
            });
        }
        self.connection.execute(
            "INSERT INTO index_roots (id, project_id, selected_path, storage_volume_id, added_at, last_indexed_at, status_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![root.id.to_string(), root.project_id.to_string(), root.selected_path, root.storage_volume_id.to_string(), timestamp(&root.added_at), optional_timestamp(&root.last_indexed_at), json(&root.status)?],
        )?;
        Ok(root.clone())
    }

    fn index_roots_for_project(&self, project_id: &ProjectId) -> Result<Vec<IndexRoot>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, selected_path, storage_volume_id, added_at, last_indexed_at, status_json FROM index_roots WHERE project_id = ?1 ORDER BY added_at DESC",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| index_root_from_tuple(row?)).collect()
    }

    fn update_index_root(&self, root: &IndexRoot) -> Result<()> {
        self.connection.execute(
            "UPDATE index_roots SET storage_volume_id = ?2, last_indexed_at = ?3, status_json = ?4 WHERE id = ?1",
            params![root.id.to_string(), root.storage_volume_id.to_string(), optional_timestamp(&root.last_indexed_at), json(&root.status)?],
        )?;
        Ok(())
    }

    fn insert_media_asset(&self, asset: &MediaAsset) -> Result<()> {
        self.connection.execute(
            "INSERT INTO media_assets (id, project_id, media_type, display_name, extension, captured_at, fingerprint_json, fast_fingerprint, byte_size, observed_modified_at, created_at, content_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![asset.id.to_string(), asset.project_id.to_string(), asset.media_type.as_str(), asset.display_name, asset.extension, optional_timestamp(&asset.captured_at), json(&asset.fingerprint)?, asset.fingerprint.fast_fingerprint, asset.fingerprint.byte_size.map(|value| value as i64), optional_timestamp(&asset.fingerprint.observed_modified_at), timestamp(&asset.created_at), asset.fingerprint.cryptographic_hash],
        )?;
        Ok(())
    }

    fn media_asset_by_fingerprint(
        &self,
        project_id: &ProjectId,
        media_type: &MediaType,
        extension: Option<&str>,
        fingerprint: &MediaFingerprint,
    ) -> Result<Option<MediaAsset>> {
        let Some(fast_fingerprint) = fingerprint.fast_fingerprint.as_deref() else {
            return Ok(None);
        };
        let Some(byte_size) = fingerprint.byte_size else {
            return Ok(None);
        };
        self.connection.query_row(
            "SELECT id, project_id, media_type, display_name, extension, captured_at, fingerprint_json, created_at FROM media_assets WHERE project_id = ?1 AND media_type = ?2 AND extension = ?3 AND fast_fingerprint = ?4 AND byte_size = ?5 LIMIT 1",
            params![project_id.to_string(), media_type.as_str(), extension.unwrap_or_default(), fast_fingerprint, byte_size as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?)),
        ).optional()?.map(media_asset_from_tuple).transpose()
    }

    fn media_asset_by_content_hash(
        &self,
        project_id: &ProjectId,
        media_type: &MediaType,
        extension: Option<&str>,
        byte_size: u64,
        content_hash: &str,
    ) -> Result<Option<MediaAsset>> {
        self.connection.query_row(
            "SELECT id, project_id, media_type, display_name, extension, captured_at, fingerprint_json, created_at FROM media_assets WHERE project_id = ?1 AND media_type = ?2 AND extension = ?3 AND byte_size = ?4 AND content_hash = ?5 LIMIT 1",
            params![project_id.to_string(), media_type.as_str(), extension.unwrap_or_default(), byte_size as i64, content_hash],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?)),
        ).optional()?.map(media_asset_from_tuple).transpose()
    }

    fn insert_file_instance(&self, instance: &FileInstance) -> Result<()> {
        self.connection.execute(
            "INSERT INTO file_instances (id, media_asset_id, storage_volume_id, index_root_id, relative_path, observed_at, is_available) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![instance.id.to_string(), instance.media_asset_id.to_string(), instance.storage_volume_id.to_string(), instance.index_root_id.as_ref().map(ToString::to_string), instance.relative_path, timestamp(&instance.observed_at), instance.is_available],
        )?;
        Ok(())
    }

    fn upsert_file_instance(&self, instance: &FileInstance) -> Result<FileInstance> {
        let existing = self
            .connection
            .query_row(
                "SELECT id FROM file_instances WHERE index_root_id = ?1 AND relative_path = ?2",
                params![
                    instance.index_root_id.as_ref().map(ToString::to_string),
                    instance.relative_path
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            self.connection.execute(
                "UPDATE file_instances SET media_asset_id = ?2, storage_volume_id = ?3, observed_at = ?4, is_available = ?5 WHERE id = ?1",
                params![id, instance.media_asset_id.to_string(), instance.storage_volume_id.to_string(), timestamp(&instance.observed_at), instance.is_available],
            )?;
            return Ok(FileInstance {
                id: parse_id!(FileInstanceId, &id)?,
                ..instance.clone()
            });
        }
        self.insert_file_instance(instance)?;
        Ok(instance.clone())
    }

    fn mark_root_instances_unavailable(&self, index_root_id: &IndexRootId) -> Result<()> {
        self.connection.execute(
            "UPDATE file_instances SET is_available = 0 WHERE index_root_id = ?1",
            params![index_root_id.to_string()],
        )?;
        Ok(())
    }

    fn associate_sidecars_for_root(&self, index_root_id: &IndexRootId) -> Result<u64> {
        let mut statement = self.connection.prepare(
            "SELECT fi.media_asset_id, fi.relative_path, a.media_type FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE fi.index_root_id = ?1",
        )?;
        let rows = statement
            .query_map(params![index_root_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut created = 0;
        for (sidecar_id, sidecar_path, media_type) in &rows {
            if media_type != "sidecar" || !sidecar_path.to_ascii_lowercase().ends_with(".xmp") {
                continue;
            }
            let Some(stem) = sidecar_stem(sidecar_path) else {
                continue;
            };
            let candidates = rows
                .iter()
                .filter(|(_, path, kind)| kind != "sidecar" && same_path_stem(path, stem))
                .map(|(asset_id, _, _)| asset_id)
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                continue;
            }
            let exists: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationships WHERE source_id = ?1 AND target_id = ?2 AND kind = 'SIDECAR_OF')",
                params![sidecar_id, candidates[0]],
                |row| row.get(0),
            )?;
            if exists {
                continue;
            }
            self.insert_relationship(&Relationship {
                id: RelationshipId::new(),
                source: EntityRef {
                    kind: capture_graph::EntityKind::MediaAsset,
                    id: sidecar_id.clone(),
                },
                target: EntityRef {
                    kind: capture_graph::EntityKind::MediaAsset,
                    id: candidates[0].clone(),
                },
                kind: RelationshipKind::SidecarOf,
                confidence: Some(1.0),
                provenance: Provenance {
                    source: "deterministic-sidecar-match".into(),
                    algorithm_id: Some("same-root-basename".into()),
                    algorithm_version: Some("m3.1".into()),
                    produced_at: Utc::now(),
                    human_confirmed: false,
                },
                created_at: Utc::now(),
            })?;
            created += 1;
        }
        Ok(created)
    }

    fn file_instances_for_asset(&self, asset_id: &MediaAssetId) -> Result<Vec<FileInstance>> {
        let mut statement = self.connection.prepare("SELECT id, media_asset_id, storage_volume_id, index_root_id, relative_path, observed_at, is_available FROM file_instances WHERE media_asset_id = ?1 ORDER BY relative_path")?;
        let rows = statement.query_map(params![asset_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get(6)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                media_asset_id,
                storage_volume_id,
                index_root_id,
                relative_path,
                observed_at,
                is_available,
            ) = row?;
            Ok(FileInstance {
                id: parse_id!(FileInstanceId, &id)?,
                media_asset_id: parse_id!(MediaAssetId, &media_asset_id)?,
                storage_volume_id: parse_id!(StorageVolumeId, &storage_volume_id)?,
                index_root_id: index_root_id
                    .as_deref()
                    .map(|value| parse_id!(IndexRootId, value))
                    .transpose()?,
                relative_path,
                observed_at: parse_timestamp(&observed_at)?,
                is_available,
            })
        })
        .collect()
    }

    fn insert_relationship(&self, relationship: &Relationship) -> Result<()> {
        relationship
            .validate()
            .map_err(|error| PersistenceError::Relationship(error.to_string()))?;
        self.connection.execute(
            "INSERT INTO relationships (id, source_kind, source_id, target_kind, target_id, kind, confidence, provenance_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![relationship.id.to_string(), json(&relationship.source.kind)?, relationship.source.id, json(&relationship.target.kind)?, relationship.target.id, relationship.kind.as_str(), relationship.confidence, json(&relationship.provenance)?, timestamp(&relationship.created_at)],
        )?;
        Ok(())
    }

    fn relationships(&self) -> Result<Vec<Relationship>> {
        let mut statement = self.connection.prepare("SELECT id, source_kind, source_id, target_kind, target_id, kind, confidence, provenance_json, created_at FROM relationships ORDER BY created_at")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get::<_, String>(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                source_kind,
                source_id,
                target_kind,
                target_id,
                kind,
                confidence,
                provenance,
                created_at,
            ) = row?;
            Ok(Relationship {
                id: parse_id!(RelationshipId, &id)?,
                source: EntityRef {
                    kind: from_json(&source_kind)?,
                    id: source_id,
                },
                target: EntityRef {
                    kind: from_json(&target_kind)?,
                    id: target_id,
                },
                kind: relationship_kind(&kind),
                confidence,
                provenance: from_json(&provenance)?,
                created_at: parse_timestamp(&created_at)?,
            })
        })
        .collect()
    }

    fn insert_derivative(&self, derivative: &Derivative) -> Result<()> {
        self.connection.execute(
            "INSERT INTO derivatives (id, source_asset_id, derived_asset_id, recipe_hint, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![derivative.id.to_string(), derivative.source_asset_id.to_string(), derivative.derived_asset_id.to_string(), derivative.recipe_hint, timestamp(&derivative.created_at)],
        )?;
        Ok(())
    }

    fn derivatives_from(&self, source_asset_id: &MediaAssetId) -> Result<Vec<Derivative>> {
        let mut statement = self.connection.prepare(
            "SELECT id, source_asset_id, derived_asset_id, recipe_hint, created_at FROM derivatives WHERE source_asset_id = ?1 ORDER BY created_at",
        )?;
        let rows = statement.query_map(params![source_asset_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, source_asset_id, derived_asset_id, recipe_hint, created_at) = row?;
            Ok(Derivative {
                id: parse_id!(DerivativeId, &id)?,
                source_asset_id: parse_id!(MediaAssetId, &source_asset_id)?,
                derived_asset_id: parse_id!(MediaAssetId, &derived_asset_id)?,
                recipe_hint,
                created_at: parse_timestamp(&created_at)?,
            })
        })
        .collect()
    }

    fn insert_backup_copy(&self, backup: &BackupCopy) -> Result<()> {
        self.connection.execute(
            "INSERT INTO backup_copies (id, source_file_instance_id, backup_file_instance_id, verified_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![backup.id.to_string(), backup.source_file_instance_id.to_string(), backup.backup_file_instance_id.to_string(), optional_timestamp(&backup.verified_at), timestamp(&backup.created_at)],
        )?;
        Ok(())
    }

    fn insert_correction(&self, correction: &Correction) -> Result<()> {
        self.connection.execute(
            "INSERT INTO corrections (id, subject_kind, subject_id, field_name, previous_value_json, corrected_value_json, corrected_at, author_hint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![correction.id.to_string(), correction.subject_kind, correction.subject_id, correction.field_name, correction.previous_value.as_ref().map(serde_json::to_string).transpose()?, json(&correction.corrected_value)?, timestamp(&correction.corrected_at), correction.author_hint],
        )?;
        Ok(())
    }

    fn corrections_for(&self, kind: &str, id: &str) -> Result<Vec<Correction>> {
        let mut statement = self.connection.prepare("SELECT id, subject_kind, subject_id, field_name, previous_value_json, corrected_value_json, corrected_at, author_hint FROM corrections WHERE subject_kind = ?1 AND subject_id = ?2 ORDER BY corrected_at")?;
        let rows = statement.query_map(params![kind, id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get(7)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                subject_kind,
                subject_id,
                field_name,
                previous_value,
                corrected_value,
                corrected_at,
                author_hint,
            ) = row?;
            Ok(Correction {
                id: parse_id!(CorrectionId, &id)?,
                subject_kind,
                subject_id,
                field_name,
                previous_value: previous_value.map(|value| from_json(&value)).transpose()?,
                corrected_value: from_json(&corrected_value)?,
                corrected_at: parse_timestamp(&corrected_at)?,
                author_hint,
            })
        })
        .collect()
    }

    fn insert_background_job(&self, job: &BackgroundJob) -> Result<()> {
        self.connection.execute(
            "INSERT INTO background_jobs (id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![job.id.to_string(), json(&job.state)?, json(&job.stage)?, job.items_completed as i64, job.items_total.map(|value| value as i64), job.files_discovered as i64, job.files_processed as i64, job.error_count as i64, job.project_id.as_ref().map(ToString::to_string), job.index_root_id.as_ref().map(ToString::to_string), job.error_message, job.resume_metadata.as_ref().map(serde_json::to_string).transpose()?, timestamp(&job.created_at), timestamp(&job.updated_at), optional_timestamp(&job.finished_at)],
        )?;
        Ok(())
    }

    fn update_background_job(&self, job: &BackgroundJob) -> Result<()> {
        self.connection.execute(
            "UPDATE background_jobs SET state_json = ?2, stage_json = ?3, items_completed = ?4, items_total = ?5, files_discovered = ?6, files_processed = ?7, error_count = ?8, error_message = ?9, resume_metadata_json = ?10, updated_at = ?11, finished_at = ?12 WHERE id = ?1",
            params![job.id.to_string(), json(&job.state)?, json(&job.stage)?, job.items_completed as i64, job.items_total.map(|value| value as i64), job.files_discovered as i64, job.files_processed as i64, job.error_count as i64, job.error_message, job.resume_metadata.as_ref().map(serde_json::to_string).transpose()?, timestamp(&job.updated_at), optional_timestamp(&job.finished_at)],
        )?;
        Ok(())
    }

    fn get_background_job(&self, id: &JobId) -> Result<Option<BackgroundJob>> {
        self.connection.query_row(
            "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at FROM background_jobs WHERE id = ?1",
            params![id.to_string()],
            background_job_tuple,
        ).optional()?.map(background_job_from_tuple).transpose()
    }

    fn recent_background_job(&self, project_id: &ProjectId) -> Result<Option<BackgroundJob>> {
        self.connection.query_row(
            "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at FROM background_jobs WHERE project_id = ?1 AND index_root_id IS NOT NULL ORDER BY created_at DESC LIMIT 1",
            params![project_id.to_string()],
            background_job_tuple,
        ).optional()?.map(background_job_from_tuple).transpose()
    }

    fn insert_index_issue(&self, issue: &IndexIssue) -> Result<()> {
        self.connection.execute(
            "INSERT INTO index_issues (id, job_id, relative_path, severity_json, message, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![issue.id.to_string(), issue.job_id.to_string(), issue.relative_path, json(&issue.severity)?, issue.message, timestamp(&issue.created_at)],
        )?;
        Ok(())
    }

    fn indexed_media(
        &self,
        project_id: &ProjectId,
        filter: MediaBrowserFilter,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<IndexedMediaRow>> {
        let filter_sql = match filter {
            MediaBrowserFilter::All => "1 = 1",
            MediaBrowserFilter::Photos => {
                "a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')"
            }
            MediaBrowserFilter::Video => "a.media_type = 'video'",
            MediaBrowserFilter::Audio => "a.media_type = 'audio'",
            MediaBrowserFilter::Sidecars => "a.media_type = 'sidecar'",
            MediaBrowserFilter::Unknown => "a.media_type = 'unknown'",
        };
        let sql = format!(
            "SELECT a.id, fi.id, a.display_name, a.media_type, a.extension, fi.relative_path, roots.selected_path, a.byte_size, a.observed_modified_at, volumes.display_name, volumes.id, a.fast_fingerprint, fi.is_available FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id JOIN index_roots roots ON roots.id = fi.index_root_id JOIN storage_volumes volumes ON volumes.id = fi.storage_volume_id WHERE a.project_id = ?1 AND {filter_sql} ORDER BY fi.observed_at DESC, fi.relative_path ASC LIMIT ?2 OFFSET ?3"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![project_id.to_string(), limit as i64, offset as i64],
            |row| {
                Ok(IndexedMediaRow {
                    asset_id: row.get(0)?,
                    file_instance_id: row.get(1)?,
                    filename: row.get(2)?,
                    media_type: row.get(3)?,
                    extension: row
                        .get::<_, String>(4)
                        .ok()
                        .filter(|value| !value.is_empty()),
                    relative_path: row.get(5)?,
                    selected_root: row.get(6)?,
                    byte_size: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
                    modified_at: row.get(8)?,
                    storage_volume: row.get(9)?,
                    storage_volume_id: row.get(10)?,
                    fingerprint_present: row.get::<_, Option<String>>(11)?.is_some(),
                    status: if row.get::<_, bool>(12)? {
                        "available".into()
                    } else {
                        "unavailable".into()
                    },
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn visual_media_page(
        &self,
        project_id: &ProjectId,
        query: &VisualMediaQuery,
    ) -> Result<VisualMediaPage> {
        if let Some(moment_id) = &query.moment_id {
            if !self.moment_belongs_to_project(project_id, moment_id)? {
                return Err(PersistenceError::InvalidData(
                    "moment does not belong to the selected project".into(),
                ));
            }
        }
        let filter_sql = visual_filter_sql(&query.filter);
        let order_sql = visual_sort_sql(query.sort);
        let direction = if query.descending { "DESC" } else { "ASC" };
        let resolved_preview_status = resolved_preview_status_sql();
        let resolved_preview_failure_reason = resolved_preview_failure_reason_sql();
        // `filter_sql`, `order_sql`, and `direction` come exclusively from enums; UI text is
        // bound below and never interpolated into SQL.
        let sql = format!(
            "WITH ranked_instances AS (SELECT fi.id, fi.media_asset_id, fi.relative_path, fi.observed_at, fi.is_available, fi.index_root_id, fi.storage_volume_id, roots.selected_path, volumes.display_name AS volume_name, volumes.id AS volume_id, ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, CASE WHEN EXISTS (SELECT 1 FROM media_metadata chosen WHERE chosen.media_asset_id = fi.media_asset_id AND chosen.source_file_instance_id = fi.id AND chosen.status = 'ready') THEN 1 ELSE 0 END DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank FROM file_instances fi LEFT JOIN index_roots roots ON roots.id = fi.index_root_id JOIN storage_volumes volumes ON volumes.id = fi.storage_volume_id), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1) SELECT a.id, pi.id, a.display_name, a.media_type, a.extension, a.byte_size, COALESCE(m.captured_at_local, a.captured_at, a.observed_modified_at), pi.observed_at, pi.relative_path, pi.selected_path, pi.volume_name, pi.volume_id, pi.is_available, CASE WHEN small.status = 'ready' THEN small.id END, CASE WHEN medium.status = 'ready' THEN medium.id END, CASE WHEN preview.status = 'ready' THEN preview.id END, {resolved_preview_status}, {resolved_preview_failure_reason}, m.width, m.height, m.duration_ms, m.camera_model, m.lens_model, m.codec FROM media_assets a JOIN primary_instances pi ON pi.media_asset_id = a.id LEFT JOIN media_metadata m ON m.media_asset_id = a.id LEFT JOIN preview_artifacts small ON small.media_asset_id = a.id AND small.source_file_instance_id = pi.id AND small.size_class = 'small' AND small.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts medium ON medium.media_asset_id = a.id AND medium.source_file_instance_id = pi.id AND medium.size_class = 'medium' AND medium.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts preview ON preview.media_asset_id = a.id AND preview.source_file_instance_id = pi.id AND preview.size_class = 'preview' AND preview.artifact_type IN ('thumbnail', 'poster') WHERE a.project_id = ?1 AND {filter_sql} AND (?2 IS NULL OR lower(a.display_name) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.camera_model, '')) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.lens_model, '')) LIKE '%' || lower(?2) || '%') AND (?3 IS NULL OR lower(COALESCE(m.camera_model, '')) = lower(?3)) AND (?4 IS NULL OR lower(COALESCE(m.lens_model, '')) = lower(?4)) AND (?5 IS NULL OR COALESCE(m.captured_at_local, a.captured_at) >= ?5) AND (?6 IS NULL OR COALESCE(m.captured_at_local, a.captured_at) <= ?6) AND (?7 IS NULL OR EXISTS (SELECT 1 FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.moment_id = ?7 AND mm.active = 1 AND mm.membership_state = 'member')) ORDER BY {order_sql} {direction}, a.id ASC LIMIT ?8 OFFSET ?9"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let bound_limit = query.limit.saturating_add(1) as i64;
        let mut items = statement
            .query_map(
                params![
                    project_id.to_string(),
                    query.search.as_deref(),
                    query.camera_model.as_deref(),
                    query.lens_model.as_deref(),
                    query.captured_from.as_deref(),
                    query.captured_to.as_deref(),
                    query.moment_id.as_deref(),
                    bound_limit,
                    query.offset as i64
                ],
                visual_media_row_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = items.len() > query.limit as usize;
        items.truncate(query.limit as usize);
        // The grid is bounded to one page. Hydrating its compact intelligence projection here
        // keeps heavyweight evidence out of the media SQL and avoids an unbounded catalog join.
        for item in &mut items {
            item.intelligence = intelligence_summary_for_asset(&self.connection, &item.asset_id)?
                .unwrap_or_default();
        }
        Ok(VisualMediaPage {
            items,
            has_more,
            cache_bytes: 0,
        })
    }

    fn media_asset_detail(&self, asset_id: &MediaAssetId) -> Result<Option<MediaAssetDetail>> {
        let resolved_preview_status = resolved_preview_status_sql();
        let resolved_preview_failure_reason = resolved_preview_failure_reason_sql();
        let sql = format!(
                "WITH ranked_instances AS (SELECT fi.id, fi.media_asset_id, fi.relative_path, fi.observed_at, fi.is_available, fi.index_root_id, fi.storage_volume_id, roots.selected_path, volumes.display_name AS volume_name, volumes.id AS volume_id, ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, CASE WHEN EXISTS (SELECT 1 FROM media_metadata chosen WHERE chosen.media_asset_id = fi.media_asset_id AND chosen.source_file_instance_id = fi.id AND chosen.status = 'ready') THEN 1 ELSE 0 END DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank FROM file_instances fi LEFT JOIN index_roots roots ON roots.id = fi.index_root_id JOIN storage_volumes volumes ON volumes.id = fi.storage_volume_id), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1) SELECT a.id, pi.id, a.display_name, a.media_type, a.extension, a.byte_size, COALESCE(m.captured_at_local, a.captured_at, a.observed_modified_at), pi.observed_at, pi.relative_path, pi.selected_path, pi.volume_name, pi.volume_id, pi.is_available, CASE WHEN small.status = 'ready' THEN small.id END, CASE WHEN medium.status = 'ready' THEN medium.id END, CASE WHEN preview.status = 'ready' THEN preview.id END, {resolved_preview_status}, {resolved_preview_failure_reason}, m.width, m.height, m.duration_ms, m.camera_model, m.lens_model, m.codec FROM media_assets a JOIN primary_instances pi ON pi.media_asset_id = a.id LEFT JOIN media_metadata m ON m.media_asset_id = a.id LEFT JOIN preview_artifacts small ON small.media_asset_id = a.id AND small.source_file_instance_id = pi.id AND small.size_class = 'small' AND small.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts medium ON medium.media_asset_id = a.id AND medium.source_file_instance_id = pi.id AND medium.size_class = 'medium' AND medium.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts preview ON preview.media_asset_id = a.id AND preview.source_file_instance_id = pi.id AND preview.size_class = 'preview' AND preview.artifact_type IN ('thumbnail', 'poster') WHERE a.id = ?1",
        );
        let item = self
            .connection
            .query_row(
                &sql,
                params![asset_id.to_string()],
                visual_media_row_from_row,
            )
            .optional()?;
        let Some(item) = item else {
            return Ok(None);
        };
        let metadata = self
            .connection
            .query_row(
                "SELECT media_asset_id, source_file_instance_id, source_fingerprint, extractor, extractor_version, status, failure_reason, extracted_at, mime_type, byte_size, captured_at_raw, captured_at_local, capture_timezone, capture_time_source, capture_time_confidence, width, height, orientation, camera_make, camera_model, lens_make, lens_model, focal_length_mm, focal_length_equivalent_mm, aperture, shutter_speed, iso, exposure_compensation, flash, white_balance, color_space, gps_present, duration_ms, frame_rate, codec, pixel_format, bitrate, audio_streams, video_streams, sample_rate, bit_depth, channels, raw_metadata_json FROM media_metadata WHERE media_asset_id = ?1",
                params![asset_id.to_string()],
                media_metadata_from_row,
            )
            .optional()?;
        let mut statement = self.connection.prepare(
            "SELECT fi.id, fi.relative_path, roots.selected_path, volumes.display_name, fi.is_available, fi.observed_at FROM file_instances fi LEFT JOIN index_roots roots ON roots.id = fi.index_root_id JOIN storage_volumes volumes ON volumes.id = fi.storage_volume_id WHERE fi.media_asset_id = ?1 ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC",
        )?;
        let copies = statement
            .query_map(params![asset_id.to_string()], |row| {
                Ok(MediaCopyView {
                    file_instance_id: row.get(0)?,
                    relative_path: row.get(1)?,
                    selected_root: row.get(2)?,
                    storage_volume: row.get(3)?,
                    is_available: row.get(4)?,
                    observed_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Some(MediaAssetDetail {
            item,
            metadata,
            copies,
            intelligence: self.capture_intelligence_detail(asset_id)?,
        }))
    }

    fn media_asset_belongs_to_project(
        &self,
        asset_id: &MediaAssetId,
        project_id: &ProjectId,
    ) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id = ?1 AND project_id = ?2)",
                params![asset_id.to_string(), project_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(Into::into)
    }

    fn visual_preparation_candidates(
        &self,
        project_id: &ProjectId,
        query: &VisualMediaQuery,
    ) -> Result<Vec<MediaPreparationCandidate>> {
        let filter_sql = visual_filter_sql(&query.filter);
        let sql = format!(
            "WITH ranked_instances AS (SELECT fi.id, fi.media_asset_id, fi.relative_path, fi.observed_at, fi.is_available, fi.index_root_id, roots.selected_path, ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, CASE WHEN EXISTS (SELECT 1 FROM media_metadata chosen WHERE chosen.media_asset_id = fi.media_asset_id AND chosen.source_file_instance_id = fi.id AND chosen.status = 'ready') THEN 1 ELSE 0 END DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank FROM file_instances fi LEFT JOIN index_roots roots ON roots.id = fi.index_root_id), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1) SELECT a.id, pi.id, a.display_name, a.media_type, pi.selected_path, pi.relative_path, COALESCE(a.content_hash, a.fast_fingerprint, a.id), pi.is_available FROM media_assets a JOIN primary_instances pi ON pi.media_asset_id = a.id LEFT JOIN media_metadata m ON m.media_asset_id = a.id WHERE a.project_id = ?1 AND {filter_sql} AND (?2 IS NULL OR lower(a.display_name) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.camera_model, '')) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.lens_model, '')) LIKE '%' || lower(?2) || '%') AND (m.media_asset_id IS NULL OR m.source_file_instance_id <> pi.id OR m.source_fingerprint <> COALESCE(a.content_hash, a.fast_fingerprint, a.id) OR (m.status = 'offline' AND pi.is_available = 1) OR (pi.is_available = 1 AND a.media_type NOT IN ('audio', 'sidecar', 'unknown') AND NOT EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.size_class = 'small' AND artifact.source_fingerprint = COALESCE(a.content_hash, a.fast_fingerprint, a.id) AND artifact.status IN ('ready', 'unsupported', 'offline', 'corrupt', 'failed', 'timeout', 'cancelled')))) ORDER BY pi.observed_at DESC, a.id ASC LIMIT ?3 OFFSET ?4"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let candidates = statement
            .query_map(
                params![
                    project_id.to_string(),
                    query.search.as_deref(),
                    query.limit as i64,
                    query.offset as i64
                ],
                |row| {
                    Ok(MediaPreparationCandidate {
                        asset_id: row.get(0)?,
                        file_instance_id: row.get(1)?,
                        filename: row.get(2)?,
                        media_type: row.get(3)?,
                        selected_root: row.get(4)?,
                        relative_path: row.get(5)?,
                        source_fingerprint: row.get(6)?,
                        is_available: row.get(7)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn failed_visual_preparation_candidates(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<MediaPreparationCandidate>> {
        let mut statement = self.connection.prepare(
            "SELECT a.id, fi.id, a.display_name, a.media_type, roots.selected_path, fi.relative_path, COALESCE(a.content_hash, a.fast_fingerprint, a.id), fi.is_available FROM media_assets a JOIN file_instances fi ON fi.media_asset_id = a.id LEFT JOIN index_roots roots ON roots.id = fi.index_root_id LEFT JOIN media_metadata m ON m.media_asset_id = a.id AND m.source_file_instance_id = fi.id WHERE a.project_id = ?1 AND fi.is_available = 1 AND (m.status IN ('failed', 'timeout') OR EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = fi.id AND artifact.status IN ('failed', 'timeout'))) ORDER BY fi.observed_at DESC, fi.id ASC",
        )?;
        let candidates = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok(MediaPreparationCandidate {
                    asset_id: row.get(0)?,
                    file_instance_id: row.get(1)?,
                    filename: row.get(2)?,
                    media_type: row.get(3)?,
                    selected_root: row.get(4)?,
                    relative_path: row.get(5)?,
                    source_fingerprint: row.get(6)?,
                    is_available: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn preparation_file_instance_candidates(
        &self,
        asset_id: &str,
    ) -> Result<Vec<MediaPreparationCandidate>> {
        let mut statement = self.connection.prepare(
            "SELECT a.id, fi.id, a.display_name, a.media_type, roots.selected_path, fi.relative_path, COALESCE(a.content_hash, a.fast_fingerprint, a.id), fi.is_available FROM media_assets a JOIN file_instances fi ON fi.media_asset_id = a.id LEFT JOIN index_roots roots ON roots.id = fi.index_root_id WHERE a.id = ?1 ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC",
        )?;
        let candidates = statement
            .query_map(params![asset_id], |row| {
                Ok(MediaPreparationCandidate {
                    asset_id: row.get(0)?,
                    file_instance_id: row.get(1)?,
                    filename: row.get(2)?,
                    media_type: row.get(3)?,
                    selected_root: row.get(4)?,
                    relative_path: row.get(5)?,
                    source_fingerprint: row.get(6)?,
                    is_available: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn capture_time_refresh_candidates(
        &self,
        project_id: &ProjectId,
        after_asset_id: Option<&str>,
        asset_limit: u32,
    ) -> Result<Vec<MediaPreparationCandidate>> {
        let asset_limit = asset_limit.clamp(1, 512);
        let mut statement = self.connection.prepare(
            "WITH selected_assets AS (
                 SELECT id
                 FROM media_assets
                 WHERE project_id = ?1
                   AND (?2 IS NULL OR id > ?2)
                   AND EXISTS (
                     SELECT 1 FROM file_instances available
                     WHERE available.media_asset_id = media_assets.id
                       AND available.is_available = 1
                   )
                 ORDER BY id ASC
                 LIMIT ?3
             )
             SELECT a.id, fi.id, a.display_name, a.media_type, roots.selected_path,
                    fi.relative_path, COALESCE(a.content_hash, a.fast_fingerprint, a.id),
                    fi.is_available
             FROM selected_assets selected
             JOIN media_assets a ON a.id = selected.id
             JOIN file_instances fi ON fi.media_asset_id = a.id AND fi.is_available = 1
             LEFT JOIN index_roots roots ON roots.id = fi.index_root_id
             ORDER BY a.id ASC, fi.observed_at DESC, fi.id ASC",
        )?;
        let candidates = statement
            .query_map(
                params![project_id.to_string(), after_asset_id, asset_limit as i64],
                |row| {
                    Ok(MediaPreparationCandidate {
                        asset_id: row.get(0)?,
                        file_instance_id: row.get(1)?,
                        filename: row.get(2)?,
                        media_type: row.get(3)?,
                        selected_root: row.get(4)?,
                        relative_path: row.get(5)?,
                        source_fingerprint: row.get(6)?,
                        is_available: row.get(7)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn capture_time_refresh_asset_count(&self, project_id: &ProjectId) -> Result<u64> {
        self.connection
            .query_row(
                "SELECT COUNT(*)
                 FROM media_assets a
                 WHERE a.project_id = ?1
                   AND EXISTS (
                     SELECT 1 FROM file_instances fi
                     WHERE fi.media_asset_id = a.id AND fi.is_available = 1
                   )",
                params![project_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as u64)
            .map_err(Into::into)
    }

    fn upsert_capture_time_observation(
        &self,
        observation: &CaptureTimeObservationRecord,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO capture_time_observations (media_asset_id, source_file_instance_id, source_fingerprint, extractor, extractor_version, status, failure_reason, extracted_at, captured_at_raw, captured_at_local, capture_timezone, capture_time_source, capture_time_confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(media_asset_id, source_file_instance_id) DO UPDATE SET
               source_fingerprint = excluded.source_fingerprint,
               extractor = excluded.extractor,
               extractor_version = excluded.extractor_version,
               status = excluded.status,
               failure_reason = excluded.failure_reason,
               extracted_at = excluded.extracted_at,
               captured_at_raw = excluded.captured_at_raw,
               captured_at_local = excluded.captured_at_local,
               capture_timezone = excluded.capture_timezone,
               capture_time_source = excluded.capture_time_source,
               capture_time_confidence = excluded.capture_time_confidence",
            params![
                observation.media_asset_id,
                observation.source_file_instance_id,
                observation.source_fingerprint,
                observation.extractor,
                observation.extractor_version,
                observation.status,
                observation.failure_reason,
                observation.extracted_at,
                observation.captured_at_raw,
                observation.captured_at_local,
                observation.capture_timezone,
                observation.capture_time_source,
                observation.capture_time_confidence,
            ],
        )?;
        Ok(())
    }

    fn upsert_media_metadata(&self, metadata: &MediaMetadataRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO media_metadata (media_asset_id, source_file_instance_id, source_fingerprint, extractor, extractor_version, status, failure_reason, extracted_at, mime_type, byte_size, captured_at_raw, captured_at_local, capture_timezone, capture_time_source, capture_time_confidence, width, height, orientation, camera_make, camera_model, lens_make, lens_model, focal_length_mm, focal_length_equivalent_mm, aperture, shutter_speed, iso, exposure_compensation, flash, white_balance, color_space, gps_present, duration_ms, frame_rate, codec, pixel_format, bitrate, audio_streams, video_streams, sample_rate, bit_depth, channels, raw_metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43) ON CONFLICT(media_asset_id) DO UPDATE SET source_file_instance_id = excluded.source_file_instance_id, source_fingerprint = excluded.source_fingerprint, extractor = excluded.extractor, extractor_version = excluded.extractor_version, status = excluded.status, failure_reason = excluded.failure_reason, extracted_at = excluded.extracted_at, mime_type = excluded.mime_type, byte_size = excluded.byte_size, captured_at_raw = excluded.captured_at_raw, captured_at_local = excluded.captured_at_local, capture_timezone = excluded.capture_timezone, capture_time_source = excluded.capture_time_source, capture_time_confidence = excluded.capture_time_confidence, width = excluded.width, height = excluded.height, orientation = excluded.orientation, camera_make = excluded.camera_make, camera_model = excluded.camera_model, lens_make = excluded.lens_make, lens_model = excluded.lens_model, focal_length_mm = excluded.focal_length_mm, focal_length_equivalent_mm = excluded.focal_length_equivalent_mm, aperture = excluded.aperture, shutter_speed = excluded.shutter_speed, iso = excluded.iso, exposure_compensation = excluded.exposure_compensation, flash = excluded.flash, white_balance = excluded.white_balance, color_space = excluded.color_space, gps_present = excluded.gps_present, duration_ms = excluded.duration_ms, frame_rate = excluded.frame_rate, codec = excluded.codec, pixel_format = excluded.pixel_format, bitrate = excluded.bitrate, audio_streams = excluded.audio_streams, video_streams = excluded.video_streams, sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth, channels = excluded.channels, raw_metadata_json = excluded.raw_metadata_json",
            params![metadata.media_asset_id, metadata.source_file_instance_id, metadata.source_fingerprint, metadata.extractor, metadata.extractor_version, metadata.status, metadata.failure_reason, metadata.extracted_at, metadata.mime_type, metadata.byte_size.map(|value| value as i64), metadata.captured_at_raw, metadata.captured_at_local, metadata.capture_timezone, metadata.capture_time_source, metadata.capture_time_confidence, metadata.width.map(|value| value as i64), metadata.height.map(|value| value as i64), metadata.orientation, metadata.camera_make, metadata.camera_model, metadata.lens_make, metadata.lens_model, metadata.focal_length_mm, metadata.focal_length_equivalent_mm, metadata.aperture, metadata.shutter_speed, metadata.iso.map(|value| value as i64), metadata.exposure_compensation, metadata.flash, metadata.white_balance, metadata.color_space, metadata.gps_present, metadata.duration_ms.map(|value| value as i64), metadata.frame_rate, metadata.codec, metadata.pixel_format, metadata.bitrate.map(|value| value as i64), metadata.audio_streams.map(|value| value as i64), metadata.video_streams.map(|value| value as i64), metadata.sample_rate.map(|value| value as i64), metadata.bit_depth.map(|value| value as i64), metadata.channels.map(|value| value as i64), json(&metadata.raw_metadata)?],
        )?;
        Ok(())
    }

    fn upsert_preview_artifact(&self, artifact: &PreviewArtifactRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO preview_artifacts (id, media_asset_id, source_file_instance_id, artifact_type, size_class, cache_relative_path, provider, generator_version, source_fingerprint, created_at, status, failure_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) ON CONFLICT(media_asset_id, source_file_instance_id, artifact_type, size_class) DO UPDATE SET cache_relative_path = excluded.cache_relative_path, provider = excluded.provider, generator_version = excluded.generator_version, source_fingerprint = excluded.source_fingerprint, created_at = excluded.created_at, status = excluded.status, failure_reason = excluded.failure_reason",
            params![artifact.id, artifact.media_asset_id, artifact.source_file_instance_id, artifact.artifact_type, artifact.size_class, artifact.cache_relative_path, artifact.provider, artifact.generator_version, artifact.source_fingerprint, artifact.created_at, artifact.status, artifact.failure_reason],
        )?;
        Ok(())
    }

    fn clear_preview_artifacts(&self, project_id: &ProjectId) -> Result<()> {
        self.connection.execute(
            "DELETE FROM preview_artifacts WHERE media_asset_id IN (SELECT id FROM media_assets WHERE project_id = ?1)",
            params![project_id.to_string()],
        )?;
        Ok(())
    }

    fn preview_render_artifact(&self, artifact_id: &str) -> Result<Option<PreviewRenderArtifact>> {
        self.connection
            .query_row(
                "SELECT id, cache_relative_path FROM preview_artifacts WHERE id = ?1 AND status = 'ready'",
                params![artifact_id],
                |row| {
                    Ok(PreviewRenderArtifact {
                        id: row.get(0)?,
                        cache_relative_path: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn latest_visual_preparation_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>> {
        self.connection
            .query_row(
                "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at FROM background_jobs WHERE project_id = ?1 AND state_json = '\"completed\"' AND items_total > 0 AND resume_metadata_json LIKE '%\"pipeline\":\"visual-preparation\"%' ORDER BY finished_at DESC, created_at DESC LIMIT 1",
                params![project_id.to_string()],
                background_job_tuple,
            )
            .optional()?
            .map(background_job_from_tuple)
            .transpose()
    }

    fn visual_preparation_terminal_counts(
        &self,
        project_id: &ProjectId,
    ) -> Result<VisualPreparationTerminalCounts> {
        let mut statement = self.connection.prepare(
            "WITH ranked_instances AS (SELECT fi.id, fi.media_asset_id, fi.is_available, fi.observed_at, ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, CASE WHEN EXISTS (SELECT 1 FROM media_metadata chosen WHERE chosen.media_asset_id = fi.media_asset_id AND chosen.source_file_instance_id = fi.id AND chosen.status = 'ready') THEN 1 ELSE 0 END DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank FROM file_instances fi), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1), terminal AS (SELECT a.id, CASE WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'ready') THEN 'ready' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'timeout') THEN 'timeout' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'failed') THEN 'failed' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'corrupt') THEN 'corrupt' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'offline') THEN 'offline' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'cancelled') THEN 'cancelled' WHEN EXISTS (SELECT 1 FROM preview_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.source_file_instance_id = pi.id AND artifact.status = 'unsupported') THEN 'unsupported' ELSE metadata.status END AS status FROM media_assets a JOIN primary_instances pi ON pi.media_asset_id = a.id LEFT JOIN media_metadata metadata ON metadata.media_asset_id = a.id AND metadata.source_file_instance_id = pi.id WHERE a.project_id = ?1) SELECT status, COUNT(*) FROM terminal WHERE status IN ('ready', 'unsupported', 'corrupt', 'offline', 'failed', 'timeout', 'cancelled') GROUP BY status",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let mut counts = VisualPreparationTerminalCounts::default();
        for row in rows {
            let (status, count) = row?;
            counts.processed += count;
            match status.as_str() {
                "ready" => counts.ready = count,
                "unsupported" => counts.unsupported = count,
                "corrupt" => counts.corrupt = count,
                "offline" => counts.offline = count,
                "failed" => counts.failed = count,
                "timeout" => counts.timeout = count,
                "cancelled" => counts.cancelled = count,
                _ => unreachable!("query filters terminal visual statuses"),
            }
        }
        Ok(counts)
    }

    fn analysis_input_candidates(
        &self,
        project_id: &ProjectId,
        provider: &str,
        provider_version: &str,
        settings_version: &str,
        face_provider: &FaceAnalysisProviderConfig,
    ) -> Result<Vec<AnalysisInputCandidate>> {
        // The selected preview is an app-owned cache artifact. Analysis never receives a browser
        // URL or a raw UI path, and cache reuse is keyed by source fingerprint + provider/version.
        self.refresh_capture_intelligence_staleness(
            project_id,
            provider,
            provider_version,
            settings_version,
        )?;
        self.refresh_face_analysis_staleness(
            project_id,
            &face_provider.provider,
            &face_provider.provider_version,
            &face_provider.settings_version,
        )?;
        let sql = r#"WITH ranked_instances AS (
            SELECT fi.id, fi.media_asset_id, fi.is_available, fi.observed_at,
              ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank
            FROM file_instances fi
          ), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1)
          SELECT a.id, a.project_id, pi.id, a.display_name, a.media_type,
            COALESCE(
              analysis_preview.source_fingerprint || CASE analysis_preview.artifact_type
                WHEN 'thumbnail' THEN '|preview:'
                ELSE '|analysis-preview:'
              END || analysis_preview.generator_version || ':' || analysis_preview.size_class,
              a.content_hash,
              a.fast_fingerprint,
              a.id
            ),
            CASE WHEN analysis_preview.status = 'ready' THEN analysis_preview.cache_relative_path END,
            analysis_preview.artifact_type,
            analysis_preview.size_class,
            CASE WHEN analysis_preview.status = 'ready' THEN 'ready'
              WHEN analysis_preview.artifact_type = 'analysis_preview' AND analysis_preview.status = 'corrupt' THEN 'corrupt'
              WHEN analysis_preview.artifact_type = 'analysis_preview' AND analysis_preview.status = 'unsupported' THEN 'unsupported'
              WHEN pi.is_available = 0 THEN 'needs_original'
              ELSE 'needs_input' END,
            analysis_preview.failure_reason,
            a.content_hash, COALESCE(m.captured_at_local, a.captured_at), m.camera_model, pi.is_available
          FROM media_assets a
          JOIN primary_instances pi ON pi.media_asset_id = a.id
          LEFT JOIN media_metadata m ON m.media_asset_id = a.id
          -- Prefer a dedicated on-demand analysis preview. A ready 1600px visual PREVIEW is a
          -- valid fallback for the current deterministic input minimum; 768/256px grid imagery
          -- is deliberately excluded. Every selected artifact must match current source evidence.
          LEFT JOIN preview_artifacts analysis_preview ON analysis_preview.id = (
            SELECT artifact.id
            FROM preview_artifacts artifact
            WHERE artifact.media_asset_id = a.id
              AND artifact.source_fingerprint = COALESCE(a.content_hash, a.fast_fingerprint, a.id)
              AND (
                (artifact.artifact_type = 'analysis_preview' AND artifact.size_class = 'analysis')
                OR (artifact.artifact_type = 'thumbnail' AND artifact.size_class IN ('preview', 'medium', 'small'))
              )
            ORDER BY CASE
                WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status = 'ready' THEN 0
                WHEN artifact.artifact_type = 'thumbnail' AND artifact.size_class = 'preview' AND artifact.status = 'ready' THEN 1
                WHEN artifact.artifact_type = 'analysis_preview' THEN 2
                WHEN artifact.artifact_type = 'thumbnail' AND artifact.size_class = 'preview' THEN 3
                WHEN artifact.artifact_type = 'thumbnail' AND artifact.size_class = 'medium' AND artifact.status = 'ready' THEN 4
                ELSE 5 END,
              artifact.created_at DESC,
              artifact.id DESC
            LIMIT 1
          )
          WHERE a.project_id = ?1
            AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
            AND NOT EXISTS (
              SELECT 1 FROM analysis_artifacts artifact
              WHERE artifact.media_asset_id = a.id
                AND artifact.artifact_type = 'capture_intelligence'
                AND artifact.provider = ?2
                AND artifact.provider_version = ?3
                AND artifact.settings_version = ?4
                AND artifact.input_fingerprint = COALESCE(
                  analysis_preview.source_fingerprint || CASE analysis_preview.artifact_type
                    WHEN 'thumbnail' THEN '|preview:'
                    ELSE '|analysis-preview:'
                  END || analysis_preview.generator_version || ':' || analysis_preview.size_class,
                  a.content_hash,
                  a.fast_fingerprint,
                  a.id
                )
                AND artifact.stale = 0
                -- A usable cached preview supersedes an earlier NEEDS_ORIGINAL result with the
                -- same source fingerprint. Completed READY evidence remains cached.
                AND (
                  artifact.status = 'ready'
                  OR (artifact.status = 'corrupt' AND analysis_preview.artifact_type = 'analysis_preview' AND analysis_preview.status = 'corrupt')
                  OR (artifact.status = 'unsupported' AND analysis_preview.artifact_type = 'analysis_preview' AND analysis_preview.status = 'unsupported')
                  OR (artifact.status = 'needs_original' AND analysis_preview.status IS NULL AND pi.is_available = 0)
                  OR (artifact.status = 'not_applicable' AND analysis_preview.status IS NULL AND pi.is_available = 0)
                )
            )
          ORDER BY pi.observed_at ASC, a.id ASC"#;
        let mut statement = self.connection.prepare(sql)?;
        let candidates = statement
            .query_map(
                params![
                    project_id.to_string(),
                    provider,
                    provider_version,
                    settings_version,
                ],
                |row| {
                    Ok(AnalysisInputCandidate {
                        asset_id: row.get(0)?,
                        project_id: row.get(1)?,
                        file_instance_id: row.get(2)?,
                        filename: row.get(3)?,
                        media_type: row.get(4)?,
                        input_fingerprint: row.get(5)?,
                        preview_relative_path: row.get(6)?,
                        preview_artifact_type: row.get(7)?,
                        preview_size_class: row.get(8)?,
                        preview_status: row.get(9)?,
                        preview_failure_reason: row.get(10)?,
                        content_hash: row.get(11)?,
                        captured_at: row.get(12)?,
                        camera_model: row.get(13)?,
                        is_available: row.get(14)?,
                        perceptual_hash: None,
                        difference_hash: None,
                        color_signature: None,
                        embedding: None,
                        bucket_keys: None,
                        technical_quality_score: None,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn face_analysis_input_candidates(
        &self,
        project_id: &ProjectId,
        provider: &str,
        provider_version: &str,
        settings_version: &str,
        face_provider: &FaceAnalysisProviderConfig,
    ) -> Result<Vec<AnalysisInputCandidate>> {
        self.refresh_capture_intelligence_staleness(
            project_id,
            provider,
            provider_version,
            settings_version,
        )?;
        self.refresh_face_analysis_staleness(
            project_id,
            &face_provider.provider,
            &face_provider.provider_version,
            &face_provider.settings_version,
        )?;
        // Face refresh deliberately joins an already-current primary artifact. It never invokes
        // the input resolver or selects an original file; only a CaptureOS-managed ready cache
        // artifact can enter this queue.
        let sql = r#"WITH ranked_instances AS (
            SELECT fi.id, fi.media_asset_id, fi.is_available, fi.observed_at,
              ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank
            FROM file_instances fi
          ), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1)
          SELECT a.id, a.project_id, pi.id, a.display_name, a.media_type,
            primary_artifact.input_fingerprint,
            analysis_preview.cache_relative_path,
            analysis_preview.artifact_type,
            analysis_preview.size_class,
            'ready',
            NULL,
            a.content_hash, COALESCE(m.captured_at_local, a.captured_at), m.camera_model, pi.is_available
          FROM media_assets a
          JOIN primary_instances pi ON pi.media_asset_id = a.id
          JOIN analysis_artifacts primary_artifact ON primary_artifact.media_asset_id = a.id
            AND primary_artifact.artifact_type = 'capture_intelligence'
            AND primary_artifact.provider = ?2
            AND primary_artifact.provider_version = ?3
            AND primary_artifact.settings_version = ?4
            AND primary_artifact.status = 'ready'
            AND primary_artifact.stale = 0
          LEFT JOIN media_metadata m ON m.media_asset_id = a.id
          JOIN preview_artifacts analysis_preview ON analysis_preview.id = (
            SELECT artifact.id
            FROM preview_artifacts artifact
            WHERE artifact.media_asset_id = a.id
              AND artifact.status = 'ready'
              AND artifact.cache_relative_path IS NOT NULL
              AND artifact.source_fingerprint = COALESCE(a.content_hash, a.fast_fingerprint, a.id)
              AND (
                (artifact.artifact_type = 'analysis_preview' AND artifact.size_class = 'analysis')
                OR (artifact.artifact_type = 'thumbnail' AND artifact.size_class IN ('preview', 'medium', 'small'))
              )
              AND (
                artifact.source_fingerprint || CASE artifact.artifact_type
                  WHEN 'thumbnail' THEN '|preview:'
                  ELSE '|analysis-preview:'
                END || artifact.generator_version || ':' || artifact.size_class
              ) = primary_artifact.input_fingerprint
            ORDER BY CASE
                WHEN artifact.artifact_type = 'analysis_preview' THEN 0
                WHEN artifact.size_class = 'preview' THEN 1
                WHEN artifact.size_class = 'medium' THEN 2
                ELSE 3 END,
              artifact.created_at DESC, artifact.id DESC
            LIMIT 1
          )
          WHERE a.project_id = ?1
            AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
            AND NOT EXISTS (
              SELECT 1
              FROM analysis_artifacts face_artifact
              WHERE face_artifact.media_asset_id = a.id
                AND face_artifact.artifact_type = 'face_detection'
                AND face_artifact.provider = ?5
                AND face_artifact.provider_version = ?6
                AND face_artifact.settings_version = ?7
                AND face_artifact.stale = 0
                AND COALESCE(
                  json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'),
                  face_artifact.input_fingerprint
                ) = primary_artifact.input_fingerprint
            )
          ORDER BY pi.observed_at ASC, a.id ASC"#;
        let mut statement = self.connection.prepare(sql)?;
        let candidates = statement
            .query_map(
                params![
                    project_id.to_string(),
                    provider,
                    provider_version,
                    settings_version,
                    face_provider.provider.as_str(),
                    face_provider.provider_version.as_str(),
                    face_provider.settings_version.as_str(),
                ],
                |row| {
                    Ok(AnalysisInputCandidate {
                        asset_id: row.get(0)?,
                        project_id: row.get(1)?,
                        file_instance_id: row.get(2)?,
                        filename: row.get(3)?,
                        media_type: row.get(4)?,
                        input_fingerprint: row.get(5)?,
                        preview_relative_path: row.get(6)?,
                        preview_artifact_type: row.get(7)?,
                        preview_size_class: row.get(8)?,
                        preview_status: row.get(9)?,
                        preview_failure_reason: row.get(10)?,
                        content_hash: row.get(11)?,
                        captured_at: row.get(12)?,
                        camera_model: row.get(13)?,
                        is_available: row.get(14)?,
                        perceptual_hash: None,
                        difference_hash: None,
                        color_signature: None,
                        embedding: None,
                        bucket_keys: None,
                        technical_quality_score: None,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)?;
        Ok(candidates)
    }

    fn upsert_analysis_artifact(&self, artifact: &AnalysisArtifact) -> Result<()> {
        let existing = self
            .connection
            .query_row(
                "SELECT media_asset_id, artifact_type, input_fingerprint, provider, provider_version, settings_version
                 FROM analysis_artifacts WHERE id = ?1",
                params![artifact.id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            existing_asset_id,
            existing_type,
            existing_input,
            existing_provider,
            existing_provider_version,
            existing_settings,
        )) = existing
        {
            let identity_changed = existing_asset_id != artifact.media_asset_id.to_string()
                || existing_type != artifact.artifact_type
                || existing_input.as_deref() != artifact.input_fingerprint.as_deref()
                || existing_provider.as_deref() != artifact.provider.as_deref()
                || existing_provider_version.as_deref() != artifact.provider_version.as_deref()
                || existing_settings.as_deref() != artifact.settings_version.as_deref();
            if identity_changed {
                return Err(PersistenceError::InvalidData(
                    "analysis artifact identity is immutable; create a new artifact revision"
                        .into(),
                ));
            }
        }
        self.connection.execute(
            "INSERT INTO analysis_artifacts (id, media_asset_id, artifact_type, payload_json, provenance_json, provider, provider_version, model_version, settings_version, input_fingerprint, generated_at, confidence, status, error_message, stale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)
             ON CONFLICT(id) DO UPDATE SET payload_json = excluded.payload_json, provenance_json = excluded.provenance_json, provider = excluded.provider, provider_version = excluded.provider_version, model_version = excluded.model_version, settings_version = excluded.settings_version, input_fingerprint = excluded.input_fingerprint, generated_at = excluded.generated_at, confidence = excluded.confidence, status = excluded.status, error_message = excluded.error_message, stale = 0",
            params![
                artifact.id.to_string(), artifact.media_asset_id.to_string(), artifact.artifact_type,
                json(&artifact.payload)?, json(&artifact.provenance)?, artifact.provider,
                artifact.provider_version, artifact.model_version, artifact.settings_version,
                artifact.input_fingerprint,
                artifact.generated_at.as_ref().map(timestamp), artifact.confidence,
                artifact.status.as_ref().map(AnalysisStatus::as_str), artifact.error_message,
            ],
        )?;
        // Artifact IDs are immutable revisions. A newer revision, even with the same cache key,
        // supersedes the prior current row without destroying it.
        self.connection.execute(
            "UPDATE analysis_artifacts
             SET stale = 1
             WHERE media_asset_id = ?1
               AND artifact_type = ?2
               AND id <> ?3
               AND stale = 0",
            params![
                artifact.media_asset_id.to_string(),
                artifact.artifact_type,
                artifact.id.to_string()
            ],
        )?;
        if artifact.artifact_type == "capture_intelligence" {
            // The primary revision is the durable source of truth for every derived evidence
            // table. Evidence is reactivated only by its corresponding upsert below.
            self.connection.execute(
                "UPDATE visual_fingerprints SET stale = 1 WHERE media_asset_id = ?1 AND stale = 0",
                params![artifact.media_asset_id.to_string()],
            )?;
            self.connection.execute(
                "UPDATE visual_embeddings SET stale = 1 WHERE media_asset_id = ?1 AND stale = 0",
                params![artifact.media_asset_id.to_string()],
            )?;
            self.connection.execute(
                "UPDATE technical_quality SET stale = 1 WHERE media_asset_id = ?1 AND stale = 0",
                params![artifact.media_asset_id.to_string()],
            )?;
            self.connection.execute(
                "UPDATE analysis_recommendations SET stale = 1 WHERE media_asset_id = ?1 AND stale = 0",
                params![artifact.media_asset_id.to_string()],
            )?;
            self.connection.execute(
                "UPDATE similarity_groups
                 SET stale = 1
                 WHERE project_id = (SELECT project_id FROM media_assets WHERE id = ?1)
                   AND stale = 0",
                params![artifact.media_asset_id.to_string()],
            )?;
        }
        mark_studio_recommendations_stale_for_asset(
            &self.connection,
            &artifact.media_asset_id.to_string(),
        )?;
        Ok(())
    }

    fn upsert_visual_fingerprint(&self, fingerprint: &VisualFingerprint) -> Result<()> {
        let embedding = fingerprint
            .embedding
            .iter()
            .map(|value| *value as u8)
            .collect::<Vec<_>>();
        self.connection.execute(
            "UPDATE visual_fingerprints
             SET stale = 1
             WHERE media_asset_id = ?1
               AND stale = 0
               AND (input_fingerprint <> ?2 OR provider <> ?3 OR provider_version <> ?4)",
            params![
                fingerprint.media_asset_id.to_string(),
                fingerprint.input_fingerprint,
                fingerprint.provider,
                fingerprint.provider_version,
            ],
        )?;
        self.connection.execute(
            "UPDATE visual_embeddings
             SET stale = 1
             WHERE media_asset_id = ?1
               AND stale = 0
               AND (input_fingerprint <> ?2 OR provider <> ?3 OR provider_version <> ?4)",
            params![
                fingerprint.media_asset_id.to_string(),
                fingerprint.input_fingerprint,
                fingerprint.provider,
                fingerprint.provider_version,
            ],
        )?;
        self.connection.execute(
            "INSERT INTO visual_fingerprints (media_asset_id, input_fingerprint, provider, provider_version, perceptual_hash, difference_hash, color_signature, bucket_keys_json, generated_at, status, error_message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(media_asset_id, input_fingerprint, provider, provider_version) DO UPDATE SET perceptual_hash = excluded.perceptual_hash, difference_hash = excluded.difference_hash, color_signature = excluded.color_signature, bucket_keys_json = excluded.bucket_keys_json, generated_at = excluded.generated_at, status = excluded.status, error_message = excluded.error_message, stale = 0",
            params![
                fingerprint.media_asset_id.to_string(), fingerprint.input_fingerprint, fingerprint.provider,
                fingerprint.provider_version, fingerprint.perceptual_hash, fingerprint.difference_hash,
                fingerprint.color_signature, json(&fingerprint.bucket_keys)?, timestamp(&fingerprint.generated_at),
                fingerprint.status.as_str(), fingerprint.error_message,
            ],
        )?;
        self.connection.execute(
            "INSERT INTO visual_embeddings (media_asset_id, input_fingerprint, provider, provider_version, dimensions, embedding, generated_at, status, error_message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(media_asset_id, input_fingerprint, provider, provider_version) DO UPDATE SET dimensions = excluded.dimensions, embedding = excluded.embedding, generated_at = excluded.generated_at, status = excluded.status, error_message = excluded.error_message, stale = 0",
            params![
                fingerprint.media_asset_id.to_string(), fingerprint.input_fingerprint, fingerprint.provider,
                fingerprint.provider_version, fingerprint.embedding_dimensions as i64, embedding,
                timestamp(&fingerprint.generated_at), fingerprint.status.as_str(), fingerprint.error_message,
            ],
        )?;
        Ok(())
    }

    fn upsert_technical_quality(&self, evidence: &TechnicalQualityEvidence) -> Result<()> {
        self.connection.execute(
            "UPDATE technical_quality
             SET stale = 1
             WHERE media_asset_id = ?1
               AND stale = 0
               AND (input_fingerprint <> ?2 OR provider <> ?3 OR provider_version <> ?4)",
            params![
                evidence.media_asset_id.to_string(),
                evidence.input_fingerprint,
                evidence.provider,
                evidence.provider_version,
            ],
        )?;
        self.connection.execute(
            "INSERT INTO technical_quality (media_asset_id, input_fingerprint, provider, provider_version, global_sharpness, sharpness_band, directional_blur_ratio, blur_level, mean_luminance, median_luminance, highlight_clipping_percent, shadow_clipping_percent, channel_clipping_percent, technical_quality_score, technical_quality_band, confidence, generated_at, status, error_message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(media_asset_id, input_fingerprint, provider, provider_version) DO UPDATE SET global_sharpness = excluded.global_sharpness, sharpness_band = excluded.sharpness_band, directional_blur_ratio = excluded.directional_blur_ratio, blur_level = excluded.blur_level, mean_luminance = excluded.mean_luminance, median_luminance = excluded.median_luminance, highlight_clipping_percent = excluded.highlight_clipping_percent, shadow_clipping_percent = excluded.shadow_clipping_percent, channel_clipping_percent = excluded.channel_clipping_percent, technical_quality_score = excluded.technical_quality_score, technical_quality_band = excluded.technical_quality_band, confidence = excluded.confidence, generated_at = excluded.generated_at, status = excluded.status, error_message = excluded.error_message, stale = 0",
            params![
                evidence.media_asset_id.to_string(), evidence.input_fingerprint, evidence.provider,
                evidence.provider_version, evidence.global_sharpness, evidence.sharpness_band,
                evidence.directional_blur_ratio, evidence.blur_level.as_str(), evidence.mean_luminance,
                evidence.median_luminance, evidence.highlight_clipping_percent,
                evidence.shadow_clipping_percent, evidence.channel_clipping_percent,
                evidence.technical_quality_score, evidence.technical_quality_band.as_str(),
                evidence.confidence, timestamp(&evidence.generated_at), evidence.status.as_str(),
                evidence.error_message,
            ],
        )?;
        mark_studio_recommendations_stale_for_asset(
            &self.connection,
            &evidence.media_asset_id.to_string(),
        )?;
        Ok(())
    }

    fn replace_face_analyses(
        &self,
        asset_id: &MediaAssetId,
        input_fingerprint: &str,
        faces: &[FaceAnalysis],
    ) -> Result<()> {
        self.connection.execute(
            "DELETE FROM face_analyses WHERE media_asset_id = ?1 AND input_fingerprint = ?2",
            params![asset_id.to_string(), input_fingerprint],
        )?;
        for face in faces {
            self.connection.execute(
                "INSERT INTO face_analyses (id, media_asset_id, input_fingerprint, provider, provider_version, x, y, width, height, detection_confidence, relative_size, visibility, pose, face_sharpness, eye_state, eye_confidence, generated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    face.id.to_string(), face.media_asset_id.to_string(), face.input_fingerprint,
                    face.provider, face.provider_version, face.x, face.y, face.width, face.height,
                    face.detection_confidence, face.relative_size, face.visibility, face.pose,
                    face.face_sharpness, face.eye_state.as_str(), face.eye_confidence,
                    timestamp(&face.generated_at),
                ],
            )?;
        }
        mark_studio_recommendations_stale_for_asset(&self.connection, &asset_id.to_string())?;
        Ok(())
    }

    fn replace_similarity_groups(
        &self,
        project_id: &ProjectId,
        groups: &[SimilarityGroup],
        members: &[SimilarityGroupMember],
    ) -> Result<()> {
        // Groups and recommendation links are audit records too. Mark the prior project view
        // stale instead of deleting it or clearing an older recommendation's group reference.
        // Deterministic IDs let an unchanged group be reactivated in place.
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE similarity_groups SET stale = 1 WHERE project_id = ?1 AND stale = 0",
            params![project_id.to_string()],
        )?;
        for group in groups {
            transaction.execute(
                "DELETE FROM similarity_group_members WHERE group_id = ?1",
                params![group.id.to_string()],
            )?;
            transaction.execute(
                "INSERT INTO similarity_groups (id, project_id, kind, representative_asset_id, grouping_method, grouping_version, similarity_confidence, time_proximity_seconds, visual_similarity, created_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)
                 ON CONFLICT(id) DO UPDATE SET project_id = excluded.project_id, kind = excluded.kind, representative_asset_id = excluded.representative_asset_id, grouping_method = excluded.grouping_method, grouping_version = excluded.grouping_version, similarity_confidence = excluded.similarity_confidence, time_proximity_seconds = excluded.time_proximity_seconds, visual_similarity = excluded.visual_similarity, created_at = excluded.created_at, stale = 0",
                params![
                    group.id.to_string(), group.project_id.to_string(), group.kind.as_str(),
                    group.representative_asset_id.to_string(), group.grouping_method,
                    group.grouping_version, group.similarity_confidence,
                    group.time_proximity_seconds.map(|value| value as i64), group.visual_similarity,
                    timestamp(&group.created_at),
                ],
            )?;
        }
        for member in members {
            transaction.execute(
                "INSERT INTO similarity_group_members (group_id, media_asset_id, ordinal, similarity_confidence, time_proximity_seconds, is_representative)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    member.group_id.to_string(), member.media_asset_id.to_string(), member.ordinal as i64,
                    member.similarity_confidence, member.time_proximity_seconds.map(|value| value as i64),
                    member.is_representative,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE project_id = ?1 AND stale = 0",
            params![project_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn upsert_ai_recommendation(&self, recommendation: &AiRecommendation) -> Result<()> {
        // Recommendations are append-only evidence. A new advisory—even for the same input—is
        // a new revision, so a human decision can keep pointing at the exact recommendation it
        // answered instead of a row whose content was later rewritten by grouping.
        let reasons = json(&recommendation.reasons)?;
        let current = self
            .connection
            .query_row(
                "SELECT similarity_group_id, label, confidence, reasons_json, status
                 FROM analysis_recommendations
                 WHERE media_asset_id = ?1
                   AND provider = ?2
                   AND provider_version = ?3
                   AND input_fingerprint = ?4
                   AND stale = 0
                 ORDER BY generated_at DESC, id DESC
                 LIMIT 1",
                params![
                    recommendation.media_asset_id.to_string(),
                    recommendation.provider,
                    recommendation.provider_version,
                    recommendation.input_fingerprint,
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let group_id = recommendation
            .similarity_group_id
            .as_ref()
            .map(ToString::to_string);
        if current.is_some_and(
            |(
                current_group,
                current_label,
                current_confidence,
                current_reasons,
                current_status,
            )| {
                current_group == group_id
                    && current_label == recommendation.label.as_str()
                    && current_confidence == recommendation.confidence
                    && current_reasons == reasons
                    && current_status == recommendation.status.as_str()
            },
        ) {
            // Reopening the browser or an already-complete job must not create synthetic history
            // revisions. Only a material advisory change deserves a new immutable row.
            return Ok(());
        }
        self.connection.execute(
            "UPDATE analysis_recommendations SET stale = 1
             WHERE media_asset_id = ?1 AND stale = 0",
            params![recommendation.media_asset_id.to_string()],
        )?;
        self.connection.execute(
            "INSERT INTO analysis_recommendations (id, media_asset_id, similarity_group_id, input_fingerprint, provider, provider_version, label, confidence, reasons_json, generated_at, status, stale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            params![
                recommendation.id.to_string(),
                recommendation.media_asset_id.to_string(),
                group_id,
                recommendation.input_fingerprint,
                recommendation.provider,
                recommendation.provider_version,
                recommendation.label.as_str(),
                recommendation.confidence,
                reasons,
                timestamp(&recommendation.generated_at),
                recommendation.status.as_str(),
            ],
        )?;
        mark_studio_recommendations_stale_for_asset(
            &self.connection,
            &recommendation.media_asset_id.to_string(),
        )?;
        Ok(())
    }

    fn upsert_human_decision(&self, decision: &HumanDecision) -> Result<()> {
        if let Some(recommendation_id) = &decision.recommendation_id {
            let belongs_to_asset: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM analysis_recommendations WHERE id = ?1 AND media_asset_id = ?2)",
                params![recommendation_id.to_string(), decision.media_asset_id.to_string()],
                |row| row.get(0),
            )?;
            if !belongs_to_asset {
                return Err(PersistenceError::InvalidData(
                    "human decision must reference a recommendation for the same media asset"
                        .into(),
                ));
            }
        }
        // M8.3's SQLite trigger advances the source revision and pending counter in this same
        // authoritative write. The later compact snapshot is therefore unable to race a visible
        // human correction, while snapshot failures remain fail-open for the correction itself.
        self.connection.execute(
            "INSERT INTO intelligence_overrides (id, media_asset_id, recommendation_id, decision, decided_at, note)
             VALUES (
                ?1,
                ?2,
                COALESCE(
                  ?3,
                  (SELECT id FROM analysis_recommendations
                   WHERE media_asset_id = ?2
                   ORDER BY stale ASC, generated_at DESC, id DESC
                   LIMIT 1)
                ),
                ?4,
                ?5,
                ?6
             )",
            params![
                decision.id.to_string(), decision.media_asset_id.to_string(),
                decision.recommendation_id.as_ref().map(ToString::to_string), decision.decision.as_str(),
                timestamp(&decision.decided_at), decision.note,
            ],
        )?;
        // M4's explicit human override remains a distinct source. It is never conflated with a
        // generic recommendation label, and the optional note is intentionally excluded.
        // The M4 human action above is authoritative and already durable. Studio source capture
        // is additive: if a compact snapshot cannot be read right now, explicit backfill on the
        // next requested update recovers it without making the human action appear unsaved.
        let mut captured_profile_id = None;
        let mut source_materialized = false;
        if let (Ok(project_id), Ok(profile_id)) = (
            self.connection.query_row(
                "SELECT project_id FROM media_assets WHERE id = ?1",
                params![decision.media_asset_id.to_string()],
                |row| row.get::<_, String>(0),
            ),
            self.ensure_default_studio_profile(),
        ) {
            captured_profile_id = Some(profile_id.clone());
            let source = StudioHistoricalRow {
                source_kind: "m4_human_override".into(),
                source_record_id: decision.id.to_string(),
                project_id,
                media_asset_id: Some(decision.media_asset_id.to_string()),
                decision_type: "culling_decision".into(),
                decision_value: Some(decision.decision.as_str().into()),
                occurred_at: timestamp(&decision.decided_at),
                review_session_id: None,
                similarity_group_id: None,
                moment_id: None,
            };
            match materialize_live_studio_training_example(&self.connection, &profile_id, &source) {
                Ok(()) => source_materialized = true,
                Err(_) => mark_studio_source_capture_deferred(&self.connection, &profile_id),
            }
        }
        if source_materialized {
            // The trigger created one action-scoped guard for this human override. A failed
            // capture deliberately leaves that guard pending for explicit historical reconciliation.
            if let Some(profile_id) = captured_profile_id.as_deref() {
                let _ = finish_studio_source_materialization(
                    &self.connection,
                    profile_id,
                    "m4_human_override",
                    &decision.id.to_string(),
                );
            }
        }
        Ok(())
    }

    fn grouping_inputs(&self, project_id: &ProjectId) -> Result<Vec<AnalysisInputCandidate>> {
        let mut statement = self.connection.prepare(
            "WITH current_artifacts AS (
               SELECT artifact.media_asset_id, artifact.input_fingerprint, artifact.provider,
                      artifact.provider_version,
                      ROW_NUMBER() OVER (
                        PARTITION BY artifact.media_asset_id
                        ORDER BY artifact.generated_at DESC, artifact.id DESC
                      ) AS position
               FROM analysis_artifacts artifact
               JOIN media_assets asset ON asset.id = artifact.media_asset_id
               WHERE asset.project_id = ?1
                 AND artifact.artifact_type = 'capture_intelligence'
                 AND artifact.status = 'ready'
                 AND artifact.stale = 0
             )
             SELECT a.id, a.project_id, '', a.display_name, a.media_type, vf.input_fingerprint, NULL, NULL, NULL, 'ready', NULL, a.content_hash,
               COALESCE(m.captured_at_local, a.captured_at), m.camera_model, 1,
               vf.perceptual_hash, vf.difference_hash, vf.color_signature, ve.embedding, vf.bucket_keys_json,
               tq.technical_quality_score
             FROM media_assets a
             JOIN current_artifacts current ON current.media_asset_id = a.id AND current.position = 1
             JOIN visual_fingerprints vf ON vf.media_asset_id = a.id
               AND vf.input_fingerprint = current.input_fingerprint
               AND vf.provider = current.provider
               AND vf.provider_version = current.provider_version
               AND vf.status = 'ready'
               AND vf.stale = 0
             JOIN visual_embeddings ve ON ve.media_asset_id = a.id
               AND ve.input_fingerprint = vf.input_fingerprint
               AND ve.provider = vf.provider
               AND ve.provider_version = vf.provider_version
               AND ve.status = 'ready'
               AND ve.stale = 0
             LEFT JOIN media_metadata m ON m.media_asset_id = a.id
             LEFT JOIN technical_quality tq ON tq.media_asset_id = a.id
               AND tq.input_fingerprint = vf.input_fingerprint
               AND tq.provider = vf.provider
               AND tq.provider_version = vf.provider_version
               AND tq.stale = 0
             WHERE a.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
             ORDER BY a.id ASC",
        )?;
        let inputs = statement
            .query_map(params![project_id.to_string()], |row| {
                let buckets: String = row.get(19)?;
                let buckets = serde_json::from_str(&buckets)
                    .map_err(|error| to_sql_error(PersistenceError::Serialization(error)))?;
                let embedding: Vec<u8> = row.get(18)?;
                Ok(AnalysisInputCandidate {
                    asset_id: row.get(0)?,
                    project_id: row.get(1)?,
                    file_instance_id: row.get(2)?,
                    filename: row.get(3)?,
                    media_type: row.get(4)?,
                    input_fingerprint: row.get(5)?,
                    preview_relative_path: row.get(6)?,
                    preview_artifact_type: row.get(7)?,
                    preview_size_class: row.get(8)?,
                    preview_status: row.get(9)?,
                    preview_failure_reason: row.get(10)?,
                    content_hash: row.get(11)?,
                    captured_at: row.get(12)?,
                    camera_model: row.get(13)?,
                    is_available: row.get(14)?,
                    perceptual_hash: row.get(15)?,
                    difference_hash: row.get(16)?,
                    color_signature: row.get(17)?,
                    embedding: Some(embedding.into_iter().map(|value| value as i8).collect()),
                    bucket_keys: Some(buckets),
                    technical_quality_score: row.get(20)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        inputs
    }

    fn capture_intelligence_detail(
        &self,
        asset_id: &MediaAssetId,
    ) -> Result<Option<CaptureIntelligenceDetail>> {
        let summary = match intelligence_summary_for_asset(&self.connection, &asset_id.to_string())?
        {
            Some(summary) => summary,
            None => return Ok(None),
        };
        let artifact = self.connection.query_row(
            "SELECT input_fingerprint, provider, provider_version, settings_version, generated_at
             FROM analysis_artifacts
             WHERE media_asset_id = ?1
               AND artifact_type = 'capture_intelligence'
               AND stale = 0
             ORDER BY generated_at DESC, id DESC LIMIT 1",
            params![asset_id.to_string()],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)),
        ).optional()?;
        let Some((input_fingerprint, provider, provider_version, settings_version, generated_at)) =
            artifact
        else {
            return Ok(None);
        };
        let input_key = input_fingerprint.clone().unwrap_or_default();
        let provider_key = provider.clone().unwrap_or_default();
        let provider_version_key = provider_version.clone().unwrap_or_default();
        let technical = self.connection.query_row(
            "SELECT global_sharpness, sharpness_band, directional_blur_ratio, blur_level, mean_luminance, median_luminance, highlight_clipping_percent, shadow_clipping_percent, channel_clipping_percent, technical_quality_score, technical_quality_band, confidence, status, error_message
             FROM technical_quality
             WHERE media_asset_id = ?1
               AND input_fingerprint = ?2
               AND provider = ?3
               AND provider_version = ?4
               AND stale = 0",
            params![asset_id.to_string(), input_key, provider_key, provider_version_key],
            |row| Ok(TechnicalQualityView {
                global_sharpness: row.get(0)?, sharpness_band: row.get(1)?, directional_blur_ratio: row.get(2)?, blur_level: row.get(3)?, mean_luminance: row.get(4)?, median_luminance: row.get(5)?, highlight_clipping_percent: row.get(6)?, shadow_clipping_percent: row.get(7)?, channel_clipping_percent: row.get(8)?, technical_quality_score: row.get(9)?, technical_quality_band: row.get(10)?, confidence: row.get(11)?, status: row.get(12)?, error_message: row.get(13)?,
            }),
        ).optional()?;
        let face_artifact = self
            .connection
            .query_row(
                "SELECT input_fingerprint, provider, provider_version, status, error_message, payload_json
             FROM analysis_artifacts
             WHERE media_asset_id = ?1
               AND artifact_type = 'face_detection'
               AND stale = 0
               AND COALESCE(
                 json_extract(payload_json, '$.input_preview_fingerprint'),
                 input_fingerprint
               ) = ?2
             ORDER BY generated_at DESC, id DESC
             LIMIT 1",
                params![
                    asset_id.to_string(),
                    input_fingerprint.clone().unwrap_or_default()
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let face_status_fallback = if face_artifact.is_none() {
            let stale_face_exists: bool = self.connection.query_row(
                "SELECT EXISTS(
                   SELECT 1
                   FROM analysis_artifacts face_artifact
                   WHERE face_artifact.media_asset_id = ?1
                     AND face_artifact.artifact_type = 'face_detection'
                     AND face_artifact.stale = 1
                     AND COALESCE(
                       json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'),
                       face_artifact.input_fingerprint
                     ) = ?2
                 )",
                params![
                    asset_id.to_string(),
                    input_fingerprint.clone().unwrap_or_default(),
                ],
                |row| row.get(0),
            )?;
            if stale_face_exists {
                "stale".into()
            } else {
                "not_applicable".into()
            }
        } else {
            "not_applicable".into()
        };
        let faces = if let Some((
            Some(face_input_fingerprint),
            Some(face_provider),
            Some(face_provider_version),
            _,
            _,
            _,
        )) = face_artifact.as_ref()
        {
            let mut statement = self.connection.prepare(
                "SELECT id, x, y, width, height, detection_confidence, relative_size, visibility, pose, face_sharpness, eye_state, eye_confidence
                 FROM face_analyses
                 WHERE media_asset_id = ?1
                   AND input_fingerprint = ?2
                   AND provider = ?3
                   AND provider_version = ?4
                 ORDER BY detection_confidence DESC, id ASC",
            )?;
            let faces = statement
                .query_map(
                    params![
                        asset_id.to_string(),
                        face_input_fingerprint,
                        face_provider,
                        face_provider_version,
                    ],
                    |row| {
                        Ok(FaceAnalysisView {
                            id: row.get(0)?,
                            x: row.get(1)?,
                            y: row.get(2)?,
                            width: row.get(3)?,
                            height: row.get(4)?,
                            detection_confidence: row.get(5)?,
                            relative_size: row.get(6)?,
                            visibility: row.get(7)?,
                            pose: row.get(8)?,
                            face_sharpness: row.get(9)?,
                            eye_state: row.get(10)?,
                            eye_confidence: row.get(11)?,
                        })
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            faces
        } else {
            Vec::new()
        };
        let recommendation_reasons = self
            .connection
            .query_row(
                "SELECT reasons_json FROM analysis_recommendations
             WHERE media_asset_id = ?1 AND input_fingerprint = ?2 AND stale = 0
             ORDER BY generated_at DESC, id DESC LIMIT 1",
                params![
                    asset_id.to_string(),
                    input_fingerprint.clone().unwrap_or_default()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default();
        // M5 current human culling state is separate from the older M4 advisory override log.
        // Prefer it for current UI while retaining both histories unchanged.
        let human_decision = self.connection.query_row(
            "SELECT COALESCE(
                (SELECT decision FROM media_decisions WHERE media_asset_id = ?1 AND decision IS NOT NULL ORDER BY updated_at DESC, id DESC LIMIT 1),
                (SELECT decision FROM intelligence_overrides WHERE media_asset_id = ?1 ORDER BY decided_at DESC, id DESC LIMIT 1)
             )",
            params![asset_id.to_string()], |row| row.get(0),
        ).optional()?.flatten();
        Ok(Some(CaptureIntelligenceDetail {
            summary,
            input_fingerprint: input_fingerprint.unwrap_or_default(),
            provider: provider.unwrap_or_else(|| "legacy".into()),
            provider_version: provider_version.unwrap_or_else(|| "legacy".into()),
            settings_version: settings_version.unwrap_or_else(|| "legacy".into()),
            generated_at: generated_at.unwrap_or_default(),
            technical,
            face_provider: face_artifact
                .as_ref()
                .and_then(|artifact| artifact.1.clone())
                .unwrap_or_else(|| "none".into()),
            face_provider_version: face_artifact
                .as_ref()
                .and_then(|artifact| artifact.2.clone())
                .unwrap_or_else(|| "none".into()),
            face_resolved_provider: face_artifact
                .as_ref()
                .and_then(|artifact| {
                    face_artifact_payload_string(&artifact.5, "resolved_face_provider")
                })
                .unwrap_or_else(|| "none".into()),
            face_resolved_provider_version: face_artifact
                .as_ref()
                .and_then(|artifact| {
                    face_artifact_payload_string(&artifact.5, "resolved_face_provider_version")
                })
                .unwrap_or_else(|| "none".into()),
            face_analysis_status: face_artifact
                .as_ref()
                .and_then(|artifact| artifact.3.clone())
                .unwrap_or(face_status_fallback),
            face_analysis_error: face_artifact
                .as_ref()
                .and_then(|artifact| artifact.4.clone()),
            face_provider_attempt_error: face_artifact.as_ref().and_then(|artifact| {
                face_artifact_payload_string(&artifact.5, "provider_attempt_error")
            }),
            face_landmark_status: face_artifact
                .as_ref()
                .and_then(|artifact| face_artifact_payload_string(&artifact.5, "landmark_status"))
                .unwrap_or_else(|| "not_applicable".into()),
            face_landmark_error: face_artifact
                .as_ref()
                .and_then(|artifact| face_artifact_payload_string(&artifact.5, "landmark_error")),
            faces,
            recommendation_reasons,
            human_decision,
        }))
    }

    fn similarity_group_for_asset(
        &self,
        asset_id: &MediaAssetId,
        limit: u32,
        offset: u32,
    ) -> Result<Option<SimilarityGroupView>> {
        let group = self.connection.query_row(
            "SELECT g.id, g.kind, g.representative_asset_id, g.grouping_method, g.grouping_version, g.similarity_confidence, g.time_proximity_seconds, g.visual_similarity
             FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id
             WHERE gm.media_asset_id = ?1 AND g.stale = 0
             ORDER BY CASE g.kind WHEN 'exact_duplicate_set' THEN 0 WHEN 'near_duplicate_set' THEN 1 WHEN 'burst' THEN 2 ELSE 3 END, g.created_at DESC, g.id ASC LIMIT 1",
            params![asset_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, f64>(5)?, row.get::<_, Option<i64>>(6)?, row.get::<_, Option<f64>>(7)?)),
        ).optional()?;
        let Some((
            id,
            kind,
            representative_asset_id,
            grouping_method,
            grouping_version,
            similarity_confidence,
            time_proximity_seconds,
            visual_similarity,
        )) = group
        else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT gm.media_asset_id, a.display_name, CASE WHEN preview.status = 'ready' THEN preview.id WHEN medium.status = 'ready' THEN medium.id WHEN small.status = 'ready' THEN small.id END, gm.similarity_confidence, gm.time_proximity_seconds, gm.is_representative
             FROM similarity_group_members gm JOIN media_assets a ON a.id = gm.media_asset_id
             LEFT JOIN preview_artifacts preview ON preview.media_asset_id = a.id AND preview.size_class = 'preview' AND preview.status = 'ready'
             LEFT JOIN preview_artifacts medium ON medium.media_asset_id = a.id AND medium.size_class = 'medium' AND medium.status = 'ready'
             LEFT JOIN preview_artifacts small ON small.media_asset_id = a.id AND small.size_class = 'small' AND small.status = 'ready'
             WHERE gm.group_id = ?1 ORDER BY gm.is_representative DESC, gm.ordinal ASC, gm.media_asset_id ASC LIMIT ?2 OFFSET ?3",
        )?;
        let members = statement
            .query_map(params![id.clone(), limit as i64, offset as i64], |row| {
                let asset: String = row.get(0)?;
                Ok(SimilarityGroupMemberView {
                    asset_id: asset.clone(),
                    filename: row.get(1)?,
                    medium_preview_url: row.get(2)?,
                    similarity_confidence: row.get(3)?,
                    time_proximity_seconds: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                    is_representative: row.get(5)?,
                    intelligence: IntelligenceSummary::default(),
                    faces: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Some(SimilarityGroupView {
            id,
            kind,
            representative_asset_id,
            grouping_method,
            grouping_version,
            similarity_confidence,
            time_proximity_seconds: time_proximity_seconds.map(|value| value as u64),
            visual_similarity,
            members,
        }))
    }

    fn start_or_resume_review_session(
        &self,
        project_id: &ProjectId,
        mode: &str,
        filter_context: Option<&str>,
    ) -> Result<ReviewSessionView> {
        validate_culling_mode(mode)?;
        if let Some(session) = self.connection.query_row(
            "SELECT id, project_id, started_at, ended_at, mode, last_asset_id, last_group_id, filter_context, photos_reviewed, sets_reviewed FROM review_sessions WHERE project_id = ?1 AND ended_at IS NULL ORDER BY started_at DESC, id DESC LIMIT 1",
            params![project_id.to_string()],
            review_session_view_from_row,
        ).optional()? {
            return Ok(session);
        }
        let now = timestamp(&Utc::now());
        let id = ReviewSessionId::new().to_string();
        self.connection.execute(
            "INSERT INTO review_sessions (id, project_id, started_at, mode, filter_context) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, project_id.to_string(), now, mode, filter_context],
        )?;
        self.connection.execute(
            "INSERT INTO review_events (id, project_id, session_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'REVIEW_STARTED', ?4, ?5)",
            params![ReviewEventId::new().to_string(), project_id.to_string(), id, json(&serde_json::json!({ "mode": mode, "filter": filter_context }))?, timestamp(&Utc::now())],
        )?;
        self.connection.query_row(
            "SELECT id, project_id, started_at, ended_at, mode, last_asset_id, last_group_id, filter_context, photos_reviewed, sets_reviewed FROM review_sessions WHERE id = ?1",
            params![id], review_session_view_from_row,
        ).map_err(Into::into)
    }

    fn culling_workspace(
        &self,
        project_id: &ProjectId,
        query: &CullingQuery,
    ) -> Result<CullingWorkspaceView> {
        validate_culling_mode(&query.mode)?;
        // Validate once before querying cached advisory rows. A corrupt active artifact is
        // marked unavailable and Smart Cull falls back to its existing generic evidence.
        let profile_id = self.ensure_default_studio_profile()?;
        let _ = self.active_studio_model(&profile_id)?;
        if let Some(moment_id) = &query.moment_id {
            if !self.moment_belongs_to_project(project_id, moment_id)? {
                return Err(PersistenceError::InvalidData(
                    "moment does not belong to the selected project".into(),
                ));
            }
        }
        let filter_sql = culling_filter_sql(&query.filter)?;
        let filter_context = query.moment_id.as_ref().map_or_else(
            || query.filter.clone(),
            |moment_id| format!("{};moment:{moment_id}", query.filter),
        );
        let session =
            self.start_or_resume_review_session(project_id, &query.mode, Some(&filter_context))?;
        let mode_queue_sql = "(?3 <> 'ai_review_queue' OR EXISTS (SELECT 1 FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 AND r.status = 'ready' AND r.label IN ('strong_candidate', 'strong_alternative', 'review', 'probable_duplicate', 'technical_issue')))";
        let group_sql = "(?3 <> 'similar_sets' OR EXISTS (SELECT 1 FROM similarity_group_members gm JOIN similarity_groups g ON g.id = gm.group_id WHERE gm.media_asset_id = a.id AND g.project_id = ?1 AND g.stale = 0 AND g.id = COALESCE(?2, (SELECT current_group.id FROM similarity_groups current_group WHERE current_group.project_id = ?1 AND current_group.stale = 0 ORDER BY current_group.created_at ASC, current_group.id ASC LIMIT 1))))";
        let sql = format!(
            "SELECT a.id FROM media_assets a LEFT JOIN media_decisions md ON md.media_asset_id = a.id AND md.project_id = ?1 WHERE a.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff') AND {mode_queue_sql} AND {group_sql} AND {filter_sql} AND (?4 IS NULL OR EXISTS (SELECT 1 FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.moment_id = ?4 AND mm.active = 1 AND mm.membership_state = 'member')) ORDER BY CASE WHEN ?3 = 'ai_review_queue' THEN CASE (SELECT r.label FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 ORDER BY r.generated_at DESC, r.id DESC LIMIT 1) WHEN 'technical_issue' THEN 0 WHEN 'review' THEN 1 WHEN 'probable_duplicate' THEN 2 WHEN 'strong_candidate' THEN 3 WHEN 'strong_alternative' THEN 4 ELSE 5 END ELSE 0 END, COALESCE(a.captured_at, a.created_at), a.id ASC LIMIT ?5 OFFSET ?6"
        );
        let bound_limit = query.limit.clamp(1, 120).saturating_add(1) as i64;
        let mut asset_ids = self
            .connection
            .prepare(&sql)?
            .query_map(
                params![
                    project_id.to_string(),
                    query.group_id.as_deref(),
                    query.mode,
                    query.moment_id.as_deref(),
                    bound_limit,
                    query.offset as i64
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = asset_ids.len() > query.limit.clamp(1, 120) as usize;
        asset_ids.truncate(query.limit.clamp(1, 120) as usize);
        let mut items = Vec::with_capacity(asset_ids.len());
        for raw_asset_id in asset_ids {
            let asset_id = parse_id!(MediaAssetId, &raw_asset_id)?;
            let Some(mut detail) = self.media_asset_detail(&asset_id)? else {
                continue;
            };
            if let Some(intelligence) = &detail.intelligence {
                detail.item.intelligence = intelligence.summary.clone();
            }
            let decision = culling_decision_for_asset(&self.connection, project_id, &raw_asset_id)?;
            let group = self.connection.query_row(
                "SELECT g.id, gm.is_representative, EXISTS(SELECT 1 FROM group_human_representatives h WHERE h.group_id = g.id AND h.media_asset_id = ?1) FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = ?1 AND g.stale = 0 ORDER BY g.created_at DESC, g.id DESC LIMIT 1",
                params![raw_asset_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)),
            ).optional()?;
            let relative_evidence = detail
                .intelligence
                .as_ref()
                .map(|intelligence| intelligence.recommendation_reasons.clone())
                .unwrap_or_default();
            items.push(CullingMediaRow {
                media: detail.item,
                decision,
                faces: detail
                    .intelligence
                    .as_ref()
                    .map(|intelligence| intelligence.faces.clone())
                    .unwrap_or_default(),
                relative_evidence,
                similarity_group_id: group.as_ref().map(|value| value.0.clone()),
                is_ai_representative: group.as_ref().is_some_and(|value| value.1),
                is_human_representative: group.as_ref().is_some_and(|value| value.2),
                studio_brain: studio_recommendation_for_asset(
                    &self.connection,
                    project_id,
                    &asset_id.to_string(),
                )?,
            });
        }
        Ok(CullingWorkspaceView {
            session,
            progress: culling_progress(&self.connection, project_id)?,
            groups: culling_group_summaries(&self.connection, project_id)?,
            items,
            has_more,
        })
    }

    fn culling_progress(&self, project_id: &ProjectId) -> Result<CullingProgress> {
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist for culling progress".into(),
            ));
        }
        culling_progress(&self.connection, project_id)
    }

    fn update_culling_decision(
        &self,
        project_id: &ProjectId,
        asset_id: &MediaAssetId,
        update: &CullingDecisionUpdate,
    ) -> Result<CullingDecisionView> {
        if !self.media_asset_belongs_to_project(asset_id, project_id)? {
            return Err(PersistenceError::InvalidData(
                "media asset does not belong to the selected project".into(),
            ));
        }
        if let Some(rating) = update.rating {
            if rating > 5 {
                return Err(PersistenceError::InvalidData(
                    "rating must be between 0 and 5".into(),
                ));
            }
        }
        if let Some(session_id) = &update.session_id {
            let valid: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_sessions WHERE id = ?1 AND project_id = ?2)",
                params![session_id, project_id.to_string()],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(PersistenceError::InvalidData(
                    "review session does not belong to the selected project".into(),
                ));
            }
        }
        // The authority record, its action-scoped Studio guard (triggered by the write below),
        // and every append-only decision-history event commit together. Explicit backfill can
        // therefore never observe a guard without the complete corresponding M5 history.
        let transaction = moment_write_transaction(&self.connection)?;
        let before = culling_decision_for_asset(&transaction, project_id, &asset_id.to_string())?;
        let source_signal_will_change = (update.clear_decision && before.decision.is_some())
            || (!update.clear_decision
                && update
                    .decision
                    .is_some_and(|decision| before.decision.as_deref() != Some(decision.as_str())))
            || update.rating.is_some_and(|rating| before.rating != rating)
            || update
                .starred
                .is_some_and(|starred| before.starred != starred);
        // M8.4 records an action-scoped source guard atomically inside the following human decision
        // write. Notes, flags, and no-op edits cannot create a marker because the SQL trigger
        // observes only a real decision/rating/star transition.
        let flags = update.flags.as_ref().map(json).transpose()?;
        let now = timestamp(&Utc::now());
        let decision_id = transaction
            .query_row(
                "SELECT id FROM media_decisions WHERE project_id = ?1 AND media_asset_id = ?2",
                params![project_id.to_string(), asset_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| MediaDecisionId::new().to_string());
        transaction.execute(
            "INSERT INTO media_decisions (id, project_id, media_asset_id, decision, rating, starred, note, flags_json, source, session_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, COALESCE(?5, 0), COALESCE(?6, 0), CASE WHEN ?7 IS NULL OR ?7 = '' THEN NULL ELSE ?7 END, COALESCE(?8, '[]'), 'human', ?9, ?10)
             ON CONFLICT(project_id, media_asset_id) DO UPDATE SET
               decision = CASE WHEN ?11 THEN NULL ELSE COALESCE(excluded.decision, media_decisions.decision) END,
               rating = COALESCE(?5, media_decisions.rating),
               starred = COALESCE(?6, media_decisions.starred),
               note = CASE WHEN ?7 IS NULL THEN media_decisions.note WHEN ?7 = '' THEN NULL ELSE ?7 END,
               flags_json = COALESCE(?8, media_decisions.flags_json),
               session_id = COALESCE(?9, media_decisions.session_id), updated_at = ?10",
            params![decision_id, project_id.to_string(), asset_id.to_string(), update.decision.map(CullingDecisionValue::as_str), update.rating.map(i64::from), update.starred.map(i64::from), update.note.as_deref(), flags, update.session_id.as_deref(), now, update.clear_decision],
        )?;
        let after = culling_decision_for_asset(&transaction, project_id, &asset_id.to_string())?;
        let changes = [
            (
                "DECISION_CHANGED",
                serde_json::json!(before.decision),
                serde_json::json!(after.decision),
                before.decision != after.decision,
            ),
            (
                "RATING_CHANGED",
                serde_json::json!(before.rating),
                serde_json::json!(after.rating),
                before.rating != after.rating,
            ),
            (
                "STAR_CHANGED",
                serde_json::json!(before.starred),
                serde_json::json!(after.starred),
                before.starred != after.starred,
            ),
            (
                "NOTE_CHANGED",
                serde_json::json!(before.note),
                serde_json::json!(after.note),
                before.note != after.note,
            ),
            (
                "FLAGS_CHANGED",
                serde_json::json!(before.flags),
                serde_json::json!(after.flags),
                before.flags != after.flags,
            ),
        ];
        let mut studio_sources = Vec::new();
        for (event_type, previous, current, changed) in changes {
            if !changed {
                continue;
            }
            let history_id = ReviewEventId::new().to_string();
            transaction.execute(
                "INSERT INTO decision_history (id, project_id, media_asset_id, decision_id, session_id, event_type, previous_value_json, current_value_json, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'human', ?9)",
                params![history_id, project_id.to_string(), asset_id.to_string(), decision_id, update.session_id.as_deref(), event_type, json(&previous)?, json(&current)?, now],
            )?;
            if matches!(
                event_type,
                "DECISION_CHANGED" | "RATING_CHANGED" | "STAR_CHANGED"
            ) {
                transaction.execute(
                    "INSERT INTO review_events (id, project_id, session_id, media_asset_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![ReviewEventId::new().to_string(), project_id.to_string(), update.session_id.as_deref(), asset_id.to_string(), event_type, json(&serde_json::json!({ "previous": previous, "current": current }))?, now],
                )?;
            }
            // Materialize only explicit human culling signals. Notes and flags deliberately do
            // not reach Studio Brain, and an unavailable/unknown recommendation is preserved as
            // such rather than reconstructed from a later UI state.
            if matches!(
                event_type,
                "DECISION_CHANGED" | "RATING_CHANGED" | "STAR_CHANGED"
            ) {
                if let Some(source) = studio_historical_row_from_live_decision(
                    &history_id,
                    &project_id.to_string(),
                    &asset_id.to_string(),
                    update.session_id.as_deref(),
                    event_type,
                    &current,
                    &now,
                ) {
                    studio_sources.push(source);
                }
            }
        }
        transaction.commit()?;
        if source_signal_will_change {
            if let Ok(profile_id) = self.ensure_default_studio_profile() {
                let mut source_materialized = true;
                for source in &studio_sources {
                    if materialize_live_studio_training_example(
                        &self.connection,
                        &profile_id,
                        source,
                    )
                    .is_err()
                    {
                        source_materialized = false;
                        mark_studio_source_capture_deferred(&self.connection, &profile_id);
                        break;
                    }
                }
                if source_materialized {
                    let _ = finish_studio_source_materialization(
                        &self.connection,
                        &profile_id,
                        "m5_culling",
                        &format!("{decision_id}:{now}"),
                    );
                }
            }
        }
        Ok(after)
    }

    fn update_review_position(
        &self,
        project_id: &ProjectId,
        session_id: &str,
        asset_id: Option<&MediaAssetId>,
        group_id: Option<&str>,
        mode: &str,
        filter_context: Option<&str>,
    ) -> Result<()> {
        validate_culling_mode(mode)?;
        if let Some(asset_id) = asset_id {
            if !self.media_asset_belongs_to_project(asset_id, project_id)? {
                return Err(PersistenceError::InvalidData(
                    "media asset does not belong to the selected project".into(),
                ));
            }
        }
        let changed = self.connection.execute(
            "UPDATE review_sessions SET last_asset_id = ?3, last_group_id = ?4, mode = ?5, filter_context = ?6 WHERE id = ?1 AND project_id = ?2 AND ended_at IS NULL",
            params![session_id, project_id.to_string(), asset_id.map(ToString::to_string), group_id, mode, filter_context],
        )?;
        if changed == 0 {
            return Err(PersistenceError::InvalidData(
                "active review session was not found for this project".into(),
            ));
        }
        Ok(())
    }

    fn set_group_human_representative(
        &self,
        project_id: &ProjectId,
        group_id: &str,
        asset_id: &MediaAssetId,
        session_id: Option<&str>,
    ) -> Result<()> {
        if !self.media_asset_belongs_to_project(asset_id, project_id)? {
            return Err(PersistenceError::InvalidData(
                "media asset does not belong to the selected project".into(),
            ));
        }
        let valid_member: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE g.id = ?1 AND g.project_id = ?2 AND g.stale = 0 AND gm.media_asset_id = ?3)",
            params![group_id, project_id.to_string(), asset_id.to_string()], |row| row.get(0),
        )?;
        if !valid_member {
            return Err(PersistenceError::InvalidData(
                "human representative must be a member of an active project similarity set".into(),
            ));
        }
        let prior = self
            .connection
            .query_row(
                "SELECT media_asset_id FROM group_human_representatives WHERE group_id = ?1",
                params![group_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if prior.as_deref() == Some(&asset_id.to_string()) {
            return Ok(());
        }
        // The representative, trigger-created action guard, immutable preference example, and
        // review event commit together so an explicit reconciliation cannot see a partial action.
        let now = timestamp(&Utc::now());
        let transaction = moment_write_transaction(&self.connection)?;
        transaction.execute(
            "INSERT INTO group_human_representatives (group_id, project_id, media_asset_id, session_id, selected_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(group_id) DO UPDATE SET media_asset_id = excluded.media_asset_id, session_id = excluded.session_id, selected_at = excluded.selected_at",
            params![group_id, project_id.to_string(), asset_id.to_string(), session_id, now],
        )?;
        let compared = transaction.prepare(
            "SELECT media_asset_id FROM similarity_group_members WHERE group_id = ?1 AND media_asset_id <> ?2 ORDER BY ordinal ASC, media_asset_id ASC"
        )?.query_map(params![group_id, asset_id.to_string()], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        // The snapshot contains only normalized technical/recommendation metadata. It intentionally
        // excludes byte data, face crops, source paths, and private image content.
        let evidence = preference_evidence_snapshot(&transaction, group_id, asset_id)?;
        let ai_recommendation = transaction.query_row(
            "SELECT json_object('assetId', g.representative_asset_id, 'recommendation', r.label, 'confidence', r.confidence) FROM similarity_groups g LEFT JOIN analysis_recommendations r ON r.media_asset_id = g.representative_asset_id AND r.stale = 0 WHERE g.id = ?1",
            params![group_id], |row| row.get::<_, String>(0),
        ).optional()?.unwrap_or_else(|| "{}".into());
        let preference_example_id = PreferenceExampleId::new().to_string();
        transaction.execute(
            "INSERT INTO preference_examples (id, project_id, similarity_group_id, chosen_asset_id, compared_asset_ids_json, technical_evidence_json, ai_recommendation_json, human_decision_context, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'group_representative', ?8)",
            params![preference_example_id, project_id.to_string(), group_id, asset_id.to_string(), json(&compared)?, evidence, ai_recommendation, now],
        )?;
        transaction.execute(
            "INSERT INTO review_events (id, project_id, session_id, media_asset_id, similarity_group_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'GROUP_REPRESENTATIVE_CHANGED', ?6, ?7)",
            params![ReviewEventId::new().to_string(), project_id.to_string(), session_id, asset_id.to_string(), group_id, json(&serde_json::json!({ "previousAssetId": prior, "chosenAssetId": asset_id.to_string() }))?, now],
        )?;
        transaction.commit()?;
        let mut source_materialized = false;
        let mut captured_profile_id = None;
        if let Ok(profile_id) = self.ensure_default_studio_profile() {
            captured_profile_id = Some(profile_id.clone());
            match materialize_live_similarity_representative(
                &self.connection,
                &profile_id,
                &preference_example_id,
                &project_id.to_string(),
                group_id,
                &asset_id.to_string(),
                &compared,
                &ai_recommendation,
                &now,
            ) {
                Ok(()) => source_materialized = true,
                Err(_) => mark_studio_source_capture_deferred(&self.connection, &profile_id),
            }
        }
        if source_materialized {
            if let Some(profile_id) = captured_profile_id.as_deref() {
                let _ = finish_studio_source_materialization(
                    &self.connection,
                    profile_id,
                    "similar_set_representative",
                    &format!("{group_id}:{now}"),
                );
            }
        }
        Ok(())
    }

    fn mark_group_review_complete(
        &self,
        project_id: &ProjectId,
        group_id: &str,
        session_id: Option<&str>,
    ) -> Result<()> {
        let valid_group: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM similarity_groups WHERE id = ?1 AND project_id = ?2 AND stale = 0)",
            params![group_id, project_id.to_string()], |row| row.get(0),
        )?;
        if !valid_group {
            return Err(PersistenceError::InvalidData(
                "similarity group does not belong to the selected project".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO group_review_completion (group_id, project_id, session_id, completed_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(group_id) DO UPDATE SET session_id = excluded.session_id, completed_at = excluded.completed_at",
            params![group_id, project_id.to_string(), session_id, timestamp(&Utc::now())],
        )?;
        Ok(())
    }

    fn finish_review_session(
        &self,
        project_id: &ProjectId,
        session_id: &str,
    ) -> Result<ReviewSessionView> {
        let now = timestamp(&Utc::now());
        let changed = self.connection.execute(
            "UPDATE review_sessions SET ended_at = ?3, photos_reviewed = ?4, sets_reviewed = ?5 WHERE id = ?1 AND project_id = ?2 AND ended_at IS NULL",
            params![session_id, project_id.to_string(), now, culling_progress(&self.connection, project_id)?.reviewed as i64, completed_group_count(&self.connection, project_id)? as i64],
        )?;
        if changed == 0 {
            return Err(PersistenceError::InvalidData(
                "active review session was not found for this project".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO review_events (id, project_id, session_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'REVIEW_FINISHED', ?4, ?5)",
            params![ReviewEventId::new().to_string(), project_id.to_string(), session_id, json(&serde_json::json!({ "nonDestructive": true }))?, now],
        )?;
        self.connection.query_row(
            "SELECT id, project_id, started_at, ended_at, mode, last_asset_id, last_group_id, filter_context, photos_reviewed, sets_reviewed FROM review_sessions WHERE id = ?1",
            params![session_id], review_session_view_from_row,
        ).map_err(Into::into)
    }

    fn culling_report(&self, project_id: &ProjectId) -> Result<Vec<CullingReportRow>> {
        let mut statement = self.connection.prepare(
            "SELECT a.display_name, a.id, md.decision, COALESCE(md.rating, 0), COALESCE(md.starred, 0),
                (SELECT r.label FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 ORDER BY r.generated_at DESC, r.id DESC LIMIT 1),
                (SELECT q.technical_quality_score FROM technical_quality q WHERE q.media_asset_id = a.id AND q.stale = 0 ORDER BY q.generated_at DESC LIMIT 1),
                (SELECT g.id FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = a.id AND g.stale = 0 ORDER BY g.created_at DESC LIMIT 1)
             FROM media_assets a LEFT JOIN media_decisions md ON md.media_asset_id = a.id AND md.project_id = ?1 WHERE a.project_id = ?1 ORDER BY lower(a.display_name), a.id"
        )?;
        let rows = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok(CullingReportRow {
                    filename: row.get(0)?,
                    media_asset_id: row.get(1)?,
                    decision: row.get(2)?,
                    rating: row.get::<_, i64>(3)? as u8,
                    starred: row.get(4)?,
                    ai_recommendation: row.get(5)?,
                    technical_score: row.get(6)?,
                    similarity_group_id: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn preference_examples(&self, project_id: &ProjectId) -> Result<Vec<PreferenceExampleView>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, similarity_group_id, chosen_asset_id, compared_asset_ids_json, technical_evidence_json, ai_recommendation_json, human_decision_context, created_at FROM preference_examples WHERE project_id = ?1 ORDER BY created_at ASC, id ASC"
        )?;
        let rows =
            statement
                .query_map(params![project_id.to_string()], |row| {
                    Ok(PreferenceExampleView {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        similarity_group_id: row.get(2)?,
                        chosen_asset_id: row.get(3)?,
                        compared_asset_ids: serde_json::from_str(&row.get::<_, String>(4)?)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    4,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        technical_evidence: serde_json::from_str(&row.get::<_, String>(5)?)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    5,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        ai_recommendation: serde_json::from_str(&row.get::<_, String>(6)?)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    6,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        human_decision_context: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn ensure_default_studio_profile(&self) -> Result<String> {
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at) VALUES (?1, 'local-default', 'Local Studio Profile', 'not_ready', 1, ?2, ?3, ?4, ?4)",
            params![Uuid::new_v4().to_string(), json(&serde_json::json!({"mode":"explicit_retrain","algorithm":"studio-linear-v1"}))?, json(&serde_json::json!({"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}))?, now],
        )?;
        let profile_id = transaction.query_row(
            "SELECT id FROM studio_profiles WHERE profile_key = 'local-default'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        transaction.commit()?;
        Ok(profile_id)
    }

    fn studio_brain_project_status(
        &self,
        project_id: &ProjectId,
    ) -> Result<StudioBrainProjectStatus> {
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist for Studio Brain status".into(),
            ));
        }
        let profile_id = self.ensure_default_studio_profile()?;
        let active_model = self.active_studio_model(&profile_id)?;
        let (profile_name, training_status, personalization_enabled, readiness_json, last_trained_at, last_error) = self.connection.query_row(
            "SELECT display_name, training_status, personalization_enabled, readiness_json, last_trained_at, last_error FROM studio_profiles WHERE id = ?1",
            params![profile_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?, row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, Option<String>>(5)?)),
        )?;
        let project_included: bool = self.connection.query_row(
            "SELECT COALESCE((SELECT included FROM project_training_preferences WHERE studio_profile_id = ?1 AND project_id = ?2), 1)",
            params![profile_id, project_id.to_string()],
            |row| row.get(0),
        )?;
        let (eligible_decision_count, keep_count, review_count, reject_count, rating_count, starred_count) = self.connection.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN event_type = 'DECISION_CHANGED' AND json_extract(current_value_json, '$') IN ('keep', 'review', 'reject') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN event_type = 'DECISION_CHANGED' AND json_extract(current_value_json, '$') = 'keep' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN event_type = 'DECISION_CHANGED' AND json_extract(current_value_json, '$') = 'review' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN event_type = 'DECISION_CHANGED' AND json_extract(current_value_json, '$') = 'reject' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN event_type = 'RATING_CHANGED' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN event_type = 'STAR_CHANGED' AND json_extract(current_value_json, '$') = 1 THEN 1 ELSE 0 END), 0)
             FROM decision_history history
             WHERE EXISTS (SELECT 1 FROM projects p WHERE p.id = history.project_id)
               AND COALESCE((SELECT pref.included FROM project_training_preferences pref WHERE pref.studio_profile_id = ?1 AND pref.project_id = history.project_id), 1) = 1",
            params![profile_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?)),
        )?;
        let representative_count: i64 = self.connection.query_row(
            "SELECT (SELECT COUNT(*) FROM preference_examples) + (SELECT COUNT(*) FROM moment_events WHERE event_type = 'MOMENT_REPRESENTATIVE_CHANGED')",
            [], |row| row.get(0),
        )?;
        let contributing_project_count: i64 = self.connection.query_row(
            "SELECT COUNT(DISTINCT history.project_id) FROM decision_history history WHERE history.event_type = 'DECISION_CHANGED' AND json_extract(history.current_value_json, '$') IN ('keep', 'review', 'reject') AND COALESCE((SELECT pref.included FROM project_training_preferences pref WHERE pref.studio_profile_id = ?1 AND pref.project_id = history.project_id), 1) = 1",
            params![profile_id], |row| row.get(0),
        )?;
        let active_model_version = active_model.map(|model| model.model_version);
        Ok(StudioBrainProjectStatus {
            profile_id,
            profile_name,
            training_status,
            personalization_enabled,
            project_included,
            eligible_decision_count: eligible_decision_count as u64,
            keep_count: keep_count as u64,
            review_count: review_count as u64,
            reject_count: reject_count as u64,
            rating_count: rating_count as u64,
            starred_count: starred_count as u64,
            representative_count: representative_count as u64,
            contributing_project_count: contributing_project_count as u64,
            active_model_version,
            last_trained_at,
            readiness: from_json(&readiness_json)?,
            last_error,
        })
    }

    fn set_project_training_included(
        &self,
        profile_id: &str,
        project_id: &ProjectId,
        included: bool,
    ) -> Result<()> {
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT INTO project_training_preferences (studio_profile_id, project_id, included, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(studio_profile_id, project_id) DO UPDATE SET included = excluded.included, updated_at = excluded.updated_at",
            params![profile_id, project_id.to_string(), included, now],
        )?;
        insert_studio_event(
            &transaction,
            profile_id,
            Some(&project_id.to_string()),
            None,
            None,
            if included {
                "PROJECT_TRAINING_INCLUDED"
            } else {
                "PROJECT_TRAINING_EXCLUDED"
            },
            &serde_json::json!({"included": included}),
            &now,
        )?;
        // The underlying human decisions remain untouched. An active derived model is now
        // based on a different eligible-source set, so it must ask for an explicit update.
        mark_studio_profile_stale_if_active(&transaction, profile_id, &now)?;
        transaction.commit()?;
        Ok(())
    }

    fn set_studio_personalization_enabled(&self, profile_id: &str, enabled: bool) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let now = timestamp(&Utc::now());
        transaction.execute(
            "UPDATE studio_profiles SET personalization_enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![profile_id, enabled, now],
        )?;
        insert_studio_event(
            &transaction,
            profile_id,
            None,
            None,
            None,
            "PERSONALIZATION_TOGGLED",
            &serde_json::json!({"enabled": enabled}),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn update_studio_profile_training_state(
        &self,
        profile_id: &str,
        status: &str,
        readiness: &serde_json::Value,
        error_message: Option<&str>,
    ) -> Result<()> {
        if !matches!(
            status,
            "not_ready" | "learning" | "ready" | "stale" | "error"
        ) {
            return Err(PersistenceError::InvalidData(
                "invalid Studio Brain profile status".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let now = timestamp(&Utc::now());
        transaction.execute(
            "UPDATE studio_profiles SET training_status = ?2, readiness_json = ?3, last_error = ?4, updated_at = ?5 WHERE id = ?1",
            params![profile_id, status, json(readiness)?, error_message, now],
        )?;
        if status == "error" {
            insert_studio_event(
                &transaction,
                profile_id,
                None,
                None,
                None,
                "STUDIO_TRAINING_FAILED",
                &serde_json::json!({"message":"A local Studio Brain candidate was not activated."}),
                &now,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn materialize_historical_studio_training_examples(&self, profile_id: &str) -> Result<u64> {
        self.ensure_default_studio_profile()?;
        // Hold the write reservation while both reading append-only authority records and
        // clearing a previous pending marker. A new human action therefore lands either wholly
        // before this reconciliation (and is included) or wholly after it (and leaves a new
        // pending marker that the following snapshot refuses). No source can be missed between
        // a successful reconciliation and a candidate activation.
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let histories = studio_historical_decision_rows(&transaction)?;
        let overrides = studio_historical_override_rows(&transaction)?;
        let representatives = studio_historical_similarity_rows(&transaction)?;
        let moment_representatives = studio_historical_moment_rows(&transaction)?;
        // Opt-out applies at source creation as well as at read time. Historical backfill is
        // explicit and idempotent, but it must not silently materialize new preference records
        // from a project the photographer has currently excluded.
        let mut included_decision_rows = Vec::new();
        for row in histories.into_iter().chain(overrides) {
            if studio_project_training_is_included(&transaction, profile_id, &row.project_id)? {
                included_decision_rows.push(row);
            }
        }
        let mut included_representatives = Vec::new();
        for row in representatives {
            if studio_project_training_is_included(&transaction, profile_id, &row.project_id)? {
                included_representatives.push(row);
            }
        }
        let mut included_moment_representatives = Vec::new();
        for row in moment_representatives {
            if studio_project_training_is_included(&transaction, profile_id, &row.project_id)? {
                included_moment_representatives.push(row);
            }
        }
        let now = timestamp(&Utc::now());
        let mut created = 0_u64;
        for row in included_decision_rows {
            let snapshot = row
                .media_asset_id
                .as_deref()
                .map(|asset| studio_feature_snapshot(&transaction, &row.project_id, asset))
                .transpose()?
                .unwrap_or_else(|| serde_json::json!({"availability":"unavailable"}));
            let row = studio_historical_row_with_snapshot_context(row, &snapshot);
            created += insert_studio_training_example(
                &transaction,
                profile_id,
                &row,
                &snapshot,
                &serde_json::json!({"status":"unavailable_historical"}),
                "unknown",
                "historical_backfill",
                &now,
            )? as u64;
        }
        for row in included_representatives {
            let snapshot =
                studio_feature_snapshot(&transaction, &row.project_id, &row.chosen_asset_id)?;
            let example = StudioHistoricalRow {
                source_kind: "similar_set_representative".into(),
                source_record_id: row.id.clone(),
                project_id: row.project_id.clone(),
                media_asset_id: Some(row.chosen_asset_id.clone()),
                decision_type: "similar_set_representative".into(),
                decision_value: None,
                occurred_at: row.created_at.clone(),
                review_session_id: None,
                similarity_group_id: Some(row.group_id.clone()),
                moment_id: None,
            };
            if insert_studio_training_example(
                &transaction,
                profile_id,
                &example,
                &snapshot,
                &row.generic_recommendation_json,
                "unknown",
                "historical_backfill",
                &now,
            )? {
                let example_id: String = transaction.query_row("SELECT id FROM studio_training_examples WHERE studio_profile_id = ?1 AND source_kind = ?2 AND source_record_id = ?3", params![profile_id, example.source_kind, example.source_record_id], |db_row| db_row.get(0))?;
                transaction.execute("INSERT OR IGNORE INTO studio_training_example_references (studio_training_example_id, role, media_asset_id) VALUES (?1, 'chosen', ?2)", params![example_id, row.chosen_asset_id])?;
                for alternative in row.alternative_asset_ids {
                    transaction.execute("INSERT OR IGNORE INTO studio_training_example_references (studio_training_example_id, role, media_asset_id) VALUES (?1, 'alternative', ?2)", params![example_id, alternative])?;
                }
                created += 1;
            }
        }
        for row in included_moment_representatives {
            let snapshot = studio_feature_snapshot(&transaction, &row.project_id, &row.asset_id)?;
            let example = StudioHistoricalRow {
                source_kind: "moment_representative".into(),
                source_record_id: row.id,
                project_id: row.project_id,
                media_asset_id: Some(row.asset_id),
                decision_type: "moment_representative".into(),
                decision_value: None,
                occurred_at: row.created_at,
                review_session_id: None,
                similarity_group_id: None,
                moment_id: Some(row.moment_id),
            };
            created += insert_studio_training_example(
                &transaction,
                profile_id,
                &example,
                &snapshot,
                &serde_json::json!({"status":"unavailable_historical"}),
                "unknown",
                "historical_backfill",
                &now,
            )? as u64;
        }
        // Every action-scoped guard visible to this write-reserved transaction has complete
        // durable authority history and was just reconciled into immutable source history.
        // An action that arrives after commit creates its own new guard, so it cannot be erased
        // by this reconciliation.
        transaction.execute(
            "DELETE FROM studio_source_materialization_guards WHERE studio_profile_id = ?1",
            params![profile_id],
        )?;
        transaction.commit()?;
        Ok(created)
    }

    fn studio_training_examples(
        &self,
        profile_id: &str,
    ) -> Result<Vec<StudioTrainingExampleRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT e.id, e.studio_profile_id, e.project_id, e.media_asset_id, e.source_kind, e.source_record_id, e.decision_type, e.decision_value, e.occurred_at, e.review_session_id, e.similarity_group_id, e.moment_id, e.generic_recommendation_json, e.studio_recommendation_id_at_decision, e.recommendation_shown, e.provenance, e.feature_schema_version, e.feature_snapshot_json, (e.training_eligible = 1 AND e.feature_schema_version = ?2 AND COALESCE(pref.included, 1) = 1 AND exclusion.studio_training_example_id IS NULL), e.created_at
             FROM studio_training_examples e
             LEFT JOIN project_training_preferences pref ON pref.studio_profile_id = e.studio_profile_id AND pref.project_id = e.project_id
             LEFT JOIN decision_training_exclusions exclusion ON exclusion.studio_profile_id = e.studio_profile_id AND exclusion.studio_training_example_id = e.id
             WHERE e.studio_profile_id = ?1 ORDER BY e.occurred_at ASC, e.id ASC"
        )?;
        let rows = statement
            .query_map(
                params![profile_id, STUDIO_FEATURE_SCHEMA_VERSION],
                studio_training_example_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn studio_training_source_revision(&self, profile_id: &str) -> Result<u64> {
        Ok(self.studio_training_source_state(profile_id)?.revision)
    }

    fn studio_training_source_state(&self, profile_id: &str) -> Result<StudioTrainingSourceState> {
        let (revision, pending): (i64, bool) = self.connection.query_row(
            "SELECT source_revision,
                    EXISTS(SELECT 1 FROM studio_source_materialization_guards guard WHERE guard.studio_profile_id = studio_profiles.id)
             FROM studio_profiles WHERE id = ?1",
            params![profile_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(StudioTrainingSourceState {
            revision: u64::try_from(revision).map_err(|_| {
                PersistenceError::InvalidData("Studio training source revision is invalid".into())
            })?,
            materialization_pending: pending,
        })
    }

    fn studio_feature_candidates(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<StudioFeatureCandidate>> {
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist for Studio Brain features".into(),
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT id FROM media_assets WHERE project_id = ?1 AND media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff') ORDER BY COALESCE(captured_at, created_at), id",
        )?;
        let asset_ids = statement
            .query_map(params![project_id.to_string()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut candidates = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            let snapshot =
                studio_feature_snapshot(&self.connection, &project_id.to_string(), &asset_id)?;
            let generic = studio_generic_recommendation_snapshot(&self.connection, &asset_id)?;
            let feature_fingerprint = studio_feature_fingerprint(&snapshot)?;
            candidates.push(StudioFeatureCandidate {
                project_id: project_id.to_string(),
                media_asset_id: asset_id,
                similarity_group_id: snapshot
                    .get("similarityGroupId")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                moment_id: snapshot
                    .get("momentId")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                feature_snapshot_json: snapshot,
                feature_fingerprint,
                generic_recommendation_json: generic,
            });
        }
        Ok(candidates)
    }

    fn studio_pairwise_preferences(
        &self,
        profile_id: &str,
    ) -> Result<Vec<StudioPairwisePreferenceRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT example.id, example.project_id, example.similarity_group_id,
                    chosen.media_asset_id, alternative.media_asset_id, example.occurred_at,
                    example.feature_snapshot_json, preference.technical_evidence_json,
                    preference.ai_recommendation_json,
                    (example.training_eligible = 1
                     AND COALESCE(project_preference.included, 1) = 1
                     AND exclusion.studio_training_example_id IS NULL)
             FROM studio_training_examples example
             JOIN studio_training_example_references chosen
               ON chosen.studio_training_example_id = example.id AND chosen.role = 'chosen'
             JOIN studio_training_example_references alternative
               ON alternative.studio_training_example_id = example.id AND alternative.role = 'alternative'
             JOIN preference_examples preference
               ON example.source_kind = 'similar_set_representative'
              AND preference.id = example.source_record_id
             LEFT JOIN project_training_preferences project_preference
               ON project_preference.studio_profile_id = example.studio_profile_id
              AND project_preference.project_id = example.project_id
             LEFT JOIN decision_training_exclusions exclusion
               ON exclusion.studio_profile_id = example.studio_profile_id
              AND exclusion.studio_training_example_id = example.id
             WHERE example.studio_profile_id = ?1
               AND example.feature_schema_version = ?2
               AND example.source_kind = 'similar_set_representative'
               AND example.similarity_group_id IS NOT NULL
             ORDER BY example.occurred_at ASC, example.id ASC, alternative.media_asset_id ASC",
        )?;
        let rows = statement
            .query_map(params![profile_id, STUDIO_FEATURE_SCHEMA_VERSION], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    json_value_from_row(row, 6)?,
                    json_value_from_row(row, 7)?,
                    json_value_from_row(row, 8)?,
                    row.get::<_, bool>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut preferences = Vec::with_capacity(rows.len());
        for (
            example_id,
            project_id,
            similarity_group_id,
            chosen_asset_id,
            alternative_asset_id,
            occurred_at,
            chosen_feature_snapshot_json,
            technical_evidence_json,
            generic_recommendation_json,
            training_eligible,
        ) in rows
        {
            let Some(alternative_feature_snapshot_json) = studio_pairwise_snapshot_from_evidence(
                &technical_evidence_json,
                &generic_recommendation_json,
                &similarity_group_id,
                &alternative_asset_id,
            ) else {
                // Historic M5 evidence can be incomplete. It is safer to abstain from a
                // relative comparison than to reconstruct a missing alternative from current
                // mutable catalog state.
                continue;
            };
            preferences.push(StudioPairwisePreferenceRecord {
                id: format!("{example_id}:{alternative_asset_id}"),
                project_id,
                similarity_group_id,
                chosen_asset_id,
                alternative_asset_id,
                occurred_at,
                chosen_feature_snapshot_json,
                alternative_feature_snapshot_json,
                training_eligible,
            });
        }
        Ok(preferences)
    }

    fn set_studio_training_example_excluded(
        &self,
        profile_id: &str,
        example_id: &str,
        excluded: bool,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let valid: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM studio_training_examples WHERE id = ?1 AND studio_profile_id = ?2)", params![example_id, profile_id], |row| row.get(0))?;
        if !valid {
            return Err(PersistenceError::InvalidData(
                "training example does not belong to the selected Studio Profile".into(),
            ));
        }
        let now = timestamp(&Utc::now());
        if excluded {
            transaction.execute("INSERT OR IGNORE INTO decision_training_exclusions (studio_profile_id, studio_training_example_id, excluded_at) VALUES (?1, ?2, ?3)", params![profile_id, example_id, now])?;
        } else {
            transaction.execute("DELETE FROM decision_training_exclusions WHERE studio_profile_id = ?1 AND studio_training_example_id = ?2", params![profile_id, example_id])?;
        }
        insert_studio_event(
            &transaction,
            profile_id,
            None,
            None,
            None,
            if excluded {
                "DECISION_TRAINING_EXCLUDED"
            } else {
                "DECISION_TRAINING_INCLUDED"
            },
            &serde_json::json!({"trainingExampleId": example_id, "excluded": excluded}),
            &now,
        )?;
        mark_studio_profile_stale_if_active(&transaction, profile_id, &now)?;
        transaction.commit()?;
        Ok(())
    }

    fn create_studio_training_run(&self, record: &StudioTrainingRunRecord) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, &record.profile_id)?;
        transaction.execute(
            "INSERT INTO studio_training_runs (id, studio_profile_id, background_job_id, algorithm, algorithm_version, feature_schema_version, parameters_json, snapshot_hash, snapshot_count, previous_active_model_id, state, error_message, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![record.id, record.profile_id, record.background_job_id, record.algorithm, record.algorithm_version, record.feature_schema_version, json(&record.parameters_json)?, record.snapshot_hash, record.snapshot_count as i64, record.previous_active_model_id, record.state, record.error_message, record.created_at, record.updated_at, record.finished_at],
        )?;
        insert_studio_event(
            &transaction,
            &record.profile_id,
            None,
            Some(&record.id),
            None,
            "STUDIO_TRAINING_STARTED",
            &serde_json::json!({"algorithm": record.algorithm, "featureSchemaVersion": record.feature_schema_version}),
            &record.created_at,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn update_studio_training_run(&self, record: &StudioTrainingRunRecord) -> Result<()> {
        self.connection.execute(
            "UPDATE studio_training_runs SET state = ?2, error_message = ?3, parameters_json = ?4, snapshot_hash = ?5, snapshot_count = ?6, updated_at = ?7, finished_at = ?8 WHERE id = ?1 AND studio_profile_id = ?9",
            params![record.id, record.state, record.error_message, json(&record.parameters_json)?, record.snapshot_hash, record.snapshot_count as i64, record.updated_at, record.finished_at, record.profile_id],
        )?;
        Ok(())
    }

    fn store_studio_training_snapshot(
        &self,
        run_id: &str,
        example_id: &str,
        split: &str,
        feature_snapshot_json: &serde_json::Value,
        label: Option<&str>,
    ) -> Result<()> {
        if !matches!(split, "train" | "validation" | "holdout" | "excluded") {
            return Err(PersistenceError::InvalidData(
                "unsupported Studio training split".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO studio_training_run_examples (studio_training_run_id, studio_training_example_id, split, feature_snapshot_json, label) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(studio_training_run_id, studio_training_example_id) DO UPDATE SET split = excluded.split, feature_snapshot_json = excluded.feature_snapshot_json, label = excluded.label",
            params![run_id, example_id, split, json(feature_snapshot_json)?, label],
        )?;
        Ok(())
    }

    fn store_studio_model(&self, record: &StudioModelRecord) -> Result<()> {
        if record.checksum.trim().is_empty() {
            return Err(PersistenceError::InvalidData(
                "Studio model checksum is required".into(),
            ));
        }
        if record.state != "candidate" {
            return Err(PersistenceError::InvalidData(
                "Studio model storage accepts only a validated candidate".into(),
            ));
        }
        verify_studio_model_artifact(record)?;
        let transaction = moment_write_transaction(&self.connection)?;
        transaction.execute(
            "INSERT INTO studio_models (id, studio_profile_id, studio_training_run_id, algorithm, model_version, feature_schema_version, artifact_json, checksum, artifact_size_bytes, state, metrics_json, created_at, activated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![record.id, record.profile_id, record.training_run_id, record.algorithm, record.model_version, record.feature_schema_version, json(&record.artifact_json)?, record.checksum, record.artifact_size_bytes as i64, record.state, json(&record.metrics_json)?, record.created_at, record.activated_at],
        )?;
        store_studio_model_metric_rows(&transaction, record)?;
        transaction.commit()?;
        Ok(())
    }

    fn activate_studio_model(
        &self,
        profile_id: &str,
        model_id: &str,
        expected_source_revision: u64,
        completed_job: &BackgroundJob,
    ) -> Result<StudioModelActivationOutcome> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let candidate = transaction.query_row(
            "SELECT id, studio_profile_id, studio_training_run_id, algorithm, model_version, feature_schema_version, artifact_json, checksum, artifact_size_bytes, state, metrics_json, created_at, activated_at FROM studio_models WHERE id = ?1 AND studio_profile_id = ?2 AND state = 'candidate'",
            params![model_id, profile_id],
            studio_model_from_row,
        ).optional()?;
        let Some(candidate) = candidate else {
            return Err(PersistenceError::InvalidData(
                "Studio candidate model is missing, invalid, or does not belong to this profile"
                    .into(),
            ));
        };
        let (current_source_revision, source_materialization_pending): (i64, bool) = transaction.query_row(
            "SELECT source_revision,
                    EXISTS(SELECT 1 FROM studio_source_materialization_guards guard WHERE guard.studio_profile_id = studio_profiles.id)
             FROM studio_profiles WHERE id = ?1",
            params![profile_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if source_materialization_pending
            || current_source_revision < 0
            || current_source_revision as u64 != expected_source_revision
        {
            // This transaction holds the write lock. A source committed immediately before it
            // is observed here; one that begins immediately after it will mark the new model
            // stale after activation. Neither path can silently omit human evidence.
            return Ok(StudioModelActivationOutcome::SourceSnapshotStale);
        }
        let expected_job_id: String = transaction.query_row(
            "SELECT background_job_id FROM studio_training_runs WHERE id = ?1",
            params![candidate.training_run_id],
            |row| row.get(0),
        )?;
        if expected_job_id != completed_job.id.to_string()
            || completed_job.state != WorkflowRunState::Completed
        {
            return Err(PersistenceError::InvalidData(
                "Studio activation must complete its own terminal background job".into(),
            ));
        }
        if let Err(error) = verify_studio_model_artifact(&candidate) {
            let now = timestamp(&Utc::now());
            transaction.execute(
                "UPDATE studio_models SET state = 'invalid' WHERE id = ?1 AND state = 'candidate'",
                params![model_id],
            )?;
            transaction.execute(
                "UPDATE studio_training_runs SET state = 'failed', error_message = ?2, updated_at = ?3, finished_at = ?3 WHERE id = ?1",
                params![candidate.training_run_id, "A local Studio Brain candidate could not be validated before activation.", now],
            )?;
            insert_studio_event(
                &transaction,
                profile_id,
                None,
                Some(&candidate.training_run_id),
                Some(model_id),
                "STUDIO_TRAINING_FAILED",
                &serde_json::json!({"reason":"candidate_artifact_validation_failed"}),
                &now,
            )?;
            transaction.commit()?;
            return Err(error);
        }
        let now = timestamp(&Utc::now());
        transaction.execute("UPDATE studio_models SET state = 'previous' WHERE studio_profile_id = ?1 AND state = 'active'", params![profile_id])?;
        transaction.execute("UPDATE studio_models SET state = 'active', activated_at = ?3 WHERE id = ?1 AND studio_profile_id = ?2 AND state = 'candidate'", params![model_id, profile_id, now])?;
        // New candidate recommendations were persisted while this model was still a candidate,
        // so no intermediate state can hide a previous active model. This single transaction
        // switches model visibility and retires prior advisory rows together.
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE studio_profile_id = ?1 AND studio_model_id <> ?2 AND stale = 0",
            params![profile_id, model_id],
        )?;
        let run_id = candidate.training_run_id;
        transaction.execute("UPDATE studio_training_runs SET state = 'completed', updated_at = ?2, finished_at = ?2 WHERE id = ?1", params![run_id, now])?;
        transaction.execute("UPDATE studio_profiles SET training_status = 'ready', last_trained_at = ?2, last_error = NULL, updated_at = ?2 WHERE id = ?1", params![profile_id, now])?;
        let updated_job = transaction.execute(
            "UPDATE background_jobs SET state_json = ?2, stage_json = ?3, items_completed = ?4, items_total = ?5, files_discovered = ?6, files_processed = ?7, error_count = ?8, error_message = ?9, resume_metadata_json = ?10, updated_at = ?11, finished_at = ?12 WHERE id = ?1",
            params![completed_job.id.to_string(), json(&completed_job.state)?, json(&completed_job.stage)?, completed_job.items_completed as i64, completed_job.items_total.map(|value| value as i64), completed_job.files_discovered as i64, completed_job.files_processed as i64, completed_job.error_count as i64, completed_job.error_message, completed_job.resume_metadata.as_ref().map(serde_json::to_string).transpose()?, timestamp(&completed_job.updated_at), optional_timestamp(&completed_job.finished_at)],
        )?;
        if updated_job != 1 {
            return Err(PersistenceError::InvalidData(
                "Studio activation background job is missing".into(),
            ));
        }
        insert_studio_event(
            &transaction,
            profile_id,
            None,
            Some(&run_id),
            Some(model_id),
            "STUDIO_MODEL_ACTIVATED",
            &serde_json::json!({"modelId": model_id}),
            &now,
        )?;
        insert_studio_event(
            &transaction,
            profile_id,
            None,
            Some(&run_id),
            Some(model_id),
            "STUDIO_TRAINING_COMPLETED",
            &serde_json::json!({"modelId": model_id}),
            &now,
        )?;
        transaction.commit()?;
        Ok(StudioModelActivationOutcome::Activated)
    }

    fn active_studio_model(&self, profile_id: &str) -> Result<Option<StudioModelRecord>> {
        let model = self.connection.query_row(
            "SELECT id, studio_profile_id, studio_training_run_id, algorithm, model_version, feature_schema_version, artifact_json, checksum, artifact_size_bytes, state, metrics_json, created_at, activated_at FROM studio_models WHERE studio_profile_id = ?1 AND state = 'active' ORDER BY activated_at DESC, id DESC LIMIT 1",
            params![profile_id], studio_model_from_row,
        ).optional()?;
        if let Some(model) = model {
            if verify_studio_model_artifact(&model).is_err() {
                invalidate_corrupt_active_studio_model(&self.connection, &model)?;
                return Ok(None);
            }
            return Ok(Some(model));
        }
        Ok(None)
    }

    fn latest_studio_training_run(
        &self,
        profile_id: &str,
    ) -> Result<Option<StudioTrainingRunRecord>> {
        self.connection.query_row(
            "SELECT id, studio_profile_id, background_job_id, algorithm, algorithm_version, feature_schema_version, parameters_json, snapshot_hash, snapshot_count, previous_active_model_id, state, error_message, created_at, updated_at, finished_at FROM studio_training_runs WHERE studio_profile_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT 1",
            params![profile_id], studio_training_run_from_row,
        ).optional().map_err(Into::into)
    }

    fn replace_studio_recommendations(
        &self,
        profile_id: &str,
        model_id: &str,
        project_id: &ProjectId,
        recommendations: &[StudioRecommendationRecord],
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let model_state: Option<String> = transaction
            .query_row(
                "SELECT state FROM studio_models WHERE id = ?1 AND studio_profile_id = ?2",
                params![model_id, profile_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(model_state) = model_state else {
            return Err(PersistenceError::InvalidData(
                "Studio model does not belong to this profile".into(),
            ));
        };
        if !matches!(model_state.as_str(), "candidate" | "active") {
            return Err(PersistenceError::InvalidData(
                "cannot write recommendations for an invalid Studio model".into(),
            ));
        }
        if model_state == "active" {
            transaction.execute("UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE studio_profile_id = ?1 AND project_id = ?2 AND studio_model_id <> ?3 AND stale = 0", params![profile_id, project_id.to_string(), model_id])?;
        }
        for record in recommendations {
            if record.profile_id != profile_id
                || record.model_id != model_id
                || record.project_id != project_id.to_string()
            {
                return Err(PersistenceError::InvalidData(
                    "Studio recommendation scope mismatch".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO studio_recommendations (id, studio_profile_id, studio_model_id, project_id, media_asset_id, feature_schema_version, feature_fingerprint, recommendation, confidence_band, explanation_json, generic_recommendation_json, agreement, status, stale, generated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'ready', 0, ?13) ON CONFLICT(studio_model_id, media_asset_id, feature_fingerprint) DO UPDATE SET recommendation = excluded.recommendation, confidence_band = excluded.confidence_band, explanation_json = excluded.explanation_json, generic_recommendation_json = excluded.generic_recommendation_json, agreement = excluded.agreement, status = 'ready', stale = 0, generated_at = excluded.generated_at",
                params![record.id, record.profile_id, record.model_id, record.project_id, record.media_asset_id, record.feature_schema_version, record.feature_fingerprint, record.recommendation, record.confidence_band, json(&record.explanation_json)?, json(&record.generic_recommendation_json)?, record.agreement, record.generated_at],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn reset_studio_personalization(&self, profile_id: &str) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_studio_profile_exists(&transaction, profile_id)?;
        let now = timestamp(&Utc::now());
        transaction.execute("UPDATE studio_models SET state = 'reset' WHERE studio_profile_id = ?1 AND state IN ('active', 'previous', 'candidate')", params![profile_id])?;
        transaction.execute("UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE studio_profile_id = ?1 AND stale = 0", params![profile_id])?;
        let example_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM studio_training_examples WHERE studio_profile_id = ?1",
            params![profile_id],
            |row| row.get(0),
        )?;
        transaction.execute("UPDATE studio_profiles SET training_status = ?2, last_trained_at = NULL, last_error = NULL, readiness_json = ?3, updated_at = ?4 WHERE id = ?1", params![profile_id, if example_count == 0 { "not_ready" } else { "learning" }, json(&serde_json::json!({"state": if example_count == 0 { "not_ready" } else { "learning" }, "reasons":["Personalized model artifacts were reset. Human decisions remain available for an explicit local retrain."]}))?, now])?;
        insert_studio_event(
            &transaction,
            profile_id,
            None,
            None,
            None,
            "PERSONALIZATION_RESET",
            &serde_json::json!({"humanDecisionsPreserved": true}),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn recover_interrupted_studio_training(&self) -> Result<u64> {
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        let changed = transaction.execute("UPDATE studio_training_runs SET state = 'interrupted', error_message = COALESCE(error_message, 'Studio Brain training was interrupted before activation; any previous personalized model remains active.'), updated_at = ?1, finished_at = ?1 WHERE state IN ('queued', 'training', 'evaluating', 'persisting')", params![now])?;
        transaction.execute("UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"studio_training\"', error_message = COALESCE(error_message, 'Studio Brain training was interrupted before activation; any previous personalized model remains active.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"studio-training\"%'", params![now])?;
        transaction.commit()?;
        Ok(changed as u64)
    }

    fn production_workspace(&self, project_id: &ProjectId) -> Result<ProductionWorkspaceView> {
        let mut plans_statement = self.connection.prepare(
            "SELECT id, project_id, name, plan_type, status, selection_rules_json, organization_json, filename_strategy_json, destination_path, destination_reserve_bytes, estimated_file_count, estimated_bytes, current_manifest_id, created_at, updated_at FROM production_plans WHERE project_id = ?1 ORDER BY updated_at DESC, id ASC",
        )?;
        let plans = plans_statement
            .query_map(params![project_id.to_string()], production_plan_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut collections_statement = self.connection.prepare(
            "SELECT collection.id, collection.project_id, collection.name, collection.kind, collection.rules_json, collection.created_at, collection.updated_at, COUNT(member.media_asset_id) FROM virtual_collections collection LEFT JOIN virtual_collection_members member ON member.virtual_collection_id = collection.id WHERE collection.project_id = ?1 GROUP BY collection.id ORDER BY collection.updated_at DESC, collection.id ASC",
        )?;
        let collections = collections_statement
            .query_map(params![project_id.to_string()], virtual_collection_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut jobs_statement = self.connection.prepare(
            "SELECT job.id, job.production_plan_id, job.export_manifest_id, job.background_job_id, job.state, job.destination_path, job.items_total, job.items_completed, job.verified_count, job.skipped_identical_count, job.failed_count, job.verified_bytes, job.created_at, job.updated_at, job.finished_at, job.error_message
             FROM export_jobs job
             JOIN export_manifests manifest ON manifest.id = job.export_manifest_id
             WHERE manifest.project_id = ?1
             ORDER BY job.updated_at DESC, job.id DESC
             LIMIT 40",
        )?;
        let recent_exports = jobs_statement
            .query_map(params![project_id.to_string()], export_job_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ProductionWorkspaceView {
            plans,
            collections,
            recent_exports,
        })
    }

    fn create_production_plan(
        &self,
        project_id: &ProjectId,
        input: &ProductionPlanInput,
    ) -> Result<ProductionPlanRecord> {
        let name = valid_production_name(&input.name)?;
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        let id = Uuid::new_v4().to_string();
        let reserve = delivery_brain::DEFAULT_DESTINATION_RESERVE_BYTES;
        transaction.execute(
            "INSERT INTO production_plans (id, project_id, name, plan_type, status, selection_rules_json, organization_json, filename_strategy_json, destination_path, destination_reserve_bytes, estimated_file_count, estimated_bytes, current_manifest_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'draft', ?5, ?6, ?7, NULL, ?8, 0, 0, NULL, ?9, ?9)",
            params![id, project_id.to_string(), name, production_plan_type_name(input.plan_type), json(&input.selection_rules)?, json(&input.organization)?, json(&input.filename_strategy)?, reserve as i64, now],
        )?;
        let configuration = serde_json::json!({
            "name": name,
            "planType": input.plan_type,
            "selectionRules": input.selection_rules,
            "organization": input.organization,
            "filenameStrategy": input.filename_strategy,
            "destinationReserveBytes": reserve,
        });
        transaction.execute(
            "INSERT INTO production_plan_versions (id, production_plan_id, version, configuration_json, created_at) VALUES (?1, ?2, 1, ?3, ?4)",
            params![Uuid::new_v4().to_string(), id, json(&configuration)?, now],
        )?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            Some(&id),
            None,
            None,
            "PRODUCTION_PLAN_CREATED",
            &serde_json::json!({"planType": input.plan_type}),
            &now,
        )?;
        transaction.commit()?;
        self.production_plan(project_id, &id)?.ok_or_else(|| {
            PersistenceError::InvalidData("created Production Plan could not be read".into())
        })
    }

    fn update_production_plan_destination(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        destination_path: Option<&str>,
    ) -> Result<ProductionPlanRecord> {
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_production_plan_exists(&transaction, project_id, plan_id)?;
        let exporting: bool = transaction.query_row(
            "SELECT status = 'exporting' FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string()],
            |row| row.get(0),
        )?;
        if exporting {
            return Err(PersistenceError::InvalidData(
                "A Production Plan cannot change destination while its frozen manifest is exporting".into(),
            ));
        }
        stale_current_manifest_for_plan(&transaction, project_id, plan_id)?;
        let now = timestamp(&Utc::now());
        let changed = transaction.execute(
            "UPDATE production_plans SET destination_path = ?3, status = 'draft', current_manifest_id = NULL, updated_at = ?4 WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string(), destination_path.map(str::trim).filter(|value| !value.is_empty()), now],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidData(
                "Production Plan destination could not be updated".into(),
            ));
        }
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            Some(plan_id),
            None,
            None,
            "PRODUCTION_PLAN_UPDATED",
            &serde_json::json!({"destinationChanged": true}),
            &now,
        )?;
        transaction.commit()?;
        self.production_plan(project_id, plan_id)?.ok_or_else(|| {
            PersistenceError::InvalidData("updated Production Plan could not be read".into())
        })
    }

    fn update_production_plan_destination_reserve(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        reserve_bytes: u64,
    ) -> Result<ProductionPlanRecord> {
        if reserve_bytes < delivery_brain::MIN_DESTINATION_RESERVE_BYTES {
            return Err(PersistenceError::InvalidData(format!(
                "Production safety reserve must be at least {} MiB",
                delivery_brain::MIN_DESTINATION_RESERVE_BYTES / (1024 * 1024)
            )));
        }
        if reserve_bytes > i64::MAX as u64 {
            return Err(PersistenceError::InvalidData(
                "Production safety reserve exceeds supported local storage accounting".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_production_plan_exists(&transaction, project_id, plan_id)?;
        let current_plan = transaction.query_row(
            "SELECT id, project_id, name, plan_type, status, selection_rules_json, organization_json, filename_strategy_json, destination_path, destination_reserve_bytes, estimated_file_count, estimated_bytes, current_manifest_id, created_at, updated_at FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string()],
            production_plan_from_row,
        )?;
        if current_plan.status == ProductionPlanStatus::Exporting {
            return Err(PersistenceError::InvalidData(
                "A Production Plan cannot change safety reserve while its frozen manifest is exporting".into(),
            ));
        }
        if current_plan.destination_reserve_bytes == reserve_bytes {
            return Ok(current_plan);
        }
        stale_current_manifest_for_plan(&transaction, project_id, plan_id)?;
        let now = timestamp(&Utc::now());
        let next_version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM production_plan_versions WHERE production_plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE production_plans SET destination_reserve_bytes = ?3, status = 'draft', current_manifest_id = NULL, updated_at = ?4 WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string(), reserve_bytes as i64, now],
        )?;
        let configuration = serde_json::json!({
            "name": current_plan.name,
            "planType": current_plan.plan_type,
            "selectionRules": current_plan.selection_rules,
            "organization": current_plan.organization,
            "filenameStrategy": current_plan.filename_strategy,
            "destinationReserveBytes": reserve_bytes,
        });
        transaction.execute(
            "INSERT INTO production_plan_versions (id, production_plan_id, version, configuration_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().to_string(), plan_id, next_version, json(&configuration)?, now],
        )?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            Some(plan_id),
            None,
            None,
            "PRODUCTION_PLAN_UPDATED",
            &serde_json::json!({"destinationReserveBytes": reserve_bytes}),
            &now,
        )?;
        transaction.commit()?;
        self.production_plan(project_id, plan_id)?.ok_or_else(|| {
            PersistenceError::InvalidData("updated Production Plan could not be read".into())
        })
    }

    fn update_production_plan_configuration(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        input: &ProductionPlanInput,
    ) -> Result<ProductionPlanRecord> {
        let name = valid_production_name(&input.name)?;
        let transaction = moment_write_transaction(&self.connection)?;
        ensure_production_plan_exists(&transaction, project_id, plan_id)?;
        let exporting: bool = transaction.query_row(
            "SELECT status = 'exporting' FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string()],
            |row| row.get(0),
        )?;
        if exporting {
            return Err(PersistenceError::InvalidData(
                "A Production Plan cannot change configuration while its frozen manifest is exporting".into(),
            ));
        }
        stale_current_manifest_for_plan(&transaction, project_id, plan_id)?;
        let now = timestamp(&Utc::now());
        let next_version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM production_plan_versions WHERE production_plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE production_plans SET name = ?3, plan_type = ?4, selection_rules_json = ?5, organization_json = ?6, filename_strategy_json = ?7, status = 'draft', current_manifest_id = NULL, updated_at = ?8 WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string(), name, production_plan_type_name(input.plan_type), json(&input.selection_rules)?, json(&input.organization)?, json(&input.filename_strategy)?, now],
        )?;
        let configuration = serde_json::json!({
            "name": input.name.trim(),
            "planType": input.plan_type,
            "selectionRules": input.selection_rules,
            "organization": input.organization,
            "filenameStrategy": input.filename_strategy,
        });
        transaction.execute(
            "INSERT INTO production_plan_versions (id, production_plan_id, version, configuration_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().to_string(), plan_id, next_version, json(&configuration)?, now],
        )?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            Some(plan_id),
            None,
            None,
            "PLAN_CONFIGURATION_UPDATED",
            &serde_json::json!({"version": next_version}),
            &now,
        )?;
        transaction.commit()?;
        self.production_plan(project_id, plan_id)?.ok_or_else(|| {
            PersistenceError::InvalidData("updated Production Plan could not be read".into())
        })
    }

    fn production_plan(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
    ) -> Result<Option<ProductionPlanRecord>> {
        self.connection
            .query_row(
                "SELECT id, project_id, name, plan_type, status, selection_rules_json, organization_json, filename_strategy_json, destination_path, destination_reserve_bytes, estimated_file_count, estimated_bytes, current_manifest_id, created_at, updated_at FROM production_plans WHERE id = ?1 AND project_id = ?2",
                params![plan_id, project_id.to_string()],
                production_plan_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn set_production_plan_override(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
        media_asset_id: &MediaAssetId,
        kind: Option<PlanOverrideKind>,
    ) -> Result<()> {
        if !self.media_asset_belongs_to_project(media_asset_id, project_id)? {
            return Err(PersistenceError::InvalidData(
                "media asset does not belong to this project".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        ensure_production_plan_exists(&transaction, project_id, plan_id)?;
        let exporting: bool = transaction.query_row(
            "SELECT status = 'exporting' FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string()],
            |row| row.get(0),
        )?;
        if exporting {
            return Err(PersistenceError::InvalidData(
                "A Production Plan cannot change selection overrides while its frozen manifest is exporting".into(),
            ));
        }
        stale_current_manifest_for_plan(&transaction, project_id, plan_id)?;
        match kind {
            Some(kind) => {
                transaction.execute(
                    "INSERT INTO production_plan_overrides (production_plan_id, media_asset_id, override_kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT(production_plan_id, media_asset_id) DO UPDATE SET override_kind = excluded.override_kind, updated_at = excluded.updated_at",
                    params![plan_id, media_asset_id.to_string(), plan_override_kind_name(kind), now],
                )?;
            }
            None => {
                transaction.execute(
                    "DELETE FROM production_plan_overrides WHERE production_plan_id = ?1 AND media_asset_id = ?2",
                    params![plan_id, media_asset_id.to_string()],
                )?;
            }
        }
        transaction.execute(
            "UPDATE production_plans SET status = 'draft', current_manifest_id = NULL, updated_at = ?3 WHERE id = ?1 AND project_id = ?2",
            params![plan_id, project_id.to_string(), now],
        )?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            Some(plan_id),
            None,
            None,
            "PRODUCTION_PLAN_UPDATED",
            &serde_json::json!({"planOverride": kind}),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn create_virtual_collection(
        &self,
        project_id: &ProjectId,
        input: &VirtualCollectionInput,
    ) -> Result<VirtualCollectionRecord> {
        let name = valid_production_name(&input.name)?;
        if input.rules.virtual_collection_id.is_some() {
            return Err(PersistenceError::InvalidData(
                "a Virtual Collection cannot recursively reference another collection".into(),
            ));
        }
        if self.get_project(project_id)?.is_none() {
            return Err(PersistenceError::InvalidData(
                "project does not exist".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let id = Uuid::new_v4().to_string();
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT INTO virtual_collections (id, project_id, name, kind, rules_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id, project_id.to_string(), name, virtual_collection_kind_name(input.kind), json(&input.rules)?, now],
        )?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            None,
            None,
            None,
            "VIRTUAL_COLLECTION_CREATED",
            &serde_json::json!({"kind": input.kind}),
            &now,
        )?;
        transaction.commit()?;
        self.connection.query_row(
            "SELECT collection.id, collection.project_id, collection.name, collection.kind, collection.rules_json, collection.created_at, collection.updated_at, COUNT(member.media_asset_id) FROM virtual_collections collection LEFT JOIN virtual_collection_members member ON member.virtual_collection_id = collection.id WHERE collection.id = ?1 GROUP BY collection.id",
            params![id], virtual_collection_from_row,
        ).map_err(Into::into)
    }

    fn set_static_virtual_collection_members(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
        media_asset_ids: &[MediaAssetId],
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM virtual_collections WHERE id = ?1 AND project_id = ?2",
                params![collection_id, project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                PersistenceError::InvalidData(
                    "Virtual Collection does not belong to this project".into(),
                )
            })?;
        if kind != "static" {
            return Err(PersistenceError::InvalidData(
                "only a static Virtual Collection accepts explicit members".into(),
            ));
        }
        let requested_members = media_asset_ids
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        for asset_id in &requested_members {
            let belongs: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id = ?1 AND project_id = ?2)",
                params![asset_id, project_id.to_string()],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(PersistenceError::InvalidData(
                    "Virtual Collection member does not belong to this project".into(),
                ));
            }
        }
        let existing_members = transaction
            .prepare(
                "SELECT media_asset_id FROM virtual_collection_members WHERE virtual_collection_id = ?1 ORDER BY media_asset_id ASC",
            )?
            .query_map(params![collection_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        if existing_members == requested_members {
            return Ok(());
        }
        let now = timestamp(&Utc::now());
        transaction.execute(
            "DELETE FROM virtual_collection_members WHERE virtual_collection_id = ?1",
            params![collection_id],
        )?;
        for asset_id in &requested_members {
            transaction.execute(
                "INSERT INTO virtual_collection_members (virtual_collection_id, media_asset_id, created_at) VALUES (?1, ?2, ?3)",
                params![collection_id, asset_id, now],
            )?;
        }
        transaction.execute(
            "UPDATE virtual_collections SET updated_at = ?2 WHERE id = ?1",
            params![collection_id, now],
        )?;
        stale_plans_referencing_collection(&transaction, project_id, collection_id, &now)?;
        advance_production_selection_revision(&transaction, project_id, &now)?;
        transaction.commit()?;
        Ok(())
    }

    fn set_static_virtual_collection_member(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
        media_asset_id: &MediaAssetId,
        included: bool,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM virtual_collections WHERE id = ?1 AND project_id = ?2",
                params![collection_id, project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                PersistenceError::InvalidData(
                    "Virtual Collection does not belong to this project".into(),
                )
            })?;
        if kind != "static" {
            return Err(PersistenceError::InvalidData(
                "only a static Virtual Collection accepts explicit members".into(),
            ));
        }
        let belongs: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id = ?1 AND project_id = ?2)",
            params![media_asset_id.to_string(), project_id.to_string()],
            |row| row.get(0),
        )?;
        if !belongs {
            return Err(PersistenceError::InvalidData(
                "Virtual Collection member does not belong to this project".into(),
            ));
        }
        let now = timestamp(&Utc::now());
        let membership_changed = if included {
            transaction.execute(
                "INSERT OR IGNORE INTO virtual_collection_members (virtual_collection_id, media_asset_id, created_at) VALUES (?1, ?2, ?3)",
                params![collection_id, media_asset_id.to_string(), now],
            )? > 0
        } else {
            transaction.execute(
                "DELETE FROM virtual_collection_members WHERE virtual_collection_id = ?1 AND media_asset_id = ?2",
                params![collection_id, media_asset_id.to_string()],
            )? > 0
        };
        if !membership_changed {
            return Ok(());
        }
        transaction.execute(
            "UPDATE virtual_collections SET updated_at = ?2 WHERE id = ?1",
            params![collection_id, now],
        )?;
        stale_plans_referencing_collection(&transaction, project_id, collection_id, &now)?;
        advance_production_selection_revision(&transaction, project_id, &now)?;
        insert_production_event(
            &transaction,
            &project_id.to_string(),
            None,
            None,
            None,
            "VIRTUAL_COLLECTION_MEMBER_UPDATED",
            &serde_json::json!({"collectionId": collection_id, "included": included}),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn virtual_collection_assets(
        &self,
        project_id: &ProjectId,
        collection_id: &str,
    ) -> Result<Vec<String>> {
        let (kind, rules): (String, String) = self.connection.query_row(
            "SELECT kind, rules_json FROM virtual_collections WHERE id = ?1 AND project_id = ?2",
            params![collection_id, project_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if kind == "static" {
            let mut statement = self.connection.prepare(
                "SELECT member.media_asset_id FROM virtual_collection_members member JOIN media_assets asset ON asset.id = member.media_asset_id WHERE member.virtual_collection_id = ?1 AND asset.project_id = ?2 ORDER BY member.media_asset_id ASC",
            )?;
            return statement
                .query_map(params![collection_id, project_id.to_string()], |row| {
                    row.get(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into);
        }
        let rules: SelectionRules = from_json(&rules)?;
        Ok(
            production_delivery_asset_candidates(&self.connection, project_id)?
                .into_iter()
                .filter(|asset| rules.matches(asset))
                .map(|asset| asset.asset_id)
                .collect(),
        )
    }

    fn production_manifest_build_input(
        &self,
        project_id: &ProjectId,
        plan_id: &str,
    ) -> Result<ProductionManifestBuildInput> {
        let plan = self.production_plan(project_id, plan_id)?.ok_or_else(|| {
            PersistenceError::InvalidData("Production Plan does not belong to this project".into())
        })?;
        let mut overrides_statement = self.connection.prepare(
            "SELECT media_asset_id, override_kind FROM production_plan_overrides WHERE production_plan_id = ?1 ORDER BY media_asset_id ASC",
        )?;
        let overrides = overrides_statement
            .query_map(params![plan_id], |row| {
                let kind: String = row.get(1)?;
                let kind = serde_json::from_str(&format!("\"{kind}\"")).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(PlanOverride {
                    media_asset_id: row.get(0)?,
                    kind,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut roots_statement = self.connection.prepare(
            "SELECT instance.id, root.selected_path FROM file_instances instance JOIN media_assets asset ON asset.id = instance.media_asset_id JOIN index_roots root ON root.id = instance.index_root_id WHERE asset.project_id = ?1",
        )?;
        let source_roots = roots_statement
            .query_map(params![project_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<BTreeMap<String, String>, _>>()?;
        let source_revision = production_source_revision(&self.connection, project_id)?;
        let virtual_collection_asset_ids = plan
            .selection_rules
            .virtual_collection_id
            .as_deref()
            .map(|collection_id| self.virtual_collection_assets(project_id, collection_id))
            .transpose()?;
        Ok((
            plan,
            overrides,
            production_delivery_asset_candidates(&self.connection, project_id)?,
            source_roots,
            source_revision,
            virtual_collection_asset_ids,
        ))
    }

    fn create_export_manifest(
        &self,
        plan: &ProductionPlanRecord,
        source_revision: u64,
        destination_path: &str,
        validation: &serde_json::Value,
        entries: &[ManifestEntryDraft],
        checksum: &str,
    ) -> Result<ExportManifestRecord> {
        if destination_path.trim().is_empty() {
            return Err(PersistenceError::InvalidData(
                "a local destination is required before creating an export manifest".into(),
            ));
        }
        if entries.iter().any(|entry| {
            entry.status != delivery_brain::ManifestEntryStatus::Planned
                || !delivery_brain::safe_destination_relative_path(&entry.destination_relative_path)
        }) {
            return Err(PersistenceError::InvalidData(
                "a blocked or unsafe dry-run cannot become an export manifest".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let current_revision =
            production_source_revision_transaction(&transaction, &plan.project_id)?;
        if current_revision != source_revision {
            return Err(PersistenceError::InvalidData(
                "Production selection sources changed while this manifest was being prepared; refresh the plan".into(),
            ));
        }
        ensure_production_plan_exists_by_string(&transaction, &plan.project_id, &plan.id)?;
        let current_plan = transaction.query_row(
            "SELECT id, project_id, name, plan_type, status, selection_rules_json, organization_json, filename_strategy_json, destination_path, destination_reserve_bytes, estimated_file_count, estimated_bytes, current_manifest_id, created_at, updated_at FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan.id, plan.project_id],
            production_plan_from_row,
        )?;
        if current_plan.name != plan.name
            || current_plan.plan_type != plan.plan_type
            || current_plan.selection_rules != plan.selection_rules
            || current_plan.organization != plan.organization
            || current_plan.filename_strategy != plan.filename_strategy
            || current_plan.destination_path != plan.destination_path
            || current_plan.destination_reserve_bytes != plan.destination_reserve_bytes
        {
            return Err(PersistenceError::InvalidData(
                "Production Plan configuration changed while this manifest was being prepared; refresh the plan".into(),
            ));
        }
        let exporting: bool = transaction.query_row(
            "SELECT status = 'exporting' FROM production_plans WHERE id = ?1 AND project_id = ?2",
            params![plan.id, plan.project_id],
            |row| row.get(0),
        )?;
        if exporting {
            return Err(PersistenceError::InvalidData(
                "A new Export Manifest cannot replace a Production Plan while its prior manifest is exporting".into(),
            ));
        }
        let version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(manifest_version), 0) + 1 FROM export_manifests WHERE production_plan_id = ?1",
            params![plan.id],
            |row| row.get(0),
        )?;
        let id = Uuid::new_v4().to_string();
        let now = timestamp(&Utc::now());
        let selected_file_count = entries.len() as u64;
        let estimated_bytes = entries.iter().try_fold(0_u64, |total, entry| {
            total.checked_add(entry.expected_byte_size).ok_or_else(|| {
                PersistenceError::InvalidData("manifest estimated bytes overflow".into())
            })
        })?;
        transaction.execute(
            "UPDATE export_manifests SET status = 'superseded' WHERE production_plan_id = ?1 AND status = 'ready'",
            params![plan.id],
        )?;
        transaction.execute(
            "INSERT INTO export_manifests (id, production_plan_id, project_id, manifest_version, source_revision, status, selection_snapshot_json, organization_snapshot_json, filename_strategy_snapshot_json, destination_path, selected_file_count, estimated_bytes, checksum, validation_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'ready', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![id, plan.id, plan.project_id, version, source_revision as i64, json(&plan.selection_rules)?, json(&plan.organization)?, json(&plan.filename_strategy)?, destination_path.trim(), selected_file_count as i64, estimated_bytes as i64, checksum, json(validation)?, now],
        )?;
        for (ordinal, entry) in entries.iter().enumerate() {
            transaction.execute(
                "INSERT INTO export_manifest_entries (id, export_manifest_id, ordinal, media_asset_id, selected_file_instance_id, source_relative_path, original_filename, destination_relative_path, destination_filename, expected_byte_size, source_checksum, human_decision, rating, starred, moment_id, moment_label, status, issue) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![Uuid::new_v4().to_string(), id, ordinal as i64, entry.media_asset_id, entry.selected_file_instance_id, entry.source_relative_path, entry.original_filename, entry.destination_relative_path, entry.destination_filename, entry.expected_byte_size as i64, entry.source_checksum, entry.human_decision, entry.rating as i64, entry.starred, entry.moment_id, entry.moment_label, manifest_entry_status_name(entry.status), entry.issue],
            )?;
        }
        transaction.execute(
            "UPDATE production_plans SET status = 'ready', destination_path = ?3, estimated_file_count = ?4, estimated_bytes = ?5, current_manifest_id = ?6, updated_at = ?7 WHERE id = ?1 AND project_id = ?2",
            params![plan.id, plan.project_id, destination_path.trim(), selected_file_count as i64, estimated_bytes as i64, id, now],
        )?;
        insert_production_event(
            &transaction,
            &plan.project_id,
            Some(&plan.id),
            Some(&id),
            None,
            "MANIFEST_CREATED",
            &serde_json::json!({"manifestVersion": version, "selectedFileCount": selected_file_count, "estimatedBytes": estimated_bytes}),
            &now,
        )?;
        transaction.commit()?;
        self.export_manifest(
            &ProjectId::try_from(plan.project_id.as_str())
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
            &id,
        )?
        .ok_or_else(|| {
            PersistenceError::InvalidData("created export manifest could not be read".into())
        })
    }

    fn export_manifest(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Option<ExportManifestRecord>> {
        self.connection
            .query_row(
                "SELECT id, production_plan_id, project_id, manifest_version, source_revision, status, selection_snapshot_json, organization_snapshot_json, filename_strategy_snapshot_json, destination_path, selected_file_count, estimated_bytes, checksum, validation_json, created_at FROM export_manifests WHERE id = ?1 AND project_id = ?2",
                params![manifest_id, project_id.to_string()],
                export_manifest_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn export_manifest_entries(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Vec<ExportManifestEntryRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT entry.id, entry.export_manifest_id, entry.ordinal, entry.media_asset_id, entry.selected_file_instance_id, entry.original_filename, entry.destination_relative_path, entry.destination_filename, entry.expected_byte_size, entry.source_checksum, entry.human_decision, entry.rating, entry.starred, entry.moment_id, entry.moment_label, entry.status, entry.issue FROM export_manifest_entries entry JOIN export_manifests manifest ON manifest.id = entry.export_manifest_id WHERE entry.export_manifest_id = ?1 AND manifest.project_id = ?2 ORDER BY entry.ordinal ASC",
        )?;
        let entries = statement
            .query_map(
                params![manifest_id, project_id.to_string()],
                export_manifest_entry_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)?;
        Ok(entries)
    }

    fn export_manifest_execution_entries(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Vec<ExportManifestExecutionEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT entry.id, entry.export_manifest_id, entry.ordinal, entry.media_asset_id, entry.selected_file_instance_id, entry.original_filename, entry.destination_relative_path, entry.destination_filename, entry.expected_byte_size, entry.source_checksum, entry.human_decision, entry.rating, entry.starred, entry.moment_id, entry.moment_label, entry.status, entry.issue, root.selected_path, entry.source_relative_path, COALESCE(instance.is_available, 0) FROM export_manifest_entries entry JOIN export_manifests manifest ON manifest.id = entry.export_manifest_id LEFT JOIN file_instances instance ON instance.id = entry.selected_file_instance_id LEFT JOIN index_roots root ON root.id = instance.index_root_id WHERE entry.export_manifest_id = ?1 AND manifest.project_id = ?2 ORDER BY entry.ordinal ASC",
        )?;
        let entries = statement
            .query_map(params![manifest_id, project_id.to_string()], |row| {
                Ok(ExportManifestExecutionEntry {
                    entry: export_manifest_entry_from_row(row)?,
                    source_root_path: row.get(17)?,
                    source_relative_path: row.get(18)?,
                    source_available: row.get(19)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)?;
        Ok(entries)
    }

    fn create_export_job(&self, record: &ExportJobRecord, job: &BackgroundJob) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM export_jobs WHERE export_manifest_id = ?1 AND state IN ('queued','running','paused'))",
            params![record.manifest_id],
            |row| row.get(0),
        )?;
        if active {
            return Err(PersistenceError::InvalidData(
                "this export manifest already has an active local export".into(),
            ));
        }
        let (manifest_project, manifest_ready, is_current): (String, bool, bool) = transaction.query_row(
            "SELECT manifest.project_id, manifest.status = 'ready', plan.current_manifest_id = manifest.id
             FROM export_manifests manifest
             JOIN production_plans plan ON plan.id = manifest.production_plan_id
             WHERE manifest.id = ?1 AND manifest.production_plan_id = ?2",
            params![record.manifest_id, record.plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if !manifest_ready || !is_current {
            return Err(PersistenceError::InvalidData(
                "This Export Manifest is stale or has been replaced; refresh the Production Plan before exporting".into(),
            ));
        }
        if job.project_id.as_ref().map(ToString::to_string).as_deref()
            != Some(manifest_project.as_str())
        {
            return Err(PersistenceError::InvalidData(
                "export background job must retain the manifest project identity".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO background_jobs (id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![job.id.to_string(), json(&job.state)?, json(&job.stage)?, job.items_completed as i64, job.items_total.map(|value| value as i64), job.files_discovered as i64, job.files_processed as i64, job.error_count as i64, job.project_id.as_ref().map(ToString::to_string), job.index_root_id.as_ref().map(ToString::to_string), job.error_message, job.resume_metadata.as_ref().map(serde_json::to_string).transpose()?, timestamp(&job.created_at), timestamp(&job.updated_at), optional_timestamp(&job.finished_at)],
        )?;
        transaction.execute(
            "INSERT INTO export_jobs (id, production_plan_id, export_manifest_id, background_job_id, state, destination_path, items_total, items_completed, verified_count, skipped_identical_count, failed_count, verified_bytes, error_message, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![record.id, record.plan_id, record.manifest_id, record.background_job_id, record.state, record.destination_path, record.items_total as i64, record.items_completed as i64, record.verified_count as i64, record.skipped_identical_count as i64, record.failed_count as i64, record.verified_bytes as i64, record.error_message, record.created_at, record.updated_at, record.finished_at],
        )?;
        transaction.execute(
            "INSERT INTO export_job_entries (export_job_id, export_manifest_entry_id, state, copied_bytes, updated_at) SELECT ?1, id, CASE WHEN status = 'planned' THEN 'pending' ELSE 'blocked' END, 0, ?2 FROM export_manifest_entries WHERE export_manifest_id = ?3",
            params![record.id, record.created_at, record.manifest_id],
        )?;
        transaction.execute(
            "UPDATE production_plans SET status = 'exporting', updated_at = ?2 WHERE id = ?1",
            params![record.plan_id, record.updated_at],
        )?;
        insert_production_event(
            &transaction,
            &manifest_project,
            Some(&record.plan_id),
            Some(&record.manifest_id),
            Some(&record.id),
            "EXPORT_STARTED",
            &serde_json::json!({"itemsTotal": record.items_total}),
            &record.created_at,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn export_job(&self, project_id: &ProjectId, job_id: &str) -> Result<Option<ExportJobRecord>> {
        self.connection
            .query_row(
                "SELECT job.id, job.production_plan_id, job.export_manifest_id, job.background_job_id, job.state, job.destination_path, job.items_total, job.items_completed, job.verified_count, job.skipped_identical_count, job.failed_count, job.verified_bytes, job.created_at, job.updated_at, job.finished_at, job.error_message FROM export_jobs job JOIN export_manifests manifest ON manifest.id = job.export_manifest_id WHERE job.id = ?1 AND manifest.project_id = ?2",
                params![job_id, project_id.to_string()],
                export_job_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn latest_export_job(
        &self,
        project_id: &ProjectId,
        manifest_id: &str,
    ) -> Result<Option<ExportJobRecord>> {
        self.connection
            .query_row(
                "SELECT job.id, job.production_plan_id, job.export_manifest_id, job.background_job_id, job.state, job.destination_path, job.items_total, job.items_completed, job.verified_count, job.skipped_identical_count, job.failed_count, job.verified_bytes, job.created_at, job.updated_at, job.finished_at, job.error_message FROM export_jobs job JOIN export_manifests manifest ON manifest.id = job.export_manifest_id WHERE job.export_manifest_id = ?1 AND manifest.project_id = ?2 ORDER BY job.updated_at DESC, job.id DESC LIMIT 1",
                params![manifest_id, project_id.to_string()],
                export_job_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn update_export_job_entry(
        &self,
        export_job_id: &str,
        update: &ExportJobEntryUpdate,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        let changed = transaction.execute(
            "UPDATE export_job_entries SET state = ?3, copied_bytes = ?4, source_checksum = ?5, destination_checksum = ?6, error_message = ?7, updated_at = ?8 WHERE export_job_id = ?1 AND export_manifest_entry_id = ?2",
            params![export_job_id, update.manifest_entry_id, update.state, update.copied_bytes as i64, update.source_checksum, update.destination_checksum, update.error_message, now],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidData(
                "export entry does not belong to this Export Job".into(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    fn cancel_pending_export_job_entries(&self, export_job_id: &str, message: &str) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let changed = transaction.execute(
            "UPDATE export_job_entries SET state = 'cancelled', error_message = ?2, updated_at = ?3 WHERE export_job_id = ?1 AND state IN ('pending', 'copying')",
            params![export_job_id, message, timestamp(&Utc::now())],
        )?;
        if changed == 0 {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM export_jobs WHERE id = ?1)",
                params![export_job_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(PersistenceError::InvalidData(
                    "Export Job does not exist".into(),
                ));
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn update_export_job(
        &self,
        record: &ExportJobRecord,
        background_job: &BackgroundJob,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let project_id: String = transaction.query_row(
            "SELECT manifest.project_id FROM export_jobs job JOIN export_manifests manifest ON manifest.id = job.export_manifest_id WHERE job.id = ?1 AND job.production_plan_id = ?2 AND job.export_manifest_id = ?3",
            params![record.id, record.plan_id, record.manifest_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE export_jobs SET state = ?2, items_total = ?3, items_completed = ?4, verified_count = ?5, skipped_identical_count = ?6, failed_count = ?7, verified_bytes = ?8, error_message = ?9, updated_at = ?10, finished_at = ?11 WHERE id = ?1",
            params![record.id, record.state, record.items_total as i64, record.items_completed as i64, record.verified_count as i64, record.skipped_identical_count as i64, record.failed_count as i64, record.verified_bytes as i64, record.error_message, record.updated_at, record.finished_at],
        )?;
        transaction.execute(
            "UPDATE background_jobs SET state_json = ?2, stage_json = ?3, items_completed = ?4, items_total = ?5, files_discovered = ?6, files_processed = ?7, error_count = ?8, error_message = ?9, resume_metadata_json = ?10, updated_at = ?11, finished_at = ?12 WHERE id = ?1",
            params![background_job.id.to_string(), json(&background_job.state)?, json(&background_job.stage)?, background_job.items_completed as i64, background_job.items_total.map(|value| value as i64), background_job.files_discovered as i64, background_job.files_processed as i64, background_job.error_count as i64, background_job.error_message, background_job.resume_metadata.as_ref().map(serde_json::to_string).transpose()?, timestamp(&background_job.updated_at), optional_timestamp(&background_job.finished_at)],
        )?;
        let plan_status = match record.state.as_str() {
            "completed" => "completed",
            "partially_completed" => "partially_completed",
            "failed" => "failed",
            "cancelled" | "interrupted" | "paused" => "ready",
            _ => "exporting",
        };
        transaction.execute(
            "UPDATE production_plans SET status = ?2, updated_at = ?3 WHERE id = ?1 AND current_manifest_id = ?4",
            params![record.plan_id, plan_status, record.updated_at, record.manifest_id],
        )?;
        if matches!(
            record.state.as_str(),
            "completed" | "partially_completed" | "failed" | "cancelled" | "interrupted"
        ) {
            let event_type = match record.state.as_str() {
                "completed" => "EXPORT_COMPLETED",
                "partially_completed" => "EXPORT_PARTIAL",
                "cancelled" => "EXPORT_CANCELLED",
                "interrupted" => "EXPORT_PAUSED",
                _ => "EXPORT_FAILED",
            };
            insert_production_event(
                &transaction,
                &project_id,
                Some(&record.plan_id),
                Some(&record.manifest_id),
                Some(&record.id),
                event_type,
                &serde_json::json!({"verifiedCount": record.verified_count, "failedCount": record.failed_count}),
                &record.updated_at,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn store_delivery_report(&self, report: &DeliveryReportRecord) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let manifest_checksum: String = transaction.query_row(
            "SELECT manifest.checksum
             FROM export_jobs job
             JOIN export_manifests manifest ON manifest.id = job.export_manifest_id
             WHERE job.id = ?1",
            params![report.export_job_id],
            |row| row.get(0),
        )?;
        if manifest_checksum != report.manifest_checksum {
            return Err(PersistenceError::InvalidData(
                "Delivery Report checksum must match the Export Manifest".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO delivery_reports (id, export_job_id, manifest_checksum, report_json, report_text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.id,
                report.export_job_id,
                report.manifest_checksum,
                json(&report.report_json)?,
                report.report_text,
                report.created_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn recover_interrupted_production_exports(&self) -> Result<u64> {
        let transaction = moment_write_transaction(&self.connection)?;
        let now = timestamp(&Utc::now());
        let changed = transaction.execute(
            "UPDATE export_jobs SET state = 'interrupted', error_message = COALESCE(error_message, 'Local export was interrupted. Previously verified files remain valid; resume to continue.'), updated_at = ?1, finished_at = ?1 WHERE state IN ('queued','running')",
            params![now],
        )?;
        transaction.execute(
            "UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"production_export\"', error_message = COALESCE(error_message, 'Local export was interrupted. Previously verified files remain valid; resume to continue.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"production-export\"%'",
            params![now],
        )?;
        transaction.execute(
            "UPDATE production_plans SET status = 'ready', updated_at = ?1 WHERE id IN (SELECT production_plan_id FROM export_jobs WHERE state = 'interrupted') AND status = 'exporting'",
            params![now],
        )?;
        transaction.commit()?;
        Ok(changed as u64)
    }

    fn latest_capture_intelligence_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>> {
        self.connection.query_row(
            "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at
             FROM background_jobs WHERE project_id = ?1 AND resume_metadata_json LIKE '%\"pipeline\":\"capture-intelligence\"%'
             ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            params![project_id.to_string()], background_job_tuple,
        ).optional()?.map(background_job_from_tuple).transpose()
    }

    fn capture_intelligence_terminal_counts(
        &self,
        project_id: &ProjectId,
    ) -> Result<CaptureIntelligenceTerminalCounts> {
        let mut statement = self.connection.prepare(
            "WITH ranked AS (
              SELECT artifact.media_asset_id,
                     CASE WHEN artifact.stale = 1 THEN 'stale' ELSE artifact.status END AS status,
                     ROW_NUMBER() OVER (PARTITION BY artifact.media_asset_id ORDER BY artifact.generated_at DESC, artifact.id DESC) AS position
              FROM analysis_artifacts artifact JOIN media_assets a ON a.id = artifact.media_asset_id
              WHERE a.project_id = ?1 AND artifact.artifact_type = 'capture_intelligence'
            ) SELECT status, COUNT(*) FROM ranked WHERE position = 1 GROUP BY status",
        )?;
        let mut counts = CaptureIntelligenceTerminalCounts::default();
        for row in statement.query_map(params![project_id.to_string()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)? as u64,
            ))
        })? {
            let (status, count) = row?;
            counts.total += count;
            match status.as_deref() {
                Some("ready") => counts.ready = count,
                Some("unsupported") => counts.unsupported = count,
                Some("corrupt") => counts.corrupt = count,
                Some("needs_original") => counts.needs_original = count,
                Some("failed") => counts.failed = count,
                Some("not_applicable") => counts.not_applicable = count,
                Some("stale") => counts.stale = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    fn recover_interrupted_capture_intelligence(&self) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"finalize\"', error_message = COALESCE(error_message, 'Capture Intelligence was interrupted before completion; resume analysis to continue.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"capture-intelligence\"%'",
            params![timestamp(&Utc::now())],
        )?;
        Ok(changed as u64)
    }

    fn upsert_local_model(&self, record: &LocalModelRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO local_model_registry (id, model_id, model_family, provider, version, local_relative_path, checksum, capability, input_size, embedding_dimension, status, license, license_url, source_url, file_size_bytes, hardware_requirements, registered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) ON CONFLICT(model_id, provider, version) DO UPDATE SET model_family = excluded.model_family, local_relative_path = excluded.local_relative_path, checksum = excluded.checksum, capability = excluded.capability, input_size = excluded.input_size, embedding_dimension = excluded.embedding_dimension, status = excluded.status, license = excluded.license, license_url = excluded.license_url, source_url = excluded.source_url, file_size_bytes = excluded.file_size_bytes, hardware_requirements = excluded.hardware_requirements, registered_at = excluded.registered_at",
            params![record.id.to_string(), record.model_id, record.model_family, record.provider, record.version, record.local_relative_path, record.checksum, record.capability, record.input_size.map(i64::from), record.embedding_dimension.map(i64::from), record.status, record.license, record.license_url, record.source_url, record.file_size_bytes.map(|value| value as i64), record.hardware_requirements, timestamp(&record.registered_at)],
        )?;
        Ok(())
    }

    fn local_model(
        &self,
        model_id: &str,
        provider: &str,
        version: &str,
    ) -> Result<Option<LocalModelRecord>> {
        self.connection
            .query_row(
                "SELECT id, model_id, model_family, provider, version, local_relative_path, checksum, capability, input_size, embedding_dimension, status, license, license_url, source_url, file_size_bytes, hardware_requirements, registered_at FROM local_model_registry WHERE model_id = ?1 AND provider = ?2 AND version = ?3",
                params![model_id, provider, version],
                local_model_from_row,
            )
            .optional()?
            .map(Ok)
            .transpose()
    }

    fn semantic_embedding_candidates(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<SemanticInputCandidate>> {
        // This query has no M4 stale-refresh side effect. The selected preview is an
        // CaptureOS-managed artifact, but semantic evidence owns its own cache lifecycle.
        let sql = r#"WITH ranked_instances AS (
              SELECT fi.id, fi.media_asset_id, fi.is_available, fi.observed_at,
                ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank
              FROM file_instances fi
            ), primary_instances AS (
              SELECT * FROM ranked_instances WHERE instance_rank = 1
            ), candidate_inputs AS (
              SELECT a.id AS asset_id, a.project_id, pi.id AS file_instance_id, a.display_name,
                a.media_type,
                COALESCE(
                  selected.source_fingerprint || CASE selected.artifact_type WHEN 'thumbnail' THEN '|preview:' ELSE '|analysis-preview:' END || selected.generator_version || ':' || selected.size_class,
                  a.content_hash, a.fast_fingerprint, a.id
                ) AS input_fingerprint,
                selected.cache_relative_path, selected.status AS selected_status, selected.failure_reason, pi.is_available
              FROM media_assets a
              JOIN primary_instances pi ON pi.media_asset_id = a.id
              LEFT JOIN preview_artifacts selected ON selected.id = (
                SELECT artifact.id
                FROM preview_artifacts artifact
                WHERE artifact.media_asset_id = a.id
                  AND artifact.source_file_instance_id = pi.id
                  AND artifact.source_fingerprint = COALESCE(a.content_hash, a.fast_fingerprint, a.id)
                  AND ((artifact.artifact_type = 'analysis_preview' AND artifact.size_class = 'analysis')
                    OR (artifact.artifact_type = 'thumbnail' AND artifact.size_class IN ('preview', 'medium', 'small')))
                ORDER BY CASE
                    WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status = 'ready' THEN 0
                    WHEN artifact.artifact_type = 'thumbnail' AND artifact.size_class = 'preview' AND artifact.status = 'ready' THEN 1
                    WHEN artifact.artifact_type = 'analysis_preview' AND artifact.status IN ('corrupt', 'unsupported') THEN 2
                    WHEN artifact.artifact_type = 'thumbnail' AND artifact.status IN ('corrupt', 'unsupported') THEN 3
                    ELSE 4 END,
                  artifact.created_at DESC, artifact.id DESC
                LIMIT 1
              )
              WHERE a.project_id = ?1
                AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
            )
            SELECT candidate.asset_id, candidate.project_id, candidate.file_instance_id, candidate.display_name,
                   candidate.media_type, candidate.input_fingerprint, candidate.cache_relative_path,
                   COALESCE(candidate.selected_status, CASE WHEN candidate.is_available = 0 THEN 'needs_original' ELSE 'pending' END),
                   candidate.failure_reason, candidate.is_available
            FROM candidate_inputs candidate
            WHERE NOT EXISTS (
              SELECT 1 FROM semantic_embeddings stored
              WHERE stored.media_asset_id = candidate.asset_id
                AND stored.input_fingerprint = candidate.input_fingerprint
                AND stored.model_id = ?2
                AND stored.provider = ?3
                AND stored.model_version = ?4
                AND stored.embedding_version = ?5
                AND stored.preprocessing_version = ?6
                AND stored.metric = ?7
                AND stored.dimensions = ?8
                AND stored.stale = 0
                AND stored.status IN ('ready', 'unsupported', 'corrupt', 'needs_original', 'failed')
            )
            ORDER BY candidate.is_available DESC, candidate.asset_id ASC"#;
        let mut statement = self.connection.prepare(sql)?;
        let candidates = statement
            .query_map(
                params![
                    project_id.to_string(),
                    model.model_id,
                    model.provider,
                    model.model_version,
                    model.embedding_version,
                    model.preprocessing_version,
                    model.metric,
                    model.dimensions as i64,
                ],
                |row| {
                    Ok(SemanticInputCandidate {
                        asset_id: row.get(0)?,
                        project_id: row.get(1)?,
                        file_instance_id: row.get(2)?,
                        filename: row.get(3)?,
                        media_type: row.get(4)?,
                        input_fingerprint: row.get(5)?,
                        preview_relative_path: row.get(6)?,
                        preview_status: row.get(7)?,
                        preview_failure_reason: row.get(8)?,
                        is_available: row.get(9)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn upsert_semantic_embedding(&self, embedding: &SemanticEmbeddingRecord) -> Result<()> {
        let belongs_to_project: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id = ?1 AND project_id = ?2)",
            params![
                embedding.media_asset_id.to_string(),
                embedding.project_id.to_string()
            ],
            |row| row.get(0),
        )?;
        if !belongs_to_project {
            return Err(PersistenceError::InvalidData(
                "semantic embedding media asset does not belong to its project".into(),
            ));
        }
        let dimensions = embedding.model.dimensions;
        let blob = embedding
            .embedding
            .as_ref()
            .map(|values| encode_semantic_vector(values, dimensions))
            .transpose()?;
        if embedding.status == AnalysisStatus::Ready && blob.is_none() {
            return Err(PersistenceError::InvalidData(
                "ready semantic embedding is missing its vector".into(),
            ));
        }
        // Preserve the prior row for audit/cache recovery, but make only the current input
        // queryable. No FileInstance appears in this identity: one logical asset has one vector.
        self.connection.execute(
            "UPDATE semantic_embeddings SET stale = 1 WHERE media_asset_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND input_fingerprint <> ?8 AND stale = 0",
            params![embedding.media_asset_id.to_string(), embedding.model.model_id, embedding.model.provider, embedding.model.model_version, embedding.model.embedding_version, embedding.model.preprocessing_version, embedding.model.metric, embedding.input_fingerprint],
        )?;
        self.connection.execute(
            "INSERT INTO semantic_embeddings (media_asset_id, project_id, input_fingerprint, model_id, provider, model_version, embedding_version, preprocessing_version, metric, dimensions, encoding, embedding_blob, generated_at, status, error_message, stale) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'f32le-normalized', ?11, ?12, ?13, ?14, 0) ON CONFLICT(media_asset_id, input_fingerprint, model_id, provider, model_version, embedding_version, preprocessing_version, metric) DO UPDATE SET project_id = excluded.project_id, dimensions = excluded.dimensions, encoding = excluded.encoding, embedding_blob = excluded.embedding_blob, generated_at = excluded.generated_at, status = excluded.status, error_message = excluded.error_message, stale = 0",
            params![embedding.media_asset_id.to_string(), embedding.project_id.to_string(), embedding.input_fingerprint, embedding.model.model_id, embedding.model.provider, embedding.model.model_version, embedding.model.embedding_version, embedding.model.preprocessing_version, embedding.model.metric, dimensions as i64, blob, timestamp(&embedding.generated_at), embedding.status.as_str(), embedding.error_message],
        )?;
        mark_studio_recommendations_stale_for_asset(
            &self.connection,
            &embedding.media_asset_id.to_string(),
        )?;
        Ok(())
    }

    fn mark_other_semantic_embeddings_stale(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE semantic_embeddings
             SET stale = 1
             WHERE project_id = ?1
               AND stale = 0
               AND (model_id <> ?2
                    OR provider <> ?3
                    OR model_version <> ?4
                    OR embedding_version <> ?5
                    OR preprocessing_version <> ?6
                    OR metric <> ?7
                    OR dimensions <> ?8)",
            params![
                project_id.to_string(),
                model.model_id,
                model.provider,
                model.model_version,
                model.embedding_version,
                model.preprocessing_version,
                model.metric,
                model.dimensions as i64,
            ],
        )?;
        if changed > 0 {
            mark_studio_recommendations_stale_for_project(
                &self.connection,
                &project_id.to_string(),
            )?;
        }
        Ok(changed as u64)
    }

    fn semantic_embeddings_for_index(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<StoredSemanticVector>> {
        let mut statement = self.connection.prepare(
            "SELECT media_asset_id, embedding_blob, dimensions FROM semantic_embeddings WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND dimensions = ?8 AND stale = 0 AND status = 'ready' ORDER BY media_asset_id ASC",
        )?;
        let vectors = statement
            .query_map(
                params![
                    project_id.to_string(),
                    model.model_id,
                    model.provider,
                    model.model_version,
                    model.embedding_version,
                    model.preprocessing_version,
                    model.metric,
                    model.dimensions as i64
                ],
                |row| {
                    let dimensions = row.get::<_, i64>(2)? as usize;
                    let blob = row.get::<_, Option<Vec<u8>>>(1)?.ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(
                            1,
                            "embedding_blob".into(),
                            rusqlite::types::Type::Null,
                        )
                    })?;
                    let asset_id: String = row.get(0)?;
                    let vector = decode_semantic_vector(&blob, dimensions).map_err(to_sql_error)?;
                    Ok(StoredSemanticVector { asset_id, vector })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        vectors
    }

    fn semantic_search_candidates_for_assets(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
        asset_ids: &[String],
    ) -> Result<Vec<SemanticSearchCandidate>> {
        if asset_ids.is_empty() {
            return Ok(Vec::new());
        }
        if asset_ids.len() > 8_192 {
            return Err(PersistenceError::InvalidData(
                "semantic search candidate request exceeded its bounded index limit".into(),
            ));
        }
        let placeholders = (0..asset_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let select_vector = if model.is_some() {
            "stored.embedding_blob, stored.dimensions"
        } else {
            "NULL AS embedding_blob, NULL AS dimensions"
        };
        let join_vector = if model.is_some() {
            "JOIN semantic_embeddings stored ON stored.media_asset_id = a.id AND stored.project_id = a.project_id AND stored.stale = 0 AND stored.status = 'ready' AND stored.model_id = ?2 AND stored.provider = ?3 AND stored.model_version = ?4 AND stored.embedding_version = ?5 AND stored.preprocessing_version = ?6 AND stored.metric = ?7 AND stored.dimensions = ?8"
        } else {
            ""
        };
        let sql = format!(
            "SELECT a.id, {select_vector},
                CASE WHEN EXISTS (SELECT 1 FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready') THEN (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = a.id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready' ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)) ELSE NULL END,
                COALESCE(decision.rating, 0), decision.decision,
                (SELECT quality.sharpness_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.blur_level FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.technical_quality_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.technical_quality_score FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                metadata.camera_model, COALESCE(metadata.captured_at_local, a.captured_at)
             FROM media_assets a
             {join_vector}
             LEFT JOIN media_decisions decision ON decision.media_asset_id = a.id AND decision.project_id = a.project_id
             LEFT JOIN media_metadata metadata ON metadata.media_asset_id = a.id
             WHERE a.project_id = ?1 AND a.id IN ({placeholders})"
        );
        let mut bound = Vec::<rusqlite::types::Value>::new();
        bound.push(project_id.to_string().into());
        if let Some(model) = model {
            bound.extend([
                model.model_id.clone().into(),
                model.provider.clone().into(),
                model.model_version.clone().into(),
                model.embedding_version.clone().into(),
                model.preprocessing_version.clone().into(),
                model.metric.clone().into(),
                (model.dimensions as i64).into(),
            ]);
        }
        bound.extend(asset_ids.iter().cloned().map(Into::into));
        let mut statement = self.connection.prepare(&sql)?;
        let candidates = statement
            .query_map(rusqlite::params_from_iter(bound), |row| {
                let dimensions = row.get::<_, Option<i64>>(2)?;
                let vector = match (row.get::<_, Option<Vec<u8>>>(1)?, dimensions) {
                    (Some(blob), Some(dimensions)) => Some(
                        decode_semantic_vector(&blob, dimensions as usize).map_err(to_sql_error)?,
                    ),
                    (None, None) => None,
                    _ => {
                        return Err(rusqlite::Error::InvalidColumnType(
                            1,
                            "embedding_blob".into(),
                            rusqlite::types::Type::Blob,
                        ))
                    }
                };
                Ok(SemanticSearchCandidate {
                    asset_id: row.get(0)?,
                    vector,
                    face_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    rating: row.get::<_, i64>(4)? as u8,
                    decision: row.get(5)?,
                    sharpness_band: row.get(6)?,
                    blur_level: row.get(7)?,
                    technical_quality_band: row.get(8)?,
                    technical_quality_score: row.get(9)?,
                    camera_model: row.get(10)?,
                    captured_at: row.get(11)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn semantic_metadata_candidates(
        &self,
        project_id: &ProjectId,
        query: &SemanticMetadataQuery,
    ) -> Result<Vec<SemanticSearchCandidate>> {
        let mut predicates = Vec::new();
        let mut bound = vec![rusqlite::types::Value::from(project_id.to_string())];
        if let Some(moment_id) = &query.moment_id {
            if !self.moment_belongs_to_project(project_id, moment_id)? {
                return Err(PersistenceError::InvalidData(
                    "moment does not belong to the selected project".into(),
                ));
            }
            predicates.push("EXISTS (SELECT 1 FROM moment_memberships mm WHERE mm.project_id = ? AND mm.media_asset_id = asset_id AND mm.moment_id = ? AND mm.active = 1 AND mm.membership_state = 'member')".to_owned());
            bound.push(project_id.to_string().into());
            bound.push(moment_id.clone().into());
        }
        if let Some(face_count) = query.face_count {
            predicates.push("face_count = ?".to_owned());
            bound.push((face_count as i64).into());
        }
        if let Some(rating) = query.rating_exact {
            predicates.push("rating = ?".to_owned());
            bound.push(i64::from(rating).into());
        }
        if let Some(rating) = query.rating_minimum {
            predicates.push("rating >= ?".to_owned());
            bound.push(i64::from(rating).into());
        }
        if let Some(decision) = &query.decision {
            predicates.push("decision = ?".to_owned());
            bound.push(decision.clone().into());
        }
        if query.require_sharp {
            predicates.push("sharpness_band IN ('excellent', 'good')".to_owned());
        }
        if query.require_blurry {
            predicates.push("blur_level IN ('moderate', 'high')".to_owned());
        }
        if query.require_technical_issue {
            predicates.push("technical_quality_band = 'technical_issue'".to_owned());
        }
        if let Some(camera_model) = &query.camera_model {
            predicates.push("lower(COALESCE(camera_model, '')) = lower(?)".to_owned());
            bound.push(camera_model.clone().into());
        }
        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        let sort_column = match query.sort {
            SemanticMetadataSort::CaptureTime => "captured_at",
            SemanticMetadataSort::TechnicalQuality => "technical_quality_score",
            SemanticMetadataSort::Rating => "rating",
        };
        let direction = if query.descending { "DESC" } else { "ASC" };
        let sql = format!(
            "WITH candidates AS (
                SELECT a.id AS asset_id,
                    CASE WHEN EXISTS (SELECT 1 FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready') THEN (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = a.id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready' ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)) ELSE NULL END AS face_count,
                    COALESCE(decision.rating, 0) AS rating, decision.decision AS decision,
                    (SELECT quality.sharpness_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1) AS sharpness_band,
                    (SELECT quality.blur_level FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1) AS blur_level,
                    (SELECT quality.technical_quality_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1) AS technical_quality_band,
                    (SELECT quality.technical_quality_score FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1) AS technical_quality_score,
                    metadata.camera_model AS camera_model,
                    COALESCE(metadata.captured_at_local, a.captured_at, a.created_at) AS captured_at
             FROM media_assets a
             LEFT JOIN media_decisions decision ON decision.media_asset_id = a.id AND decision.project_id = a.project_id
             LEFT JOIN media_metadata metadata ON metadata.media_asset_id = a.id
                 WHERE a.project_id = ?
                   AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')
             )
             SELECT asset_id, NULL AS embedding_blob, NULL AS dimensions, face_count, rating, decision,
                    sharpness_band, blur_level, technical_quality_band, technical_quality_score,
                    camera_model, captured_at
             FROM candidates {where_clause}
             ORDER BY ({sort_column} IS NULL) ASC, {sort_column} {direction}, asset_id ASC
             LIMIT ? OFFSET ?"
        );
        bound.push(i64::from(query.limit.clamp(1, 250)).into());
        bound.push(i64::from(query.offset).into());
        let mut statement = self.connection.prepare(&sql)?;
        let candidates = statement
            .query_map(rusqlite::params_from_iter(bound), |row| {
                Ok(SemanticSearchCandidate {
                    asset_id: row.get(0)?,
                    vector: None,
                    face_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    rating: row.get::<_, i64>(4)? as u8,
                    decision: row.get(5)?,
                    sharpness_band: row.get(6)?,
                    blur_level: row.get(7)?,
                    technical_quality_band: row.get(8)?,
                    technical_quality_score: row.get(9)?,
                    camera_model: row.get(10)?,
                    captured_at: row.get(11)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn upsert_semantic_index_version(&self, version: &SemanticIndexVersion) -> Result<()> {
        self.connection.execute(
            "INSERT INTO semantic_index_versions (id, project_id, model_id, provider, model_version, embedding_version, preprocessing_version, metric, dimensions, index_format, index_relative_path, index_checksum, embedding_count, status, stale, created_at, rebuilt_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) ON CONFLICT(project_id, model_id, provider, model_version, embedding_version, preprocessing_version, metric, index_format) DO UPDATE SET index_relative_path = excluded.index_relative_path, index_checksum = excluded.index_checksum, embedding_count = excluded.embedding_count, status = excluded.status, stale = excluded.stale, rebuilt_at = excluded.rebuilt_at",
            params![version.id.to_string(), version.project_id.to_string(), version.model.model_id, version.model.provider, version.model.model_version, version.model.embedding_version, version.model.preprocessing_version, version.model.metric, version.model.dimensions as i64, version.index_format, version.index_relative_path, version.index_checksum, version.embedding_count as i64, version.status, version.stale, timestamp(&version.created_at), timestamp(&version.rebuilt_at)],
        )?;
        Ok(())
    }

    fn active_semantic_index_version(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Option<SemanticIndexVersion>> {
        self.connection.query_row(
            "SELECT id, project_id, model_id, provider, model_version, embedding_version, preprocessing_version, metric, dimensions, index_format, index_relative_path, index_checksum, embedding_count, status, stale, created_at, rebuilt_at FROM semantic_index_versions WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND dimensions = ?8 AND status = 'ready' AND stale = 0 ORDER BY rebuilt_at DESC, id DESC LIMIT 1",
            params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric, model.dimensions as i64],
            semantic_index_version_from_row,
        ).optional()?.map(Ok).transpose()
    }

    fn mark_other_semantic_index_versions_stale(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE semantic_index_versions SET stale = 1, status = 'stale' WHERE project_id = ?1 AND stale = 0 AND (model_id <> ?2 OR provider <> ?3 OR model_version <> ?4 OR embedding_version <> ?5 OR preprocessing_version <> ?6 OR metric <> ?7 OR dimensions <> ?8)",
            params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric, model.dimensions as i64],
        )?;
        Ok(changed as u64)
    }

    fn record_magic_search_history(
        &self,
        project_id: &ProjectId,
        query_text: &str,
        normalized_query: &str,
        plan: &serde_json::Value,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO magic_search_history (id, project_id, query_text, normalized_query, plan_json, used_at, use_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1) ON CONFLICT(project_id, normalized_query) DO UPDATE SET query_text = excluded.query_text, plan_json = excluded.plan_json, used_at = excluded.used_at, use_count = magic_search_history.use_count + 1",
            params![MagicSearchHistoryId::new().to_string(), project_id.to_string(), query_text, normalized_query, json(plan)?, timestamp(&Utc::now())],
        )?;
        Ok(())
    }

    fn magic_search_history(
        &self,
        project_id: &ProjectId,
        limit: u32,
    ) -> Result<Vec<MagicSearchHistoryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, query_text, normalized_query, plan_json, used_at, use_count FROM magic_search_history WHERE project_id = ?1 ORDER BY used_at DESC, id DESC LIMIT ?2",
        )?;
        let history = statement
            .query_map(
                params![project_id.to_string(), limit.clamp(1, 100) as i64],
                |row| {
                    let plan: String = row.get(4)?;
                    let plan = serde_json::from_str(&plan).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(MagicSearchHistoryEntry {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        query_text: row.get(2)?,
                        normalized_query: row.get(3)?,
                        plan,
                        used_at: row.get(5)?,
                        use_count: row.get::<_, i64>(6)? as u64,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        history
    }

    fn clear_magic_search_history(&self, project_id: &ProjectId) -> Result<u64> {
        Ok(self.connection.execute(
            "DELETE FROM magic_search_history WHERE project_id = ?1",
            params![project_id.to_string()],
        )? as u64)
    }

    fn latest_semantic_indexing_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>> {
        self.connection.query_row(
            "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at FROM background_jobs WHERE project_id = ?1 AND resume_metadata_json LIKE '%\"pipeline\":\"semantic-indexing\"%' ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            params![project_id.to_string()], background_job_tuple,
        ).optional()?.map(background_job_from_tuple).transpose()
    }

    fn semantic_index_terminal_counts(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<SemanticIndexTerminalCounts> {
        let mut statement = self.connection.prepare(
            "WITH ranked AS (SELECT status, stale, ROW_NUMBER() OVER (PARTITION BY media_asset_id ORDER BY generated_at DESC) AS position FROM semantic_embeddings WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND dimensions = ?8) SELECT CASE WHEN stale = 1 THEN 'stale' ELSE status END, COUNT(*) FROM ranked WHERE position = 1 GROUP BY CASE WHEN stale = 1 THEN 'stale' ELSE status END",
        )?;
        let mut counts = SemanticIndexTerminalCounts::default();
        for row in statement.query_map(
            params![
                project_id.to_string(),
                model.model_id,
                model.provider,
                model.model_version,
                model.embedding_version,
                model.preprocessing_version,
                model.metric,
                model.dimensions as i64
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
        )? {
            let (status, count) = row?;
            counts.total += count;
            match status.as_str() {
                "ready" => counts.ready = count,
                "unsupported" => counts.unsupported = count,
                "corrupt" => counts.corrupt = count,
                "needs_original" => counts.needs_original = count,
                "failed" => counts.failed = count,
                "stale" => counts.stale = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    fn recover_interrupted_semantic_indexing(&self) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"semantic_index\"', error_message = COALESCE(error_message, 'Magic Search indexing was interrupted before completion; resume local indexing to continue.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"semantic-indexing\"%'",
            params![timestamp(&Utc::now())],
        )?;
        Ok(changed as u64)
    }

    fn moment_analysis_inputs(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
    ) -> Result<Vec<MomentAnalysisInput>> {
        self.moment_analysis_inputs_for_predicate(
            project_id,
            model,
            "",
            Vec::new(),
            "(COALESCE(metadata.captured_at_local, a.captured_at) IS NULL) ASC, COALESCE(metadata.captured_at_local, a.captured_at) ASC, a.id ASC",
        )
    }

    fn moment_incremental_analysis_window(
        &self,
        project_id: &ProjectId,
        model: Option<&SemanticModelConfig>,
    ) -> Result<Option<MomentIncrementalAnalysisWindow>> {
        let active_timeline = self
            .connection
            .query_row(
                "SELECT id, active_run_id, semantic_model_key FROM shoot_timelines WHERE project_id = ?1",
                params![project_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((timeline_id, Some(active_run_id), active_semantic_model_key)) = active_timeline
        else {
            return Ok(None);
        };

        // Whole Moment IDs are selected first, so the bounded context cannot start midway
        // through a structural group. Two completed/current Moments provide enough continuity
        // context for an append-only tail without loading historic project vectors.
        let mut recent_moments = self
            .connection
            .prepare(
                "SELECT record.id, record.ordinal
                 FROM moment_records record
                 WHERE record.project_id = ?1
                   AND record.timeline_id = ?2
                   AND record.stale = 0
                   AND EXISTS (
                       SELECT 1 FROM moment_memberships member
                       WHERE member.project_id = record.project_id
                         AND member.moment_id = record.id
                         AND member.active = 1
                         AND member.membership_state = 'member'
                   )
                 ORDER BY record.ordinal DESC, record.id DESC
                 LIMIT 2",
            )?
            .query_map(params![project_id.to_string(), timeline_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        recent_moments
            .sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        let context_moment_ids: Vec<String> = recent_moments
            .iter()
            .map(|(moment_id, _)| moment_id.clone())
            .collect();

        let maximum_active_ordinal: Option<i64> = self.connection.query_row(
            "SELECT MAX(ordinal) FROM moment_memberships
             WHERE project_id = ?1 AND active = 1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        let global_ordinal_base = match maximum_active_ordinal {
            Some(value) => u64::try_from(value)
                .map_err(|_| {
                    PersistenceError::InvalidData(
                        "active Moment membership ordinal must be non-negative".into(),
                    )
                })?
                .saturating_add(1),
            None => 0,
        };

        let affected_tail_start_ordinal = if context_moment_ids.is_empty() {
            global_ordinal_base
        } else {
            let placeholders = std::iter::repeat_n("?", context_moment_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT MIN(ordinal) FROM moment_memberships
                 WHERE project_id = ? AND active = 1 AND membership_state = 'member'
                   AND moment_id IN ({placeholders})"
            );
            let mut parameters = vec![rusqlite::types::Value::from(project_id.to_string())];
            parameters.extend(context_moment_ids.iter().cloned().map(Into::into));
            let minimum: Option<i64> =
                self.connection
                    .query_row(&sql, rusqlite::params_from_iter(parameters), |row| {
                        row.get(0)
                    })?;
            match minimum {
                Some(value) => u64::try_from(value).map_err(|_| {
                    PersistenceError::InvalidData(
                        "active Moment membership ordinal must be non-negative".into(),
                    )
                })?,
                None => global_ordinal_base,
            }
        };

        let moment_ordinal_base: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(record.ordinal), -1) + 1
             FROM moment_records record
             WHERE record.project_id = ?1
               AND record.timeline_id = ?2
               AND record.stale = 0
               AND NOT EXISTS (
                   SELECT 1 FROM moment_memberships member
                   WHERE member.project_id = record.project_id
                     AND member.moment_id = record.id
                     AND member.active = 1
                     AND member.membership_state = 'member'
                     AND member.ordinal >= ?3
               )",
            params![
                project_id.to_string(),
                timeline_id,
                affected_tail_start_ordinal as i64
            ],
            |row| row.get(0),
        )?;
        let moment_ordinal_base = u64::try_from(moment_ordinal_base).map_err(|_| {
            PersistenceError::InvalidData(
                "preserved active Moment ordinal must be non-negative".into(),
            )
        })?;

        let previous_latest_captured_at = self
            .connection
            .query_row(
                "SELECT COALESCE(metadata.captured_at_local, asset.captured_at)
                 FROM moment_memberships member
                 JOIN media_assets asset ON asset.id = member.media_asset_id
                 LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id
                 WHERE member.project_id = ?1
                   AND member.active = 1
                   AND member.membership_state = 'member'
                   AND COALESCE(metadata.captured_at_local, asset.captured_at) IS NOT NULL
                 ORDER BY member.ordinal DESC, member.media_asset_id DESC
                 LIMIT 1",
                params![project_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        // The analysis-input query operates on media asset IDs, whereas the query above chose
        // Moment IDs to guarantee a whole-group context. Resolve the active member assets here
        // instead of accidentally comparing media IDs with Moment-record IDs.
        let context_asset_ids = if context_moment_ids.is_empty() {
            Vec::new()
        } else {
            let placeholders = std::iter::repeat_n("?", context_moment_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT member.media_asset_id
                 FROM moment_memberships member
                 WHERE member.project_id = ?
                   AND member.active = 1
                   AND member.membership_state = 'member'
                   AND member.moment_id IN ({placeholders})
                 GROUP BY member.media_asset_id
                 ORDER BY MIN(member.ordinal) ASC, member.media_asset_id ASC"
            );
            let mut parameters = vec![rusqlite::types::Value::from(project_id.to_string())];
            parameters.extend(context_moment_ids.iter().cloned().map(Into::into));
            self.connection
                .prepare(&sql)?
                .query_map(rusqlite::params_from_iter(parameters), |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?
        };

        let preceding_context = if context_asset_ids.is_empty() {
            Vec::new()
        } else {
            let first_extra_parameter = if model.is_some() { 9 } else { 2 };
            let placeholders = (0..context_asset_ids.len())
                .map(|offset| format!("?{}", first_extra_parameter + offset))
                .collect::<Vec<_>>()
                .join(",");
            self.moment_analysis_inputs_for_predicate(
                project_id,
                model,
                &format!(" AND a.id IN ({placeholders})"),
                context_asset_ids.into_iter().map(Into::into).collect(),
                "(COALESCE(metadata.captured_at_local, a.captured_at) IS NULL) ASC, COALESCE(metadata.captured_at_local, a.captured_at) ASC, a.id ASC",
            )?
        };

        // Do not pull active members again. The second clause deliberately includes durable
        // `ungrouped` results so a photo that later gains a usable capture timestamp or local
        // embedding can participate in an explicit update without being lost.
        let pending_inputs = self.moment_analysis_inputs_for_predicate(
            project_id,
            model,
            " AND (
                NOT EXISTS (
                    SELECT 1 FROM moment_memberships active_member
                    WHERE active_member.project_id = a.project_id
                      AND active_member.media_asset_id = a.id
                      AND active_member.active = 1
                      AND active_member.membership_state = 'member'
                )
                OR EXISTS (
                    SELECT 1 FROM moment_memberships active_ungrouped
                    WHERE active_ungrouped.project_id = a.project_id
                      AND active_ungrouped.media_asset_id = a.id
                      AND active_ungrouped.active = 1
                      AND active_ungrouped.membership_state = 'ungrouped'
                )
            )",
            Vec::new(),
            "(COALESCE(metadata.captured_at_local, a.captured_at) IS NULL) ASC, COALESCE(metadata.captured_at_local, a.captured_at) ASC, a.id ASC",
        )?;

        Ok(Some(MomentIncrementalAnalysisWindow {
            timeline_id,
            active_run_id,
            active_semantic_model_key,
            previous_latest_captured_at,
            global_ordinal_base,
            affected_tail_start_ordinal,
            moment_ordinal_base,
            preceding_context,
            pending_inputs,
        }))
    }

    fn active_moment_override_operations(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<MomentOverrideOperation>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, operation, left_asset_id, right_asset_id, created_at, active
             FROM moment_override_operations
             WHERE project_id = ?1 AND active = 1
             ORDER BY created_at ASC, id ASC",
        )?;
        let overrides = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok(MomentOverrideOperation {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    operation: row.get(2)?,
                    left_asset_id: row.get(3)?,
                    right_asset_id: row.get(4)?,
                    created_at: row.get(5)?,
                    active: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        overrides
    }

    fn replace_active_moment_analysis(
        &self,
        timeline: &MomentTimelineStatusRecord,
        run: &MomentAnalysisRunRecord,
        segments: &[TimelineSegmentRecord],
        moments: &[MomentRecord],
        memberships: &[MomentMembershipRecord],
        boundaries: &[MomentBoundaryEvidenceRecord],
    ) -> Result<()> {
        if timeline.project_id != run.project_id {
            return Err(PersistenceError::InvalidData(
                "timeline and analysis run project IDs differ".into(),
            ));
        }
        let referenced_assets = validate_moment_projection_payload(
            timeline,
            run,
            Some(0),
            0,
            segments,
            moments,
            memberships,
            boundaries,
        )?;
        let selected_project = ProjectId::try_from(timeline.project_id.as_str())
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
        let transaction = moment_write_transaction(&self.connection)?;
        assert_moment_assets_belong_to_project(
            &transaction,
            &selected_project,
            &referenced_assets,
        )?;
        transaction.execute(
            "UPDATE moment_memberships SET active = 0 WHERE project_id = ?1 AND active = 1",
            params![timeline.project_id],
        )?;
        transaction.execute(
            "UPDATE moment_records SET stale = 1 WHERE project_id = ?1 AND stale = 0",
            params![timeline.project_id],
        )?;
        transaction.execute(
            "UPDATE timeline_segments SET stale = 1 WHERE project_id = ?1 AND stale = 0",
            params![timeline.project_id],
        )?;
        assert_active_moment_projection_contiguous(
            &transaction,
            &timeline.project_id,
            moments,
            memberships,
        )?;
        transaction.execute(
            "INSERT INTO shoot_timelines (id, project_id, active_run_id, state, analyzer_id, analyzer_version, boundary_algorithm_version, semantic_model_key, input_catalog_version, created_at, updated_at, last_analyzed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)
             ON CONFLICT(project_id) DO UPDATE SET id = excluded.id, active_run_id = excluded.active_run_id, state = excluded.state, analyzer_id = excluded.analyzer_id, analyzer_version = excluded.analyzer_version, boundary_algorithm_version = excluded.boundary_algorithm_version, semantic_model_key = excluded.semantic_model_key, input_catalog_version = excluded.input_catalog_version, updated_at = excluded.updated_at, last_analyzed_at = excluded.last_analyzed_at",
            params![timeline.timeline_id, timeline.project_id, run.id, timeline.state, timeline.analyzer_id, timeline.analyzer_version, timeline.boundary_algorithm_version, timeline.semantic_model_key, timeline.input_catalog_version, timeline.updated_at],
        )?;
        transaction.execute(
            "INSERT INTO moment_analysis_runs (id, timeline_id, project_id, state, analyzer_id, analyzer_version, boundary_algorithm_version, semantic_model_key, input_catalog_version, items_total, items_completed, error_count, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![run.id, run.timeline_id, run.project_id, run.state, run.analyzer_id, run.analyzer_version, run.boundary_algorithm_version, run.semantic_model_key, run.input_catalog_version, run.items_total as i64, run.items_completed as i64, run.error_count as i64, run.started_at, run.finished_at],
        )?;
        for segment in segments {
            transaction.execute(
                "INSERT INTO timeline_segments (id, project_id, run_id, ordinal, started_at, ended_at, asset_count, boundary_category, boundary_evidence_json, created_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![segment.id, segment.project_id, segment.run_id, segment.ordinal as i64, segment.started_at, segment.ended_at, segment.asset_count as i64, segment.boundary_category, json(&segment.boundary_evidence)?, segment.created_at, segment.stale],
            )?;
        }
        for moment in moments {
            let centroid = moment
                .centroid
                .as_ref()
                .map(|values| encode_semantic_vector(values, values.len()))
                .transpose()?;
            transaction.execute(
                "INSERT INTO moment_records (id, project_id, timeline_id, run_id, segment_id, anchor_asset_id, ordinal, started_at, ended_at, asset_count, ai_representative_asset_id, centroid_blob, centroid_dimensions, suggested_label, label_confidence, label_evidence_json, label_state, created_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![moment.id, moment.project_id, moment.timeline_id, moment.run_id, moment.segment_id, moment.anchor_asset_id, moment.ordinal as i64, moment.started_at, moment.ended_at, moment.asset_count as i64, moment.ai_representative_asset_id, centroid, moment.centroid_dimensions.map(|value| value as i64), moment.suggested_label, moment.label_confidence, json(&moment.label_evidence)?, moment.label_state, moment.created_at, moment.stale],
            )?;
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_CREATED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), moment.project_id, moment.id, json(&serde_json::json!({"source":"local_analysis", "runId": moment.run_id}))?, moment.created_at],
            )?;
        }
        for membership in memberships {
            transaction.execute(
                "INSERT INTO moment_memberships (id, project_id, run_id, moment_id, media_asset_id, ordinal, membership_state, created_at, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![membership.id, membership.project_id, membership.run_id, membership.moment_id, membership.media_asset_id, membership.ordinal as i64, membership.membership_state, membership.created_at, membership.active],
            )?;
        }
        remap_human_labels_to_active_moments(&transaction, &timeline.project_id)?;
        remap_coverage_confirmations_to_active_moments(&transaction, &timeline.project_id)?;
        remap_human_representatives_to_active_moments(&transaction, &timeline.project_id)?;
        for boundary in boundaries {
            transaction.execute(
                "INSERT INTO moment_boundary_evidence (id, project_id, run_id, left_asset_id, right_asset_id, ordinal, category, components_json, explanation, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![boundary.id, boundary.project_id, boundary.run_id, boundary.left_asset_id, boundary.right_asset_id, boundary.ordinal as i64, boundary.category, json(&boundary.components)?, boundary.explanation, boundary.created_at],
            )?;
        }
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale'
             WHERE project_id = ?1 AND stale = 0",
            params![timeline.project_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_active_moment_analysis_tail(
        &self,
        timeline: &MomentTimelineStatusRecord,
        run: &MomentAnalysisRunRecord,
        affected_tail_start_ordinal: u64,
        segments: &[TimelineSegmentRecord],
        moments: &[MomentRecord],
        memberships: &[MomentMembershipRecord],
        boundaries: &[MomentBoundaryEvidenceRecord],
    ) -> Result<()> {
        let expected_active_run_id = timeline.active_run_id.as_deref().ok_or_else(|| {
            PersistenceError::InvalidData(
                "tail replacement requires an existing active Moment analysis run".into(),
            )
        })?;
        if run.id == expected_active_run_id {
            return Err(PersistenceError::InvalidData(
                "tail replacement must create a distinct Moment analysis run".into(),
            ));
        }
        let tail_start = i64::try_from(affected_tail_start_ordinal).map_err(|_| {
            PersistenceError::InvalidData(
                "affected tail ordinal exceeds SQLite integer range".into(),
            )
        })?;
        let referenced_assets = validate_moment_projection_payload(
            timeline,
            run,
            None,
            affected_tail_start_ordinal,
            segments,
            moments,
            memberships,
            boundaries,
        )?;
        let selected_project = ProjectId::try_from(timeline.project_id.as_str())
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
        // Validate every mutable part of the existing projection while holding the writer
        // reservation. A second catalog connection cannot slip a structural edit between these
        // checks and the replacement below.
        let transaction = moment_write_transaction(&self.connection)?;
        assert_moment_assets_belong_to_project(
            &transaction,
            &selected_project,
            &referenced_assets,
        )?;
        let new_membership_assets: BTreeSet<String> = memberships
            .iter()
            .map(|membership| membership.media_asset_id.clone())
            .collect();

        // A tail writer must never cut an existing active Moment in half. The window method
        // selects whole latest Moment(s), but this check also protects direct callers.
        let crosses_existing_moment: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM moment_memberships earlier
                JOIN moment_memberships later
                  ON later.project_id = earlier.project_id
                 AND later.moment_id = earlier.moment_id
                 AND later.active = 1
                 AND later.membership_state = 'member'
                WHERE earlier.project_id = ?1
                  AND earlier.active = 1
                  AND earlier.membership_state = 'member'
                  AND earlier.ordinal < ?2
                  AND later.ordinal >= ?2
            )",
            params![timeline.project_id, tail_start],
            |row| row.get(0),
        )?;
        if crosses_existing_moment {
            return Err(PersistenceError::InvalidData(
                "affected tail start would split an active Moment; request a wider window or full rebuild"
                    .into(),
            ));
        }

        let old_tail_assets: BTreeSet<String> = transaction
            .prepare(
                "SELECT media_asset_id FROM moment_memberships
                 WHERE project_id = ?1 AND active = 1 AND ordinal >= ?2
                 ORDER BY ordinal ASC, media_asset_id ASC",
            )?
            .query_map(params![timeline.project_id, tail_start], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        if !old_tail_assets.is_subset(&new_membership_assets) {
            return Err(PersistenceError::InvalidData(
                "tail replacement would drop an active membership; use a full rebuild instead"
                    .into(),
            ));
        }

        // A label and representative follow their photographer-selected asset through a
        // resegmented tail. The replacement must retain those assets even when its generated
        // card anchor changes.
        let protected_tail_label_anchors: BTreeSet<String> = transaction
            .prepare(
                "SELECT record.anchor_asset_id
                 FROM moment_records record
                 WHERE record.project_id = ?1
                   AND record.timeline_id = ?2
                   AND record.stale = 0
                   AND EXISTS (
                       SELECT 1 FROM moment_memberships member
                       WHERE member.project_id = record.project_id
                         AND member.moment_id = record.id
                         AND member.active = 1
                         AND member.membership_state = 'member'
                         AND member.ordinal >= ?3
                   )
                   AND EXISTS (
                       SELECT 1
                       FROM moment_human_labels label
                       WHERE label.project_id = record.project_id
                         AND label.anchor_asset_id = record.anchor_asset_id
                   )",
            )?
            .query_map(
                params![timeline.project_id, timeline.timeline_id, tail_start],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<_, _>>()?;
        if !protected_tail_label_anchors.is_subset(&new_membership_assets) {
            return Err(PersistenceError::InvalidData(
                "tail replacement would orphan a human Moment label anchor asset; use a full rebuild with that asset"
                    .into(),
            ));
        }
        let protected_tail_representative_assets: BTreeSet<String> = transaction
            .prepare(
                "SELECT representative.media_asset_id
                 FROM moment_human_representatives representative
                 JOIN moment_records record
                   ON record.project_id = representative.project_id
                  AND record.anchor_asset_id = representative.anchor_asset_id
                 WHERE representative.project_id = ?1
                   AND record.timeline_id = ?2
                   AND record.stale = 0
                   AND EXISTS (
                       SELECT 1 FROM moment_memberships member
                       WHERE member.project_id = record.project_id
                         AND member.moment_id = record.id
                         AND member.active = 1
                         AND member.membership_state = 'member'
                         AND member.ordinal >= ?3
                   )",
            )?
            .query_map(
                params![timeline.project_id, timeline.timeline_id, tail_start],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<_, _>>()?;
        if !protected_tail_representative_assets.is_subset(&new_membership_assets) {
            return Err(PersistenceError::InvalidData(
                "tail replacement would orphan a photographer-selected representative; use a full rebuild with that selected asset"
                    .into(),
            ));
        }
        transaction.execute(
            "UPDATE timeline_segments
             SET stale = 1
             WHERE project_id = ?1
               AND stale = 0
               AND EXISTS (
                   SELECT 1
                   FROM moment_records record
                   JOIN moment_memberships member ON member.moment_id = record.id
                   WHERE record.segment_id = timeline_segments.id
                     AND record.project_id = timeline_segments.project_id
                     AND record.stale = 0
                     AND member.project_id = timeline_segments.project_id
                     AND member.active = 1
                     AND member.membership_state = 'member'
                     AND member.ordinal >= ?2
               )",
            params![timeline.project_id, tail_start],
        )?;
        transaction.execute(
            "UPDATE moment_records
             SET stale = 1
             WHERE project_id = ?1
               AND timeline_id = ?2
               AND stale = 0
               AND EXISTS (
                   SELECT 1 FROM moment_memberships member
                   WHERE member.project_id = moment_records.project_id
                     AND member.moment_id = moment_records.id
                     AND member.active = 1
                     AND member.membership_state = 'member'
                     AND member.ordinal >= ?3
               )",
            params![timeline.project_id, timeline.timeline_id, tail_start],
        )?;
        transaction.execute(
            "UPDATE moment_memberships
             SET active = 0
             WHERE project_id = ?1 AND active = 1 AND ordinal >= ?2",
            params![timeline.project_id, tail_start],
        )?;
        assert_active_moment_projection_contiguous(
            &transaction,
            &timeline.project_id,
            moments,
            memberships,
        )?;

        for moment in moments {
            let collides_with_preserved_active: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM moment_records
                    WHERE project_id = ?1 AND timeline_id = ?2 AND stale = 0 AND ordinal = ?3
                )",
                params![
                    timeline.project_id,
                    timeline.timeline_id,
                    moment.ordinal as i64
                ],
                |row| row.get(0),
            )?;
            if collides_with_preserved_active {
                return Err(PersistenceError::InvalidData(
                    "tail Moment ordinal collides with preserved active projection; core must offset tail Moment ordinals"
                        .into(),
                ));
            }
        }

        let timeline_updated = transaction.execute(
            "UPDATE shoot_timelines
             SET active_run_id = ?1,
                 state = ?2,
                 analyzer_id = ?3,
                 analyzer_version = ?4,
                 boundary_algorithm_version = ?5,
                 semantic_model_key = ?6,
                 input_catalog_version = ?7,
                 updated_at = ?8,
                 last_analyzed_at = ?8
             WHERE id = ?9 AND project_id = ?10 AND active_run_id = ?11",
            params![
                run.id,
                timeline.state,
                timeline.analyzer_id,
                timeline.analyzer_version,
                timeline.boundary_algorithm_version,
                timeline.semantic_model_key,
                timeline.input_catalog_version,
                timeline.updated_at,
                timeline.timeline_id,
                timeline.project_id,
                expected_active_run_id,
            ],
        )?;
        if timeline_updated != 1 {
            return Err(PersistenceError::InvalidData(
                "active Moment timeline changed before tail replacement; request a fresh window"
                    .into(),
            ));
        }
        transaction.execute(
            "INSERT INTO moment_analysis_runs (id, timeline_id, project_id, state, analyzer_id, analyzer_version, boundary_algorithm_version, semantic_model_key, input_catalog_version, items_total, items_completed, error_count, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![run.id, run.timeline_id, run.project_id, run.state, run.analyzer_id, run.analyzer_version, run.boundary_algorithm_version, run.semantic_model_key, run.input_catalog_version, run.items_total as i64, run.items_completed as i64, run.error_count as i64, run.started_at, run.finished_at],
        )?;
        for segment in segments {
            transaction.execute(
                "INSERT INTO timeline_segments (id, project_id, run_id, ordinal, started_at, ended_at, asset_count, boundary_category, boundary_evidence_json, created_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![segment.id, segment.project_id, segment.run_id, segment.ordinal as i64, segment.started_at, segment.ended_at, segment.asset_count as i64, segment.boundary_category, json(&segment.boundary_evidence)?, segment.created_at, segment.stale],
            )?;
        }
        for moment in moments {
            let centroid = moment
                .centroid
                .as_ref()
                .map(|values| encode_semantic_vector(values, values.len()))
                .transpose()?;
            transaction.execute(
                "INSERT INTO moment_records (id, project_id, timeline_id, run_id, segment_id, anchor_asset_id, ordinal, started_at, ended_at, asset_count, ai_representative_asset_id, centroid_blob, centroid_dimensions, suggested_label, label_confidence, label_evidence_json, label_state, created_at, stale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![moment.id, moment.project_id, moment.timeline_id, moment.run_id, moment.segment_id, moment.anchor_asset_id, moment.ordinal as i64, moment.started_at, moment.ended_at, moment.asset_count as i64, moment.ai_representative_asset_id, centroid, moment.centroid_dimensions.map(|value| value as i64), moment.suggested_label, moment.label_confidence, json(&moment.label_evidence)?, moment.label_state, moment.created_at, moment.stale],
            )?;
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_CREATED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), moment.project_id, moment.id, json(&serde_json::json!({"source":"local_incremental_analysis", "runId": moment.run_id}))?, moment.created_at],
            )?;
        }
        for membership in memberships {
            transaction.execute(
                "INSERT INTO moment_memberships (id, project_id, run_id, moment_id, media_asset_id, ordinal, membership_state, created_at, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![membership.id, membership.project_id, membership.run_id, membership.moment_id, membership.media_asset_id, membership.ordinal as i64, membership.membership_state, membership.created_at, membership.active],
            )?;
        }
        remap_human_labels_to_active_moments(&transaction, &timeline.project_id)?;
        remap_coverage_confirmations_to_active_moments(&transaction, &timeline.project_id)?;
        remap_human_representatives_to_active_moments(&transaction, &timeline.project_id)?;
        for boundary in boundaries {
            transaction.execute(
                "INSERT INTO moment_boundary_evidence (id, project_id, run_id, left_asset_id, right_asset_id, ordinal, category, components_json, explanation, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![boundary.id, boundary.project_id, boundary.run_id, boundary.left_asset_id, boundary.right_asset_id, boundary.ordinal as i64, boundary.category, json(&boundary.components)?, boundary.explanation, boundary.created_at],
            )?;
        }
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale'
             WHERE project_id = ?1 AND stale = 0",
            params![timeline.project_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn moment_timeline_status(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<MomentTimelineStatusRecord>> {
        self.connection.query_row(
            "SELECT t.id, t.project_id, t.state, t.analyzer_id, t.analyzer_version, t.boundary_algorithm_version, t.semantic_model_key, t.input_catalog_version, t.active_run_id,
                (SELECT COUNT(*) FROM moment_records m WHERE m.project_id = t.project_id AND m.timeline_id = t.id AND m.stale = 0),
                (SELECT COUNT(*) FROM moment_memberships mm WHERE mm.project_id = t.project_id AND mm.active = 1),
                (SELECT COUNT(*) FROM moment_memberships mm WHERE mm.project_id = t.project_id AND mm.active = 1 AND mm.membership_state = 'ungrouped'),
                t.updated_at
             FROM shoot_timelines t WHERE t.project_id = ?1",
            params![project_id.to_string()],
            |row| Ok(MomentTimelineStatusRecord {
                timeline_id: row.get(0)?, project_id: row.get(1)?, state: row.get(2)?, analyzer_id: row.get(3)?, analyzer_version: row.get(4)?, boundary_algorithm_version: row.get(5)?, semantic_model_key: row.get(6)?, input_catalog_version: row.get(7)?, active_run_id: row.get(8)?, moment_count: row.get::<_, i64>(9)? as u64, eligible_count: row.get::<_, i64>(10)? as u64, ungrouped_count: row.get::<_, i64>(11)? as u64, updated_at: row.get(12)?,
            }),
        ).optional().map_err(Into::into)
    }

    fn moment_timeline_page(
        &self,
        project_id: &ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<MomentTimelinePage> {
        let limit = limit.clamp(1, 120);
        let mut moments = moment_timeline_rows(
            &self.connection,
            project_id,
            None,
            limit.saturating_add(1),
            offset,
        )?;
        let has_more = moments.len() > limit as usize;
        moments.truncate(limit as usize);
        Ok(MomentTimelinePage {
            timeline: self.moment_timeline_status(project_id)?,
            gaps: timeline_gaps(&self.connection, project_id)?,
            moments,
            has_more,
        })
    }

    fn moment_detail(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
    ) -> Result<Option<MomentDetailRecord>> {
        let Some(moment) =
            moment_timeline_rows(&self.connection, project_id, Some(moment_id), 1, 0)?
                .into_iter()
                .next()
        else {
            return Ok(None);
        };
        let evidence_json = self.connection.query_row(
            "SELECT label_evidence_json FROM moment_records WHERE id = ?1 AND project_id = ?2 AND stale = 0",
            params![moment_id, project_id.to_string()], |row| row.get::<_, String>(0),
        )?;
        let evidence: serde_json::Value = serde_json::from_str(&evidence_json)?;
        let label_evidence = evidence
            .get("concepts")
            .and_then(serde_json::Value::as_array)
            .map(|concepts| {
                concepts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(MomentDetailRecord {
            membership_count: moment.asset_count,
            has_human_label: moment.human_label.is_some(),
            has_human_representative: moment.human_representative_asset_id.is_some(),
            moment,
            label_evidence,
        }))
    }

    fn moment_belongs_to_project(&self, project_id: &ProjectId, moment_id: &str) -> Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM moment_records WHERE id = ?1 AND project_id = ?2 AND stale = 0)",
            params![moment_id, project_id.to_string()], |row| row.get(0),
        ).map_err(Into::into)
    }

    fn moment_contains_asset(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_id: &str,
    ) -> Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND media_asset_id = ?3 AND active = 1 AND membership_state = 'member')",
            params![project_id.to_string(), moment_id, asset_id], |row| row.get(0),
        ).map_err(Into::into)
    }

    fn filter_active_moment_assets(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<String>> {
        if !self.moment_belongs_to_project(project_id, moment_id)? {
            return Err(PersistenceError::InvalidData(
                "moment does not belong to the selected project".into(),
            ));
        }
        if asset_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", asset_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT media_asset_id FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member' AND media_asset_id IN ({placeholders})"
        );
        let mut bound = vec![
            rusqlite::types::Value::from(project_id.to_string()),
            moment_id.to_owned().into(),
        ];
        bound.extend(asset_ids.iter().cloned().map(Into::into));
        self.connection
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(bound), |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn rename_moment(&self, project_id: &ProjectId, moment_id: &str, label: &str) -> Result<()> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 160 {
            return Err(PersistenceError::InvalidData(
                "moment label must contain 1 through 160 characters".into(),
            ));
        }
        let now = timestamp(&Utc::now());
        let transaction = moment_write_transaction(&self.connection)?;
        let anchor = moment_anchor_asset_id(&transaction, project_id, moment_id)?;
        transaction.execute(
            "INSERT INTO moment_human_labels (id, project_id, anchor_asset_id, label, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(project_id, anchor_asset_id) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at",
            params![Uuid::new_v4().to_string(), project_id.to_string(), anchor, label, now],
        )?;
        transaction.execute(
            "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_RENAMED', ?4, ?5)",
            params![Uuid::new_v4().to_string(), project_id.to_string(), moment_id, json(&serde_json::json!({"label": label}))?, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn set_moment_human_representative(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        asset_id: &str,
    ) -> Result<()> {
        // Validate the user-visible target before setting a guard so an invalid request cannot
        // leave an otherwise healthy local profile waiting for restart recovery.
        if !self.moment_belongs_to_project(project_id, moment_id)? {
            return Err(PersistenceError::InvalidData(
                "moment does not belong to the selected project".into(),
            ));
        }
        let member_belongs_to_moment: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND media_asset_id = ?3 AND active = 1 AND membership_state = 'member')",
            params![project_id.to_string(), moment_id, asset_id],
            |row| row.get(0),
        )?;
        if !member_belongs_to_moment {
            return Err(PersistenceError::InvalidData(
                "human representative must belong to the selected moment".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let anchor = moment_anchor_asset_id(&transaction, project_id, moment_id)?;
        let member_belongs_to_moment: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND media_asset_id = ?3 AND active = 1 AND membership_state = 'member')",
            params![project_id.to_string(), moment_id, asset_id],
            |row| row.get(0),
        )?;
        if !member_belongs_to_moment {
            return Err(PersistenceError::InvalidData(
                "human representative must belong to the selected moment".into(),
            ));
        }
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT INTO moment_human_representatives (id, project_id, anchor_asset_id, media_asset_id, selected_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id, anchor_asset_id) DO UPDATE SET media_asset_id = excluded.media_asset_id, selected_at = excluded.selected_at",
            params![Uuid::new_v4().to_string(), project_id.to_string(), anchor, asset_id, now],
        )?;
        let moment_event_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_REPRESENTATIVE_CHANGED', ?4, ?5)",
            params![moment_event_id, project_id.to_string(), moment_id, json(&serde_json::json!({"assetId": asset_id}))?, now],
        )?;
        transaction.commit()?;
        let mut source_materialized = false;
        let mut captured_profile_id = None;
        if let Ok(profile_id) = self.ensure_default_studio_profile() {
            captured_profile_id = Some(profile_id.clone());
            match materialize_live_moment_representative(
                &self.connection,
                &profile_id,
                &moment_event_id,
                &project_id.to_string(),
                moment_id,
                asset_id,
                &now,
            ) {
                Ok(()) => source_materialized = true,
                Err(_) => mark_studio_source_capture_deferred(&self.connection, &profile_id),
            }
        }
        if source_materialized {
            if let Some(profile_id) = captured_profile_id.as_deref() {
                let _ = finish_studio_source_materialization(
                    &self.connection,
                    profile_id,
                    "moment_representative",
                    &moment_event_id,
                );
            }
        }
        Ok(())
    }

    fn merge_adjacent_moments(
        &self,
        project_id: &ProjectId,
        left_moment_id: &str,
        right_moment_id: &str,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let left = moment_record_for_edit(&transaction, project_id, left_moment_id)?;
        let right = moment_record_for_edit(&transaction, project_id, right_moment_id)?;
        if left.0 != right.0 || right.3 != left.3 + 1 {
            return Err(PersistenceError::InvalidData(
                "only adjacent active moments can be merged".into(),
            ));
        }
        if left.1 != right.1 {
            return Err(PersistenceError::InvalidData(
                "These adjacent Moments are from different local analysis runs. Rebuild AI timeline before merging across this boundary.".into(),
            ));
        }
        rebind_human_label_for_active_moment(
            &transaction,
            project_id,
            &right.4,
            &left.4,
            left_moment_id,
        )?;
        let left_last = transaction.query_row(
            "SELECT media_asset_id FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member' ORDER BY ordinal DESC, media_asset_id DESC LIMIT 1",
            params![project_id.to_string(), left_moment_id], |row| row.get::<_, String>(0),
        )?;
        let right_first = transaction.query_row(
            "SELECT media_asset_id FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member' ORDER BY ordinal ASC, media_asset_id ASC LIMIT 1",
            params![project_id.to_string(), right_moment_id], |row| row.get::<_, String>(0),
        )?;
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT INTO moment_override_operations (id, project_id, operation, left_asset_id, right_asset_id, created_at, active) VALUES (?1, ?2, 'merge', ?3, ?4, ?5, 1)
             ON CONFLICT(project_id, operation, left_asset_id, right_asset_id) DO UPDATE SET active = 1, created_at = excluded.created_at",
            params![Uuid::new_v4().to_string(), project_id.to_string(), left_last, right_first, now],
        )?;
        transaction.execute(
            "UPDATE moment_memberships SET moment_id = ?3 WHERE project_id = ?1 AND moment_id = ?2 AND active = 1",
            params![project_id.to_string(), right_moment_id, left_moment_id],
        )?;
        if let Some(asset_id) = rebind_human_representative_for_active_moment(
            &transaction,
            project_id,
            &right.4,
            &left.4,
            left_moment_id,
        )? {
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_REPRESENTATIVE_CHANGED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), project_id.to_string(), left_moment_id, json(&serde_json::json!({"assetId": asset_id, "source": "merge_rebind", "previousMomentId": right_moment_id}))?, now],
            )?;
        }
        // A coverage confirmation is a photographer decision about a particular asset. When its
        // selected Moment is absorbed, retain that decision on the surviving active Moment rather
        // than leaving it pointed at a stale generated record.
        transaction.execute(
            "UPDATE coverage_confirmations SET moment_id = ?1 WHERE project_id = ?2 AND moment_id = ?3",
            params![left_moment_id, project_id.to_string(), right_moment_id],
        )?;
        transaction.execute(
            "UPDATE moment_records SET stale = 1 WHERE id = ?1 AND project_id = ?2",
            params![right_moment_id, project_id.to_string()],
        )?;
        transaction.execute(
            "UPDATE timeline_segments SET stale = 1 WHERE id = ?1 AND project_id = ?2",
            params![right.2, project_id.to_string()],
        )?;
        transaction.execute(
            "UPDATE moment_records SET asset_count = (SELECT COUNT(*) FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member'), ended_at = (SELECT COALESCE(metadata.captured_at_local, asset.captured_at) FROM moment_memberships member JOIN media_assets asset ON asset.id = member.media_asset_id LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id WHERE member.project_id = ?1 AND member.moment_id = ?2 AND member.active = 1 ORDER BY member.ordinal DESC LIMIT 1) WHERE id = ?2",
            params![project_id.to_string(), left_moment_id],
        )?;
        let active_order = active_moment_ids_for_project(&transaction, project_id)?;
        reindex_active_moment_projection(&transaction, project_id, &active_order)?;
        transaction.execute(
            "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_MERGED', ?4, ?5)",
            params![Uuid::new_v4().to_string(), project_id.to_string(), left_moment_id, json(&serde_json::json!({"mergedMomentId": right_moment_id}))?, now],
        )?;
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale'
             WHERE project_id = ?1 AND stale = 0",
            params![project_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn split_moment(
        &self,
        project_id: &ProjectId,
        moment_id: &str,
        after_asset_id: &str,
    ) -> Result<()> {
        let transaction = moment_write_transaction(&self.connection)?;
        let (timeline_id, run_id, _segment_id, _moment_ordinal, source_anchor, _started, _ended) =
            moment_record_for_edit(&transaction, project_id, moment_id)?;
        let after_ordinal: i64 = transaction.query_row(
            "SELECT ordinal FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND media_asset_id = ?3 AND active = 1 AND membership_state = 'member'",
            params![project_id.to_string(), moment_id, after_asset_id], |row| row.get(0),
        ).optional()?.ok_or_else(|| PersistenceError::InvalidData("split point must be a member of the selected moment".into()))?;
        let next = transaction.query_row(
            "SELECT media_asset_id, ordinal FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member' AND ordinal > ?3 ORDER BY ordinal ASC, media_asset_id ASC LIMIT 1",
            params![project_id.to_string(), moment_id, after_ordinal], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        ).optional()?.ok_or_else(|| PersistenceError::InvalidData("split point must leave at least one photo after it".into()))?;
        let (next_asset_id, _next_membership_ordinal) = next;
        let new_moment_id = Uuid::new_v4().to_string();
        let new_segment_id = Uuid::new_v4().to_string();
        let now = timestamp(&Utc::now());
        let (after_time, next_time): (Option<String>, Option<String>) = transaction.query_row(
            "SELECT (SELECT COALESCE(metadata.captured_at_local, asset.captured_at) FROM media_assets asset LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id WHERE asset.id = ?1), (SELECT COALESCE(metadata.captured_at_local, asset.captured_at) FROM media_assets asset LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id WHERE asset.id = ?2)",
            params![after_asset_id, next_asset_id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut active_order = active_moment_ids_for_project(&transaction, project_id)?;
        let insertion_index = active_order
            .iter()
            .position(|id| id == moment_id)
            .ok_or_else(|| {
                PersistenceError::InvalidData(
                    "selected Moment changed before its split could be saved".into(),
                )
            })?
            + 1;
        active_order.insert(insertion_index, new_moment_id.clone());
        let new_ordinal = next_moment_record_ordinal(&transaction, project_id, &run_id)?;
        let previous_segment_ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) FROM timeline_segments WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )?;
        let segment_ordinal = previous_segment_ordinal.checked_add(1).ok_or_else(|| {
            PersistenceError::InvalidData(
                "timeline segment ordinal exceeds SQLite integer range".into(),
            )
        })?;
        transaction.execute(
            "INSERT INTO timeline_segments (id, project_id, run_id, ordinal, started_at, ended_at, asset_count, boundary_category, boundary_evidence_json, created_at, stale) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0, 'strong', ?6, ?7, 0)",
            params![new_segment_id, project_id.to_string(), run_id, segment_ordinal, next_time, json(&serde_json::json!({"manual": true, "afterAssetId": after_asset_id}))?, now],
        )?;
        transaction.execute(
            "INSERT INTO moment_records (id, project_id, timeline_id, run_id, segment_id, anchor_asset_id, ordinal, started_at, ended_at, asset_count, ai_representative_asset_id, centroid_blob, centroid_dimensions, suggested_label, label_confidence, label_evidence_json, label_state, created_at, stale) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 0, NULL, NULL, NULL, NULL, NULL, '{}', 'abstained', ?9, 0)",
            params![new_moment_id, project_id.to_string(), timeline_id, run_id, new_segment_id, next_asset_id, new_ordinal, next_time, now],
        )?;
        transaction.execute(
            "UPDATE moment_memberships SET moment_id = ?4 WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member' AND ordinal > ?3",
            params![project_id.to_string(), moment_id, after_ordinal, new_moment_id],
        )?;
        if let Some(asset_id) = rebind_human_representative_for_active_moment(
            &transaction,
            project_id,
            &source_anchor,
            &next_asset_id,
            &new_moment_id,
        )? {
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_REPRESENTATIVE_CHANGED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), project_id.to_string(), new_moment_id, json(&serde_json::json!({"assetId": asset_id, "source": "split_rebind", "previousMomentId": moment_id}))?, now],
            )?;
        }
        // Keep a human confirmation attached to the same selected asset when that asset moves
        // into the second Moment created by this split.
        transaction.execute(
            "UPDATE coverage_confirmations
             SET moment_id = ?1
             WHERE project_id = ?2
               AND moment_id = ?3
               AND media_asset_id IN (
                   SELECT media_asset_id
                   FROM moment_memberships
                   WHERE project_id = ?2
                     AND moment_id = ?1
                     AND active = 1
                     AND membership_state = 'member'
               )",
            params![new_moment_id, project_id.to_string(), moment_id],
        )?;
        transaction.execute(
            "UPDATE moment_records SET asset_count = (SELECT COUNT(*) FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member'), ended_at = ?3 WHERE id = ?2",
            params![project_id.to_string(), moment_id, after_time],
        )?;
        transaction.execute(
            "UPDATE moment_records SET asset_count = (SELECT COUNT(*) FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND active = 1 AND membership_state = 'member'), ended_at = (SELECT COALESCE(metadata.captured_at_local, asset.captured_at) FROM moment_memberships member JOIN media_assets asset ON asset.id = member.media_asset_id LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id WHERE member.project_id = ?1 AND member.moment_id = ?2 AND member.active = 1 ORDER BY member.ordinal DESC LIMIT 1) WHERE id = ?2",
            params![project_id.to_string(), new_moment_id],
        )?;
        reindex_active_moment_projection(&transaction, project_id, &active_order)?;
        transaction.execute(
            "INSERT INTO moment_override_operations (id, project_id, operation, left_asset_id, right_asset_id, created_at, active) VALUES (?1, ?2, 'split', ?3, ?4, ?5, 1) ON CONFLICT(project_id, operation, left_asset_id, right_asset_id) DO UPDATE SET active = 1, created_at = excluded.created_at",
            params![Uuid::new_v4().to_string(), project_id.to_string(), after_asset_id, next_asset_id, now],
        )?;
        transaction.execute(
            "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'MOMENT_SPLIT', ?4, ?5), (?6, ?2, ?7, 'MOMENT_CREATED', ?8, ?5)",
            params![Uuid::new_v4().to_string(), project_id.to_string(), moment_id, json(&serde_json::json!({"afterAssetId": after_asset_id, "newMomentId": new_moment_id}))?, now, Uuid::new_v4().to_string(), new_moment_id, json(&serde_json::json!({"source":"human_split", "afterAssetId": after_asset_id}))?],
        )?;
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'stale'
             WHERE project_id = ?1 AND stale = 0",
            params![project_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn coverage_checklist_items(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<CoverageChecklistItemView>> {
        let mut statement = self.connection.prepare(
            "SELECT item.id, item.phrase, item.created_at, confirmation.state, confirmation.confirmed_at, confirmation.moment_id, confirmation.media_asset_id
             FROM coverage_checklist_items item LEFT JOIN coverage_confirmations confirmation ON confirmation.checklist_item_id = item.id AND confirmation.project_id = item.project_id
             WHERE item.project_id = ?1 ORDER BY item.created_at ASC, item.id ASC",
        )?;
        let items = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok(CoverageChecklistItemView {
                    id: row.get(0)?,
                    text: row.get(1)?,
                    created_at: row.get(2)?,
                    confirmation_state: row.get(3)?,
                    confirmed_at: row.get(4)?,
                    moment_id: row.get(5)?,
                    media_asset_id: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        items
    }

    fn create_coverage_checklist_item(&self, item: &CoverageChecklistItemRecord) -> Result<()> {
        let phrase = item.text.trim();
        if phrase.is_empty() || phrase.chars().count() > 160 {
            return Err(PersistenceError::InvalidData(
                "checklist phrase must contain 1 through 160 characters".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        transaction.execute(
            "INSERT INTO coverage_checklist_items (id, project_id, phrase, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(project_id, phrase) DO NOTHING",
            params![item.id, item.project_id, phrase, item.created_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn update_coverage_confirmation(
        &self,
        project_id: &ProjectId,
        checklist_item_id: &str,
        state: &str,
        moment_id: Option<&str>,
        media_asset_id: Option<&str>,
    ) -> Result<()> {
        if !matches!(state, "confirmed_covered" | "needs_review" | "not_covered") {
            return Err(PersistenceError::InvalidData(
                "unsupported coverage confirmation state".into(),
            ));
        }
        let transaction = moment_write_transaction(&self.connection)?;
        let checklist_owned: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM coverage_checklist_items WHERE id = ?1 AND project_id = ?2)", params![checklist_item_id, project_id.to_string()], |row| row.get(0),
        )?;
        if !checklist_owned {
            return Err(PersistenceError::InvalidData(
                "checklist item does not belong to the selected project".into(),
            ));
        }
        if let Some(moment_id) = moment_id {
            let moment_belongs_to_project: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM moment_records WHERE id = ?1 AND project_id = ?2 AND stale = 0)",
                params![moment_id, project_id.to_string()],
                |row| row.get(0),
            )?;
            if !moment_belongs_to_project {
                return Err(PersistenceError::InvalidData(
                    "moment does not belong to the selected project".into(),
                ));
            }
        }
        if let Some(asset_id) = media_asset_id {
            let asset = MediaAssetId::try_from(asset_id)
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
            let asset_belongs_to_project: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id = ?1 AND project_id = ?2)",
                params![asset.to_string(), project_id.to_string()],
                |row| row.get(0),
            )?;
            if !asset_belongs_to_project {
                return Err(PersistenceError::InvalidData(
                    "media asset does not belong to the selected project".into(),
                ));
            }
            if let Some(moment_id) = moment_id {
                let member_belongs_to_moment: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM moment_memberships WHERE project_id = ?1 AND moment_id = ?2 AND media_asset_id = ?3 AND active = 1 AND membership_state = 'member')",
                    params![project_id.to_string(), moment_id, asset_id],
                    |row| row.get(0),
                )?;
                if !member_belongs_to_moment {
                    return Err(PersistenceError::InvalidData(
                        "coverage asset must belong to the selected moment".into(),
                    ));
                }
            }
        }
        let now = timestamp(&Utc::now());
        transaction.execute(
            "INSERT INTO coverage_confirmations (id, project_id, checklist_item_id, state, moment_id, media_asset_id, confirmed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id, checklist_item_id) DO UPDATE SET state = excluded.state, moment_id = excluded.moment_id, media_asset_id = excluded.media_asset_id, confirmed_at = excluded.confirmed_at",
            params![Uuid::new_v4().to_string(), project_id.to_string(), checklist_item_id, state, moment_id, media_asset_id, now],
        )?;
        transaction.execute(
            "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, 'COVERAGE_CONFIRMED', ?4, ?5)",
            params![Uuid::new_v4().to_string(), project_id.to_string(), moment_id, json(&serde_json::json!({"checklistItemId": checklist_item_id, "state": state, "assetId": media_asset_id}))?, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn record_camera_clock_offset_diagnostics(
        &self,
        diagnostics: &[CameraClockOffsetDiagnosticRecord],
    ) -> Result<()> {
        if diagnostics.is_empty() {
            return Ok(());
        }
        let first = &diagnostics[0];
        if first.project_id.trim().is_empty() || first.run_id.trim().is_empty() {
            return Err(PersistenceError::InvalidData(
                "camera clock diagnostic requires a project and Moment analysis run".into(),
            ));
        }
        let mut diagnostic_ids = BTreeSet::new();
        for diagnostic in diagnostics {
            if diagnostic.project_id != first.project_id || diagnostic.run_id != first.run_id {
                return Err(PersistenceError::InvalidData(
                    "camera clock diagnostics must be recorded for one project and analysis run"
                        .into(),
                ));
            }
            if diagnostic.id.trim().is_empty()
                || diagnostic.camera_a.trim().is_empty()
                || diagnostic.camera_b.trim().is_empty()
                || diagnostic.camera_a == diagnostic.camera_b
                || !diagnostic_ids.insert(diagnostic.id.as_str())
            {
                return Err(PersistenceError::InvalidData(
                    "camera clock diagnostic requires unique IDs and two distinct camera labels"
                        .into(),
                ));
            }
        }
        let run_belongs_to_project: bool = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM moment_analysis_runs
                WHERE id = ?1 AND project_id = ?2
            )",
            params![first.run_id, first.project_id],
            |row| row.get(0),
        )?;
        if !run_belongs_to_project {
            return Err(PersistenceError::InvalidData(
                "camera clock diagnostic run does not belong to the selected project".into(),
            ));
        }

        let transaction = self.connection.unchecked_transaction()?;
        for diagnostic in diagnostics {
            transaction.execute(
                "INSERT INTO camera_clock_offset_diagnostics
                    (id, project_id, run_id, camera_a, camera_b, possible_offset_seconds, evidence_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    diagnostic.id,
                    diagnostic.project_id,
                    diagnostic.run_id,
                    diagnostic.camera_a,
                    diagnostic.camera_b,
                    diagnostic.possible_offset_seconds,
                    json(&diagnostic.evidence_json)?,
                    diagnostic.created_at,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn latest_camera_clock_offset_diagnostics(
        &self,
        project_id: &ProjectId,
    ) -> Result<Vec<CameraClockOffsetDiagnosticRecord>> {
        // Diagnostics are specific to a structural-analysis run. Showing an older run after a
        // newer active projection exists would be misleading, so no rows is intentionally
        // returned when the active run has no diagnostic evidence.
        let mut statement = self.connection.prepare(
            "SELECT diagnostic.id, diagnostic.project_id, diagnostic.run_id,
                    diagnostic.camera_a, diagnostic.camera_b,
                    diagnostic.possible_offset_seconds, diagnostic.evidence_json,
                    diagnostic.created_at
             FROM shoot_timelines timeline
             JOIN camera_clock_offset_diagnostics diagnostic
               ON diagnostic.project_id = timeline.project_id
              AND diagnostic.run_id = timeline.active_run_id
             WHERE timeline.project_id = ?1
             ORDER BY diagnostic.camera_a ASC, diagnostic.camera_b ASC, diagnostic.id ASC",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            let evidence_json = row.get::<_, String>(6)?;
            Ok(CameraClockOffsetDiagnosticRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                run_id: row.get(2)?,
                camera_a: row.get(3)?,
                camera_b: row.get(4)?,
                possible_offset_seconds: row.get(5)?,
                evidence_json: serde_json::from_str(&evidence_json)
                    .map_err(|error| to_sql_error(PersistenceError::from(error)))?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn moment_search_candidates(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<MomentSearchCandidate>> {
        let mut by_id = std::collections::HashMap::<String, MomentTimelineRow>::new();
        let mut after = None::<(u64, String)>;
        loop {
            let rows = moment_timeline_rows_after(
                &self.connection,
                project_id,
                MOMENT_SEARCH_ROW_PAGE_SIZE,
                after.as_ref().map(|(ordinal, id)| (*ordinal, id.as_str())),
            )?;
            let page_len = rows.len();
            if page_len == 0 {
                break;
            }
            after = rows.last().map(|row| (row.ordinal, row.id.clone()));
            for row in rows {
                by_id.insert(row.id.clone(), row);
            }
            if page_len < MOMENT_SEARCH_ROW_PAGE_SIZE as usize {
                break;
            }
        }
        let mut statement = self.connection.prepare(
            "SELECT id, centroid_blob, centroid_dimensions FROM moment_records WHERE project_id = ?1 AND stale = 0 AND centroid_blob IS NOT NULL AND centroid_dimensions = ?2 ORDER BY ordinal ASC, id ASC",
        )?;
        let mut candidates = Vec::new();
        for row in statement.query_map(
            params![project_id.to_string(), model.dimensions as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )? {
            let (id, blob, dimensions) = row?;
            if let Some(view) = by_id.remove(&id) {
                candidates.push(MomentSearchCandidate {
                    moment_id: id,
                    centroid: decode_semantic_vector(&blob, dimensions as usize)?,
                    row: view,
                });
            }
        }
        Ok(candidates)
    }

    fn latest_moment_analysis_job(&self, project_id: &ProjectId) -> Result<Option<BackgroundJob>> {
        self.connection.query_row(
            "SELECT id, state_json, stage_json, items_completed, items_total, files_discovered, files_processed, error_count, project_id, index_root_id, error_message, resume_metadata_json, created_at, updated_at, finished_at FROM background_jobs WHERE project_id = ?1 AND resume_metadata_json LIKE '%\"pipeline\":\"moment-analysis\"%' ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            params![project_id.to_string()], background_job_tuple,
        ).optional()?.map(background_job_from_tuple).transpose()
    }

    fn recover_interrupted_moment_analysis(&self) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"moment_analysis\"', error_message = COALESCE(error_message, 'Moment analysis was interrupted before completion; start a local update or rebuild to continue.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"moment-analysis\"%'",
            params![timestamp(&Utc::now())],
        )?;
        Ok(changed as u64)
    }

    fn project_index_summary(&self, project_id: &ProjectId) -> Result<ProjectIndexSummary> {
        let media_assets = self.connection.query_row(
            "SELECT COUNT(*) FROM media_assets WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let file_instances = self.connection.query_row(
            "SELECT COUNT(*) FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE a.project_id = ?1",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let storage_volumes = self.connection.query_row(
            "SELECT COUNT(DISTINCT fi.storage_volume_id) FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE a.project_id = ?1",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let files_discovered = self.connection.query_row(
            "SELECT COALESCE((SELECT files_discovered FROM background_jobs WHERE project_id = ?1 AND state_json = '\"completed\"' ORDER BY created_at DESC LIMIT 1), 0)",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let supported_media_count = self.connection.query_row(
            "SELECT COUNT(*) FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE a.project_id = ?1 AND a.media_type <> 'unknown'",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let unknown_count = self.connection.query_row(
            "SELECT COUNT(*) FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE a.project_id = ?1 AND a.media_type = 'unknown'",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let duplicate_fast_fingerprint_count = self.connection.query_row(
            "SELECT COUNT(*) FROM (SELECT fi.media_asset_id FROM file_instances fi JOIN media_assets a ON a.id = fi.media_asset_id WHERE a.project_id = ?1 AND a.fast_fingerprint IS NOT NULL GROUP BY fi.media_asset_id HAVING COUNT(*) > 1)",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let moment_count = self.connection.query_row(
            "SELECT COUNT(*) FROM moment_records WHERE project_id = ?1 AND stale = 0",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let (last_indexed_folder, storage_volume_identity) = self.connection.query_row(
            "SELECT roots.selected_path, volumes.filesystem_identity FROM index_roots roots JOIN storage_volumes volumes ON volumes.id = roots.storage_volume_id WHERE roots.project_id = ?1 ORDER BY roots.last_indexed_at DESC, roots.added_at DESC LIMIT 1",
            params![project_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional()?.unwrap_or((String::new(), None));
        Ok(ProjectIndexSummary {
            media_assets,
            file_instances,
            storage_volumes,
            files_discovered,
            supported_media_count,
            unknown_count,
            duplicate_fast_fingerprint_count,
            moment_count,
            last_indexed_folder: (!last_indexed_folder.is_empty()).then_some(last_indexed_folder),
            storage_volume_identity,
        })
    }

    fn project_library(&self) -> Result<Vec<ProjectLibraryItem>> {
        // ISO-8601 timestamps sort lexically, so this remains a deterministic recent-activity
        // view without duplicating mutable activity timestamps onto the `projects` record.
        let mut statement = self.connection.prepare(
            "SELECT
                p.id,
                p.name,
                p.created_at,
                MAX(
                    p.created_at,
                    COALESCE((SELECT MAX(created_at) FROM media_assets WHERE project_id = p.id), p.created_at),
                    COALESCE((SELECT MAX(COALESCE(last_indexed_at, added_at)) FROM index_roots WHERE project_id = p.id), p.created_at),
                    COALESCE((SELECT MAX(updated_at) FROM background_jobs WHERE project_id = p.id), p.created_at),
                    COALESCE((SELECT MAX(updated_at) FROM ingest_jobs WHERE project_id = p.id), p.created_at)
                ) AS last_activity_at,
                (SELECT COUNT(*) FROM media_assets WHERE project_id = p.id) AS media_asset_count,
                (SELECT COUNT(*) FROM storage_volumes v
                  WHERE EXISTS(
                      SELECT 1 FROM index_roots roots
                       WHERE roots.project_id = p.id AND roots.storage_volume_id = v.id
                  ) OR EXISTS(
                      SELECT 1 FROM file_instances fi
                      JOIN media_assets a ON a.id = fi.media_asset_id
                       WHERE a.project_id = p.id AND fi.storage_volume_id = v.id
                  )) AS storage_volume_count,
                CASE
                    WHEN EXISTS(
                        SELECT 1
                          FROM ingest_jobs j
                          JOIN ingest_items i ON i.ingest_job_id = j.id
                         WHERE j.project_id = p.id AND i.state_json = '\"verified\"'
                    ) THEN 'verified_copy_history'
                    WHEN EXISTS(SELECT 1 FROM ingest_jobs WHERE project_id = p.id)
                        THEN 'ingest_history_recorded'
                    ELSE 'not_recorded'
                END AS protection_state
             FROM projects p
             ORDER BY last_activity_at DESC, p.created_at DESC, p.id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ProjectLibraryItem {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                last_activity_at: row.get(3)?,
                media_asset_count: row.get::<_, i64>(4)? as u64,
                storage_volume_count: row.get::<_, i64>(5)? as u64,
                protection_state: row.get(6)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn insert_ingest_job(&self, job: &IngestJob) -> Result<()> {
        self.connection.execute(
            "INSERT INTO ingest_jobs (id, project_id, start_request_id, state_json, policy_json, guardian_state_json, safe_to_eject, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![job.id.to_string(), job.project_id.to_string(), job.start_request_id, json(&job.state)?, json(&job.policy)?, json(&job.guardian_state)?, job.safe_to_eject, timestamp(&job.created_at), timestamp(&job.updated_at), optional_timestamp(&job.finished_at)],
        )?;
        Ok(())
    }

    fn update_ingest_job(&self, job: &IngestJob) -> Result<()> {
        self.connection.execute(
            "UPDATE ingest_jobs SET state_json = ?2, policy_json = ?3, guardian_state_json = ?4, safe_to_eject = ?5, updated_at = ?6, finished_at = ?7 WHERE id = ?1",
            params![job.id.to_string(), json(&job.state)?, json(&job.policy)?, json(&job.guardian_state)?, job.safe_to_eject, timestamp(&job.updated_at), optional_timestamp(&job.finished_at)],
        )?;
        Ok(())
    }

    fn ingest_job(&self, id: &IngestJobId) -> Result<Option<IngestJob>> {
        self.connection.query_row(
            "SELECT id, project_id, start_request_id, state_json, policy_json, guardian_state_json, safe_to_eject, created_at, updated_at, finished_at FROM ingest_jobs WHERE id = ?1",
            params![id.to_string()],
            ingest_job_tuple,
        ).optional()?.map(ingest_job_from_tuple).transpose()
    }

    fn ingest_job_by_start_request_id(&self, start_request_id: &str) -> Result<Option<IngestJob>> {
        self.connection.query_row(
            "SELECT id, project_id, start_request_id, state_json, policy_json, guardian_state_json, safe_to_eject, created_at, updated_at, finished_at FROM ingest_jobs WHERE start_request_id = ?1",
            params![start_request_id],
            ingest_job_tuple,
        ).optional()?.map(ingest_job_from_tuple).transpose()
    }

    fn ingest_jobs_for_project(&self, project_id: &ProjectId) -> Result<Vec<IngestJobSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT j.id, j.state_json, j.policy_json, j.guardian_state_json, j.safe_to_eject, j.created_at, j.updated_at, j.finished_at, COUNT(i.id), COALESCE(SUM(CASE WHEN i.state_json = '\"verified\"' THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN i.state_json IN ('\"failed\"', '\"conflict\"', '\"source_changed\"', '\"unavailable\"') THEN 1 ELSE 0 END), 0), COALESCE(SUM(i.byte_size), 0), COALESCE(SUM(CASE WHEN i.state_json = '\"verified\"' THEN i.byte_size ELSE 0 END), 0) FROM ingest_jobs j LEFT JOIN ingest_items i ON i.ingest_job_id = j.id WHERE j.project_id = ?1 GROUP BY j.id ORDER BY j.created_at DESC",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], ingest_job_summary_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn insert_ingest_source(&self, source: &IngestSource, warnings: &[String]) -> Result<()> {
        self.connection.execute(
            "INSERT INTO ingest_sources (id, ingest_job_id, label, selected_path, storage_volume_id, file_count, total_bytes, status_json, warnings_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![source.id.to_string(), source.ingest_job_id.to_string(), source.label, source.selected_path, source.storage_volume_id.to_string(), source.file_count as i64, source.total_bytes as i64, json(&source.status)?, json(&warnings)?],
        )?;
        Ok(())
    }

    fn update_ingest_source_status(
        &self,
        id: &IngestSourceId,
        status: IngestItemState,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE ingest_sources SET status_json = ?2 WHERE id = ?1",
            params![id.to_string(), json(&status)?],
        )?;
        Ok(())
    }

    fn ingest_sources(&self, job_id: &IngestJobId) -> Result<Vec<IngestSourceSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT id, label, selected_path, storage_volume_id, file_count, total_bytes, status_json, warnings_json FROM ingest_sources WHERE ingest_job_id = ?1 ORDER BY label, id",
        )?;
        let rows = statement.query_map(params![job_id.to_string()], |row| {
            let status: IngestItemState =
                from_json(&row.get::<_, String>(6)?).map_err(to_sql_error)?;
            let warnings: Vec<String> =
                from_json(&row.get::<_, String>(7)?).map_err(to_sql_error)?;
            Ok(IngestSourceSummary {
                id: row.get(0)?,
                label: row.get(1)?,
                selected_path: row.get(2)?,
                storage_volume_id: row.get(3)?,
                file_count: row.get::<_, i64>(4)? as u64,
                total_bytes: row.get::<_, i64>(5)? as u64,
                status: enum_label(&status).map_err(to_sql_error)?,
                warnings,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn insert_ingest_destination(&self, destination: &IngestDestination) -> Result<()> {
        self.connection.execute(
            "INSERT INTO ingest_destinations (id, ingest_job_id, role_json, selected_path, storage_volume_id, available_bytes, required_bytes, writable, status_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![destination.id.to_string(), destination.ingest_job_id.to_string(), json(&destination.role)?, destination.selected_path, destination.storage_volume_id.to_string(), destination.available_bytes.map(|value| value as i64), destination.required_bytes as i64, destination.writable, json(&destination.status)?],
        )?;
        Ok(())
    }

    fn update_ingest_destination_status(
        &self,
        id: &IngestDestinationId,
        status: IngestItemState,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE ingest_destinations SET status_json = ?2 WHERE id = ?1",
            params![id.to_string(), json(&status)?],
        )?;
        Ok(())
    }

    fn ingest_destinations(&self, job_id: &IngestJobId) -> Result<Vec<IngestDestinationSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT d.id, d.role_json, d.selected_path, d.storage_volume_id, v.display_name, d.available_bytes, d.required_bytes, d.writable, d.status_json FROM ingest_destinations d JOIN storage_volumes v ON v.id = d.storage_volume_id WHERE d.ingest_job_id = ?1 ORDER BY CASE d.role_json WHEN '\"master\"' THEN 0 ELSE 1 END, d.id",
        )?;
        let rows = statement.query_map(params![job_id.to_string()], |row| {
            let role: IngestDestinationRole =
                from_json(&row.get::<_, String>(1)?).map_err(to_sql_error)?;
            let status: IngestItemState =
                from_json(&row.get::<_, String>(8)?).map_err(to_sql_error)?;
            Ok(IngestDestinationSummary {
                id: row.get(0)?,
                role: enum_label(&role).map_err(to_sql_error)?,
                selected_path: row.get(2)?,
                storage_volume_id: row.get(3)?,
                storage_volume_name: row.get(4)?,
                available_bytes: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
                required_bytes: row.get::<_, i64>(6)? as u64,
                writable: row.get(7)?,
                status: enum_label(&status).map_err(to_sql_error)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn upsert_ingest_item(&self, item: &IngestItemRecord) -> Result<IngestItemRecord> {
        let existing = self.connection.query_row(
            "SELECT id FROM ingest_items WHERE ingest_job_id = ?1 AND ingest_source_id = ?2 AND ingest_destination_id = ?3 AND source_relative_path = ?4",
            params![item.ingest_job_id.to_string(), item.ingest_source_id.to_string(), item.ingest_destination_id.to_string(), item.source_relative_path],
            |row| row.get::<_, String>(0),
        ).optional()?;
        let id = existing.unwrap_or_else(|| item.id.to_string());
        self.connection.execute(
            "INSERT INTO ingest_items (id, ingest_job_id, ingest_source_id, ingest_destination_id, media_asset_id, source_relative_path, destination_relative_path, byte_size, source_hash, destination_hash, state_json, verification_state_json, source_file_instance_id, destination_file_instance_id, error_message, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT(ingest_job_id, ingest_source_id, ingest_destination_id, source_relative_path) DO UPDATE SET media_asset_id = excluded.media_asset_id, destination_relative_path = excluded.destination_relative_path, byte_size = excluded.byte_size, source_hash = excluded.source_hash, destination_hash = excluded.destination_hash, state_json = excluded.state_json, verification_state_json = excluded.verification_state_json, source_file_instance_id = excluded.source_file_instance_id, destination_file_instance_id = excluded.destination_file_instance_id, error_message = excluded.error_message, updated_at = excluded.updated_at",
            params![id, item.ingest_job_id.to_string(), item.ingest_source_id.to_string(), item.ingest_destination_id.to_string(), item.media_asset_id.as_ref().map(ToString::to_string), item.source_relative_path, item.destination_relative_path, item.byte_size as i64, item.source_hash, item.destination_hash, json(&item.state)?, json(&item.verification_state)?, item.source_file_instance_id.as_ref().map(ToString::to_string), item.destination_file_instance_id.as_ref().map(ToString::to_string), item.error_message, timestamp(&item.updated_at)],
        )?;
        Ok(IngestItemRecord {
            id: parse_id!(IngestItemId, &id)?,
            ..item.clone()
        })
    }

    fn ingest_items(&self, job_id: &IngestJobId) -> Result<Vec<IngestItemRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, ingest_job_id, ingest_source_id, ingest_destination_id, media_asset_id, source_relative_path, destination_relative_path, byte_size, source_hash, destination_hash, state_json, verification_state_json, source_file_instance_id, destination_file_instance_id, error_message, updated_at FROM ingest_items WHERE ingest_job_id = ?1 ORDER BY source_relative_path, ingest_destination_id",
        )?;
        let rows = statement.query_map(params![job_id.to_string()], ingest_item_row)?;
        rows.map(|row| ingest_item_from_tuple(row?)).collect()
    }

    fn insert_ingest_audit_event(&self, event: &IngestAuditEvent) -> Result<()> {
        self.connection.execute(
            "INSERT INTO ingest_audit_events (id, ingest_job_id, event_type, detail, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event.id.to_string(), event.ingest_job_id.to_string(), event.event_type, event.detail, timestamp(&event.created_at)],
        )?;
        Ok(())
    }

    fn ingest_audit_events(&self, job_id: &IngestJobId) -> Result<Vec<IngestAuditEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT id, ingest_job_id, event_type, detail, created_at FROM ingest_audit_events WHERE ingest_job_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![job_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let ingest_job_id: String = row.get(1)?;
            let created_at: String = row.get(4)?;
            Ok(IngestAuditEvent {
                id: parse_id!(IngestAuditEventId, &id).map_err(to_sql_error)?,
                ingest_job_id: parse_id!(IngestJobId, &ingest_job_id).map_err(to_sql_error)?,
                event_type: row.get(2)?,
                detail: row.get(3)?,
                created_at: parse_timestamp(&created_at).map_err(to_sql_error)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn ingest_report(&self, job_id: &IngestJobId) -> Result<Option<IngestReport>> {
        let Some(job) = self.ingest_job(job_id)? else {
            return Ok(None);
        };
        let summary = ingest_job_summary_for_id(&self.connection, job_id)?;
        let destinations = self.ingest_destinations(job_id)?;
        let destination_count = destinations.len();
        let distinct_volumes = destinations
            .iter()
            .map(|destination| destination.storage_volume_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let mut statement = self.connection.prepare(
            "SELECT source_relative_path || ': ' || error_message FROM ingest_items WHERE ingest_job_id = ?1 AND error_message IS NOT NULL ORDER BY updated_at DESC LIMIT 8",
        )?;
        let recent_errors = statement
            .query_map(params![job.id.to_string()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(Some(IngestReport {
            job: summary,
            sources: self.ingest_sources(job_id)?,
            destinations,
            recent_errors,
            same_volume_warning: destination_count > 1 && distinct_volumes < destination_count,
        }))
    }

    fn recover_interrupted_ingests(&self) -> Result<u64> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM ingest_jobs WHERE state_json = '\"running\"'")?;
        let interrupted_ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let now = timestamp(&Utc::now());
        let changed = self.connection.execute(
            "UPDATE ingest_jobs SET state_json = '\"interrupted\"', updated_at = ?1 WHERE state_json = '\"running\"'",
            params![now],
        )?;
        self.connection.execute(
            "UPDATE ingest_items SET state_json = '\"failed\"', verification_state_json = '\"not_verified\"', error_message = COALESCE(error_message, 'Job interrupted before verification completed'), updated_at = ?1 WHERE state_json IN ('\"copying\"', '\"copied\"', '\"verifying\"')",
            params![timestamp(&Utc::now())],
        )?;
        for job_id in interrupted_ids {
            self.connection.execute(
                "INSERT INTO ingest_audit_events (id, ingest_job_id, event_type, detail, created_at) VALUES (?1, ?2, 'JOB_INTERRUPTED', ?3, ?4)",
                params![
                    IngestAuditEventId::new().to_string(),
                    job_id,
                    "Recovered at application startup; incomplete items require retry.",
                    &now,
                ],
            )?;
        }
        Ok(changed as u64)
    }

    fn recover_interrupted_visual_preparations(&self) -> Result<u64> {
        let now = timestamp(&Utc::now());
        let changed = self.connection.execute(
            "UPDATE background_jobs SET state_json = '\"interrupted\"', stage_json = '\"finalize\"', error_message = COALESCE(error_message, 'Preview preparation was interrupted before completion; retry failed previews or prepare the page again.'), updated_at = ?1, finished_at = ?1 WHERE state_json = '\"running\"' AND resume_metadata_json LIKE '%\"pipeline\":\"visual-preparation\"%'",
            params![now],
        )?;
        Ok(changed as u64)
    }

    fn counts(&self) -> Result<CatalogCounts> {
        Ok(CatalogCounts {
            projects: self.count("projects")?,
            media_assets: self.count("media_assets")?,
            file_instances: self.count("file_instances")?,
            storage_volumes: self.count("storage_volumes")?,
        })
    }
}

type IngestJobTuple = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    bool,
    String,
    String,
    Option<String>,
);
type IngestItemTuple = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

fn ingest_job_tuple(row: &Row<'_>) -> rusqlite::Result<IngestJobTuple> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn ingest_job_from_tuple(value: IngestJobTuple) -> Result<IngestJob> {
    let (
        id,
        project_id,
        start_request_id,
        state,
        policy,
        guardian_state,
        safe_to_eject,
        created_at,
        updated_at,
        finished_at,
    ) = value;
    Ok(IngestJob {
        id: parse_id!(IngestJobId, &id)?,
        project_id: parse_id!(ProjectId, &project_id)?,
        start_request_id,
        state: from_json(&state)?,
        policy: from_json(&policy)?,
        guardian_state: from_json(&guardian_state)?,
        safe_to_eject,
        created_at: parse_timestamp(&created_at)?,
        updated_at: parse_timestamp(&updated_at)?,
        finished_at: finished_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
    })
}

fn ingest_item_row(row: &Row<'_>) -> rusqlite::Result<IngestItemTuple> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}

fn ingest_item_from_tuple(value: IngestItemTuple) -> Result<IngestItemRecord> {
    let (
        id,
        ingest_job_id,
        ingest_source_id,
        ingest_destination_id,
        media_asset_id,
        source_relative_path,
        destination_relative_path,
        byte_size,
        source_hash,
        destination_hash,
        state,
        verification_state,
        source_file_instance_id,
        destination_file_instance_id,
        error_message,
        updated_at,
    ) = value;
    Ok(IngestItemRecord {
        id: parse_id!(IngestItemId, &id)?,
        ingest_job_id: parse_id!(IngestJobId, &ingest_job_id)?,
        ingest_source_id: parse_id!(IngestSourceId, &ingest_source_id)?,
        ingest_destination_id: parse_id!(IngestDestinationId, &ingest_destination_id)?,
        media_asset_id: media_asset_id
            .as_deref()
            .map(|value| parse_id!(MediaAssetId, value))
            .transpose()?,
        source_relative_path,
        destination_relative_path,
        byte_size: byte_size as u64,
        source_hash,
        destination_hash,
        state: from_json(&state)?,
        verification_state: from_json(&verification_state)?,
        source_file_instance_id: source_file_instance_id
            .as_deref()
            .map(|value| parse_id!(FileInstanceId, value))
            .transpose()?,
        destination_file_instance_id: destination_file_instance_id
            .as_deref()
            .map(|value| parse_id!(FileInstanceId, value))
            .transpose()?,
        error_message,
        updated_at: parse_timestamp(&updated_at)?,
    })
}

fn ingest_job_summary_row(row: &Row<'_>) -> rusqlite::Result<IngestJobSummary> {
    let state: IngestJobState = from_json(&row.get::<_, String>(1)?).map_err(to_sql_error)?;
    let policy: IngestProtectionPolicy =
        from_json(&row.get::<_, String>(2)?).map_err(to_sql_error)?;
    let guardian_state: CaptureGuardianState =
        from_json(&row.get::<_, String>(3)?).map_err(to_sql_error)?;
    Ok(IngestJobSummary {
        id: row.get(0)?,
        state: enum_label(&state).map_err(to_sql_error)?,
        policy: enum_label(&policy).map_err(to_sql_error)?,
        guardian_state: enum_label(&guardian_state).map_err(to_sql_error)?,
        safe_to_eject: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        finished_at: row.get(7)?,
        files_total: row.get::<_, i64>(8)? as u64,
        files_verified: row.get::<_, i64>(9)? as u64,
        files_failed: row.get::<_, i64>(10)? as u64,
        bytes_total: row.get::<_, i64>(11)? as u64,
        bytes_verified: row.get::<_, i64>(12)? as u64,
    })
}

fn ingest_job_summary_for_id(
    connection: &Connection,
    job_id: &IngestJobId,
) -> Result<IngestJobSummary> {
    connection
        .query_row(
            "SELECT j.id, j.state_json, j.policy_json, j.guardian_state_json, j.safe_to_eject, j.created_at, j.updated_at, j.finished_at, COUNT(i.id), COALESCE(SUM(CASE WHEN i.state_json = '\"verified\"' THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN i.state_json IN ('\"failed\"', '\"conflict\"', '\"source_changed\"', '\"unavailable\"') THEN 1 ELSE 0 END), 0), COALESCE(SUM(i.byte_size), 0), COALESCE(SUM(CASE WHEN i.state_json = '\"verified\"' THEN i.byte_size ELSE 0 END), 0) FROM ingest_jobs j LEFT JOIN ingest_items i ON i.ingest_job_id = j.id WHERE j.id = ?1 GROUP BY j.id",
            params![job_id.to_string()],
            ingest_job_summary_row,
        )
        .map_err(Into::into)
}

fn moment_timeline_rows(
    connection: &Connection,
    project_id: &ProjectId,
    moment_id: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<Vec<MomentTimelineRow>> {
    moment_timeline_rows_with_cursor(connection, project_id, moment_id, limit, offset, None)
}

/// Reads a bounded page after a stable `(ordinal, id)` cursor. It is used by local Moment
/// semantic search so a project with more than 10,000 Moments is still searched in full without
/// offset-scan degradation.
fn moment_timeline_rows_after(
    connection: &Connection,
    project_id: &ProjectId,
    limit: u32,
    after: Option<(u64, &str)>,
) -> Result<Vec<MomentTimelineRow>> {
    moment_timeline_rows_with_cursor(connection, project_id, None, limit, 0, after)
}

fn moment_timeline_rows_with_cursor(
    connection: &Connection,
    project_id: &ProjectId,
    moment_id: Option<&str>,
    limit: u32,
    offset: u32,
    after: Option<(u64, &str)>,
) -> Result<Vec<MomentTimelineRow>> {
    let after_ordinal = after
        .map(|(ordinal, _)| i64::try_from(ordinal))
        .transpose()
        .map_err(|_| {
            PersistenceError::InvalidData("Moment ordinal exceeds SQLite integer range".into())
        })?;
    let after_id = after.map(|(_, id)| id);
    let mut statement = connection.prepare(
        "SELECT m.id, m.ordinal, m.started_at, m.ended_at, m.asset_count,
            CASE WHEN m.ai_representative_asset_id IS NOT NULL AND EXISTS (
                SELECT 1 FROM moment_memberships representative_member
                WHERE representative_member.project_id = m.project_id
                  AND representative_member.moment_id = m.id
                  AND representative_member.media_asset_id = m.ai_representative_asset_id
                  AND representative_member.active = 1
                  AND representative_member.membership_state = 'member'
            ) THEN m.ai_representative_asset_id ELSE NULL END,
            CASE WHEN human_rep.media_asset_id IS NOT NULL AND EXISTS (
                SELECT 1 FROM moment_memberships representative_member
                WHERE representative_member.project_id = m.project_id
                  AND representative_member.moment_id = m.id
                  AND representative_member.media_asset_id = human_rep.media_asset_id
                  AND representative_member.active = 1
                  AND representative_member.membership_state = 'member'
            ) THEN human_rep.media_asset_id ELSE NULL END,
            m.suggested_label, human_label.label,
            COALESCE(human_label.label, m.suggested_label, 'Untitled Moment'), m.label_state,
            (SELECT COUNT(DISTINCT g.id) FROM moment_memberships member JOIN similarity_group_members group_member ON group_member.media_asset_id = member.media_asset_id JOIN similarity_groups g ON g.id = group_member.group_id AND g.stale = 0 WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COALESCE(SUM(CASE WHEN decision.decision = 'keep' THEN 1 ELSE 0 END), 0) FROM moment_memberships member LEFT JOIN media_decisions decision ON decision.project_id = member.project_id AND decision.media_asset_id = member.media_asset_id WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COALESCE(SUM(CASE WHEN decision.decision = 'reject' THEN 1 ELSE 0 END), 0) FROM moment_memberships member LEFT JOIN media_decisions decision ON decision.project_id = member.project_id AND decision.media_asset_id = member.media_asset_id WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COALESCE(SUM(CASE WHEN decision.decision = 'review' THEN 1 ELSE 0 END), 0) FROM moment_memberships member LEFT JOIN media_decisions decision ON decision.project_id = member.project_id AND decision.media_asset_id = member.media_asset_id WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COALESCE(SUM(CASE WHEN decision.decision IS NULL THEN 1 ELSE 0 END), 0) FROM moment_memberships member LEFT JOIN media_decisions decision ON decision.project_id = member.project_id AND decision.media_asset_id = member.media_asset_id WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COALESCE(SUM(CASE WHEN decision.starred = 1 THEN 1 ELSE 0 END), 0) FROM moment_memberships member LEFT JOIN media_decisions decision ON decision.project_id = member.project_id AND decision.media_asset_id = member.media_asset_id WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member'),
            (SELECT COUNT(*) FROM moment_memberships member WHERE member.project_id = m.project_id AND member.moment_id = m.id AND member.active = 1 AND member.membership_state = 'member' AND EXISTS (SELECT 1 FROM technical_quality quality WHERE quality.media_asset_id = member.media_asset_id AND quality.stale = 0 AND quality.status = 'ready' AND quality.technical_quality_band = 'technical_issue')),
            (SELECT category FROM moment_boundary_evidence boundary WHERE boundary.project_id = m.project_id AND boundary.run_id = m.run_id AND boundary.right_asset_id = m.anchor_asset_id ORDER BY boundary.ordinal DESC LIMIT 1),
            (SELECT explanation FROM moment_boundary_evidence boundary WHERE boundary.project_id = m.project_id AND boundary.run_id = m.run_id AND boundary.right_asset_id = m.anchor_asset_id ORDER BY boundary.ordinal DESC LIMIT 1),
            EXISTS(SELECT 1 FROM moment_human_labels label WHERE label.project_id = m.project_id AND label.anchor_asset_id = m.anchor_asset_id)
              OR EXISTS(SELECT 1 FROM moment_human_representatives representative WHERE representative.project_id = m.project_id AND representative.anchor_asset_id = m.anchor_asset_id)
              OR EXISTS(
                  SELECT 1
                  FROM moment_override_operations operation
                  JOIN moment_memberships member
                    ON member.project_id = operation.project_id
                   AND member.media_asset_id IN (operation.left_asset_id, operation.right_asset_id)
                   AND member.active = 1
                   AND member.membership_state = 'member'
                  WHERE operation.project_id = m.project_id
                    AND operation.active = 1
                    AND member.moment_id = m.id
              ),
            m.run_id
         FROM moment_records m
         LEFT JOIN moment_human_labels human_label ON human_label.project_id = m.project_id AND human_label.anchor_asset_id = m.anchor_asset_id
         LEFT JOIN moment_human_representatives human_rep ON human_rep.project_id = m.project_id AND human_rep.anchor_asset_id = m.anchor_asset_id
         WHERE m.project_id = ?1
           AND m.stale = 0
           AND (?2 IS NULL OR m.id = ?2)
           AND (
               ?5 IS NULL
               OR m.ordinal > ?5
               OR (m.ordinal = ?5 AND m.id > ?6)
           )
         ORDER BY m.ordinal ASC, m.id ASC LIMIT ?3 OFFSET ?4",
    )?;
    let rows = statement
        .query_map(
            params![
                project_id.to_string(),
                moment_id,
                limit as i64,
                offset as i64,
                after_ordinal,
                after_id,
            ],
            |row| {
                Ok(MomentTimelineRow {
                    id: row.get(0)?,
                    run_id: row.get(21)?,
                    ordinal: row.get::<_, i64>(1)? as u64,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    asset_count: row.get::<_, i64>(4)? as u64,
                    ai_representative_asset_id: row.get(5)?,
                    human_representative_asset_id: row.get(6)?,
                    suggested_label: row.get(7)?,
                    human_label: row.get(8)?,
                    display_label: row.get(9)?,
                    label_state: row.get(10)?,
                    similar_set_count: row.get::<_, i64>(11)? as u64,
                    keep_count: row.get::<_, i64>(12)? as u64,
                    reject_count: row.get::<_, i64>(13)? as u64,
                    review_count: row.get::<_, i64>(14)? as u64,
                    unreviewed_count: row.get::<_, i64>(15)? as u64,
                    starred_count: row.get::<_, i64>(16)? as u64,
                    technical_issue_count: row.get::<_, i64>(17)? as u64,
                    boundary_category: row.get(18)?,
                    boundary_explanation: row.get(19)?,
                    human_override_present: row.get(20)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into);
    rows
}

fn timeline_gaps(connection: &Connection, project_id: &ProjectId) -> Result<Vec<TimelineGapView>> {
    let mut statement = connection.prepare(
        "SELECT started_at, ended_at FROM moment_records WHERE project_id = ?1 AND stale = 0 AND started_at IS NOT NULL AND ended_at IS NOT NULL ORDER BY ordinal ASC, id ASC",
    )?;
    let points = statement
        .query_map(params![project_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut gaps = Vec::new();
    for pair in points.windows(2) {
        let Some(previous_end) = moment_chronology_millis(&pair[0].1) else {
            continue;
        };
        let Some(next_start) = moment_chronology_millis(&pair[1].0) else {
            continue;
        };
        let seconds = (next_start - previous_end) / 1_000;
        // This is a factual display threshold only; it does not participate in segmentation or
        // claim that an expected scene/event is missing.
        if seconds >= 60 {
            gaps.push(TimelineGapView {
                started_at: pair[0].1.clone(),
                ended_at: pair[1].0.clone(),
                duration_seconds: seconds as u64,
                explanation: "No capture activity recorded in this interval.".into(),
            });
        }
    }
    Ok(gaps)
}

/// Moment cards retain their stored capture strings. For an EXIF camera wall-clock value whose
/// offset is explicitly unknown, this derives only a stable local ordering coordinate so factual
/// gap display can remain available; it does not claim or persist a UTC conversion.
fn moment_chronology_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc).timestamp_millis())
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
                .ok()
                .map(|value| Utc.from_utc_datetime(&value).timestamp_millis())
        })
}

fn moment_anchor_asset_id(
    connection: &Connection,
    project_id: &ProjectId,
    moment_id: &str,
) -> Result<String> {
    connection
        .query_row(
            "SELECT anchor_asset_id FROM moment_records WHERE id = ?1 AND project_id = ?2 AND stale = 0",
            params![moment_id, project_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| PersistenceError::InvalidData("moment does not belong to the selected project".into()))
}

type EditableMomentRecord = (
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
);

fn moment_record_for_edit(
    connection: &Connection,
    project_id: &ProjectId,
    moment_id: &str,
) -> Result<EditableMomentRecord> {
    connection
        .query_row(
            "SELECT timeline_id, run_id, segment_id, ordinal, anchor_asset_id, started_at, ended_at FROM moment_records WHERE id = ?1 AND project_id = ?2 AND stale = 0",
            params![moment_id, project_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .optional()?
        .ok_or_else(|| PersistenceError::InvalidData("moment does not belong to the selected project".into()))
}

/// Moment writes change a user-visible timeline projection. Acquire SQLite's writer reservation
/// before making the multi-statement change so a competing catalog writer cannot interleave an
/// ordinal/override update between validation and commit. `Transaction` still rolls back on any
/// error unless its caller explicitly commits it.
fn moment_write_transaction(connection: &Connection) -> Result<Transaction<'_>> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(Into::into)
}

/// Rebind a confirmation whose generated Moment record was replaced to the current active
/// Moment containing the photographer-selected asset. If no asset was selected, use the old
/// Moment's durable anchor asset as a conservative structural link. A confirmation whose anchor
/// is no longer an active member keeps its human state but deliberately loses the stale Moment
/// reference rather than implying that it belongs to an unrelated replacement card.
fn remap_coverage_confirmations_to_active_moments(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE coverage_confirmations
         SET moment_id = (
             SELECT active_member.moment_id
             FROM moment_memberships active_member
             JOIN moment_records active_record ON active_record.id = active_member.moment_id
             WHERE active_member.project_id = coverage_confirmations.project_id
               AND active_member.active = 1
               AND active_member.membership_state = 'member'
               AND active_record.stale = 0
               AND active_member.media_asset_id = COALESCE(
                   coverage_confirmations.media_asset_id,
                   (
                       SELECT stale_record.anchor_asset_id
                       FROM moment_records stale_record
                       WHERE stale_record.id = coverage_confirmations.moment_id
                         AND stale_record.project_id = coverage_confirmations.project_id
                       LIMIT 1
                   )
               )
             ORDER BY active_member.ordinal ASC, active_member.id ASC
             LIMIT 1
         )
         WHERE project_id = ?1
           AND moment_id IS NOT NULL
           AND EXISTS (
               SELECT 1
               FROM moment_records stale_record
               WHERE stale_record.id = coverage_confirmations.moment_id
                 AND stale_record.project_id = coverage_confirmations.project_id
                 AND stale_record.stale = 1
           )",
        params![project_id],
    )?;
    Ok(())
}

/// Keep a photographer's name attached to the active card containing its durable anchor asset
/// after generated Moments are replaced. Two different human names converging onto one new card
/// are ambiguous authority, so the enclosing replacement fails closed instead of silently
/// choosing or erasing either name.
fn remap_human_labels_to_active_moments(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<()> {
    let labels = transaction
        .prepare(
            "SELECT anchor_asset_id, label, created_at, updated_at
             FROM moment_human_labels
             WHERE project_id = ?1
             ORDER BY updated_at ASC, anchor_asset_id ASC",
        )?
        .query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // destination anchor -> (destination Moment ID, source anchor, label, created at, updated at)
    let mut assignments = BTreeMap::<String, (String, String, String, String, String)>::new();
    let mut labels_by_destination = BTreeMap::<String, BTreeSet<String>>::new();
    let mut source_destinations = Vec::<(String, String)>::new();
    let mut destination_anchors = BTreeSet::<String>::new();
    for (source_anchor, label, created_at, updated_at) in labels {
        let destination: Option<(String, String)> = transaction
            .query_row(
                "SELECT record.id, record.anchor_asset_id
                 FROM moment_memberships member
                 JOIN moment_records record ON record.id = member.moment_id
                 WHERE member.project_id = ?1
                   AND member.media_asset_id = ?2
                   AND member.active = 1
                   AND member.membership_state = 'member'
                   AND record.stale = 0
                 ORDER BY member.ordinal ASC, member.id ASC
                 LIMIT 1",
                params![project_id, source_anchor],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((destination_moment_id, destination_anchor)) = destination else {
            // No active card contains the former anchor. Keep its local history rather than
            // inventing a new attachment.
            continue;
        };
        labels_by_destination
            .entry(destination_anchor.clone())
            .or_default()
            .insert(label.clone());
        destination_anchors.insert(destination_anchor.clone());
        source_destinations.push((source_anchor.clone(), destination_anchor.clone()));
        let replace_destination = assignments
            .get(&destination_anchor)
            .map(|(_, existing_source, _, _, existing_updated_at)| {
                (updated_at.as_str(), source_anchor.as_str())
                    >= (existing_updated_at.as_str(), existing_source.as_str())
            })
            .unwrap_or(true);
        if replace_destination {
            assignments.insert(
                destination_anchor,
                (
                    destination_moment_id,
                    source_anchor,
                    label,
                    created_at,
                    updated_at,
                ),
            );
        }
    }
    if let Some((destination_anchor, _)) = labels_by_destination
        .iter()
        .find(|(_, labels)| labels.len() > 1)
    {
        return Err(PersistenceError::InvalidData(format!(
            "multiple human Moment labels would converge on active anchor {destination_anchor}; resolve the names before rebuilding"
        )));
    }

    for (
        destination_anchor,
        (destination_moment_id, source_anchor, label, created_at, updated_at),
    ) in assignments
    {
        transaction.execute(
            "INSERT INTO moment_human_labels (id, project_id, anchor_asset_id, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, anchor_asset_id) DO UPDATE SET
               label = excluded.label,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at",
            params![Uuid::new_v4().to_string(), project_id, destination_anchor, label, created_at, updated_at],
        )?;
        if source_anchor != destination_anchor {
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at)
                 VALUES (?1, ?2, ?3, 'MOMENT_RENAMED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), project_id, destination_moment_id, json(&serde_json::json!({"label": label, "previousAnchorAssetId": source_anchor, "source": "generated_projection_replaced"}))?, timestamp(&Utc::now())],
            )?;
        }
    }
    for (source_anchor, destination_anchor) in source_destinations {
        if source_anchor != destination_anchor && !destination_anchors.contains(&source_anchor) {
            transaction.execute(
                "DELETE FROM moment_human_labels
                 WHERE project_id = ?1 AND anchor_asset_id = ?2",
                params![project_id, source_anchor],
            )?;
        }
    }
    Ok(())
}

/// Rebind a right-side human label during an explicit merge. A photographer has to resolve
/// conflicting names themselves; choosing one automatically would rewrite their authority.
fn rebind_human_label_for_active_moment(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    source_anchor_asset_id: &str,
    destination_anchor_asset_id: &str,
    destination_moment_id: &str,
) -> Result<Option<String>> {
    if source_anchor_asset_id == destination_anchor_asset_id {
        return Ok(None);
    }
    let source: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT label, created_at, updated_at
             FROM moment_human_labels
             WHERE project_id = ?1 AND anchor_asset_id = ?2",
            params![project_id.to_string(), source_anchor_asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((label, created_at, updated_at)) = source else {
        return Ok(None);
    };
    let destination_label: Option<String> = transaction
        .query_row(
            "SELECT label FROM moment_human_labels
             WHERE project_id = ?1 AND anchor_asset_id = ?2",
            params![project_id.to_string(), destination_anchor_asset_id],
            |row| row.get(0),
        )
        .optional()?;
    match destination_label.as_deref() {
        Some(existing) if existing != label => {
            return Err(PersistenceError::InvalidData(
                "Cannot merge Moments with different human labels. Rename them to the same label first."
                    .into(),
            ));
        }
        Some(_) => {
            // Keep the surviving card's timestamp/provenance when both cards already carry the
            // same human name. Moving the right row's older timestamp backward would make a
            // later generated remap choose the wrong otherwise-equivalent history.
        }
        None => {
            transaction.execute(
                "INSERT INTO moment_human_labels (id, project_id, anchor_asset_id, label, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![Uuid::new_v4().to_string(), project_id.to_string(), destination_anchor_asset_id, label, created_at, updated_at],
            )?;
        }
    }
    transaction.execute(
        "DELETE FROM moment_human_labels
         WHERE project_id = ?1 AND anchor_asset_id = ?2",
        params![project_id.to_string(), source_anchor_asset_id],
    )?;
    transaction.execute(
        "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at)
         VALUES (?1, ?2, ?3, 'MOMENT_RENAMED', ?4, ?5)",
        params![Uuid::new_v4().to_string(), project_id.to_string(), destination_moment_id, json(&serde_json::json!({"label": label, "previousAnchorAssetId": source_anchor_asset_id, "source": "merge_rebind"}))?, timestamp(&Utc::now())],
    )?;
    Ok(Some(label))
}

/// Rebind each currently displayable human representative to the active Moment containing the
/// photographer-selected asset after a generated projection is replaced. Representative rows are
/// keyed by the generated card's anchor, so leaving an old key in place would silently hide a
/// valid human choice after resegmentation. If several prior cards resolve to one destination,
/// the latest explicit selection wins deterministically; the original human events remain intact.
fn remap_human_representatives_to_active_moments(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<()> {
    let representatives = transaction
        .prepare(
            "SELECT anchor_asset_id, media_asset_id, selected_at
             FROM moment_human_representatives
             WHERE project_id = ?1
             ORDER BY selected_at ASC, anchor_asset_id ASC",
        )?
        .query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // destination anchor -> (destination Moment ID, source anchor, selected asset, selected at)
    let mut assignments = BTreeMap::<String, (String, String, String, String)>::new();
    let mut source_destinations = Vec::<(String, String)>::new();
    let mut destination_anchors = BTreeSet::<String>::new();
    for (source_anchor, selected_asset_id, selected_at) in representatives {
        let destination: Option<(String, String)> = transaction
            .query_row(
                "SELECT record.id, record.anchor_asset_id
                 FROM moment_memberships member
                 JOIN moment_records record ON record.id = member.moment_id
                 WHERE member.project_id = ?1
                   AND member.media_asset_id = ?2
                   AND member.active = 1
                   AND member.membership_state = 'member'
                   AND record.stale = 0
                 ORDER BY member.ordinal ASC, member.id ASC
                 LIMIT 1",
                params![project_id, selected_asset_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((destination_moment_id, destination_anchor)) = destination else {
            // The asset is not part of the active projection. Preserve the durable human row,
            // but do not claim it belongs to an unrelated replacement Moment.
            continue;
        };
        destination_anchors.insert(destination_anchor.clone());
        source_destinations.push((source_anchor.clone(), destination_anchor.clone()));
        let replace_destination = assignments
            .get(&destination_anchor)
            .map(|(_, existing_source, _, existing_selected_at)| {
                (selected_at.as_str(), source_anchor.as_str())
                    >= (existing_selected_at.as_str(), existing_source.as_str())
            })
            .unwrap_or(true);
        if replace_destination {
            assignments.insert(
                destination_anchor,
                (
                    destination_moment_id,
                    source_anchor,
                    selected_asset_id,
                    selected_at,
                ),
            );
        }
    }

    for (
        destination_anchor,
        (destination_moment_id, source_anchor, selected_asset_id, selected_at),
    ) in assignments
    {
        transaction.execute(
            "INSERT INTO moment_human_representatives (id, project_id, anchor_asset_id, media_asset_id, selected_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id, anchor_asset_id) DO UPDATE SET
               media_asset_id = excluded.media_asset_id,
               selected_at = excluded.selected_at",
            params![Uuid::new_v4().to_string(), project_id, destination_anchor, selected_asset_id, selected_at],
        )?;
        if source_anchor != destination_anchor {
            transaction.execute(
                "INSERT INTO moment_events (id, project_id, moment_id, event_type, details_json, created_at)
                 VALUES (?1, ?2, ?3, 'MOMENT_REPRESENTATIVE_CHANGED', ?4, ?5)",
                params![Uuid::new_v4().to_string(), project_id, destination_moment_id, json(&serde_json::json!({"assetId": selected_asset_id, "previousAnchorAssetId": source_anchor, "source": "generated_projection_replaced"}))?, timestamp(&Utc::now())],
            )?;
        }
    }
    for (source_anchor, destination_anchor) in source_destinations {
        if source_anchor != destination_anchor && !destination_anchors.contains(&source_anchor) {
            transaction.execute(
                "DELETE FROM moment_human_representatives
                 WHERE project_id = ?1 AND anchor_asset_id = ?2",
                params![project_id, source_anchor],
            )?;
        }
    }
    Ok(())
}

/// A human representative must always belong to the active Moment shown in the UI. When a manual
/// structural edit moves the selected asset into another active Moment, rebind the selection to
/// that Moment's anchor. If prior history already has a representative for the destination
/// anchor, the later explicit human choice wins; both choices remain represented by append-only
/// Moment events. Returns the representative now displayed for the destination when the moved
/// choice won.
fn rebind_human_representative_for_active_moment(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    source_anchor_asset_id: &str,
    destination_anchor_asset_id: &str,
    destination_moment_id: &str,
) -> Result<Option<String>> {
    if source_anchor_asset_id == destination_anchor_asset_id {
        return Ok(None);
    }
    let moved_selection: Option<(String, String)> = transaction
        .query_row(
            "SELECT representative.media_asset_id, representative.selected_at
             FROM moment_human_representatives representative
             WHERE representative.project_id = ?1
               AND representative.anchor_asset_id = ?2
               AND EXISTS (
                   SELECT 1
                   FROM moment_memberships member
                   WHERE member.project_id = representative.project_id
                     AND member.moment_id = ?3
                     AND member.media_asset_id = representative.media_asset_id
                     AND member.active = 1
                     AND member.membership_state = 'member'
               )",
            params![
                project_id.to_string(),
                source_anchor_asset_id,
                destination_moment_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((asset_id, selected_at)) = moved_selection else {
        return Ok(None);
    };

    transaction.execute(
        "INSERT INTO moment_human_representatives (id, project_id, anchor_asset_id, media_asset_id, selected_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id, anchor_asset_id) DO UPDATE SET
           media_asset_id = CASE
             WHEN excluded.selected_at >= moment_human_representatives.selected_at
             THEN excluded.media_asset_id
             ELSE moment_human_representatives.media_asset_id
           END,
           selected_at = CASE
             WHEN excluded.selected_at >= moment_human_representatives.selected_at
             THEN excluded.selected_at
             ELSE moment_human_representatives.selected_at
           END",
        params![Uuid::new_v4().to_string(), project_id.to_string(), destination_anchor_asset_id, asset_id, selected_at],
    )?;
    let displayed_asset_id: String = transaction.query_row(
        "SELECT media_asset_id FROM moment_human_representatives
         WHERE project_id = ?1 AND anchor_asset_id = ?2",
        params![project_id.to_string(), destination_anchor_asset_id],
        |row| row.get(0),
    )?;
    transaction.execute(
        "DELETE FROM moment_human_representatives
         WHERE project_id = ?1 AND anchor_asset_id = ?2",
        params![project_id.to_string(), source_anchor_asset_id],
    )?;
    Ok((displayed_asset_id == asset_id).then_some(displayed_asset_id))
}

/// Returns the complete active display order. A project can contain a preserved incremental
/// prefix and one or more newer tail runs, so structural edits must consider all active cards.
fn active_moment_ids_for_project(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
) -> Result<Vec<String>> {
    transaction
        .prepare(
            "SELECT id
             FROM moment_records
             WHERE project_id = ?1 AND stale = 0
             ORDER BY ordinal ASC, id ASC",
        )?
        .query_map(params![project_id.to_string()], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Allocates a temporary ordinal outside the complete current run. It is used only while the
/// surrounding transaction is assembling a split, before the run is atomically resequenced.
fn next_moment_record_ordinal(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    run_id: &str,
) -> Result<i64> {
    let highest: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(ordinal), -1)
         FROM moment_records
         WHERE project_id = ?1 AND run_id = ?2",
        params![project_id.to_string(), run_id],
        |row| row.get(0),
    )?;
    highest.checked_add(1).ok_or_else(|| {
        PersistenceError::InvalidData("Moment ordinal exceeds SQLite integer range".into())
    })
}

/// Reassigns the complete active project projection to contiguous display ordinals. An
/// incremental timeline can intentionally consist of a preserved older-run prefix and newer tail
/// runs. If a structural edit changes the prefix length, every later active run must move with it
/// in the same transaction rather than retaining a gap or colliding with the edited run.
fn reindex_active_moment_projection(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    active_order: &[String],
) -> Result<()> {
    let active_rows = transaction
        .prepare(
            "SELECT id, run_id
             FROM moment_records
             WHERE project_id = ?1 AND stale = 0",
        )?
        .query_map(params![project_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let active_ids = active_rows
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let requested_active_ids = active_order.iter().cloned().collect::<BTreeSet<_>>();
    if requested_active_ids.len() != active_order.len() || active_ids != requested_active_ids {
        return Err(PersistenceError::InvalidData(
            "Moment ordinal repair received a stale or incomplete active projection".into(),
        ));
    }
    let run_by_id = active_rows.into_iter().collect::<BTreeMap<_, _>>();
    let mut targets_by_run = BTreeMap::<String, Vec<(String, i64)>>::new();
    for (ordinal, moment_id) in active_order.iter().enumerate() {
        let ordinal = i64::try_from(ordinal).map_err(|_| {
            PersistenceError::InvalidData("Moment count exceeds SQLite integer range".into())
        })?;
        let run_id = run_by_id.get(moment_id).ok_or_else(|| {
            PersistenceError::InvalidData(
                "Moment ordinal repair received an inactive Moment ID".into(),
            )
        })?;
        targets_by_run
            .entry(run_id.clone())
            .or_default()
            .push((moment_id.clone(), ordinal));
    }
    for (run_id, active_targets) in targets_by_run {
        reindex_moment_record_ordinals(transaction, project_id, &run_id, &active_targets)?;
    }
    Ok(())
}

/// Reassigns one run's ordinal namespace without ever colliding with its unique index. The
/// caller supplies explicit global display ordinals for active cards; stale rows are placed
/// deterministically after them by their prior ordinal and stable ID. The two-phase move is
/// necessary because stale records participate in `UNIQUE(run_id, ordinal)` too.
fn reindex_moment_record_ordinals(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    run_id: &str,
    active_targets: &[(String, i64)],
) -> Result<()> {
    let rows = transaction
        .prepare(
            "SELECT id, stale, ordinal
             FROM moment_records
             WHERE project_id = ?1 AND run_id = ?2
             ORDER BY stale ASC, ordinal ASC, id ASC",
        )?
        .query_map(params![project_id.to_string(), run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        if active_targets.is_empty() {
            return Ok(());
        }
        return Err(PersistenceError::InvalidData(
            "cannot order active Moments for an empty analysis run".into(),
        ));
    }

    let active_ids = rows
        .iter()
        .filter(|(_, stale, _)| !stale)
        .map(|(id, _, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    let requested_active_ids = active_targets
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    let requested_ordinals = active_targets
        .iter()
        .map(|(_, ordinal)| *ordinal)
        .collect::<BTreeSet<_>>();
    if requested_active_ids.len() != active_targets.len()
        || requested_ordinals.len() != active_targets.len()
        || active_ids.len() != active_targets.len()
        || active_ids != requested_active_ids
    {
        return Err(PersistenceError::InvalidData(
            "Moment ordinal repair received a stale or incomplete active projection".into(),
        ));
    }

    let mut target_ordinals = active_targets
        .iter()
        .map(|(id, ordinal)| (id.clone(), *ordinal))
        .collect::<BTreeMap<_, _>>();
    let stale_target_start = active_targets
        .iter()
        .map(|(_, ordinal)| *ordinal)
        .max()
        .unwrap_or(-1)
        .checked_add(1)
        .ok_or_else(|| {
            PersistenceError::InvalidData("Moment ordinal exceeds SQLite integer range".into())
        })?;
    for (index, (id, _, _)) in rows.iter().filter(|(_, stale, _)| *stale).enumerate() {
        let index = i64::try_from(index).map_err(|_| {
            PersistenceError::InvalidData("Moment count exceeds SQLite integer range".into())
        })?;
        let ordinal = stale_target_start.checked_add(index).ok_or_else(|| {
            PersistenceError::InvalidData("Moment ordinal exceeds SQLite integer range".into())
        })?;
        target_ordinals.insert(id.clone(), ordinal);
    }
    let lowest_ordinal = rows
        .iter()
        .map(|(_, _, ordinal)| *ordinal)
        .min()
        .unwrap_or(0);
    let highest_ordinal = rows
        .iter()
        .map(|(_, _, ordinal)| *ordinal)
        .max()
        .unwrap_or(-1);
    let final_highest_ordinal = target_ordinals.values().copied().max().unwrap_or(-1);
    let staging_floor = highest_ordinal.max(final_highest_ordinal);
    let staging_offset = staging_floor
        .checked_sub(lowest_ordinal)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            PersistenceError::InvalidData("Moment ordinal exceeds SQLite integer range".into())
        })?;
    highest_ordinal.checked_add(staging_offset).ok_or_else(|| {
        PersistenceError::InvalidData(
            "Moment ordinal repair cannot allocate a collision-free staging range".into(),
        )
    })?;

    // Every staged ordinal is strictly above both the current and final maximum. This bulk move
    // therefore has a disjoint target range even while SQLite enforces uniqueness row by row.
    transaction.execute(
        "UPDATE moment_records
         SET ordinal = ordinal + ?3
         WHERE project_id = ?1 AND run_id = ?2",
        params![project_id.to_string(), run_id, staging_offset],
    )?;
    for (moment_id, ordinal) in target_ordinals {
        transaction.execute(
            "UPDATE moment_records
             SET ordinal = ?1
             WHERE id = ?2 AND project_id = ?3 AND run_id = ?4",
            params![ordinal, moment_id, project_id.to_string(), run_id],
        )?;
    }
    Ok(())
}
fn intelligence_summary_for_asset(
    connection: &Connection,
    asset_id: &str,
) -> Result<Option<IntelligenceSummary>> {
    let current: Option<IntelligenceSummary> = connection
        .query_row(
            "SELECT artifact.status, tq.technical_quality_band, tq.technical_quality_score,
                recommendation.label, recommendation.confidence,
                (SELECT g.id FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = artifact.media_asset_id AND g.stale = 0 ORDER BY CASE g.kind WHEN 'exact_duplicate_set' THEN 0 WHEN 'near_duplicate_set' THEN 1 WHEN 'burst' THEN 2 ELSE 3 END, g.created_at DESC LIMIT 1),
                (SELECT g.kind FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = artifact.media_asset_id AND g.stale = 0 ORDER BY CASE g.kind WHEN 'exact_duplicate_set' THEN 0 WHEN 'near_duplicate_set' THEN 1 WHEN 'burst' THEN 2 ELSE 3 END, g.created_at DESC LIMIT 1),
                (SELECT COUNT(*) FROM similarity_group_members gm WHERE gm.group_id = (SELECT g.id FROM similarity_groups g JOIN similarity_group_members chosen ON chosen.group_id = g.id WHERE chosen.media_asset_id = artifact.media_asset_id AND g.stale = 0 ORDER BY CASE g.kind WHEN 'exact_duplicate_set' THEN 0 WHEN 'near_duplicate_set' THEN 1 WHEN 'burst' THEN 2 ELSE 3 END, g.created_at DESC LIMIT 1)),
                (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = artifact.media_asset_id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = artifact.media_asset_id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = artifact.input_fingerprint ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)),
                (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = artifact.media_asset_id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = artifact.media_asset_id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = artifact.input_fingerprint ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1) AND face.eye_state = 'open'),
                (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = artifact.media_asset_id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = artifact.media_asset_id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = artifact.input_fingerprint ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1) AND face.eye_state = 'closed'),
                tq.blur_level, tq.sharpness_band, tq.confidence, artifact.error_message
             FROM analysis_artifacts artifact
             LEFT JOIN technical_quality tq ON tq.media_asset_id = artifact.media_asset_id
               AND tq.input_fingerprint = artifact.input_fingerprint
               AND tq.provider = artifact.provider
               AND tq.provider_version = artifact.provider_version
               AND tq.stale = 0
             LEFT JOIN analysis_recommendations recommendation ON recommendation.id = (
               SELECT current_recommendation.id
               FROM analysis_recommendations current_recommendation
               WHERE current_recommendation.media_asset_id = artifact.media_asset_id
                 AND current_recommendation.input_fingerprint = artifact.input_fingerprint
                 AND current_recommendation.stale = 0
               ORDER BY current_recommendation.generated_at DESC, current_recommendation.id DESC
               LIMIT 1
             )
             WHERE artifact.media_asset_id = ?1
               AND artifact.artifact_type = 'capture_intelligence'
               AND artifact.stale = 0
             ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1",
            params![asset_id],
            |row| {
                Ok(IntelligenceSummary {
                    status: row.get(0)?, technical_quality_band: row.get(1)?, technical_quality_score: row.get(2)?,
                    recommendation: row.get(3)?, recommendation_confidence: row.get(4)?,
                    similarity_group_id: row.get(5)?, similarity_group_kind: row.get(6)?,
                    similar_count: row.get::<_, i64>(7)? as u64, face_count: row.get::<_, i64>(8)? as u64,
                    open_eyes_count: row.get::<_, i64>(9)? as u64, possible_closed_eyes_count: row.get::<_, i64>(10)? as u64,
                    blur_level: row.get(11)?, sharpness_band: row.get(12)?, confidence: row.get(13)?, unavailable_reason: row.get(14)?,
                })
            },
        )
        .optional()?;
    if current.is_some() {
        return Ok(current);
    }

    // History exists, but candidate discovery has already found an incompatible preview or
    // analyzer tuple. Show a truthful transient state rather than presenting its old READY
    // evidence as current—or claiming the asset has never been analyzed.
    let stale_error = connection
        .query_row(
            "SELECT error_message
             FROM analysis_artifacts
             WHERE media_asset_id = ?1
               AND artifact_type = 'capture_intelligence'
               AND stale = 1
             ORDER BY generated_at DESC, id DESC
             LIMIT 1",
            params![asset_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(stale_error.map(|error| IntelligenceSummary {
        status: Some("stale".into()),
        unavailable_reason: Some(error.unwrap_or_else(|| {
            "A newer local preview or analyzer version requires reanalysis".into()
        })),
        ..IntelligenceSummary::default()
    }))
}

fn enum_label<T: serde::Serialize>(value: &T) -> Result<String> {
    let serialized = json(value)?;
    Ok(serialized.trim_matches('"').to_owned())
}

fn to_sql_error(error: PersistenceError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn timestamp(value: &Timestamp) -> String {
    value.to_rfc3339()
}
fn optional_timestamp(value: &Option<Timestamp>) -> Option<String> {
    value.as_ref().map(timestamp)
}
fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn validate_culling_mode(mode: &str) -> Result<()> {
    match mode {
        "all_photos" | "similar_sets" | "ai_review_queue" => Ok(()),
        _ => Err(PersistenceError::InvalidData(
            "culling mode must be all_photos, similar_sets, or ai_review_queue".into(),
        )),
    }
}

fn culling_filter_sql(filter: &str) -> Result<&'static str> {
    match filter {
        "all" => Ok("1 = 1"),
        "unreviewed" => Ok("md.decision IS NULL"),
        "keep" => Ok("md.decision = 'keep'"),
        "reject" => Ok("md.decision = 'reject'"),
        "review" => Ok("md.decision = 'review'"),
        "starred" => Ok("COALESCE(md.starred, 0) = 1"),
        "five_star" => Ok("COALESCE(md.rating, 0) = 5"),
        "four_plus" => Ok("COALESCE(md.rating, 0) >= 4"),
        "strong_candidates" => Ok("EXISTS (SELECT 1 FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 AND r.status = 'ready' AND r.label = 'strong_candidate')"),
        "technical_issues" => Ok("EXISTS (SELECT 1 FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 AND r.status = 'ready' AND r.label = 'technical_issue')"),
        "possible_duplicates" => Ok("EXISTS (SELECT 1 FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 AND r.status = 'ready' AND r.label = 'probable_duplicate')"),
        "similar_groups" => Ok("EXISTS (SELECT 1 FROM similarity_group_members gm JOIN similarity_groups g ON g.id = gm.group_id WHERE gm.media_asset_id = a.id AND g.stale = 0)"),
        "faces" => Ok("EXISTS (SELECT 1 FROM face_analyses f WHERE f.media_asset_id = a.id)"),
        "blur_review" => Ok("EXISTS (SELECT 1 FROM technical_quality q WHERE q.media_asset_id = a.id AND q.stale = 0 AND q.blur_level IN ('moderate', 'high', 'uncertain'))"),
        _ => Err(PersistenceError::InvalidData("unsupported culling filter".into())),
    }
}

fn review_session_view_from_row(row: &Row<'_>) -> rusqlite::Result<ReviewSessionView> {
    Ok(ReviewSessionView {
        id: row.get(0)?,
        project_id: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        mode: row.get(4)?,
        last_asset_id: row.get(5)?,
        last_group_id: row.get(6)?,
        filter_context: row.get(7)?,
        photos_reviewed: row.get::<_, i64>(8)? as u64,
        sets_reviewed: row.get::<_, i64>(9)? as u64,
    })
}

fn culling_decision_for_asset(
    connection: &Connection,
    project_id: &ProjectId,
    asset_id: &str,
) -> Result<CullingDecisionView> {
    connection
        .query_row(
            "SELECT decision, rating, starred, note, flags_json, updated_at FROM media_decisions WHERE project_id = ?1 AND media_asset_id = ?2",
            params![project_id.to_string(), asset_id],
            |row| {
                let flags: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?)
                    .map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error)))?;
                Ok(CullingDecisionView {
                    decision: row.get(0)?, rating: row.get::<_, i64>(1)? as u8, starred: row.get(2)?, note: row.get(3)?, flags, updated_at: row.get(5)?,
                })
            },
        )
        .optional()?
        .map_or_else(|| Ok(CullingDecisionView::default()), Ok)
}

/// A source snapshot and a model artifact must share one compatibility identity. A future
/// schema must be explicitly migrated/backfilled; old compact snapshots are never reinterpreted
/// as new feature vectors merely because their JSON happens to parse.
const STUDIO_FEATURE_SCHEMA_VERSION: &str = studio_brain::STUDIO_BRAIN_FEATURE_SCHEMA_VERSION;

#[derive(Debug, Clone)]
struct StudioHistoricalRow {
    source_kind: String,
    source_record_id: String,
    project_id: String,
    media_asset_id: Option<String>,
    decision_type: String,
    decision_value: Option<String>,
    occurred_at: String,
    review_session_id: Option<String>,
    similarity_group_id: Option<String>,
    moment_id: Option<String>,
}

#[derive(Debug, Clone)]
struct StudioHistoricalSimilarityRow {
    id: String,
    project_id: String,
    group_id: String,
    chosen_asset_id: String,
    alternative_asset_ids: Vec<String>,
    generic_recommendation_json: serde_json::Value,
    created_at: String,
}

#[derive(Debug, Clone)]
struct StudioHistoricalMomentRow {
    id: String,
    project_id: String,
    moment_id: String,
    asset_id: String,
    created_at: String,
}

fn ensure_studio_profile_exists(transaction: &Transaction<'_>, profile_id: &str) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM studio_profiles WHERE id = ?1)",
        params![profile_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(PersistenceError::InvalidData(
            "Studio Profile does not exist locally".into(),
        ));
    }
    Ok(())
}

// This is intentionally a direct field mapping for one append-only event row. Keeping the
// optional foreign keys explicit makes call sites state their provenance rather than allowing a
// lossy generic payload to conceal it.
#[allow(clippy::too_many_arguments)]
fn insert_studio_event(
    transaction: &Transaction<'_>,
    profile_id: &str,
    project_id: Option<&str>,
    run_id: Option<&str>,
    model_id: Option<&str>,
    event_type: &str,
    details: &serde_json::Value,
    created_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO studio_events (id, studio_profile_id, project_id, studio_training_run_id, studio_model_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![Uuid::new_v4().to_string(), profile_id, project_id, run_id, model_id, event_type, json(details)?, created_at],
    )?;
    Ok(())
}

fn mark_studio_profile_stale_if_active(
    transaction: &Transaction<'_>,
    profile_id: &str,
    updated_at: &str,
) -> Result<()> {
    let has_active_model: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM studio_models WHERE studio_profile_id = ?1 AND state = 'active')",
        params![profile_id],
        |row| row.get(0),
    )?;
    if has_active_model {
        transaction.execute(
            "UPDATE studio_profiles SET training_status = 'stale', updated_at = ?2 WHERE id = ?1 AND training_status <> 'error'",
            params![profile_id, updated_at],
        )?;
    }
    Ok(())
}

/// Resolves exactly the action guard created atomically with a human authority write. It never
/// clears another action's deferred guard, including one from a different project that is opted
/// out of future training.
fn finish_studio_source_materialization(
    connection: &Connection,
    profile_id: &str,
    source_kind: &str,
    source_record_id: &str,
) -> Result<()> {
    let transaction = moment_write_transaction(connection)?;
    transaction.execute(
        "DELETE FROM studio_source_materialization_guards
         WHERE studio_profile_id = ?1 AND source_kind = ?2 AND source_record_id = ?3",
        params![profile_id, source_kind, source_record_id],
    )?;
    transaction.execute(
        "UPDATE studio_profiles SET updated_at = ?2 WHERE id = ?1",
        params![profile_id, timestamp(&Utc::now())],
    )?;
    transaction.commit()?;
    Ok(())
}

fn studio_historical_decision_rows(connection: &Connection) -> Result<Vec<StudioHistoricalRow>> {
    let mut statement = connection.prepare(
        "SELECT id, project_id, media_asset_id, session_id, event_type, current_value_json, created_at
         FROM decision_history
         WHERE source = 'human' AND event_type IN ('DECISION_CHANGED', 'RATING_CHANGED', 'STAR_CHANGED')
         ORDER BY created_at ASC, id ASC",
    )?;
    let mut rows = Vec::new();
    for result in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })? {
        let (id, project_id, media_asset_id, session_id, event_type, current_json, created_at) =
            result?;
        let value: serde_json::Value = from_json(&current_json)?;
        let (decision_type, decision_value) = match event_type.as_str() {
            "DECISION_CHANGED" => {
                let Some(value) = value
                    .as_str()
                    .filter(|value| matches!(*value, "keep" | "review" | "reject"))
                else {
                    continue;
                };
                ("culling_decision".into(), Some(value.to_owned()))
            }
            "RATING_CHANGED" => {
                let Some(value) = value.as_u64().filter(|value| *value <= 5) else {
                    continue;
                };
                ("rating".into(), Some(value.to_string()))
            }
            "STAR_CHANGED" => {
                let Some(value) = value.as_bool() else {
                    continue;
                };
                ("star".into(), Some(value.to_string()))
            }
            _ => continue,
        };
        rows.push(StudioHistoricalRow {
            source_kind: "decision_history".into(),
            source_record_id: id,
            project_id,
            media_asset_id: Some(media_asset_id),
            decision_type,
            decision_value,
            occurred_at: created_at,
            review_session_id: session_id,
            similarity_group_id: None,
            moment_id: None,
        });
    }
    Ok(rows)
}

fn studio_historical_override_rows(connection: &Connection) -> Result<Vec<StudioHistoricalRow>> {
    let mut statement = connection.prepare(
        "SELECT override_record.id, asset.project_id, override_record.media_asset_id, override_record.decision, override_record.decided_at
         FROM intelligence_overrides override_record JOIN media_assets asset ON asset.id = override_record.media_asset_id
         WHERE override_record.decision IN ('keep', 'review', 'reject')
         ORDER BY override_record.decided_at ASC, override_record.id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(StudioHistoricalRow {
                source_kind: "m4_human_override".into(),
                source_record_id: row.get(0)?,
                project_id: row.get(1)?,
                media_asset_id: Some(row.get(2)?),
                decision_type: "culling_decision".into(),
                decision_value: Some(row.get(3)?),
                occurred_at: row.get(4)?,
                review_session_id: None,
                similarity_group_id: None,
                moment_id: None,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn studio_historical_similarity_rows(
    connection: &Connection,
) -> Result<Vec<StudioHistoricalSimilarityRow>> {
    let mut statement = connection.prepare(
        "SELECT id, project_id, similarity_group_id, chosen_asset_id, compared_asset_ids_json, ai_recommendation_json, created_at
         FROM preference_examples ORDER BY created_at ASC, id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            let alternatives: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            let recommendation: serde_json::Value = serde_json::from_str(&row.get::<_, String>(5)?)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            Ok(StudioHistoricalSimilarityRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                group_id: row.get(2)?,
                chosen_asset_id: row.get(3)?,
                alternative_asset_ids: alternatives,
                generic_recommendation_json: recommendation,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn studio_historical_moment_rows(
    connection: &Connection,
) -> Result<Vec<StudioHistoricalMomentRow>> {
    let mut statement = connection.prepare(
        "SELECT event.id, event.project_id, event.moment_id, json_extract(event.details_json, '$.assetId'), event.created_at
         FROM moment_events event JOIN media_assets asset ON asset.id = json_extract(event.details_json, '$.assetId')
         WHERE event.event_type = 'MOMENT_REPRESENTATIVE_CHANGED'
         ORDER BY event.created_at ASC, event.id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(StudioHistoricalMomentRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                moment_id: row.get(2)?,
                asset_id: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// The immutable row fields are deliberately explicit: callers must supply the captured feature
// snapshot, generic evidence, displayed state, provenance, and timestamp separately.
#[allow(clippy::too_many_arguments)]
fn insert_studio_training_example(
    transaction: &Transaction<'_>,
    profile_id: &str,
    row: &StudioHistoricalRow,
    feature_snapshot: &serde_json::Value,
    generic_recommendation: &serde_json::Value,
    recommendation_shown: &str,
    provenance: &str,
    created_at: &str,
) -> Result<bool> {
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO studio_training_examples (id, studio_profile_id, project_id, media_asset_id, source_kind, source_record_id, decision_type, decision_value, occurred_at, review_session_id, similarity_group_id, moment_id, generic_recommendation_json, studio_recommendation_id_at_decision, recommendation_shown, provenance, feature_schema_version, feature_snapshot_json, training_eligible, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16, ?17, 1, ?18)",
        params![Uuid::new_v4().to_string(), profile_id, row.project_id, row.media_asset_id, row.source_kind, row.source_record_id, row.decision_type, row.decision_value, row.occurred_at, row.review_session_id, row.similarity_group_id, row.moment_id, json(generic_recommendation)?, recommendation_shown, provenance, STUDIO_FEATURE_SCHEMA_VERSION, json(feature_snapshot)?, created_at],
    )?;
    Ok(changed == 1)
}

fn studio_historical_row_from_live_decision(
    history_id: &str,
    project_id: &str,
    asset_id: &str,
    session_id: Option<&str>,
    event_type: &str,
    current: &serde_json::Value,
    occurred_at: &str,
) -> Option<StudioHistoricalRow> {
    let (decision_type, decision_value) = match event_type {
        "DECISION_CHANGED" => (
            "culling_decision",
            current
                .as_str()
                .filter(|value| matches!(*value, "keep" | "review" | "reject"))
                .map(ToOwned::to_owned),
        ),
        "RATING_CHANGED" => (
            "rating",
            current
                .as_u64()
                .filter(|value| *value <= 5)
                .map(|value| value.to_string()),
        ),
        "STAR_CHANGED" => ("star", current.as_bool().map(|value| value.to_string())),
        _ => return None,
    };
    decision_value.map(|decision_value| StudioHistoricalRow {
        source_kind: "decision_history".into(),
        source_record_id: history_id.into(),
        project_id: project_id.into(),
        media_asset_id: Some(asset_id.into()),
        decision_type: decision_type.into(),
        decision_value: Some(decision_value),
        occurred_at: occurred_at.into(),
        review_session_id: session_id.map(ToOwned::to_owned),
        similarity_group_id: None,
        moment_id: None,
    })
}

/// Project participation is intentionally evaluated at source-materialization time as well as
/// at training-query time. This keeps an opted-out project from creating new preference records
/// while retaining every underlying human decision unchanged in its normal M5 history.
fn studio_project_training_is_included(
    connection: &Connection,
    profile_id: &str,
    project_id: &str,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT COALESCE((SELECT included
                                FROM project_training_preferences
                                WHERE studio_profile_id = ?1 AND project_id = ?2), 1)",
            params![profile_id, project_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn materialize_live_studio_training_example(
    connection: &Connection,
    profile_id: &str,
    row: &StudioHistoricalRow,
) -> Result<()> {
    // An opt-out controls contribution, not use of an already-active local model. Do not even
    // materialize a new source row while the project is excluded; later re-inclusion can use
    // the explicit, bounded historical backfill during a requested training run.
    if !studio_project_training_is_included(connection, profile_id, &row.project_id)? {
        return Ok(());
    }
    let asset_id = row.media_asset_id.as_deref().ok_or_else(|| {
        PersistenceError::InvalidData("live Studio signal requires a media asset".into())
    })?;
    let snapshot = studio_feature_snapshot(connection, &row.project_id, asset_id)?;
    let row = studio_historical_row_with_snapshot_context(row.clone(), &snapshot);
    let generic = studio_generic_recommendation_snapshot(connection, asset_id)?;
    let transaction = moment_write_transaction(connection)?;
    let inserted = insert_studio_training_example(
        &transaction,
        profile_id,
        &row,
        &snapshot,
        &generic,
        "unknown",
        "explicit_human",
        &row.occurred_at,
    )?;
    if inserted {
        mark_studio_profile_stale_if_active(&transaction, profile_id, &row.occurred_at)?;
    }
    transaction.commit()?;
    Ok(())
}

fn studio_historical_row_with_snapshot_context(
    mut row: StudioHistoricalRow,
    snapshot: &serde_json::Value,
) -> StudioHistoricalRow {
    if row.similarity_group_id.is_none() {
        row.similarity_group_id = snapshot
            .get("similarityGroupId")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
    }
    if row.moment_id.is_none() {
        row.moment_id = snapshot
            .get("momentId")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
    }
    row
}

// A representative source needs all of the chosen/alternative evidence to create immutable
// pairwise references; grouping these into an opaque JSON argument would weaken validation.
#[allow(clippy::too_many_arguments)]
fn materialize_live_similarity_representative(
    connection: &Connection,
    profile_id: &str,
    preference_example_id: &str,
    project_id: &str,
    group_id: &str,
    chosen_asset_id: &str,
    alternatives: &[String],
    generic_recommendation_json: &str,
    occurred_at: &str,
) -> Result<()> {
    if !studio_project_training_is_included(connection, profile_id, project_id)? {
        return Ok(());
    }
    let snapshot = studio_feature_snapshot(connection, project_id, chosen_asset_id)?;
    let generic: serde_json::Value = from_json(generic_recommendation_json)?;
    let source = StudioHistoricalRow {
        source_kind: "similar_set_representative".into(),
        source_record_id: preference_example_id.into(),
        project_id: project_id.into(),
        media_asset_id: Some(chosen_asset_id.into()),
        decision_type: "similar_set_representative".into(),
        decision_value: None,
        occurred_at: occurred_at.into(),
        review_session_id: None,
        similarity_group_id: Some(group_id.into()),
        moment_id: None,
    };
    let transaction = moment_write_transaction(connection)?;
    let inserted = insert_studio_training_example(
        &transaction,
        profile_id,
        &source,
        &snapshot,
        &generic,
        "unknown",
        "explicit_human",
        occurred_at,
    )?;
    if inserted {
        let example_id: String = transaction.query_row("SELECT id FROM studio_training_examples WHERE studio_profile_id = ?1 AND source_kind = ?2 AND source_record_id = ?3", params![profile_id, source.source_kind, source.source_record_id], |row| row.get(0))?;
        transaction.execute("INSERT OR IGNORE INTO studio_training_example_references (studio_training_example_id, role, media_asset_id) VALUES (?1, 'chosen', ?2)", params![example_id, chosen_asset_id])?;
        for alternative in alternatives {
            transaction.execute("INSERT OR IGNORE INTO studio_training_example_references (studio_training_example_id, role, media_asset_id) VALUES (?1, 'alternative', ?2)", params![example_id, alternative])?;
        }
        mark_studio_profile_stale_if_active(&transaction, profile_id, occurred_at)?;
    }
    transaction.commit()?;
    Ok(())
}

fn materialize_live_moment_representative(
    connection: &Connection,
    profile_id: &str,
    moment_event_id: &str,
    project_id: &str,
    moment_id: &str,
    asset_id: &str,
    occurred_at: &str,
) -> Result<()> {
    if !studio_project_training_is_included(connection, profile_id, project_id)? {
        return Ok(());
    }
    let snapshot = studio_feature_snapshot(connection, project_id, asset_id)?;
    let generic = studio_generic_recommendation_snapshot(connection, asset_id)?;
    let source = StudioHistoricalRow {
        source_kind: "moment_representative".into(),
        source_record_id: moment_event_id.into(),
        project_id: project_id.into(),
        media_asset_id: Some(asset_id.into()),
        decision_type: "moment_representative".into(),
        decision_value: None,
        occurred_at: occurred_at.into(),
        review_session_id: None,
        similarity_group_id: None,
        moment_id: Some(moment_id.into()),
    };
    let transaction = moment_write_transaction(connection)?;
    if insert_studio_training_example(
        &transaction,
        profile_id,
        &source,
        &snapshot,
        &generic,
        "unknown",
        "explicit_human",
        occurred_at,
    )? {
        mark_studio_profile_stale_if_active(&transaction, profile_id, occurred_at)?;
    }
    transaction.commit()?;
    Ok(())
}

fn json_value_from_row(row: &Row<'_>, index: usize) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn studio_training_example_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<StudioTrainingExampleRecord> {
    Ok(StudioTrainingExampleRecord {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        project_id: row.get(2)?,
        media_asset_id: row.get(3)?,
        source_kind: row.get(4)?,
        source_record_id: row.get(5)?,
        decision_type: row.get(6)?,
        decision_value: row.get(7)?,
        occurred_at: row.get(8)?,
        review_session_id: row.get(9)?,
        similarity_group_id: row.get(10)?,
        moment_id: row.get(11)?,
        generic_recommendation_json: json_value_from_row(row, 12)?,
        studio_recommendation_id_at_decision: row.get(13)?,
        recommendation_shown: row.get(14)?,
        provenance: row.get(15)?,
        feature_schema_version: row.get(16)?,
        feature_snapshot_json: json_value_from_row(row, 17)?,
        training_eligible: row.get(18)?,
        created_at: row.get(19)?,
    })
}

fn studio_training_run_from_row(row: &Row<'_>) -> rusqlite::Result<StudioTrainingRunRecord> {
    Ok(StudioTrainingRunRecord {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        background_job_id: row.get(2)?,
        algorithm: row.get(3)?,
        algorithm_version: row.get(4)?,
        feature_schema_version: row.get(5)?,
        parameters_json: json_value_from_row(row, 6)?,
        snapshot_hash: row.get(7)?,
        snapshot_count: row.get::<_, i64>(8)? as u64,
        previous_active_model_id: row.get(9)?,
        state: row.get(10)?,
        error_message: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        finished_at: row.get(14)?,
    })
}

fn studio_model_from_row(row: &Row<'_>) -> rusqlite::Result<StudioModelRecord> {
    Ok(StudioModelRecord {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        training_run_id: row.get(2)?,
        algorithm: row.get(3)?,
        model_version: row.get(4)?,
        feature_schema_version: row.get(5)?,
        artifact_json: json_value_from_row(row, 6)?,
        checksum: row.get(7)?,
        artifact_size_bytes: row.get::<_, i64>(8)? as u64,
        state: row.get(9)?,
        metrics_json: json_value_from_row(row, 10)?,
        created_at: row.get(11)?,
        activated_at: row.get(12)?,
    })
}

/// Store compact, queryable evaluation summaries beside the immutable candidate artifact. The
/// complete structured report remains in `studio_models.metrics_json`; these rows deliberately
/// contain no filenames, paths, notes, embeddings, decisions, or other preference source data.
fn store_studio_model_metric_rows(
    transaction: &Transaction<'_>,
    record: &StudioModelRecord,
) -> Result<()> {
    let evaluation = record.metrics_json.get("evaluation");
    let split_metadata = evaluation
        .and_then(|value| value.get("split"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"strategy":"unavailable"}));
    let metadata = serde_json::json!({
        "source": "candidate_evaluation",
        "algorithm": record.algorithm,
        "featureSchemaVersion": record.feature_schema_version,
        "split": split_metadata,
    });
    let model_metrics = [
        ("personalized", "personalModel"),
        ("generic", "genericBaseline"),
        ("majority", "majorityBaseline"),
    ];
    let metric_fields = [
        ("accuracy", "accuracy"),
        ("macro_f1", "macroF1"),
        ("log_loss", "logLoss"),
        ("brier_score", "brierScore"),
        ("expected_calibration_error", "expectedCalibrationError"),
    ];
    for (prefix, key) in model_metrics {
        let Some(summary) = evaluation.and_then(|value| value.get(key)) else {
            continue;
        };
        let sample_count = summary
            .get("sampleCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            .min(i64::MAX as u64) as i64;
        for (metric_name, field) in metric_fields {
            let Some(value) = summary
                .get(field)
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite())
            else {
                continue;
            };
            transaction.execute(
                "INSERT INTO studio_model_metrics (id, studio_model_id, split, metric_name, metric_value, sample_count, metadata_json, created_at)
                 VALUES (?1, ?2, 'validation', ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    record.id,
                    format!("{prefix}_{metric_name}"),
                    value,
                    sample_count,
                    json(&metadata)?,
                    record.created_at,
                ],
            )?;
        }
    }
    if let Some(temperature) = evaluation
        .and_then(|value| value.get("calibration"))
        .and_then(|value| value.get("temperature"))
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
    {
        transaction.execute(
            "INSERT INTO studio_model_metrics (id, studio_model_id, split, metric_name, metric_value, sample_count, metadata_json, created_at)
             VALUES (?1, ?2, 'validation', 'calibration_temperature', ?3, 0, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                record.id,
                temperature,
                json(&metadata)?,
                record.created_at,
            ],
        )?;
    }
    Ok(())
}

/// Persistence validates the small structured artifact at its trust boundary as well as the
/// core validating it before storage. This is corruption detection, not a claim of protection
/// against a user who intentionally changes both a local database row and its checksum.
fn verify_studio_model_artifact(record: &StudioModelRecord) -> Result<()> {
    if record.feature_schema_version != studio_brain::STUDIO_BRAIN_FEATURE_SCHEMA_VERSION {
        return Err(PersistenceError::InvalidData(
            "Studio model feature schema is incompatible with this build".into(),
        ));
    }
    let encoded = VerifiedModelArtifact {
        artifact_json: serde_json::to_string(&record.artifact_json)?,
        checksum: record.checksum.clone(),
    };
    let artifact = decode_verified_model_artifact(&encoded).map_err(|error| {
        PersistenceError::InvalidData(format!("Studio model artifact validation failed: {error}"))
    })?;
    if artifact.feature_schema_version != record.feature_schema_version {
        return Err(PersistenceError::InvalidData(
            "Studio model artifact and persisted feature schema differ".into(),
        ));
    }
    Ok(())
}

fn invalidate_corrupt_active_studio_model(
    connection: &Connection,
    record: &StudioModelRecord,
) -> Result<()> {
    let transaction = moment_write_transaction(connection)?;
    let now = timestamp(&Utc::now());
    let changed = transaction.execute(
        "UPDATE studio_models SET state = 'invalid' WHERE id = ?1 AND studio_profile_id = ?2 AND state = 'active'",
        params![record.id, record.profile_id],
    )?;
    if changed > 0 {
        transaction.execute(
            "UPDATE studio_recommendations SET stale = 1, status = 'unavailable' WHERE studio_model_id = ?1 AND stale = 0",
            params![record.id],
        )?;
        transaction.execute(
            "UPDATE studio_profiles SET training_status = 'error', last_error = ?2, updated_at = ?3 WHERE id = ?1",
            params![record.profile_id, "A local Studio Brain model could not be validated. Generic technical evidence remains available.", now],
        )?;
        insert_studio_event(
            &transaction,
            &record.profile_id,
            None,
            Some(&record.training_run_id),
            Some(&record.id),
            "STUDIO_MODEL_INVALIDATED",
            &serde_json::json!({"reason":"artifact_validation_failed","genericFallbackAvailable":true}),
            &now,
        )?;
    }
    transaction.commit()?;
    Ok(())
}

/// Feature snapshots include only bounded technical, anonymous face/eye availability, generic
/// recommendation, Similar Set, Moment, and semantic-availability signals. When any of those
/// derived inputs changes, the old advisory projection remains historical but is never shown as
/// current. A later explicit Studio update rebuilds it from the active model and fresh evidence.
fn mark_studio_recommendations_stale_for_asset(
    connection: &Connection,
    asset_id: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE media_asset_id = ?1 AND stale = 0",
        params![asset_id],
    )?;
    Ok(())
}

fn mark_studio_recommendations_stale_for_project(
    connection: &Connection,
    project_id: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE studio_recommendations SET stale = 1, status = 'stale' WHERE project_id = ?1 AND stale = 0",
        params![project_id],
    )?;
    Ok(())
}

/// M8 source materialization is deliberately fail-open relative to an already-committed human
/// action. The next explicit, idempotent historical backfill can recover the source row; until
/// then an active profile is marked stale rather than pretending it contains the new choice.
/// This is best effort because it runs on the error path and must never turn a valid M4/M5/M7
/// action into a reported failure.
fn mark_studio_source_capture_deferred(connection: &Connection, profile_id: &str) {
    let _ = connection.execute(
        "UPDATE studio_profiles
         SET source_revision = source_revision + 1,
             training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') THEN 'stale' ELSE training_status END,
             readiness_json = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') THEN '{\"state\":\"stale\",\"reasons\":[\"A local training source will be recovered during your next explicit Studio Brain update.\"]}' ELSE readiness_json END,
             last_error = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') THEN NULL ELSE last_error END,
             updated_at = ?2
         WHERE id = ?1",
        params![profile_id, timestamp(&Utc::now())],
    );
}

fn studio_generic_recommendation_snapshot(
    connection: &Connection,
    asset_id: &str,
) -> Result<serde_json::Value> {
    let value = connection.query_row(
        "SELECT label, confidence, id FROM analysis_recommendations WHERE media_asset_id = ?1 AND stale = 0 AND status = 'ready' ORDER BY generated_at DESC, id DESC LIMIT 1",
        params![asset_id],
        |row| Ok(serde_json::json!({"label": row.get::<_, String>(0)?, "confidence": row.get::<_, f64>(1)?, "id": row.get::<_, String>(2)?})),
    ).optional()?;
    Ok(value.unwrap_or_else(|| serde_json::json!({"status":"unavailable"})))
}

/// Reconstructs only the *alternative* side of an M5 `PreferenceExample` from that example's
/// immutable compact evidence. It deliberately never consults the current asset record: a
/// changed technical analysis, Similar Set, or Moment must not rewrite historic training data.
fn studio_pairwise_snapshot_from_evidence(
    technical_evidence: &serde_json::Value,
    generic_recommendation: &serde_json::Value,
    similarity_group_id: &str,
    asset_id: &str,
) -> Option<serde_json::Value> {
    let members = technical_evidence.as_array()?;
    let member = members.iter().find(|candidate| {
        candidate.get("assetId").and_then(serde_json::Value::as_str) == Some(asset_id)
    })?;
    let technical_score = member
        .get("technicalQualityScore")
        .and_then(serde_json::Value::as_f64);
    let higher_technical_peers = technical_score.map(|score| {
        members
            .iter()
            .filter_map(|candidate| {
                candidate
                    .get("technicalQualityScore")
                    .and_then(serde_json::Value::as_f64)
            })
            .filter(|candidate_score| *candidate_score > score)
            .count()
    });
    let generic_asset_id = generic_recommendation
        .get("assetId")
        .and_then(serde_json::Value::as_str);
    let generic_label = member
        .get("aiRecommendation")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            generic_recommendation
                .get("recommendation")
                .and_then(serde_json::Value::as_str)
        });
    Some(serde_json::json!({
        "schemaVersion": STUDIO_FEATURE_SCHEMA_VERSION,
        "technicalQualityScore": technical_score,
        "globalSharpness": member.get("globalSharpness").and_then(serde_json::Value::as_f64),
        "directionalBlurRatio": serde_json::Value::Null,
        "similarityGroupId": similarity_group_id,
        "similarityGroupSize": members.len(),
        "isGenericRepresentative": generic_asset_id == Some(asset_id),
        "higherTechnicalPeers": higher_technical_peers,
        "genericRecommendation": generic_label,
        "semanticEvidenceAvailable": false,
    }))
}

/// Builds only compact M8 permitted features. Missing values remain `null` and each is paired
/// with an explicit availability flag by the model crate; zero is never used to silently mean
/// missing. The query does not select originals, paths, notes, raw embeddings, or identities.
fn studio_feature_snapshot(
    connection: &Connection,
    project_id: &str,
    asset_id: &str,
) -> Result<serde_json::Value> {
    let row = connection.query_row(
        "SELECT
           (SELECT technical_quality_score FROM technical_quality q WHERE q.media_asset_id = a.id AND q.stale = 0 AND q.status = 'ready' ORDER BY q.generated_at DESC LIMIT 1),
           (SELECT global_sharpness FROM technical_quality q WHERE q.media_asset_id = a.id AND q.stale = 0 AND q.status = 'ready' ORDER BY q.generated_at DESC LIMIT 1),
           (SELECT directional_blur_ratio FROM technical_quality q WHERE q.media_asset_id = a.id AND q.stale = 0 AND q.status = 'ready' ORDER BY q.generated_at DESC LIMIT 1),
           CASE WHEN EXISTS (
             SELECT 1 FROM analysis_artifacts face_artifact
             WHERE face_artifact.media_asset_id = a.id
               AND face_artifact.artifact_type = 'face_detection'
               AND face_artifact.stale = 0
               AND face_artifact.status = 'ready'
               AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (
                 SELECT current_artifact.input_fingerprint FROM analysis_artifacts current_artifact
                 WHERE current_artifact.media_asset_id = a.id
                   AND current_artifact.artifact_type = 'capture_intelligence'
                   AND current_artifact.stale = 0
                 ORDER BY current_artifact.generated_at DESC, current_artifact.id DESC LIMIT 1
               )
           ) THEN (
             SELECT COUNT(*) FROM face_analyses face
             WHERE face.media_asset_id = a.id
               AND face.input_fingerprint = (
                 SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact
                 WHERE face_artifact.media_asset_id = a.id
                   AND face_artifact.artifact_type = 'face_detection'
                   AND face_artifact.stale = 0
                   AND face_artifact.status = 'ready'
                   AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (
                     SELECT current_artifact.input_fingerprint FROM analysis_artifacts current_artifact
                     WHERE current_artifact.media_asset_id = a.id
                       AND current_artifact.artifact_type = 'capture_intelligence'
                       AND current_artifact.stale = 0
                     ORDER BY current_artifact.generated_at DESC, current_artifact.id DESC LIMIT 1
                   )
                 ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1
               )
           ) ELSE NULL END,
           CASE WHEN EXISTS (
             SELECT 1 FROM analysis_artifacts face_artifact
             WHERE face_artifact.media_asset_id = a.id
               AND face_artifact.artifact_type = 'face_detection'
               AND face_artifact.stale = 0
               AND face_artifact.status = 'ready'
               AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (
                 SELECT current_artifact.input_fingerprint FROM analysis_artifacts current_artifact
                 WHERE current_artifact.media_asset_id = a.id
                   AND current_artifact.artifact_type = 'capture_intelligence'
                   AND current_artifact.stale = 0
                 ORDER BY current_artifact.generated_at DESC, current_artifact.id DESC LIMIT 1
               )
           ) THEN (
             SELECT COUNT(*) FROM face_analyses face
             WHERE face.media_asset_id = a.id AND face.eye_state = 'open'
               AND face.input_fingerprint = (
                 SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact
                 WHERE face_artifact.media_asset_id = a.id
                   AND face_artifact.artifact_type = 'face_detection'
                   AND face_artifact.stale = 0
                   AND face_artifact.status = 'ready'
                   AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (
                     SELECT current_artifact.input_fingerprint FROM analysis_artifacts current_artifact
                     WHERE current_artifact.media_asset_id = a.id
                       AND current_artifact.artifact_type = 'capture_intelligence'
                       AND current_artifact.stale = 0
                     ORDER BY current_artifact.generated_at DESC, current_artifact.id DESC LIMIT 1
                   )
                 ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1
               )
           ) ELSE NULL END,
           (SELECT g.id FROM similarity_groups g JOIN similarity_group_members gm ON gm.group_id = g.id WHERE gm.media_asset_id = a.id AND g.project_id = a.project_id AND g.stale = 0 ORDER BY g.created_at DESC, g.id DESC LIMIT 1),
           CASE WHEN EXISTS (SELECT 1 FROM similarity_groups selected JOIN similarity_group_members selected_member ON selected_member.group_id = selected.id WHERE selected_member.media_asset_id = a.id AND selected.project_id = a.project_id AND selected.stale = 0) THEN (SELECT COUNT(*) FROM similarity_group_members gm JOIN similarity_groups g ON g.id = gm.group_id WHERE g.id = (SELECT selected.id FROM similarity_groups selected JOIN similarity_group_members selected_member ON selected_member.group_id = selected.id WHERE selected_member.media_asset_id = a.id AND selected.project_id = a.project_id AND selected.stale = 0 ORDER BY selected.created_at DESC, selected.id DESC LIMIT 1)) ELSE NULL END,
           (SELECT gm.is_representative FROM similarity_group_members gm JOIN similarity_groups g ON g.id = gm.group_id WHERE gm.media_asset_id = a.id AND g.project_id = a.project_id AND g.stale = 0 ORDER BY g.created_at DESC, g.id DESC LIMIT 1),
           CASE WHEN EXISTS (SELECT 1 FROM similarity_groups selected JOIN similarity_group_members selected_member ON selected_member.group_id = selected.id WHERE selected_member.media_asset_id = a.id AND selected.project_id = a.project_id AND selected.stale = 0) THEN (SELECT COUNT(*) FROM similarity_group_members peer JOIN technical_quality peer_quality ON peer_quality.media_asset_id = peer.media_asset_id AND peer_quality.stale = 0 AND peer_quality.status = 'ready' WHERE peer.group_id = (SELECT selected.id FROM similarity_groups selected JOIN similarity_group_members selected_member ON selected_member.group_id = selected.id WHERE selected_member.media_asset_id = a.id AND selected.project_id = a.project_id AND selected.stale = 0 ORDER BY selected.created_at DESC, selected.id DESC LIMIT 1) AND peer_quality.technical_quality_score > COALESCE((SELECT technical_quality_score FROM technical_quality own_quality WHERE own_quality.media_asset_id = a.id AND own_quality.stale = 0 AND own_quality.status = 'ready' ORDER BY own_quality.generated_at DESC LIMIT 1), -1.0)) ELSE NULL END,
           (SELECT mm.moment_id FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.active = 1 AND mm.membership_state = 'member' ORDER BY mm.ordinal ASC, mm.id ASC LIMIT 1),
           CASE WHEN EXISTS (SELECT 1 FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.active = 1 AND mm.membership_state = 'member') THEN (SELECT COUNT(*) FROM moment_memberships member WHERE member.project_id = a.project_id AND member.moment_id = (SELECT mm.moment_id FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.active = 1 AND mm.membership_state = 'member' ORDER BY mm.ordinal ASC, mm.id ASC LIMIT 1) AND member.active = 1 AND member.membership_state = 'member') ELSE NULL END,
           (SELECT mm.ordinal FROM moment_memberships mm WHERE mm.project_id = a.project_id AND mm.media_asset_id = a.id AND mm.active = 1 AND mm.membership_state = 'member' ORDER BY mm.ordinal ASC, mm.id ASC LIMIT 1),
           (SELECT label FROM analysis_recommendations recommendation WHERE recommendation.media_asset_id = a.id AND recommendation.stale = 0 AND recommendation.status = 'ready' ORDER BY recommendation.generated_at DESC, recommendation.id DESC LIMIT 1),
           (SELECT confidence FROM analysis_recommendations recommendation WHERE recommendation.media_asset_id = a.id AND recommendation.stale = 0 AND recommendation.status = 'ready' ORDER BY recommendation.generated_at DESC, recommendation.id DESC LIMIT 1),
           EXISTS(SELECT 1 FROM semantic_embeddings embedding WHERE embedding.media_asset_id = a.id AND embedding.status = 'ready' AND embedding.stale = 0),
           a.captured_at
         FROM media_assets a WHERE a.id = ?1 AND a.project_id = ?2",
        params![asset_id, project_id],
        |row| Ok(serde_json::json!({
            "schemaVersion": STUDIO_FEATURE_SCHEMA_VERSION,
            "technicalQualityScore": row.get::<_, Option<f64>>(0)?,
            "globalSharpness": row.get::<_, Option<f64>>(1)?,
            "directionalBlurRatio": row.get::<_, Option<f64>>(2)?,
            "anonymousFaceCount": row.get::<_, Option<i64>>(3)?,
            "openEyesCount": row.get::<_, Option<i64>>(4)?,
            "similarityGroupId": row.get::<_, Option<String>>(5)?,
            "similarityGroupSize": row.get::<_, Option<i64>>(6)?,
            "isGenericRepresentative": row.get::<_, Option<bool>>(7)?,
            "higherTechnicalPeers": row.get::<_, Option<i64>>(8)?,
            "momentId": row.get::<_, Option<String>>(9)?,
            "momentSize": row.get::<_, Option<i64>>(10)?,
            "momentOrdinal": row.get::<_, Option<i64>>(11)?,
            "genericRecommendation": row.get::<_, Option<String>>(12)?,
            "genericConfidence": row.get::<_, Option<f64>>(13)?,
            "semanticEvidenceAvailable": row.get::<_, bool>(14)?,
            "capturedAt": row.get::<_, Option<String>>(15)?,
        })),
    ).optional()?;
    row.ok_or_else(|| {
        PersistenceError::InvalidData(
            "Studio feature asset does not belong to the selected project".into(),
        )
    })
}

/// Recommendation rows are a cache of this compact evidence projection, never a second source
/// of truth. Recomputing the same canonical JSON hash at read time prevents a candidate written
/// just before an analysis/Similar Set/Moment update from being presented as current even if its
/// asynchronous stale-marking write has not happened yet.
fn studio_feature_fingerprint(snapshot: &serde_json::Value) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(snapshot)?)
        .to_hex()
        .to_string())
}

fn studio_recommendation_for_asset(
    connection: &Connection,
    project_id: &ProjectId,
    asset_id: &str,
) -> Result<Option<StudioRecommendationView>> {
    let feature_snapshot = studio_feature_snapshot(connection, &project_id.to_string(), asset_id)?;
    let feature_fingerprint = studio_feature_fingerprint(&feature_snapshot)?;
    let row = connection.query_row(
        "SELECT recommendation.recommendation, recommendation.confidence_band, model.model_version, recommendation.explanation_json, recommendation.generic_recommendation_json, recommendation.agreement, recommendation.generated_at
         FROM studio_recommendations recommendation
         JOIN studio_models model ON model.id = recommendation.studio_model_id AND model.state = 'active'
         JOIN studio_profiles profile ON profile.id = recommendation.studio_profile_id AND profile.profile_key = 'local-default' AND profile.personalization_enabled = 1
         WHERE recommendation.project_id = ?1 AND recommendation.media_asset_id = ?2
           AND recommendation.feature_schema_version = ?3
           AND recommendation.feature_fingerprint = ?4
           AND recommendation.status = 'ready' AND recommendation.stale = 0
         ORDER BY recommendation.generated_at DESC, recommendation.id DESC LIMIT 1",
        params![project_id.to_string(), asset_id, STUDIO_FEATURE_SCHEMA_VERSION, feature_fingerprint],
        |row| {
            let explanation: serde_json::Value = json_value_from_row(row, 3)?;
            let factors = explanation.get("factors").and_then(serde_json::Value::as_array)
                .map(|values| values.iter().filter_map(serde_json::Value::as_str).map(ToOwned::to_owned).collect())
                .unwrap_or_default();
            let generic: serde_json::Value = json_value_from_row(row, 4)?;
            Ok(StudioRecommendationView {
                recommendation: row.get(0)?, confidence_band: row.get(1)?, model_version: row.get(2)?, explanation_factors: factors,
                generic_recommendation: generic.get("label").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
                agreement: row.get(5)?, generated_at: row.get(6)?,
            })
        },
    ).optional()?;
    Ok(row)
}

fn culling_progress(connection: &Connection, project_id: &ProjectId) -> Result<CullingProgress> {
    let mut progress = connection.query_row(
        "SELECT COUNT(*),
            COALESCE(SUM(CASE WHEN md.decision IS NOT NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.decision = 'keep' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.decision = 'reject' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.decision = 'review' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.decision IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.starred = 1 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN md.rating = 5 THEN 1 ELSE 0 END), 0)
         FROM media_assets a LEFT JOIN media_decisions md ON md.media_asset_id = a.id AND md.project_id = ?1
         WHERE a.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')",
        params![project_id.to_string()],
        |row| Ok(CullingProgress { total: row.get::<_, i64>(0)? as u64, reviewed: row.get::<_, i64>(1)? as u64, keep: row.get::<_, i64>(2)? as u64, reject: row.get::<_, i64>(3)? as u64, review: row.get::<_, i64>(4)? as u64, unreviewed: row.get::<_, i64>(5)? as u64, starred: row.get::<_, i64>(6)? as u64, five_star: row.get::<_, i64>(7)? as u64, ..CullingProgress::default() }),
    ).map_err(PersistenceError::from)?;
    let (sets_total, sets_reviewed) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN completion.group_id IS NOT NULL OR NOT EXISTS (SELECT 1 FROM similarity_group_members missing_member LEFT JOIN media_decisions missing_decision ON missing_decision.media_asset_id = missing_member.media_asset_id AND missing_decision.project_id = ?1 WHERE missing_member.group_id = g.id AND missing_decision.decision IS NULL) THEN 1 ELSE 0 END), 0) FROM similarity_groups g LEFT JOIN group_review_completion completion ON completion.group_id = g.id WHERE g.project_id = ?1 AND g.stale = 0",
        params![project_id.to_string()],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
    )?;
    progress.sets_total = sets_total;
    progress.sets_reviewed = sets_reviewed;
    let (strong_candidate_kept, technical_issue_kept, strong_candidate_rejected) = connection.query_row(
        "SELECT
            COALESCE(SUM(CASE WHEN latest.label = 'strong_candidate' AND md.decision = 'keep' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN latest.label = 'technical_issue' AND md.decision = 'keep' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN latest.label = 'strong_candidate' AND md.decision = 'reject' THEN 1 ELSE 0 END), 0)
         FROM media_decisions md
         JOIN media_assets a ON a.id = md.media_asset_id AND a.project_id = md.project_id
         LEFT JOIN analysis_recommendations latest ON latest.id = (SELECT r.id FROM analysis_recommendations r WHERE r.media_asset_id = md.media_asset_id AND r.stale = 0 ORDER BY r.generated_at DESC, r.id DESC LIMIT 1)
         WHERE md.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')",
        params![project_id.to_string()],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? as u64)),
    )?;
    progress.strong_candidate_kept = strong_candidate_kept;
    progress.technical_issue_kept = technical_issue_kept;
    progress.strong_candidate_rejected = strong_candidate_rejected;
    Ok(progress)
}

fn culling_group_summaries(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<Vec<CullingGroupSummary>> {
    let mut statement = connection.prepare(
        "SELECT g.id, g.kind, COUNT(gm.media_asset_id), g.representative_asset_id, ai.display_name, h.media_asset_id, human.display_name,
            COALESCE(SUM(CASE WHEN md.decision IS NOT NULL THEN 1 ELSE 0 END), 0),
            CASE WHEN completion.group_id IS NOT NULL THEN 'explicit_user_completion'
                 WHEN COUNT(gm.media_asset_id) = COALESCE(SUM(CASE WHEN md.decision IS NOT NULL THEN 1 ELSE 0 END), 0) THEN 'auto_all_reviewed'
                 ELSE NULL END
         FROM similarity_groups g JOIN media_assets ai ON ai.id = g.representative_asset_id
         JOIN similarity_group_members gm ON gm.group_id = g.id
         LEFT JOIN media_decisions md ON md.media_asset_id = gm.media_asset_id AND md.project_id = ?1
         LEFT JOIN group_human_representatives h ON h.group_id = g.id
         LEFT JOIN media_assets human ON human.id = h.media_asset_id
         LEFT JOIN group_review_completion completion ON completion.group_id = g.id
         WHERE g.project_id = ?1 AND g.stale = 0 GROUP BY g.id, g.kind, g.representative_asset_id, ai.display_name, h.media_asset_id, human.display_name, completion.group_id
         ORDER BY g.created_at ASC, g.id ASC LIMIT 240"
    )?;
    let rows = statement
        .query_map(params![project_id.to_string()], |row| {
            let completion_kind: Option<String> = row.get(8)?;
            Ok(CullingGroupSummary {
                id: row.get(0)?,
                kind: row.get(1)?,
                member_count: row.get::<_, i64>(2)? as u64,
                ai_representative_asset_id: row.get(3)?,
                ai_representative_filename: row.get(4)?,
                human_representative_asset_id: row.get(5)?,
                human_representative_filename: row.get(6)?,
                reviewed_count: row.get::<_, i64>(7)? as u64,
                completed: completion_kind.is_some(),
                completion_kind,
                studio_starting_point_asset_id: None,
                studio_starting_point_reason: None,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut rows = rows;
    for summary in &mut rows {
        if let Some((asset_id, explanation)) =
            current_studio_similar_set_starting_point(connection, project_id, &summary.id)?
        {
            summary.studio_starting_point_asset_id = Some(asset_id);
            summary.studio_starting_point_reason =
                studio_similar_set_starting_point_reason(&explanation);
        }
    }
    Ok(rows)
}

/// Similar Set advice must pass the exact same current-feature check as the per-asset culling
/// card. A cached candidate rank is never allowed to become a visible starting point after its
/// technical, generic, semantic-availability, Similar Set, or Moment context changed.
fn current_studio_similar_set_starting_point(
    connection: &Connection,
    project_id: &ProjectId,
    group_id: &str,
) -> Result<Option<(String, String)>> {
    let candidates = connection
        .prepare(
            "SELECT suggestion.media_asset_id, suggestion.feature_fingerprint, suggestion.explanation_json
             FROM studio_recommendations suggestion
             JOIN studio_models studio_model
               ON studio_model.id = suggestion.studio_model_id AND studio_model.state = 'active'
             JOIN studio_profiles studio_profile
               ON studio_profile.id = suggestion.studio_profile_id
              AND studio_profile.profile_key = 'local-default'
              AND studio_profile.personalization_enabled = 1
             JOIN similarity_group_members suggested_member
               ON suggested_member.media_asset_id = suggestion.media_asset_id
              AND suggested_member.group_id = ?2
             WHERE suggestion.project_id = ?1
               AND suggestion.feature_schema_version = ?3
               AND suggestion.status = 'ready'
               AND suggestion.stale = 0
               AND json_extract(suggestion.explanation_json, '$.similarSetRank') IS NOT NULL
             ORDER BY CAST(json_extract(suggestion.explanation_json, '$.similarSetRank') AS INTEGER) ASC,
                      suggestion.generated_at DESC, suggestion.id DESC",
        )?
        .query_map(
            params![project_id.to_string(), group_id, STUDIO_FEATURE_SCHEMA_VERSION],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (asset_id, stored_fingerprint, explanation) in candidates {
        let snapshot = studio_feature_snapshot(connection, &project_id.to_string(), &asset_id)?;
        if studio_feature_fingerprint(&snapshot)? == stored_fingerprint {
            return Ok(Some((asset_id, explanation)));
        }
    }
    Ok(None)
}

fn studio_similar_set_starting_point_reason(value: &str) -> Option<String> {
    let explanation = serde_json::from_str::<serde_json::Value>(value).ok()?;
    let factor = explanation
        .get("pairwiseFactors")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
        .and_then(serde_json::Value::as_str)?;
    Some(format!(
        "A local comparable-set model ranked this non-binding starting point because {factor}."
    ))
}

fn completed_group_count(connection: &Connection, project_id: &ProjectId) -> Result<u64> {
    Ok(culling_group_summaries(connection, project_id)?
        .into_iter()
        .filter(|group| group.completed)
        .count() as u64)
}

fn preference_evidence_snapshot(
    connection: &Connection,
    group_id: &str,
    chosen_asset_id: &MediaAssetId,
) -> Result<String> {
    let mut statement = connection.prepare(
        "SELECT gm.media_asset_id, tq.technical_quality_score, tq.global_sharpness, tq.blur_level, tq.technical_quality_band, r.label, r.confidence
         FROM similarity_group_members gm
         LEFT JOIN technical_quality tq ON tq.media_asset_id = gm.media_asset_id AND tq.stale = 0
         LEFT JOIN analysis_recommendations r ON r.id = (SELECT latest.id FROM analysis_recommendations latest WHERE latest.media_asset_id = gm.media_asset_id AND latest.stale = 0 ORDER BY latest.generated_at DESC, latest.id DESC LIMIT 1)
         WHERE gm.group_id = ?1 ORDER BY gm.ordinal ASC, gm.media_asset_id ASC"
    )?;
    let rows = statement.query_map(params![group_id], |row| Ok(serde_json::json!({
        "assetId": row.get::<_, String>(0)?, "technicalQualityScore": row.get::<_, Option<f64>>(1)?, "globalSharpness": row.get::<_, Option<f64>>(2)?, "blurLevel": row.get::<_, Option<String>>(3)?, "technicalQualityBand": row.get::<_, Option<String>>(4)?, "aiRecommendation": row.get::<_, Option<String>>(5)?, "aiConfidence": row.get::<_, Option<f64>>(6)?, "chosen": row.get::<_, String>(0)? == chosen_asset_id.to_string(),
    })))?.collect::<std::result::Result<Vec<_>, _>>()?;
    json(&rows)
}
/// Face-provider details live in the versioned artifact payload, not in schema columns. This
/// keeps rectangle detection, optional landmarks, and provider-attempt provenance independently
/// evolvable while old artifacts remain readable during a face-only cache refresh.
fn face_artifact_payload_string(payload: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get(key)?
        .as_str()
        .map(ToOwned::to_owned)
}
fn from_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(Into::into)
}

fn enum_from_text<T: serde::de::DeserializeOwned>(value: &str) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn json_from_row<T: serde::de::DeserializeOwned>(value: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| to_sql_error(error.into()))
}

fn production_plan_type_name(value: ProductionPlanType) -> &'static str {
    match value {
        ProductionPlanType::ClientDelivery => "client_delivery",
        ProductionPlanType::EditorWorkset => "editor_workset",
        ProductionPlanType::PortfolioSelects => "portfolio_selects",
        ProductionPlanType::ProofGallery => "proof_gallery",
        ProductionPlanType::BackupArchive => "backup_archive",
        ProductionPlanType::Custom => "custom",
    }
}

fn virtual_collection_kind_name(value: VirtualCollectionKind) -> &'static str {
    match value {
        VirtualCollectionKind::Static => "static",
        VirtualCollectionKind::Dynamic => "dynamic",
    }
}

fn plan_override_kind_name(value: PlanOverrideKind) -> &'static str {
    match value {
        PlanOverrideKind::ForceInclude => "force_include",
        PlanOverrideKind::ForceExclude => "force_exclude",
    }
}

fn manifest_entry_status_name(value: delivery_brain::ManifestEntryStatus) -> &'static str {
    match value {
        delivery_brain::ManifestEntryStatus::Planned => "planned",
        delivery_brain::ManifestEntryStatus::BlockedSourceUnavailable => {
            "blocked_source_unavailable"
        }
        delivery_brain::ManifestEntryStatus::BlockedInternalCollision => {
            "blocked_internal_collision"
        }
    }
}

fn production_plan_from_row(row: &Row<'_>) -> rusqlite::Result<ProductionPlanRecord> {
    let plan_type: String = row.get(3)?;
    let status: String = row.get(4)?;
    let rules: String = row.get(5)?;
    let organization: String = row.get(6)?;
    let filename_strategy: String = row.get(7)?;
    Ok(ProductionPlanRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        plan_type: enum_from_text(&plan_type)?,
        status: enum_from_text(&status)?,
        selection_rules: json_from_row(&rules)?,
        organization: json_from_row(&organization)?,
        filename_strategy: json_from_row(&filename_strategy)?,
        destination_path: row.get(8)?,
        destination_reserve_bytes: row.get::<_, i64>(9)? as u64,
        estimated_file_count: row.get::<_, i64>(10)? as u64,
        estimated_bytes: row.get::<_, i64>(11)? as u64,
        current_manifest_id: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn virtual_collection_from_row(row: &Row<'_>) -> rusqlite::Result<VirtualCollectionRecord> {
    let kind: String = row.get(3)?;
    let rules: String = row.get(4)?;
    Ok(VirtualCollectionRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        kind: enum_from_text(&kind)?,
        rules: json_from_row(&rules)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        asset_count: row.get::<_, i64>(7)? as u64,
    })
}

fn export_manifest_from_row(row: &Row<'_>) -> rusqlite::Result<ExportManifestRecord> {
    let selection_snapshot: String = row.get(6)?;
    let organization_snapshot: String = row.get(7)?;
    let filename_strategy_snapshot: String = row.get(8)?;
    let validation: String = row.get(13)?;
    Ok(ExportManifestRecord {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        project_id: row.get(2)?,
        manifest_version: row.get::<_, i64>(3)? as u64,
        source_revision: row.get::<_, i64>(4)? as u64,
        status: row.get(5)?,
        selection_snapshot: json_from_row(&selection_snapshot)?,
        organization_snapshot: json_from_row(&organization_snapshot)?,
        filename_strategy_snapshot: json_from_row(&filename_strategy_snapshot)?,
        destination_path: row.get(9)?,
        selected_file_count: row.get::<_, i64>(10)? as u64,
        estimated_bytes: row.get::<_, i64>(11)? as u64,
        checksum: row.get(12)?,
        validation: json_from_row(&validation)?,
        created_at: row.get(14)?,
    })
}

fn export_manifest_entry_from_row(row: &Row<'_>) -> rusqlite::Result<ExportManifestEntryRecord> {
    Ok(ExportManifestEntryRecord {
        id: row.get(0)?,
        manifest_id: row.get(1)?,
        ordinal: row.get::<_, i64>(2)? as u64,
        media_asset_id: row.get(3)?,
        selected_file_instance_id: row.get(4)?,
        original_filename: row.get(5)?,
        destination_relative_path: row.get(6)?,
        destination_filename: row.get(7)?,
        expected_byte_size: row.get::<_, i64>(8)? as u64,
        source_checksum: row.get(9)?,
        human_decision: row.get(10)?,
        rating: row.get::<_, i64>(11)? as u8,
        starred: row.get(12)?,
        moment_id: row.get(13)?,
        moment_label: row.get(14)?,
        status: row.get(15)?,
        issue: row.get(16)?,
    })
}

fn export_job_from_row(row: &Row<'_>) -> rusqlite::Result<ExportJobRecord> {
    Ok(ExportJobRecord {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        manifest_id: row.get(2)?,
        background_job_id: row.get(3)?,
        state: row.get(4)?,
        destination_path: row.get(5)?,
        items_total: row.get::<_, i64>(6)? as u64,
        items_completed: row.get::<_, i64>(7)? as u64,
        verified_count: row.get::<_, i64>(8)? as u64,
        skipped_identical_count: row.get::<_, i64>(9)? as u64,
        failed_count: row.get::<_, i64>(10)? as u64,
        verified_bytes: row.get::<_, i64>(11)? as u64,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        finished_at: row.get(14)?,
        error_message: row.get(15)?,
    })
}

fn valid_production_name(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() {
        return Err(PersistenceError::InvalidData(
            "Production Plan and Virtual Collection names cannot be empty".into(),
        ));
    }
    if name.chars().count() > 240 {
        return Err(PersistenceError::InvalidData(
            "Production Plan and Virtual Collection names must be 240 characters or fewer".into(),
        ));
    }
    Ok(name.into())
}

fn ensure_production_plan_exists(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    plan_id: &str,
) -> Result<()> {
    ensure_production_plan_exists_by_string(transaction, &project_id.to_string(), plan_id)
}

fn ensure_production_plan_exists_by_string(
    transaction: &Transaction<'_>,
    project_id: &str,
    plan_id: &str,
) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM production_plans WHERE id = ?1 AND project_id = ?2)",
        params![plan_id, project_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(PersistenceError::InvalidData(
            "Production Plan does not belong to this project".into(),
        ));
    }
    Ok(())
}

/// A frozen manifest records a resolved collection membership snapshot. Changing that collection
/// must therefore stale—not silently rewrite—any currently referenced plan/manifest.
fn stale_plans_referencing_collection(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    collection_id: &str,
    updated_at: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE export_manifests
         SET status = 'stale'
         WHERE status = 'ready'
           AND production_plan_id IN (
             SELECT id FROM production_plans
             WHERE project_id = ?1
               AND json_extract(selection_rules_json, '$.virtualCollectionId') = ?2
           )",
        params![project_id.to_string(), collection_id],
    )?;
    transaction.execute(
        "UPDATE production_plans
         SET status = 'stale', updated_at = ?3
         WHERE project_id = ?1
           AND current_manifest_id IS NOT NULL
           AND json_extract(selection_rules_json, '$.virtualCollectionId') = ?2
           AND status <> 'exporting'",
        params![project_id.to_string(), collection_id, updated_at],
    )?;
    Ok(())
}

/// Plan edits replace intent. The prior current manifest remains durable audit history, but it
/// must be visibly stale and cannot be mistaken for a fresh export candidate.
fn stale_current_manifest_for_plan(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    plan_id: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE export_manifests
         SET status = 'stale'
         WHERE id = (
           SELECT current_manifest_id FROM production_plans
           WHERE id = ?1 AND project_id = ?2
         ) AND status = 'ready'",
        params![plan_id, project_id.to_string()],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // explicit optional foreign-key provenance mirrors one SQL event row
fn insert_production_event(
    transaction: &Transaction<'_>,
    project_id: &str,
    plan_id: Option<&str>,
    manifest_id: Option<&str>,
    export_job_id: Option<&str>,
    event_type: &str,
    details: &serde_json::Value,
    created_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO production_events (id, project_id, production_plan_id, export_manifest_id, export_job_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![Uuid::new_v4().to_string(), project_id, plan_id, manifest_id, export_job_id, event_type, json(details)?, created_at],
    )?;
    Ok(())
}

fn production_source_revision(connection: &Connection, project_id: &ProjectId) -> Result<u64> {
    connection
        .query_row(
            "SELECT source_revision FROM production_project_revisions WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|value| value as u64)
        .map_or(Ok(0), Ok)
}

fn production_source_revision_transaction(
    transaction: &Transaction<'_>,
    project_id: &str,
) -> Result<u64> {
    transaction
        .query_row(
            "SELECT source_revision FROM production_project_revisions WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|value| value as u64)
        .map_or(Ok(0), Ok)
}

/// Static collection membership is explicit, plan-relevant selection evidence. Advance the
/// project revision in the same transaction so a dry-run built just before that human action
/// cannot freeze an obsolete collection snapshot. The existing M9 trigger marks any affected
/// ready manifests stale before a later export can use them.
fn advance_production_selection_revision(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    updated_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO production_project_revisions (project_id, source_revision, updated_at)
         VALUES (?1, 1, ?2)
         ON CONFLICT(project_id) DO UPDATE
         SET source_revision = source_revision + 1, updated_at = excluded.updated_at",
        params![project_id.to_string(), updated_at],
    )?;
    Ok(())
}

fn production_delivery_asset_candidates(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<Vec<DeliveryAssetCandidate>> {
    #[derive(Default)]
    struct AssetRow {
        candidate: Option<DeliveryAssetCandidate>,
    }
    let mut statement = connection.prepare(
        "WITH active_moments AS (
             SELECT membership.media_asset_id, record.id AS moment_id, record.ordinal, label.label AS human_label, record.suggested_label,
                    ROW_NUMBER() OVER (PARTITION BY membership.media_asset_id ORDER BY record.ordinal ASC, record.id ASC) AS position
             FROM moment_memberships membership
             JOIN moment_records record ON record.id = membership.moment_id AND record.stale = 0
             LEFT JOIN moment_human_labels label ON label.project_id = record.project_id AND label.anchor_asset_id = record.anchor_asset_id
             WHERE membership.project_id = ?1 AND membership.active = 1 AND membership.membership_state = 'member'
           )
         SELECT asset.id, asset.display_name, NULLIF(asset.extension, ''), COALESCE(asset.byte_size, metadata.byte_size, 0),
                COALESCE(metadata.captured_at_local, asset.captured_at), metadata.camera_model,
                decision.decision, COALESCE(decision.rating, 0), COALESCE(decision.starred, 0),
                moment.moment_id, moment.ordinal, moment.human_label, moment.suggested_label,
                instance.id, root.id, instance.relative_path,
                CASE WHEN instance.is_available = 1 AND root.selected_path IS NOT NULL THEN 1 ELSE 0 END,
                EXISTS(SELECT 1 FROM backup_copies backup WHERE backup.verified_at IS NOT NULL AND (backup.source_file_instance_id = instance.id OR backup.backup_file_instance_id = instance.id)),
                instance.observed_at, asset.content_hash
         FROM media_assets asset
         LEFT JOIN media_metadata metadata ON metadata.media_asset_id = asset.id
         LEFT JOIN media_decisions decision ON decision.project_id = asset.project_id AND decision.media_asset_id = asset.id
         LEFT JOIN active_moments moment ON moment.media_asset_id = asset.id AND moment.position = 1
         LEFT JOIN file_instances instance ON instance.media_asset_id = asset.id
         LEFT JOIN index_roots root ON root.id = instance.index_root_id
         WHERE asset.project_id = ?1
         ORDER BY COALESCE(metadata.captured_at_local, asset.captured_at, asset.observed_modified_at, asset.created_at) ASC, asset.id ASC, instance.id ASC",
    )?;
    let mut assets = BTreeMap::<String, AssetRow>::new();
    for row in statement.query_map(params![project_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, bool>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<i64>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, bool>(16)?,
            row.get::<_, bool>(17)?,
            row.get::<_, Option<String>>(18)?,
            row.get::<_, Option<String>>(19)?,
        ))
    })? {
        let (
            asset_id,
            original_filename,
            extension,
            byte_size,
            captured_at,
            camera,
            human_decision,
            rating,
            starred,
            moment_id,
            moment_ordinal,
            human_label,
            suggested_label,
            instance_id,
            root_id,
            relative_path,
            available,
            verified,
            observed_at,
            content_checksum,
        ) = row?;
        let entry = assets.entry(asset_id.clone()).or_default();
        if entry.candidate.is_none() {
            entry.candidate = Some(DeliveryAssetCandidate {
                asset_id,
                original_filename,
                extension,
                byte_size: byte_size as u64,
                captured_at,
                camera,
                human_decision,
                rating: rating as u8,
                starred,
                moment: moment_id.map(|id| delivery_brain::MomentDestination {
                    id,
                    ordinal: moment_ordinal.unwrap_or(0) as u64,
                    human_label,
                    suggested_label,
                }),
                sources: Vec::new(),
            });
        }
        if let (Some(file_instance_id), Some(relative_path), Some(observed_at)) =
            (instance_id, relative_path, observed_at)
        {
            entry
                .candidate
                .as_mut()
                .expect("candidate initialized")
                .sources
                .push(DeliverySourceCandidate {
                    file_instance_id,
                    source_root_id: root_id,
                    relative_path,
                    available,
                    verified,
                    observed_at,
                    byte_size: byte_size as u64,
                    content_checksum: content_checksum.clone(),
                });
        }
    }
    Ok(assets
        .into_values()
        .filter_map(|row| row.candidate)
        .collect())
}
fn parse_timestamp(value: &str) -> Result<Timestamp> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))
}
type IndexRootTuple = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
);
type MediaAssetTuple = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);
type BackgroundJobTuple = (
    String,
    String,
    String,
    i64,
    Option<i64>,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
);

fn index_root_from_tuple(value: IndexRootTuple) -> Result<IndexRoot> {
    let (id, project_id, selected_path, storage_volume_id, added_at, last_indexed_at, status) =
        value;
    Ok(IndexRoot {
        id: parse_id!(IndexRootId, &id)?,
        project_id: parse_id!(ProjectId, &project_id)?,
        selected_path,
        storage_volume_id: parse_id!(StorageVolumeId, &storage_volume_id)?,
        added_at: parse_timestamp(&added_at)?,
        last_indexed_at: last_indexed_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        status: from_json(&status)?,
    })
}

fn media_asset_from_tuple(value: MediaAssetTuple) -> Result<MediaAsset> {
    let (id, project_id, media_type, display_name, extension, captured_at, fingerprint, created_at) =
        value;
    Ok(MediaAsset {
        id: parse_id!(MediaAssetId, &id)?,
        project_id: parse_id!(ProjectId, &project_id)?,
        media_type: media_type_from_str(&media_type)?,
        display_name,
        extension: (!extension.is_empty()).then_some(extension),
        captured_at: captured_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        fingerprint: from_json(&fingerprint)?,
        created_at: parse_timestamp(&created_at)?,
    })
}

fn background_job_tuple(row: &Row<'_>) -> rusqlite::Result<BackgroundJobTuple> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
    ))
}

fn background_job_from_tuple(value: BackgroundJobTuple) -> Result<BackgroundJob> {
    let (
        id,
        state,
        stage,
        items_completed,
        items_total,
        files_discovered,
        files_processed,
        error_count,
        project_id,
        index_root_id,
        error_message,
        resume_metadata,
        created_at,
        updated_at,
        finished_at,
    ) = value;
    Ok(BackgroundJob {
        id: parse_id!(JobId, &id)?,
        state: from_json(&state)?,
        stage: from_json(&stage)?,
        items_completed: items_completed as u64,
        items_total: items_total.map(|value| value as u64),
        files_discovered: files_discovered as u64,
        files_processed: files_processed as u64,
        error_count: error_count as u64,
        project_id: project_id
            .as_deref()
            .map(|value| parse_id!(ProjectId, value))
            .transpose()?,
        index_root_id: index_root_id
            .as_deref()
            .map(|value| parse_id!(IndexRootId, value))
            .transpose()?,
        error_message,
        resume_metadata: resume_metadata.map(|value| from_json(&value)).transpose()?,
        created_at: parse_timestamp(&created_at)?,
        updated_at: parse_timestamp(&updated_at)?,
        finished_at: finished_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
    })
}

fn media_type_from_str(value: &str) -> Result<MediaType> {
    match value {
        "raw_photo" => Ok(MediaType::RawPhoto),
        "jpeg" => Ok(MediaType::Jpeg),
        "heif" => Ok(MediaType::Heif),
        "png" => Ok(MediaType::Png),
        "tiff" => Ok(MediaType::Tiff),
        "video" => Ok(MediaType::Video),
        "audio" => Ok(MediaType::Audio),
        "sidecar" => Ok(MediaType::Sidecar),
        "proxy" => Ok(MediaType::Proxy),
        "thumbnail" => Ok(MediaType::Thumbnail),
        "project_document" => Ok(MediaType::ProjectDocument),
        "unknown" => Ok(MediaType::Unknown),
        _ => Err(PersistenceError::InvalidData(format!(
            "unknown media type: {value}"
        ))),
    }
}

fn relationship_kind(value: &str) -> RelationshipKind {
    match value {
        "CAPTURED_BY" => RelationshipKind::CapturedBy,
        "CONTAINS_PERSON" => RelationshipKind::ContainsPerson,
        "BELONGS_TO_MOMENT" => RelationshipKind::BelongsToMoment,
        "BELONGS_TO_SCENE" => RelationshipKind::BelongsToScene,
        "STORED_ON" => RelationshipKind::StoredOn,
        "BACKED_UP_AS" => RelationshipKind::BackedUpAs,
        "COPIED_FROM" => RelationshipKind::CopiedFrom,
        "VERIFIED_COPY_OF" => RelationshipKind::VerifiedCopyOf,
        "DERIVED_FROM" => RelationshipKind::DerivedFrom,
        "SIMILAR_TO" => RelationshipKind::SimilarTo,
        "HAS_AUDIO" => RelationshipKind::HasAudio,
        "CAPTURED_NEAR" => RelationshipKind::CapturedNear,
        "CREATED_BY" => RelationshipKind::CreatedBy,
        "EXPORTED_TO" => RelationshipKind::ExportedTo,
        "SIDECAR_OF" => RelationshipKind::SidecarOf,
        "MEMBER_OF_SEQUENCE" => RelationshipKind::MemberOfSequence,
        "HAS_FACE_ANALYSIS" => RelationshipKind::HasFaceAnalysis,
        "HAS_TECHNICAL_ANALYSIS" => RelationshipKind::HasTechnicalAnalysis,
        custom => RelationshipKind::Custom(custom.into()),
    }
}

fn visual_filter_sql(filter: &VisualMediaFilter) -> &'static str {
    match filter {
        VisualMediaFilter::All => "1 = 1",
        VisualMediaFilter::Photos => "a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff')",
        VisualMediaFilter::Raw => "a.media_type = 'raw_photo'",
        VisualMediaFilter::JpegHeif => "a.media_type IN ('jpeg', 'heif')",
        VisualMediaFilter::Video => "a.media_type = 'video'",
        VisualMediaFilter::Audio => "a.media_type = 'audio'",
        VisualMediaFilter::Offline => "pi.is_available = 0",
        VisualMediaFilter::Available => "pi.is_available = 1",
        VisualMediaFilter::StrongCandidates => "EXISTS (SELECT 1 FROM analysis_recommendations intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.label = 'strong_candidate' AND intelligence_filter.status = 'ready' AND intelligence_filter.stale = 0 AND intelligence_filter.input_fingerprint = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1))",
        VisualMediaFilter::TechnicalIssues => "EXISTS (SELECT 1 FROM technical_quality intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.technical_quality_band = 'technical_issue' AND intelligence_filter.status = 'ready' AND intelligence_filter.stale = 0 AND intelligence_filter.input_fingerprint = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1))",
        VisualMediaFilter::ProbableDuplicates => "EXISTS (SELECT 1 FROM analysis_recommendations intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.label = 'probable_duplicate' AND intelligence_filter.status = 'ready' AND intelligence_filter.stale = 0 AND intelligence_filter.input_fingerprint = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1))",
        VisualMediaFilter::SimilarGroups => "EXISTS (SELECT 1 FROM similarity_group_members intelligence_filter JOIN similarity_groups intelligence_group ON intelligence_group.id = intelligence_filter.group_id WHERE intelligence_filter.media_asset_id = a.id AND intelligence_group.stale = 0)",
        VisualMediaFilter::Faces => "EXISTS (SELECT 1 FROM face_analyses intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1) ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1))",
        VisualMediaFilter::PossibleClosedEyes => "EXISTS (SELECT 1 FROM face_analyses intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.eye_state = 'closed' AND intelligence_filter.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND COALESCE(json_extract(face_artifact.payload_json, '$.input_preview_fingerprint'), face_artifact.input_fingerprint) = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1) ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1))",
        VisualMediaFilter::BlurReview => "EXISTS (SELECT 1 FROM technical_quality intelligence_filter WHERE intelligence_filter.media_asset_id = a.id AND intelligence_filter.blur_level IN ('moderate', 'high', 'uncertain') AND intelligence_filter.status = 'ready' AND intelligence_filter.stale = 0 AND intelligence_filter.input_fingerprint = (SELECT artifact.input_fingerprint FROM analysis_artifacts artifact WHERE artifact.media_asset_id = a.id AND artifact.artifact_type = 'capture_intelligence' AND artifact.stale = 0 ORDER BY artifact.generated_at DESC, artifact.id DESC LIMIT 1))",
    }
}

fn sidecar_stem(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(stem, _)| stem)
}

fn same_path_stem(path: &str, expected_stem: &str) -> bool {
    sidecar_stem(path).is_some_and(|stem| stem.eq_ignore_ascii_case(expected_stem))
}

fn visual_sort_sql(sort: VisualMediaSort) -> &'static str {
    match sort {
        VisualMediaSort::CaptureTime => {
            "COALESCE(m.captured_at_local, a.captured_at, a.observed_modified_at, pi.observed_at)"
        }
        VisualMediaSort::Filename => "lower(a.display_name)",
        VisualMediaSort::FileSize => "COALESCE(a.byte_size, 0)",
        VisualMediaSort::DateIndexed => "pi.observed_at",
        VisualMediaSort::MediaType => "a.media_type",
    }
}

/// Mirrors capture-core's logical terminal-result rule: any usable cached rendition makes the
/// logical asset ready, regardless of another rendition/provider attempt that failed.
fn resolved_preview_status_sql() -> &'static str {
    "CASE WHEN small.status = 'ready' OR medium.status = 'ready' OR preview.status = 'ready' THEN 'ready' WHEN small.status = 'timeout' OR medium.status = 'timeout' OR preview.status = 'timeout' THEN 'timeout' WHEN small.status = 'failed' OR medium.status = 'failed' OR preview.status = 'failed' THEN 'failed' WHEN small.status = 'corrupt' OR medium.status = 'corrupt' OR preview.status = 'corrupt' THEN 'corrupt' WHEN small.status = 'offline' OR medium.status = 'offline' OR preview.status = 'offline' THEN 'offline' WHEN small.status = 'cancelled' OR medium.status = 'cancelled' OR preview.status = 'cancelled' THEN 'cancelled' WHEN small.status = 'unsupported' OR medium.status = 'unsupported' OR preview.status = 'unsupported' THEN 'unsupported' ELSE COALESCE(m.status, 'pending') END"
}

fn resolved_preview_failure_reason_sql() -> &'static str {
    "CASE WHEN small.status = 'ready' OR medium.status = 'ready' OR preview.status = 'ready' THEN NULL ELSE COALESCE(preview.failure_reason, medium.failure_reason, small.failure_reason, m.failure_reason) END"
}

fn visual_media_row_from_row(row: &Row<'_>) -> rusqlite::Result<VisualMediaRow> {
    Ok(VisualMediaRow {
        asset_id: row.get(0)?,
        file_instance_id: row.get(1)?,
        filename: row.get(2)?,
        media_type: row.get(3)?,
        extension: row
            .get::<_, String>(4)
            .ok()
            .filter(|value| !value.is_empty()),
        byte_size: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
        captured_at: row.get(6)?,
        indexed_at: row.get(7)?,
        relative_path: row.get(8)?,
        selected_root: row.get(9)?,
        storage_volume: row.get(10)?,
        storage_volume_id: row.get(11)?,
        is_available: row.get(12)?,
        thumbnail_preview_url: row.get(13)?,
        medium_preview_url: row.get(14)?,
        preview_preview_url: row.get(15)?,
        preview_status: row.get(16)?,
        preview_failure_reason: row.get(17)?,
        width: row.get::<_, Option<i64>>(18)?.map(|value| value as u32),
        height: row.get::<_, Option<i64>>(19)?.map(|value| value as u32),
        duration_ms: row.get::<_, Option<i64>>(20)?.map(|value| value as u64),
        camera_model: row.get(21)?,
        lens_model: row.get(22)?,
        codec: row.get(23)?,
        intelligence: IntelligenceSummary::default(),
    })
}

fn media_metadata_from_row(row: &Row<'_>) -> rusqlite::Result<MediaMetadataRecord> {
    let raw_metadata: String = row.get(42)?;
    let raw_metadata = serde_json::from_str(&raw_metadata).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(42, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(MediaMetadataRecord {
        media_asset_id: row.get(0)?,
        source_file_instance_id: row.get(1)?,
        source_fingerprint: row.get(2)?,
        extractor: row.get(3)?,
        extractor_version: row.get(4)?,
        status: row.get(5)?,
        failure_reason: row.get(6)?,
        extracted_at: row.get(7)?,
        mime_type: row.get(8)?,
        byte_size: row.get::<_, Option<i64>>(9)?.map(|value| value as u64),
        captured_at_raw: row.get(10)?,
        captured_at_local: row.get(11)?,
        capture_timezone: row.get(12)?,
        capture_time_source: row.get(13)?,
        capture_time_confidence: row.get(14)?,
        width: row.get::<_, Option<i64>>(15)?.map(|value| value as u32),
        height: row.get::<_, Option<i64>>(16)?.map(|value| value as u32),
        orientation: row.get(17)?,
        camera_make: row.get(18)?,
        camera_model: row.get(19)?,
        lens_make: row.get(20)?,
        lens_model: row.get(21)?,
        focal_length_mm: row.get(22)?,
        focal_length_equivalent_mm: row.get(23)?,
        aperture: row.get(24)?,
        shutter_speed: row.get(25)?,
        iso: row.get::<_, Option<i64>>(26)?.map(|value| value as u32),
        exposure_compensation: row.get(27)?,
        flash: row.get(28)?,
        white_balance: row.get(29)?,
        color_space: row.get(30)?,
        gps_present: row.get(31)?,
        duration_ms: row.get::<_, Option<i64>>(32)?.map(|value| value as u64),
        frame_rate: row.get(33)?,
        codec: row.get(34)?,
        pixel_format: row.get(35)?,
        bitrate: row.get::<_, Option<i64>>(36)?.map(|value| value as u64),
        audio_streams: row.get::<_, Option<i64>>(37)?.map(|value| value as u32),
        video_streams: row.get::<_, Option<i64>>(38)?.map(|value| value as u32),
        sample_rate: row.get::<_, Option<i64>>(39)?.map(|value| value as u32),
        bit_depth: row.get::<_, Option<i64>>(40)?.map(|value| value as u32),
        channels: row.get::<_, Option<i64>>(41)?.map(|value| value as u32),
        raw_metadata,
    })
}

fn local_model_from_row(row: &Row<'_>) -> rusqlite::Result<LocalModelRecord> {
    let id_value: String = row.get(0)?;
    let registered_at: String = row.get(16)?;
    Ok(LocalModelRecord {
        id: LocalModelId::try_from(id_value.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        model_id: row.get(1)?,
        model_family: row.get(2)?,
        provider: row.get(3)?,
        version: row.get(4)?,
        local_relative_path: row.get(5)?,
        checksum: row.get(6)?,
        capability: row.get(7)?,
        input_size: row.get::<_, Option<i64>>(8)?.map(|value| value as u32),
        embedding_dimension: row.get::<_, Option<i64>>(9)?.map(|value| value as u32),
        status: row.get(10)?,
        license: row.get(11)?,
        license_url: row.get(12)?,
        source_url: row.get(13)?,
        file_size_bytes: row.get::<_, Option<i64>>(14)?.map(|value| value as u64),
        hardware_requirements: row.get(15)?,
        registered_at: parse_timestamp(&registered_at).map_err(to_sql_error)?,
    })
}

fn semantic_index_version_from_row(row: &Row<'_>) -> rusqlite::Result<SemanticIndexVersion> {
    let id_value: String = row.get(0)?;
    let project_value: String = row.get(1)?;
    let created_at: String = row.get(15)?;
    let rebuilt_at: String = row.get(16)?;
    Ok(SemanticIndexVersion {
        id: SemanticIndexVersionId::try_from(id_value.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        project_id: ProjectId::try_from(project_value.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        model: SemanticModelConfig {
            model_id: row.get(2)?,
            provider: row.get(3)?,
            model_version: row.get(4)?,
            embedding_version: row.get(5)?,
            preprocessing_version: row.get(6)?,
            metric: row.get(7)?,
            dimensions: row.get::<_, i64>(8)? as usize,
        },
        index_format: row.get(9)?,
        index_relative_path: row.get(10)?,
        index_checksum: row.get(11)?,
        embedding_count: row.get::<_, i64>(12)? as u64,
        status: row.get(13)?,
        stale: row.get(14)?,
        created_at: parse_timestamp(&created_at).map_err(to_sql_error)?,
        rebuilt_at: parse_timestamp(&rebuilt_at).map_err(to_sql_error)?,
    })
}

fn encode_semantic_vector(values: &[f32], dimensions: usize) -> Result<Vec<u8>> {
    if values.len() != dimensions
        || values.is_empty()
        || values.iter().any(|value| !value.is_finite())
    {
        return Err(PersistenceError::InvalidData(
            "semantic embedding dimensions or values were invalid".into(),
        ));
    }
    let mut encoded = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        encoded.extend_from_slice(&value.to_le_bytes());
    }
    Ok(encoded)
}

fn decode_semantic_vector(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>> {
    if dimensions == 0 || bytes.len() != dimensions.saturating_mul(std::mem::size_of::<f32>()) {
        return Err(PersistenceError::InvalidData(
            "semantic embedding blob dimensions were invalid".into(),
        ));
    }
    let values = bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    if values.iter().any(|value| !value.is_finite()) {
        return Err(PersistenceError::InvalidData(
            "semantic embedding blob contained a non-finite value".into(),
        ));
    }
    Ok(values)
}

const MIGRATION_001: &str = r#"
BEGIN;
CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE shoots (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), name TEXT NOT NULL, captured_at TEXT);
CREATE TABLE moments (id TEXT PRIMARY KEY, shoot_id TEXT NOT NULL REFERENCES shoots(id), name TEXT NOT NULL, started_at TEXT, ended_at TEXT);
CREATE TABLE person_clusters (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), display_name TEXT, created_at TEXT NOT NULL);
CREATE TABLE storage_volumes (id TEXT PRIMARY KEY, display_name TEXT NOT NULL, filesystem_identity TEXT, capacity_bytes INTEGER, mount_state TEXT NOT NULL, mount_location TEXT, last_seen_at TEXT, filesystem_type TEXT, classification TEXT NOT NULL);
CREATE TABLE capture_devices (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), display_name TEXT NOT NULL, device_type TEXT NOT NULL);
CREATE TABLE media_assets (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), media_type TEXT NOT NULL, display_name TEXT NOT NULL, captured_at TEXT, fingerprint_json TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE file_instances (id TEXT PRIMARY KEY, media_asset_id TEXT NOT NULL REFERENCES media_assets(id), storage_volume_id TEXT NOT NULL REFERENCES storage_volumes(id), relative_path TEXT NOT NULL, observed_at TEXT NOT NULL, is_available INTEGER NOT NULL CHECK (is_available IN (0, 1)));
CREATE TABLE derivatives (id TEXT PRIMARY KEY, source_asset_id TEXT NOT NULL REFERENCES media_assets(id), derived_asset_id TEXT NOT NULL REFERENCES media_assets(id), recipe_hint TEXT, created_at TEXT NOT NULL);
CREATE TABLE backup_copies (id TEXT PRIMARY KEY, source_file_instance_id TEXT NOT NULL REFERENCES file_instances(id), backup_file_instance_id TEXT NOT NULL REFERENCES file_instances(id), verified_at TEXT, created_at TEXT NOT NULL);
CREATE TABLE relationships (id TEXT PRIMARY KEY, source_kind TEXT NOT NULL, source_id TEXT NOT NULL, target_kind TEXT NOT NULL, target_id TEXT NOT NULL, kind TEXT NOT NULL, confidence REAL, provenance_json TEXT NOT NULL, created_at TEXT NOT NULL, CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)));
CREATE TABLE analysis_artifacts (id TEXT PRIMARY KEY, media_asset_id TEXT NOT NULL REFERENCES media_assets(id), artifact_type TEXT NOT NULL, payload_json TEXT NOT NULL, provenance_json TEXT NOT NULL);
CREATE TABLE corrections (id TEXT PRIMARY KEY, subject_kind TEXT NOT NULL, subject_id TEXT NOT NULL, field_name TEXT NOT NULL, previous_value_json TEXT, corrected_value_json TEXT NOT NULL, corrected_at TEXT NOT NULL, author_hint TEXT);
CREATE TABLE workflow_runs (id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL, state_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE background_jobs (id TEXT PRIMARY KEY, state_json TEXT NOT NULL, stage_json TEXT NOT NULL, items_completed INTEGER NOT NULL CHECK (items_completed >= 0), items_total INTEGER, error_message TEXT, resume_metadata_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE operation_records (id TEXT PRIMARY KEY, kind_json TEXT NOT NULL, subject_kind TEXT NOT NULL, subject_id TEXT NOT NULL, occurred_at TEXT NOT NULL, details_json TEXT NOT NULL);
CREATE TABLE provenance_records (id TEXT PRIMARY KEY, subject_kind TEXT NOT NULL, subject_id TEXT NOT NULL, provenance_json TEXT NOT NULL);
CREATE INDEX idx_shoots_project ON shoots(project_id);
CREATE INDEX idx_moments_shoot ON moments(shoot_id);
CREATE INDEX idx_assets_project_capture ON media_assets(project_id, captured_at);
CREATE INDEX idx_instances_asset ON file_instances(media_asset_id);
CREATE INDEX idx_instances_volume ON file_instances(storage_volume_id);
CREATE INDEX idx_relationships_source ON relationships(source_kind, source_id);
CREATE INDEX idx_relationships_target ON relationships(target_kind, target_id);
CREATE INDEX idx_corrections_subject ON corrections(subject_kind, subject_id);
CREATE INDEX idx_jobs_state ON background_jobs(state_json);
COMMIT;
"#;

const MIGRATION_002: &str = r#"
BEGIN;
CREATE TABLE index_roots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  selected_path TEXT NOT NULL,
  storage_volume_id TEXT NOT NULL REFERENCES storage_volumes(id),
  added_at TEXT NOT NULL,
  last_indexed_at TEXT,
  status_json TEXT NOT NULL,
  UNIQUE(project_id, selected_path)
);
ALTER TABLE media_assets ADD COLUMN extension TEXT NOT NULL DEFAULT '';
ALTER TABLE media_assets ADD COLUMN fast_fingerprint TEXT;
ALTER TABLE media_assets ADD COLUMN byte_size INTEGER;
ALTER TABLE media_assets ADD COLUMN observed_modified_at TEXT;
ALTER TABLE file_instances ADD COLUMN index_root_id TEXT REFERENCES index_roots(id);
ALTER TABLE background_jobs ADD COLUMN files_discovered INTEGER NOT NULL DEFAULT 0;
ALTER TABLE background_jobs ADD COLUMN files_processed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE background_jobs ADD COLUMN error_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE background_jobs ADD COLUMN project_id TEXT REFERENCES projects(id);
ALTER TABLE background_jobs ADD COLUMN index_root_id TEXT REFERENCES index_roots(id);
ALTER TABLE background_jobs ADD COLUMN finished_at TEXT;
CREATE TABLE index_issues (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES background_jobs(id),
  relative_path TEXT,
  severity_json TEXT NOT NULL,
  message TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_instances_root_path ON file_instances(index_root_id, relative_path);
CREATE INDEX idx_roots_project ON index_roots(project_id, last_indexed_at);
CREATE INDEX idx_assets_merge ON media_assets(project_id, media_type, extension, fast_fingerprint, byte_size);
CREATE INDEX idx_jobs_project ON background_jobs(project_id, created_at);
CREATE INDEX idx_issues_job ON index_issues(job_id);
COMMIT;
"#;

// M1 originally stored the selected folder basename and path in the volume record.
// Index roots retain those values; this migration resets only affected volume metadata
// to a neutral device-derived label until the next safe local volume observation updates it.
const MIGRATION_003: &str = r#"
BEGIN;
UPDATE storage_volumes
SET display_name = CASE
      WHEN filesystem_identity IS NULL OR filesystem_identity = '' THEN 'Local volume'
      ELSE 'Volume (' || filesystem_identity || ')'
    END,
    mount_location = NULL,
    filesystem_type = NULL
WHERE id IN (SELECT DISTINCT storage_volume_id FROM index_roots);
COMMIT;
"#;

const MIGRATION_004: &str = r#"
BEGIN;
ALTER TABLE media_assets ADD COLUMN content_hash TEXT;
CREATE TABLE ingest_jobs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  state_json TEXT NOT NULL,
  policy_json TEXT NOT NULL,
  guardian_state_json TEXT NOT NULL,
  safe_to_eject INTEGER NOT NULL CHECK (safe_to_eject IN (0, 1)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE ingest_sources (
  id TEXT PRIMARY KEY,
  ingest_job_id TEXT NOT NULL REFERENCES ingest_jobs(id),
  label TEXT NOT NULL,
  selected_path TEXT NOT NULL,
  storage_volume_id TEXT NOT NULL REFERENCES storage_volumes(id),
  file_count INTEGER NOT NULL,
  total_bytes INTEGER NOT NULL,
  status_json TEXT NOT NULL,
  warnings_json TEXT NOT NULL
);
CREATE TABLE ingest_destinations (
  id TEXT PRIMARY KEY,
  ingest_job_id TEXT NOT NULL REFERENCES ingest_jobs(id),
  role_json TEXT NOT NULL,
  selected_path TEXT NOT NULL,
  storage_volume_id TEXT NOT NULL REFERENCES storage_volumes(id),
  available_bytes INTEGER,
  required_bytes INTEGER NOT NULL,
  writable INTEGER NOT NULL CHECK (writable IN (0, 1)),
  status_json TEXT NOT NULL
);
CREATE TABLE ingest_items (
  id TEXT PRIMARY KEY,
  ingest_job_id TEXT NOT NULL REFERENCES ingest_jobs(id),
  ingest_source_id TEXT NOT NULL REFERENCES ingest_sources(id),
  ingest_destination_id TEXT NOT NULL REFERENCES ingest_destinations(id),
  media_asset_id TEXT REFERENCES media_assets(id),
  source_relative_path TEXT NOT NULL,
  destination_relative_path TEXT NOT NULL,
  byte_size INTEGER NOT NULL,
  source_hash TEXT,
  destination_hash TEXT,
  state_json TEXT NOT NULL,
  verification_state_json TEXT NOT NULL,
  source_file_instance_id TEXT REFERENCES file_instances(id),
  destination_file_instance_id TEXT REFERENCES file_instances(id),
  error_message TEXT,
  updated_at TEXT NOT NULL,
  UNIQUE(ingest_job_id, ingest_source_id, ingest_destination_id, source_relative_path)
);
CREATE TABLE ingest_audit_events (
  id TEXT PRIMARY KEY,
  ingest_job_id TEXT NOT NULL REFERENCES ingest_jobs(id),
  event_type TEXT NOT NULL,
  detail TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_assets_content_hash ON media_assets(project_id, media_type, extension, byte_size, content_hash);
CREATE INDEX idx_ingest_jobs_project ON ingest_jobs(project_id, created_at DESC);
CREATE INDEX idx_ingest_sources_job ON ingest_sources(ingest_job_id);
CREATE INDEX idx_ingest_destinations_job ON ingest_destinations(ingest_job_id);
CREATE INDEX idx_ingest_items_job ON ingest_items(ingest_job_id, state_json);
CREATE INDEX idx_ingest_audit_job ON ingest_audit_events(ingest_job_id, created_at);
COMMIT;
"#;

const MIGRATION_005: &str = r#"
BEGIN;
ALTER TABLE ingest_jobs ADD COLUMN start_request_id TEXT;
CREATE UNIQUE INDEX idx_ingest_jobs_start_request
  ON ingest_jobs(start_request_id)
  WHERE start_request_id IS NOT NULL;
COMMIT;
"#;

const MIGRATION_006: &str = r#"
BEGIN;
CREATE TABLE media_metadata (
  media_asset_id TEXT PRIMARY KEY REFERENCES media_assets(id),
  source_file_instance_id TEXT NOT NULL REFERENCES file_instances(id),
  source_fingerprint TEXT NOT NULL,
  extractor TEXT NOT NULL,
  extractor_version TEXT NOT NULL,
  status TEXT NOT NULL,
  failure_reason TEXT,
  extracted_at TEXT NOT NULL,
  mime_type TEXT,
  byte_size INTEGER,
  captured_at_raw TEXT,
  captured_at_local TEXT,
  capture_timezone TEXT,
  capture_time_source TEXT,
  width INTEGER,
  height INTEGER,
  orientation TEXT,
  camera_make TEXT,
  camera_model TEXT,
  lens_make TEXT,
  lens_model TEXT,
  focal_length_mm REAL,
  focal_length_equivalent_mm REAL,
  aperture REAL,
  shutter_speed TEXT,
  iso INTEGER,
  exposure_compensation TEXT,
  flash TEXT,
  white_balance TEXT,
  color_space TEXT,
  gps_present INTEGER,
  duration_ms INTEGER,
  frame_rate TEXT,
  codec TEXT,
  pixel_format TEXT,
  bitrate INTEGER,
  audio_streams INTEGER,
  video_streams INTEGER,
  sample_rate INTEGER,
  bit_depth INTEGER,
  channels INTEGER,
  raw_metadata_json TEXT NOT NULL
);
CREATE TABLE preview_artifacts (
  id TEXT PRIMARY KEY,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  source_file_instance_id TEXT NOT NULL REFERENCES file_instances(id),
  artifact_type TEXT NOT NULL,
  size_class TEXT NOT NULL,
  cache_relative_path TEXT NOT NULL,
  provider TEXT NOT NULL,
  generator_version TEXT NOT NULL,
  source_fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL,
  failure_reason TEXT,
  UNIQUE(media_asset_id, source_file_instance_id, artifact_type, size_class)
);
CREATE INDEX idx_metadata_camera ON media_metadata(camera_model, lens_model, captured_at_local);
CREATE INDEX idx_preview_asset_source ON preview_artifacts(media_asset_id, source_file_instance_id, size_class, status);
COMMIT;
"#;

const MIGRATION_007: &str = r#"
BEGIN;
CREATE INDEX idx_background_jobs_visual_recovery
  ON background_jobs(state_json, updated_at);
COMMIT;
"#;

// M4 is intentionally additive. Legacy analysis rows remain readable through their original
// payload/provenance columns; nullable lifecycle fields make their unknown cache provenance
// explicit instead of fabricating a current analyzer result.
const MIGRATION_008: &str = r#"
BEGIN;
ALTER TABLE analysis_artifacts ADD COLUMN provider TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN provider_version TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN model_version TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN settings_version TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN input_fingerprint TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN generated_at TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN confidence REAL;
ALTER TABLE analysis_artifacts ADD COLUMN status TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN error_message TEXT;
ALTER TABLE analysis_artifacts ADD COLUMN stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1));

CREATE TABLE visual_fingerprints (
  media_asset_id TEXT PRIMARY KEY REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  perceptual_hash TEXT NOT NULL,
  difference_hash TEXT NOT NULL,
  color_signature BLOB NOT NULL,
  bucket_keys_json TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT
);
CREATE TABLE visual_embeddings (
  media_asset_id TEXT PRIMARY KEY REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK (dimensions > 0),
  embedding BLOB NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT
);
CREATE TABLE similarity_groups (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  kind TEXT NOT NULL,
  representative_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  grouping_method TEXT NOT NULL,
  grouping_version TEXT NOT NULL,
  similarity_confidence REAL NOT NULL CHECK (similarity_confidence >= 0.0 AND similarity_confidence <= 1.0),
  time_proximity_seconds INTEGER,
  visual_similarity REAL,
  created_at TEXT NOT NULL
);
CREATE TABLE similarity_group_members (
  group_id TEXT NOT NULL REFERENCES similarity_groups(id) ON DELETE CASCADE,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  ordinal INTEGER NOT NULL,
  similarity_confidence REAL NOT NULL CHECK (similarity_confidence >= 0.0 AND similarity_confidence <= 1.0),
  time_proximity_seconds INTEGER,
  is_representative INTEGER NOT NULL CHECK (is_representative IN (0, 1)),
  PRIMARY KEY(group_id, media_asset_id)
);
CREATE TABLE face_analyses (
  id TEXT PRIMARY KEY,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  x REAL NOT NULL CHECK (x >= 0.0 AND x <= 1.0),
  y REAL NOT NULL CHECK (y >= 0.0 AND y <= 1.0),
  width REAL NOT NULL CHECK (width > 0.0 AND width <= 1.0),
  height REAL NOT NULL CHECK (height > 0.0 AND height <= 1.0),
  detection_confidence REAL NOT NULL CHECK (detection_confidence >= 0.0 AND detection_confidence <= 1.0),
  relative_size REAL NOT NULL CHECK (relative_size >= 0.0 AND relative_size <= 1.0),
  visibility TEXT,
  pose TEXT,
  face_sharpness REAL,
  eye_state TEXT NOT NULL,
  eye_confidence REAL,
  generated_at TEXT NOT NULL
);
CREATE TABLE technical_quality (
  media_asset_id TEXT PRIMARY KEY REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  global_sharpness REAL,
  sharpness_band TEXT NOT NULL,
  directional_blur_ratio REAL,
  blur_level TEXT NOT NULL,
  mean_luminance REAL,
  median_luminance REAL,
  highlight_clipping_percent REAL,
  shadow_clipping_percent REAL,
  channel_clipping_percent REAL,
  technical_quality_score REAL,
  technical_quality_band TEXT NOT NULL,
  confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT
);
CREATE TABLE analysis_recommendations (
  id TEXT PRIMARY KEY,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  similarity_group_id TEXT REFERENCES similarity_groups(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  label TEXT NOT NULL,
  confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
  reasons_json TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  UNIQUE(media_asset_id, provider, provider_version, input_fingerprint)
);
CREATE TABLE intelligence_overrides (
  id TEXT PRIMARY KEY,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  recommendation_id TEXT REFERENCES analysis_recommendations(id),
  decision TEXT NOT NULL,
  decided_at TEXT NOT NULL,
  note TEXT
);
CREATE TABLE local_model_registry (
  id TEXT PRIMARY KEY,
  model_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  version TEXT NOT NULL,
  local_relative_path TEXT,
  checksum TEXT,
  capability TEXT NOT NULL,
  status TEXT NOT NULL,
  license TEXT NOT NULL,
  source_url TEXT,
  file_size_bytes INTEGER,
  hardware_requirements TEXT,
  registered_at TEXT NOT NULL,
  UNIQUE(model_id, provider, version)
);

CREATE INDEX idx_analysis_artifacts_cache
  ON analysis_artifacts(media_asset_id, artifact_type, provider, provider_version, input_fingerprint, status);
CREATE INDEX idx_visual_fingerprints_bucket ON visual_fingerprints(perceptual_hash, provider_version);
CREATE INDEX idx_similarity_groups_project ON similarity_groups(project_id, kind, created_at);
CREATE INDEX idx_similarity_members_asset ON similarity_group_members(media_asset_id, group_id);
CREATE INDEX idx_faces_asset ON face_analyses(media_asset_id, input_fingerprint);
CREATE INDEX idx_quality_band ON technical_quality(technical_quality_band, blur_level);
CREATE INDEX idx_recommendations_asset ON analysis_recommendations(media_asset_id, generated_at);
CREATE INDEX idx_overrides_asset ON intelligence_overrides(media_asset_id, decided_at);
CREATE INDEX idx_intelligence_jobs_recovery ON background_jobs(state_json, updated_at)
  WHERE resume_metadata_json LIKE '%"pipeline":"capture-intelligence"%';
COMMIT;
"#;

// Version 9 turns the originally current-only M4 evidence tables into append-preserving
// revision stores. A current row is `stale = 0`; a prior row remains queryable for audit but
// can never be confused with the evidence for the currently selected preview/analyzer tuple.
// `migrate_m4_evidence_history` runs this with foreign-key enforcement paused only for the
// table rebuild and validates it again immediately afterwards.
const MIGRATION_009: &str = r#"
BEGIN;
DROP INDEX IF EXISTS idx_visual_fingerprints_bucket;
DROP INDEX IF EXISTS idx_quality_band;
DROP INDEX IF EXISTS idx_recommendations_asset;

ALTER TABLE visual_fingerprints RENAME TO visual_fingerprints_v8;
CREATE TABLE visual_fingerprints (
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  perceptual_hash TEXT NOT NULL,
  difference_hash TEXT NOT NULL,
  color_signature BLOB NOT NULL,
  bucket_keys_json TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  PRIMARY KEY(media_asset_id, input_fingerprint, provider, provider_version)
);
INSERT INTO visual_fingerprints (
  media_asset_id, input_fingerprint, provider, provider_version, perceptual_hash,
  difference_hash, color_signature, bucket_keys_json, generated_at, status, error_message, stale
)
SELECT media_asset_id, input_fingerprint, provider, provider_version, perceptual_hash,
  difference_hash, color_signature, bucket_keys_json, generated_at, status, error_message, 0
FROM visual_fingerprints_v8;
DROP TABLE visual_fingerprints_v8;

ALTER TABLE visual_embeddings RENAME TO visual_embeddings_v8;
CREATE TABLE visual_embeddings (
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK (dimensions > 0),
  embedding BLOB NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  PRIMARY KEY(media_asset_id, input_fingerprint, provider, provider_version)
);
INSERT INTO visual_embeddings (
  media_asset_id, input_fingerprint, provider, provider_version, dimensions,
  embedding, generated_at, status, error_message, stale
)
SELECT media_asset_id, input_fingerprint, provider, provider_version, dimensions,
  embedding, generated_at, status, error_message, 0
FROM visual_embeddings_v8;
DROP TABLE visual_embeddings_v8;

ALTER TABLE technical_quality RENAME TO technical_quality_v8;
CREATE TABLE technical_quality (
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  global_sharpness REAL,
  sharpness_band TEXT NOT NULL,
  directional_blur_ratio REAL,
  blur_level TEXT NOT NULL,
  mean_luminance REAL,
  median_luminance REAL,
  highlight_clipping_percent REAL,
  shadow_clipping_percent REAL,
  channel_clipping_percent REAL,
  technical_quality_score REAL,
  technical_quality_band TEXT NOT NULL,
  confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  PRIMARY KEY(media_asset_id, input_fingerprint, provider, provider_version)
);
INSERT INTO technical_quality (
  media_asset_id, input_fingerprint, provider, provider_version, global_sharpness,
  sharpness_band, directional_blur_ratio, blur_level, mean_luminance, median_luminance,
  highlight_clipping_percent, shadow_clipping_percent, channel_clipping_percent,
  technical_quality_score, technical_quality_band, confidence, generated_at, status,
  error_message, stale
)
SELECT media_asset_id, input_fingerprint, provider, provider_version, global_sharpness,
  sharpness_band, directional_blur_ratio, blur_level, mean_luminance, median_luminance,
  highlight_clipping_percent, shadow_clipping_percent, channel_clipping_percent,
  technical_quality_score, technical_quality_band, confidence, generated_at, status,
  error_message, 0
FROM technical_quality_v8;
DROP TABLE technical_quality_v8;

ALTER TABLE analysis_recommendations RENAME TO analysis_recommendations_v8;
CREATE TABLE analysis_recommendations (
  id TEXT PRIMARY KEY,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  similarity_group_id TEXT REFERENCES similarity_groups(id),
  input_fingerprint TEXT NOT NULL,
  provider TEXT NOT NULL,
  provider_version TEXT NOT NULL,
  label TEXT NOT NULL,
  confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
  reasons_json TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1))
);
INSERT INTO analysis_recommendations (
  id, media_asset_id, similarity_group_id, input_fingerprint, provider, provider_version,
  label, confidence, reasons_json, generated_at, status, stale
)
SELECT id, media_asset_id, similarity_group_id, input_fingerprint, provider, provider_version,
  label, confidence, reasons_json, generated_at, status, 0
FROM analysis_recommendations_v8;
DROP TABLE analysis_recommendations_v8;

ALTER TABLE similarity_groups ADD COLUMN stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1));

CREATE INDEX idx_visual_fingerprints_bucket
  ON visual_fingerprints(provider_version, stale, perceptual_hash);
CREATE INDEX idx_visual_embeddings_current
  ON visual_embeddings(media_asset_id, input_fingerprint, provider, provider_version, stale);
CREATE INDEX idx_quality_band
  ON technical_quality(stale, technical_quality_band, blur_level);
CREATE INDEX idx_technical_quality_current
  ON technical_quality(media_asset_id, input_fingerprint, provider, provider_version, stale);
CREATE INDEX idx_recommendations_asset
  ON analysis_recommendations(media_asset_id, generated_at);
CREATE INDEX idx_recommendations_current
  ON analysis_recommendations(media_asset_id, provider, provider_version, input_fingerprint, stale, generated_at);
CREATE INDEX idx_similarity_groups_active
  ON similarity_groups(project_id, stale, kind, created_at);
COMMIT;
"#;

// M5 culling state is fully separate from Capture Intelligence evidence. Current review state
// is cheap to query; every mutation also receives an immutable local history/event row. Nothing
// here references a source path or provides a route to change source media.
const MIGRATION_010: &str = r#"
BEGIN;
CREATE TABLE review_sessions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  started_at TEXT NOT NULL,
  ended_at TEXT,
  mode TEXT NOT NULL,
  last_asset_id TEXT REFERENCES media_assets(id),
  last_group_id TEXT REFERENCES similarity_groups(id),
  filter_context TEXT,
  photos_reviewed INTEGER NOT NULL DEFAULT 0,
  sets_reviewed INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE media_decisions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  decision TEXT CHECK (decision IN ('keep', 'review', 'reject')),
  rating INTEGER NOT NULL DEFAULT 0 CHECK (rating >= 0 AND rating <= 5),
  starred INTEGER NOT NULL DEFAULT 0 CHECK (starred IN (0, 1)),
  note TEXT,
  flags_json TEXT NOT NULL DEFAULT '[]',
  source TEXT NOT NULL DEFAULT 'human',
  session_id TEXT REFERENCES review_sessions(id),
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, media_asset_id)
);
CREATE TABLE decision_history (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  media_asset_id TEXT REFERENCES media_assets(id),
  decision_id TEXT REFERENCES media_decisions(id),
  session_id TEXT REFERENCES review_sessions(id),
  event_type TEXT NOT NULL CHECK (event_type IN ('DECISION_CHANGED', 'RATING_CHANGED', 'STAR_CHANGED', 'NOTE_CHANGED', 'FLAGS_CHANGED')),
  previous_value_json TEXT,
  current_value_json TEXT NOT NULL,
  source TEXT NOT NULL DEFAULT 'human',
  created_at TEXT NOT NULL
);
CREATE TABLE review_events (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  session_id TEXT REFERENCES review_sessions(id),
  media_asset_id TEXT REFERENCES media_assets(id),
  similarity_group_id TEXT REFERENCES similarity_groups(id),
  event_type TEXT NOT NULL CHECK (event_type IN ('REVIEW_STARTED', 'DECISION_CHANGED', 'RATING_CHANGED', 'STAR_CHANGED', 'GROUP_REPRESENTATIVE_CHANGED', 'REVIEW_FINISHED')),
  details_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL
);
CREATE TABLE group_human_representatives (
  group_id TEXT PRIMARY KEY REFERENCES similarity_groups(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  session_id TEXT REFERENCES review_sessions(id),
  selected_at TEXT NOT NULL
);
CREATE TABLE group_review_completion (
  group_id TEXT PRIMARY KEY REFERENCES similarity_groups(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  session_id TEXT REFERENCES review_sessions(id),
  completed_at TEXT NOT NULL
);
CREATE TABLE preference_examples (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  similarity_group_id TEXT NOT NULL REFERENCES similarity_groups(id),
  chosen_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  compared_asset_ids_json TEXT NOT NULL,
  technical_evidence_json TEXT NOT NULL,
  ai_recommendation_json TEXT NOT NULL,
  human_decision_context TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_media_decisions_project_state ON media_decisions(project_id, decision, rating, starred, updated_at);
CREATE INDEX idx_media_decisions_asset ON media_decisions(media_asset_id);
CREATE INDEX idx_decision_history_project_asset ON decision_history(project_id, media_asset_id, created_at);
CREATE INDEX idx_review_sessions_project_active ON review_sessions(project_id, ended_at, started_at);
CREATE INDEX idx_review_events_project_type ON review_events(project_id, event_type, created_at);
CREATE INDEX idx_preference_examples_group ON preference_examples(project_id, similarity_group_id, created_at);
COMMIT;
"#;

// M6 semantic retrieval is deliberately isolated from M4's compact visual descriptor and M5's
// human workflow state. Embeddings are durable, local derived evidence; the on-disk ANN index is
// only a rebuildable acceleration artifact referenced by `semantic_index_versions`.
const MIGRATION_011: &str = r#"
BEGIN;
ALTER TABLE local_model_registry ADD COLUMN model_family TEXT;
ALTER TABLE local_model_registry ADD COLUMN input_size INTEGER;
ALTER TABLE local_model_registry ADD COLUMN embedding_dimension INTEGER;
ALTER TABLE local_model_registry ADD COLUMN license_url TEXT;

CREATE TABLE semantic_embeddings (
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  input_fingerprint TEXT NOT NULL,
  model_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  model_version TEXT NOT NULL,
  embedding_version TEXT NOT NULL,
  preprocessing_version TEXT NOT NULL,
  metric TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK (dimensions > 0),
  encoding TEXT NOT NULL,
  embedding_blob BLOB,
  generated_at TEXT NOT NULL,
  status TEXT NOT NULL,
  error_message TEXT,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  PRIMARY KEY(media_asset_id, input_fingerprint, model_id, provider, model_version, embedding_version, preprocessing_version, metric)
);
CREATE TABLE semantic_index_versions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  model_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  model_version TEXT NOT NULL,
  embedding_version TEXT NOT NULL,
  preprocessing_version TEXT NOT NULL,
  metric TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK (dimensions > 0),
  index_format TEXT NOT NULL,
  index_relative_path TEXT NOT NULL,
  index_checksum TEXT NOT NULL,
  embedding_count INTEGER NOT NULL CHECK (embedding_count >= 0),
  status TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  created_at TEXT NOT NULL,
  rebuilt_at TEXT NOT NULL,
  UNIQUE(project_id, model_id, provider, model_version, embedding_version, preprocessing_version, metric, index_format)
);
CREATE TABLE magic_search_history (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  query_text TEXT NOT NULL,
  normalized_query TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  used_at TEXT NOT NULL,
  use_count INTEGER NOT NULL DEFAULT 1 CHECK (use_count > 0),
  UNIQUE(project_id, normalized_query)
);
-- Foundation only: a saved definition has no materialized media membership and never changes
-- human decisions or project identity.
CREATE TABLE saved_magic_searches (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  display_name TEXT NOT NULL,
  query_text TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, display_name)
);
CREATE INDEX idx_semantic_embeddings_active_project_model
  ON semantic_embeddings(project_id, model_id, provider, model_version, embedding_version, preprocessing_version, stale, status);
CREATE INDEX idx_semantic_embeddings_asset_active
  ON semantic_embeddings(media_asset_id, stale, status);
CREATE INDEX idx_semantic_index_versions_active
  ON semantic_index_versions(project_id, stale, status, rebuilt_at);
CREATE INDEX idx_magic_search_history_project_used
  ON magic_search_history(project_id, used_at DESC);
CREATE INDEX idx_saved_magic_searches_project
  ON saved_magic_searches(project_id, updated_at DESC);
COMMIT;
"#;

// M7 keeps the legacy Phase-0 `shoots` / `moments` scaffold untouched. These tables hold a
// versioned, project-scoped local analysis projection with direct memberships and explicit human
// override records; they never become the source of truth for Similar Sets or culling decisions.
const MIGRATION_012: &str = r#"
BEGIN;
CREATE TABLE shoot_timelines (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL UNIQUE REFERENCES projects(id),
  active_run_id TEXT,
  state TEXT NOT NULL,
  analyzer_id TEXT NOT NULL,
  analyzer_version TEXT NOT NULL,
  boundary_algorithm_version TEXT NOT NULL,
  semantic_model_key TEXT,
  input_catalog_version TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_analyzed_at TEXT
);
CREATE TABLE moment_analysis_runs (
  id TEXT PRIMARY KEY,
  timeline_id TEXT NOT NULL REFERENCES shoot_timelines(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  state TEXT NOT NULL,
  analyzer_id TEXT NOT NULL,
  analyzer_version TEXT NOT NULL,
  boundary_algorithm_version TEXT NOT NULL,
  semantic_model_key TEXT,
  input_catalog_version TEXT NOT NULL,
  items_total INTEGER NOT NULL CHECK (items_total >= 0),
  items_completed INTEGER NOT NULL CHECK (items_completed >= 0),
  error_count INTEGER NOT NULL CHECK (error_count >= 0),
  started_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE timeline_segments (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  run_id TEXT NOT NULL REFERENCES moment_analysis_runs(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  started_at TEXT,
  ended_at TEXT,
  asset_count INTEGER NOT NULL CHECK (asset_count >= 0),
  boundary_category TEXT,
  boundary_evidence_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  UNIQUE(run_id, ordinal)
);
CREATE TABLE moment_records (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  timeline_id TEXT NOT NULL REFERENCES shoot_timelines(id),
  run_id TEXT NOT NULL REFERENCES moment_analysis_runs(id),
  segment_id TEXT NOT NULL REFERENCES timeline_segments(id),
  anchor_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  started_at TEXT,
  ended_at TEXT,
  asset_count INTEGER NOT NULL CHECK (asset_count >= 0),
  ai_representative_asset_id TEXT REFERENCES media_assets(id),
  centroid_blob BLOB,
  centroid_dimensions INTEGER,
  suggested_label TEXT,
  label_confidence REAL,
  label_evidence_json TEXT NOT NULL,
  label_state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  UNIQUE(run_id, ordinal)
);
CREATE TABLE moment_memberships (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  run_id TEXT NOT NULL REFERENCES moment_analysis_runs(id),
  moment_id TEXT REFERENCES moment_records(id),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  membership_state TEXT NOT NULL CHECK (membership_state IN ('member', 'ungrouped')),
  created_at TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
  UNIQUE(run_id, media_asset_id)
);
CREATE TABLE moment_boundary_evidence (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  run_id TEXT NOT NULL REFERENCES moment_analysis_runs(id),
  left_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  right_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  category TEXT NOT NULL,
  components_json TEXT NOT NULL,
  explanation TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(run_id, ordinal)
);
CREATE TABLE moment_human_labels (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  anchor_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  label TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, anchor_asset_id)
);
CREATE TABLE moment_human_representatives (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  anchor_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  selected_at TEXT NOT NULL,
  UNIQUE(project_id, anchor_asset_id)
);
CREATE TABLE moment_override_operations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  operation TEXT NOT NULL CHECK (operation IN ('split', 'merge')),
  left_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  right_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  created_at TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
  UNIQUE(project_id, operation, left_asset_id, right_asset_id)
);
CREATE TABLE moment_events (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  moment_id TEXT,
  event_type TEXT NOT NULL CHECK (event_type IN ('MOMENT_CREATED', 'MOMENT_RENAMED', 'MOMENT_MERGED', 'MOMENT_SPLIT', 'MOMENT_REPRESENTATIVE_CHANGED', 'COVERAGE_CONFIRMED')),
  details_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE coverage_checklist_items (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  phrase TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(project_id, phrase)
);
CREATE TABLE coverage_confirmations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  checklist_item_id TEXT NOT NULL REFERENCES coverage_checklist_items(id),
  state TEXT NOT NULL CHECK (state IN ('confirmed_covered', 'needs_review', 'not_covered')),
  moment_id TEXT,
  media_asset_id TEXT REFERENCES media_assets(id),
  confirmed_at TEXT NOT NULL,
  UNIQUE(project_id, checklist_item_id)
);
CREATE TABLE camera_clock_offset_diagnostics (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  run_id TEXT NOT NULL REFERENCES moment_analysis_runs(id),
  camera_a TEXT NOT NULL,
  camera_b TEXT NOT NULL,
  possible_offset_seconds INTEGER,
  evidence_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_shoot_timelines_project ON shoot_timelines(project_id, updated_at DESC);
CREATE INDEX idx_moment_runs_project ON moment_analysis_runs(project_id, started_at DESC);
CREATE INDEX idx_segments_project_run ON timeline_segments(project_id, run_id, ordinal);
CREATE INDEX idx_moment_records_active ON moment_records(project_id, timeline_id, stale, ordinal);
CREATE INDEX idx_moment_memberships_asset ON moment_memberships(project_id, media_asset_id, active);
CREATE INDEX idx_moment_memberships_moment ON moment_memberships(project_id, moment_id, active, ordinal);
CREATE INDEX idx_moment_boundary_run ON moment_boundary_evidence(project_id, run_id, ordinal);
CREATE INDEX idx_moment_events_project ON moment_events(project_id, created_at DESC);
CREATE INDEX idx_coverage_items_project ON coverage_checklist_items(project_id, created_at DESC);
COMMIT;
"#;

// M7.1 keeps one logical resolved timestamp on `media_metadata` while retaining the narrowly
// scoped per-copy observations needed to explain conflicts. It is additive: existing previews,
// semantic embeddings, culling decisions, Similar Sets, and M7 human events are untouched.
const MIGRATION_013: &str = r#"
BEGIN;
ALTER TABLE media_metadata ADD COLUMN capture_time_confidence TEXT;
CREATE TABLE capture_time_observations (
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  source_file_instance_id TEXT NOT NULL REFERENCES file_instances(id),
  source_fingerprint TEXT NOT NULL,
  extractor TEXT NOT NULL,
  extractor_version TEXT NOT NULL,
  status TEXT NOT NULL,
  failure_reason TEXT,
  extracted_at TEXT NOT NULL,
  captured_at_raw TEXT,
  captured_at_local TEXT,
  capture_timezone TEXT,
  capture_time_source TEXT,
  capture_time_confidence TEXT,
  PRIMARY KEY(media_asset_id, source_file_instance_id)
);
CREATE INDEX idx_capture_time_observations_asset
  ON capture_time_observations(media_asset_id, capture_time_source, captured_at_local);
COMMIT;
"#;

// M8 Studio Brain is an additive, local-only derived-data layer. Human decisions and all M0–M7
// records remain the durable source of authority; models and recommendations are rebuildable.
const MIGRATION_014: &str = r#"
BEGIN;
CREATE TABLE studio_profiles (
  id TEXT PRIMARY KEY,
  profile_key TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  training_status TEXT NOT NULL CHECK (training_status IN ('not_ready', 'learning', 'ready', 'stale', 'error')),
  personalization_enabled INTEGER NOT NULL DEFAULT 1 CHECK (personalization_enabled IN (0, 1)),
  training_settings_json TEXT NOT NULL,
  readiness_json TEXT NOT NULL,
  last_trained_at TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE project_training_preferences (
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id),
  included INTEGER NOT NULL CHECK (included IN (0, 1)),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(studio_profile_id, project_id)
);
CREATE TABLE studio_training_examples (
  id TEXT PRIMARY KEY,
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id),
  media_asset_id TEXT REFERENCES media_assets(id),
  source_kind TEXT NOT NULL,
  source_record_id TEXT NOT NULL,
  decision_type TEXT NOT NULL,
  decision_value TEXT,
  occurred_at TEXT NOT NULL,
  review_session_id TEXT REFERENCES review_sessions(id),
  similarity_group_id TEXT REFERENCES similarity_groups(id),
  moment_id TEXT,
  generic_recommendation_json TEXT NOT NULL,
  studio_recommendation_id_at_decision TEXT,
  recommendation_shown TEXT NOT NULL CHECK (recommendation_shown IN ('shown', 'not_shown', 'unknown')),
  provenance TEXT NOT NULL CHECK (provenance IN ('explicit_human', 'historical_backfill')),
  feature_schema_version TEXT NOT NULL,
  feature_snapshot_json TEXT NOT NULL,
  training_eligible INTEGER NOT NULL DEFAULT 1 CHECK (training_eligible IN (0, 1)),
  created_at TEXT NOT NULL,
  UNIQUE(studio_profile_id, source_kind, source_record_id)
);
CREATE TABLE studio_training_example_references (
  studio_training_example_id TEXT NOT NULL REFERENCES studio_training_examples(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('chosen', 'alternative', 'moment_member')),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  PRIMARY KEY(studio_training_example_id, role, media_asset_id)
);
CREATE TABLE decision_training_exclusions (
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  studio_training_example_id TEXT NOT NULL REFERENCES studio_training_examples(id) ON DELETE CASCADE,
  excluded_at TEXT NOT NULL,
  PRIMARY KEY(studio_profile_id, studio_training_example_id)
);
CREATE TABLE studio_training_runs (
  id TEXT PRIMARY KEY,
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  background_job_id TEXT NOT NULL REFERENCES background_jobs(id),
  algorithm TEXT NOT NULL,
  algorithm_version TEXT NOT NULL,
  feature_schema_version TEXT NOT NULL,
  parameters_json TEXT NOT NULL,
  snapshot_hash TEXT NOT NULL,
  snapshot_count INTEGER NOT NULL CHECK (snapshot_count >= 0),
  previous_active_model_id TEXT,
  state TEXT NOT NULL CHECK (state IN ('queued', 'training', 'evaluating', 'persisting', 'completed', 'failed', 'interrupted', 'not_activated')),
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE studio_training_run_examples (
  studio_training_run_id TEXT NOT NULL REFERENCES studio_training_runs(id) ON DELETE CASCADE,
  studio_training_example_id TEXT NOT NULL REFERENCES studio_training_examples(id),
  split TEXT NOT NULL CHECK (split IN ('train', 'validation', 'holdout', 'excluded')),
  feature_snapshot_json TEXT NOT NULL,
  label TEXT,
  PRIMARY KEY(studio_training_run_id, studio_training_example_id)
);
CREATE TABLE studio_models (
  id TEXT PRIMARY KEY,
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  studio_training_run_id TEXT NOT NULL REFERENCES studio_training_runs(id) ON DELETE CASCADE,
  algorithm TEXT NOT NULL,
  model_version TEXT NOT NULL,
  feature_schema_version TEXT NOT NULL,
  artifact_json TEXT NOT NULL,
  checksum TEXT NOT NULL,
  artifact_size_bytes INTEGER NOT NULL CHECK (artifact_size_bytes >= 0),
  state TEXT NOT NULL CHECK (state IN ('candidate', 'active', 'previous', 'invalid', 'reset')),
  metrics_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  activated_at TEXT
);
CREATE TABLE studio_model_metrics (
  id TEXT PRIMARY KEY,
  studio_model_id TEXT NOT NULL REFERENCES studio_models(id) ON DELETE CASCADE,
  split TEXT NOT NULL,
  metric_name TEXT NOT NULL,
  metric_value REAL,
  sample_count INTEGER NOT NULL CHECK (sample_count >= 0),
  metadata_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE studio_recommendations (
  id TEXT PRIMARY KEY,
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  studio_model_id TEXT NOT NULL REFERENCES studio_models(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id),
  feature_schema_version TEXT NOT NULL,
  feature_fingerprint TEXT NOT NULL,
  recommendation TEXT NOT NULL CHECK (recommendation IN ('likely_keep', 'likely_review', 'likely_reject', 'not_enough_evidence')),
  confidence_band TEXT NOT NULL CHECK (confidence_band IN ('high', 'moderate', 'low', 'unavailable')),
  explanation_json TEXT NOT NULL,
  generic_recommendation_json TEXT NOT NULL,
  agreement TEXT NOT NULL CHECK (agreement IN ('agrees', 'differs', 'unavailable')),
  status TEXT NOT NULL CHECK (status IN ('ready', 'stale', 'unavailable', 'error')),
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  generated_at TEXT NOT NULL,
  UNIQUE(studio_model_id, media_asset_id, feature_fingerprint)
);
CREATE TABLE studio_events (
  id TEXT PRIMARY KEY,
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  project_id TEXT REFERENCES projects(id),
  studio_training_run_id TEXT REFERENCES studio_training_runs(id),
  studio_model_id TEXT REFERENCES studio_models(id),
  event_type TEXT NOT NULL,
  details_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_studio_examples_profile_project
  ON studio_training_examples(studio_profile_id, project_id, occurred_at);
CREATE INDEX idx_studio_examples_asset
  ON studio_training_examples(studio_profile_id, media_asset_id, occurred_at);
CREATE INDEX idx_studio_runs_profile_updated
  ON studio_training_runs(studio_profile_id, updated_at DESC);
CREATE INDEX idx_studio_recommendations_project_asset
  ON studio_recommendations(studio_profile_id, project_id, media_asset_id, stale, status);
CREATE INDEX idx_studio_events_profile_created
  ON studio_events(studio_profile_id, created_at DESC);
CREATE UNIQUE INDEX idx_studio_models_one_active
  ON studio_models(studio_profile_id) WHERE state = 'active';
CREATE UNIQUE INDEX idx_studio_runs_one_inflight
  ON studio_training_runs(studio_profile_id)
  WHERE state IN ('queued', 'training', 'evaluating', 'persisting');
COMMIT;
"#;

// M8.1 closes the final candidate-activation race without changing any human source record.
// Triggers advance only when the eligible source set can change; activation checks the value
// while it holds the same IMMEDIATE SQLite transaction that makes a model visible.
const MIGRATION_015: &str = r#"
BEGIN;
ALTER TABLE studio_profiles ADD COLUMN source_revision INTEGER NOT NULL DEFAULT 0;
CREATE TRIGGER studio_source_revision_example_insert
AFTER INSERT ON studio_training_examples
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = NEW.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_example_delete
AFTER DELETE ON studio_training_examples
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = OLD.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_exclusion_insert
AFTER INSERT ON decision_training_exclusions
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = NEW.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_exclusion_delete
AFTER DELETE ON decision_training_exclusions
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = OLD.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_project_preference_insert
AFTER INSERT ON project_training_preferences
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = NEW.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_project_preference_update
AFTER UPDATE OF included ON project_training_preferences
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = NEW.studio_profile_id;
END;
CREATE TRIGGER studio_source_revision_project_preference_delete
AFTER DELETE ON project_training_preferences
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1
  WHERE id = OLD.studio_profile_id;
END;
COMMIT;
"#;

// M8.2 records an in-progress authoritative source append. A training snapshot must not begin
// between an intentional human write and its immutable history/source materialization.
const MIGRATION_016: &str = r#"
BEGIN;
ALTER TABLE studio_profiles
  ADD COLUMN source_materialization_pending INTEGER NOT NULL DEFAULT 0
  CHECK (source_materialization_pending >= 0);
COMMIT;
"#;

// M8.3 records the source-append guard in the same SQLite statement/transaction as each
// authority-bearing human action. This closes the gap between a visible human choice and its
// later compact feature snapshot without ever making a Studio snapshot failure rewrite or
// reject that choice. The pending counter is cleared only after successful source capture or a
// complete explicit historical reconciliation.
const MIGRATION_017: &str = r#"
BEGIN;
CREATE TRIGGER studio_source_guard_m4_override
AFTER INSERT ON intelligence_overrides
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = (SELECT project_id FROM media_assets WHERE id = NEW.media_asset_id)), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_culling_insert
AFTER INSERT ON media_decisions
WHEN NEW.source = 'human' AND (NEW.decision IS NOT NULL OR NEW.rating <> 0 OR NEW.starred <> 0)
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_culling_update
AFTER UPDATE OF decision, rating, starred ON media_decisions
WHEN NEW.source = 'human'
 AND (NEW.decision IS NOT OLD.decision OR NEW.rating IS NOT OLD.rating OR NEW.starred IS NOT OLD.starred)
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_group_representative_insert
AFTER INSERT ON group_human_representatives
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_group_representative_update
AFTER UPDATE OF media_asset_id ON group_human_representatives
WHEN NEW.media_asset_id IS NOT OLD.media_asset_id
BEGIN
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_moment_representative_event
AFTER INSERT ON moment_events
WHEN NEW.event_type = 'MOMENT_REPRESENTATIVE_CHANGED'
 AND json_extract(NEW.details_json, '$.source') IS NULL
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      source_materialization_pending = source_materialization_pending + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
COMMIT;
"#;

// M8.4 makes each pending marker action-scoped. A profile-wide counter cannot distinguish a
// failed capture in one project from a later opted-out action in another; deleting only the
// matching durable guard makes reconciliation and activation fail closed without blocking the
// underlying human action.
const MIGRATION_018: &str = r#"
BEGIN;
DROP TRIGGER studio_source_guard_m4_override;
DROP TRIGGER studio_source_guard_culling_insert;
DROP TRIGGER studio_source_guard_culling_update;
DROP TRIGGER studio_source_guard_group_representative_insert;
DROP TRIGGER studio_source_guard_group_representative_update;
DROP TRIGGER studio_source_guard_moment_representative_event;
CREATE TABLE studio_source_materialization_guards (
  studio_profile_id TEXT NOT NULL REFERENCES studio_profiles(id) ON DELETE CASCADE,
  source_kind TEXT NOT NULL,
  source_record_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(studio_profile_id, source_kind, source_record_id)
);
CREATE INDEX idx_studio_source_guards_profile
  ON studio_source_materialization_guards(studio_profile_id, created_at);
CREATE TRIGGER studio_source_guard_m4_override
AFTER INSERT ON intelligence_overrides
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'm4_human_override', NEW.id, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = (SELECT project_id FROM media_assets WHERE id = NEW.media_asset_id)), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = (SELECT project_id FROM media_assets WHERE id = NEW.media_asset_id)), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_culling_insert
AFTER INSERT ON media_decisions
WHEN NEW.source = 'human' AND (NEW.decision IS NOT NULL OR NEW.rating <> 0 OR NEW.starred <> 0)
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'm5_culling', NEW.id || ':' || NEW.updated_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = NEW.project_id), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_culling_update
AFTER UPDATE OF decision, rating, starred ON media_decisions
WHEN NEW.source = 'human'
 AND (NEW.decision IS NOT OLD.decision OR NEW.rating IS NOT OLD.rating OR NEW.starred IS NOT OLD.starred)
BEGIN
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'm5_culling', NEW.id || ':' || NEW.updated_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = NEW.project_id), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_group_representative_insert
AFTER INSERT ON group_human_representatives
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'similar_set_representative', NEW.group_id || ':' || NEW.selected_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = NEW.project_id), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_group_representative_update
AFTER UPDATE OF media_asset_id ON group_human_representatives
WHEN NEW.media_asset_id IS NOT OLD.media_asset_id
BEGIN
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'similar_set_representative', NEW.group_id || ':' || NEW.selected_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = NEW.project_id), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
CREATE TRIGGER studio_source_guard_moment_representative_event
AFTER INSERT ON moment_events
WHEN NEW.event_type = 'MOMENT_REPRESENTATIVE_CHANGED'
 AND json_extract(NEW.details_json, '$.source') IS NULL
BEGIN
  INSERT OR IGNORE INTO studio_profiles (id, profile_key, display_name, training_status, personalization_enabled, training_settings_json, readiness_json, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'local-default', 'Local Studio Profile', 'not_ready', 1, '{"mode":"explicit_retrain","algorithm":"studio-linear-v1"}', '{"state":"not_ready","reasons":["No explicit human decision history has been materialized for training."]}', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
  SELECT profile.id, 'moment_representative', NEW.id, strftime('%Y-%m-%dT%H:%M:%fZ','now')
  FROM studio_profiles profile
  WHERE profile.profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = profile.id AND preference.project_id = NEW.project_id), 1) = 1;
  UPDATE studio_profiles
  SET source_revision = source_revision + 1,
      training_status = CASE WHEN EXISTS (SELECT 1 FROM studio_models model WHERE model.studio_profile_id = studio_profiles.id AND model.state = 'active') AND training_status <> 'error' THEN 'stale' ELSE training_status END,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  WHERE profile_key = 'local-default'
    AND COALESCE((SELECT included FROM project_training_preferences preference WHERE preference.studio_profile_id = studio_profiles.id AND preference.project_id = NEW.project_id), 1) = 1;
END;
-- M17's counter was an intermediate implementation. All current pending state is represented by
-- action-scoped rows above. Preserve any older unresolved counter as a recovery guard so it is
-- reconciled by the next explicit history pass rather than becoming silently activatable.
INSERT OR IGNORE INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at)
SELECT id, 'legacy_m17_recovery', 'pre-m18', updated_at
FROM studio_profiles
WHERE source_materialization_pending > 0;
UPDATE studio_profiles SET source_materialization_pending = 0;
COMMIT;
"#;

// M9 keeps the intended plan, its immutable dry-run manifest, and an actual export execution
// deliberately separate. The local destination path is catalog-private; manifest entries persist
// only FileInstance identity and a safe relative target, never a source absolute path.
const MIGRATION_019: &str = r#"
BEGIN;
CREATE TABLE production_project_revisions (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  source_revision INTEGER NOT NULL DEFAULT 0 CHECK (source_revision >= 0),
  updated_at TEXT NOT NULL
);
CREATE TABLE production_plans (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  plan_type TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('draft','ready','exporting','completed','partially_completed','blocked','failed','stale')),
  selection_rules_json TEXT NOT NULL,
  organization_json TEXT NOT NULL,
  filename_strategy_json TEXT NOT NULL,
  destination_path TEXT,
  destination_reserve_bytes INTEGER NOT NULL CHECK (destination_reserve_bytes >= 0),
  estimated_file_count INTEGER NOT NULL DEFAULT 0 CHECK (estimated_file_count >= 0),
  estimated_bytes INTEGER NOT NULL DEFAULT 0 CHECK (estimated_bytes >= 0),
  current_manifest_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE production_plan_versions (
  id TEXT PRIMARY KEY,
  production_plan_id TEXT NOT NULL REFERENCES production_plans(id) ON DELETE CASCADE,
  version INTEGER NOT NULL CHECK (version > 0),
  configuration_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(production_plan_id, version)
);
CREATE TABLE virtual_collections (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('static','dynamic')),
  rules_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, name)
);
CREATE TABLE virtual_collection_members (
  virtual_collection_id TEXT NOT NULL REFERENCES virtual_collections(id) ON DELETE CASCADE,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(virtual_collection_id, media_asset_id)
);
CREATE TABLE production_plan_overrides (
  production_plan_id TEXT NOT NULL REFERENCES production_plans(id) ON DELETE CASCADE,
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
  override_kind TEXT NOT NULL CHECK (override_kind IN ('force_include','force_exclude')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(production_plan_id, media_asset_id)
);
CREATE TABLE export_manifests (
  id TEXT PRIMARY KEY,
  production_plan_id TEXT NOT NULL REFERENCES production_plans(id) ON DELETE RESTRICT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  manifest_version INTEGER NOT NULL CHECK (manifest_version > 0),
  source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
  status TEXT NOT NULL CHECK (status IN ('ready','stale','blocked','superseded')),
  selection_snapshot_json TEXT NOT NULL,
  organization_snapshot_json TEXT NOT NULL,
  filename_strategy_snapshot_json TEXT NOT NULL,
  destination_path TEXT NOT NULL,
  selected_file_count INTEGER NOT NULL CHECK (selected_file_count >= 0),
  estimated_bytes INTEGER NOT NULL CHECK (estimated_bytes >= 0),
  checksum TEXT NOT NULL,
  validation_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(production_plan_id, manifest_version)
);
CREATE TABLE export_manifest_entries (
  id TEXT PRIMARY KEY,
  export_manifest_id TEXT NOT NULL REFERENCES export_manifests(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  media_asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
  selected_file_instance_id TEXT REFERENCES file_instances(id) ON DELETE RESTRICT,
  source_relative_path TEXT,
  original_filename TEXT NOT NULL,
  destination_relative_path TEXT NOT NULL,
  destination_filename TEXT NOT NULL,
  expected_byte_size INTEGER NOT NULL CHECK (expected_byte_size >= 0),
  source_checksum TEXT,
  human_decision TEXT,
  rating INTEGER NOT NULL CHECK (rating >= 0 AND rating <= 5),
  starred INTEGER NOT NULL CHECK (starred IN (0, 1)),
  moment_id TEXT,
  moment_label TEXT,
  status TEXT NOT NULL CHECK (status IN ('planned','blocked_source_unavailable','blocked_internal_collision')),
  issue TEXT,
  UNIQUE(export_manifest_id, ordinal),
  UNIQUE(export_manifest_id, destination_relative_path)
);
CREATE TABLE export_jobs (
  id TEXT PRIMARY KEY,
  production_plan_id TEXT NOT NULL REFERENCES production_plans(id) ON DELETE RESTRICT,
  export_manifest_id TEXT NOT NULL REFERENCES export_manifests(id) ON DELETE RESTRICT,
  background_job_id TEXT NOT NULL REFERENCES background_jobs(id) ON DELETE RESTRICT,
  state TEXT NOT NULL CHECK (state IN ('queued','running','paused','interrupted','completed','partially_completed','failed','cancelled')),
  destination_path TEXT NOT NULL,
  items_total INTEGER NOT NULL CHECK (items_total >= 0),
  items_completed INTEGER NOT NULL DEFAULT 0 CHECK (items_completed >= 0),
  verified_count INTEGER NOT NULL DEFAULT 0 CHECK (verified_count >= 0),
  skipped_identical_count INTEGER NOT NULL DEFAULT 0 CHECK (skipped_identical_count >= 0),
  failed_count INTEGER NOT NULL DEFAULT 0 CHECK (failed_count >= 0),
  verified_bytes INTEGER NOT NULL DEFAULT 0 CHECK (verified_bytes >= 0),
  error_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT,
  UNIQUE(background_job_id)
);
CREATE UNIQUE INDEX idx_export_jobs_active_manifest
  ON export_jobs(export_manifest_id)
  WHERE state IN ('queued','running','paused');
CREATE TABLE export_job_entries (
  export_job_id TEXT NOT NULL REFERENCES export_jobs(id) ON DELETE CASCADE,
  export_manifest_entry_id TEXT NOT NULL REFERENCES export_manifest_entries(id) ON DELETE RESTRICT,
  state TEXT NOT NULL CHECK (state IN ('pending','copying','copied','verifying','verified','skipped_identical','blocked','failed','cancelled')),
  copied_bytes INTEGER NOT NULL DEFAULT 0 CHECK (copied_bytes >= 0),
  source_checksum TEXT,
  destination_checksum TEXT,
  error_message TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(export_job_id, export_manifest_entry_id)
);
CREATE TABLE delivery_reports (
  id TEXT PRIMARY KEY,
  export_job_id TEXT NOT NULL REFERENCES export_jobs(id) ON DELETE CASCADE,
  manifest_checksum TEXT NOT NULL,
  report_json TEXT NOT NULL,
  report_text TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(export_job_id)
);
CREATE TABLE production_events (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  production_plan_id TEXT REFERENCES production_plans(id) ON DELETE SET NULL,
  export_manifest_id TEXT REFERENCES export_manifests(id) ON DELETE SET NULL,
  export_job_id TEXT REFERENCES export_jobs(id) ON DELETE SET NULL,
  event_type TEXT NOT NULL,
  details_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_production_plans_project ON production_plans(project_id, updated_at DESC);
CREATE INDEX idx_collection_project ON virtual_collections(project_id, updated_at DESC);
CREATE INDEX idx_manifest_project_plan ON export_manifests(project_id, production_plan_id, created_at DESC);
CREATE INDEX idx_manifest_entries_manifest ON export_manifest_entries(export_manifest_id, ordinal);
CREATE INDEX idx_export_jobs_project ON export_jobs(production_plan_id, updated_at DESC);
CREATE INDEX idx_export_job_entries_state ON export_job_entries(export_job_id, state);
CREATE INDEX idx_production_events_project ON production_events(project_id, created_at DESC);
CREATE TRIGGER production_project_revision_from_decision
AFTER INSERT ON decision_history
BEGIN
  INSERT INTO production_project_revisions (project_id, source_revision, updated_at)
  VALUES (NEW.project_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
  ON CONFLICT(project_id) DO UPDATE SET source_revision = source_revision + 1, updated_at = excluded.updated_at;
END;
CREATE TRIGGER production_project_revision_from_moment
AFTER INSERT ON moment_events
BEGIN
  INSERT INTO production_project_revisions (project_id, source_revision, updated_at)
  VALUES (NEW.project_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
  ON CONFLICT(project_id) DO UPDATE SET source_revision = source_revision + 1, updated_at = excluded.updated_at;
END;
CREATE TRIGGER production_stale_plans_after_source_change
AFTER UPDATE OF source_revision ON production_project_revisions
BEGIN
  UPDATE production_plans
  SET status = 'stale', updated_at = NEW.updated_at
  WHERE project_id = NEW.project_id
    AND current_manifest_id IS NOT NULL
    AND status <> 'exporting';
  UPDATE export_manifests
  SET status = 'stale'
  WHERE project_id = NEW.project_id
    AND source_revision < NEW.source_revision
    AND status = 'ready';
END;
COMMIT;
"#;

/// M9 follow-up: a first human authority event creates, rather than updates, the project
/// revision row. Preserve migration 019 for already-created M9 catalogs and make that path
/// stale historical revision-zero manifests safely.
const MIGRATION_020: &str = r#"
BEGIN;
UPDATE production_plans
SET status = 'stale', updated_at = COALESCE(
  (SELECT revision.updated_at FROM production_project_revisions revision WHERE revision.project_id = production_plans.project_id),
  updated_at
)
WHERE current_manifest_id IN (
  SELECT manifest.id
  FROM export_manifests manifest
  JOIN production_project_revisions revision ON revision.project_id = manifest.project_id
  WHERE manifest.production_plan_id = production_plans.id
    AND manifest.source_revision < revision.source_revision
)
  AND status <> 'exporting';
UPDATE export_manifests
SET status = 'stale'
WHERE status = 'ready'
  AND EXISTS (
    SELECT 1 FROM production_project_revisions revision
    WHERE revision.project_id = export_manifests.project_id
      AND export_manifests.source_revision < revision.source_revision
  );
CREATE TRIGGER production_stale_plans_after_initial_source_change
AFTER INSERT ON production_project_revisions
WHEN NEW.source_revision > 0
BEGIN
  UPDATE production_plans
  SET status = 'stale', updated_at = NEW.updated_at
  WHERE project_id = NEW.project_id
    AND current_manifest_id IS NOT NULL
    AND status <> 'exporting';
  UPDATE export_manifests
  SET status = 'stale'
  WHERE project_id = NEW.project_id
    AND source_revision < NEW.source_revision
    AND status = 'ready';
END;
COMMIT;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use capture_graph::EntityKind;
    use chrono::TimeZone;
    use std::time::Instant;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn id<T>(value: u128, make: impl FnOnce(Uuid) -> T) -> T {
        make(Uuid::from_u128(value))
    }
    fn now() -> Timestamp {
        Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
    }
    fn project() -> Project {
        Project {
            id: id(1, ProjectId::from_uuid),
            name: "Golden Wedding".into(),
            created_at: now(),
        }
    }
    fn volume() -> StorageVolume {
        StorageVolume {
            id: id(2, StorageVolumeId::from_uuid),
            display_name: "Master SSD".into(),
            filesystem_identity: Some("fixture-master".into()),
            capacity_bytes: Some(2_000_000),
            mount_state: MountState::Online,
            mount_location: Some("/fixture/master".into()),
            last_seen_at: Some(now()),
            filesystem_type: Some("APFS".into()),
            classification: StorageClassification::Removable,
        }
    }
    fn asset(project_id: ProjectId) -> MediaAsset {
        MediaAsset {
            id: id(3, MediaAssetId::from_uuid),
            project_id,
            media_type: MediaType::RawPhoto,
            display_name: "IMG_0001.ARW".into(),
            extension: Some("arw".into()),
            captured_at: Some(now()),
            fingerprint: MediaFingerprint {
                fast_fingerprint: Some("fast-1".into()),
                byte_size: Some(10),
                ..Default::default()
            },
            created_at: now(),
        }
    }

    fn culling_asset(
        repository: &SqliteRepository,
        project_id: &ProjectId,
        value: u128,
    ) -> MediaAsset {
        let asset = MediaAsset {
            id: id(value, MediaAssetId::from_uuid),
            project_id: project_id.clone(),
            media_type: MediaType::Jpeg,
            display_name: format!("DSC{value:05}.JPG"),
            extension: Some("jpg".into()),
            captured_at: Some(now()),
            fingerprint: MediaFingerprint {
                fast_fingerprint: Some(format!("fast-{value}")),
                byte_size: Some(10),
                ..Default::default()
            },
            created_at: now(),
        };
        repository.insert_media_asset(&asset).unwrap();
        repository
            .insert_file_instance(&FileInstance {
                id: id(value + 100_000, FileInstanceId::from_uuid),
                media_asset_id: asset.id.clone(),
                storage_volume_id: id(2, StorageVolumeId::from_uuid),
                index_root_id: None,
                relative_path: format!("RAW/{}", asset.display_name),
                observed_at: now(),
                is_available: true,
            })
            .unwrap();
        asset
    }

    fn metadata(asset_id: &MediaAssetId, file_instance_id: &FileInstanceId) -> MediaMetadataRecord {
        MediaMetadataRecord {
            media_asset_id: asset_id.to_string(),
            source_file_instance_id: file_instance_id.to_string(),
            source_fingerprint: "fast-1".into(),
            extractor: "test-provider".into(),
            extractor_version: "1".into(),
            status: "ready".into(),
            failure_reason: None,
            extracted_at: timestamp(&now()),
            mime_type: Some("image/jpeg".into()),
            byte_size: Some(10),
            captured_at_raw: Some("2024-01-01 10:00:00".into()),
            captured_at_local: Some(timestamp(&now())),
            capture_timezone: None,
            capture_time_source: Some("exif".into()),
            capture_time_confidence: Some("high".into()),
            width: Some(4000),
            height: Some(3000),
            orientation: Some("1".into()),
            camera_make: Some("Sony".into()),
            camera_model: Some("A7 IV".into()),
            lens_make: None,
            lens_model: Some("FE 85mm F1.8".into()),
            focal_length_mm: Some(85.0),
            focal_length_equivalent_mm: None,
            aperture: Some(1.8),
            shutter_speed: Some("1/320".into()),
            iso: Some(800),
            exposure_compensation: None,
            flash: None,
            white_balance: None,
            color_space: Some("sRGB".into()),
            gps_present: Some(false),
            duration_ms: None,
            frame_rate: None,
            codec: None,
            pixel_format: None,
            bitrate: None,
            audio_streams: None,
            video_streams: None,
            sample_rate: None,
            bit_depth: None,
            channels: None,
            raw_metadata: serde_json::json!({ "EXIF": { "Make": "Sony" } }),
        }
    }

    fn unavailable_face_provider_config() -> FaceAnalysisProviderConfig {
        FaceAnalysisProviderConfig::new(
            "none",
            "no-approved-local-provider.v1",
            "m4.face-analysis.v1",
        )
    }

    fn intelligence_artifact(
        asset_id: &MediaAssetId,
        artifact_id: u128,
        input_fingerprint: &str,
    ) -> AnalysisArtifact {
        AnalysisArtifact {
            id: id(artifact_id, AnalysisArtifactId::from_uuid),
            media_asset_id: asset_id.clone(),
            artifact_type: "capture_intelligence".into(),
            payload: serde_json::json!({ "test": true }),
            provenance: Provenance {
                source: "persistence-test".into(),
                algorithm_id: Some("deterministic-test".into()),
                algorithm_version: Some("v1".into()),
                produced_at: now(),
                human_confirmed: false,
            },
            provider: Some("deterministic-test".into()),
            provider_version: Some("v1".into()),
            model_version: None,
            settings_version: Some("analysis-settings-v1".into()),
            input_fingerprint: Some(input_fingerprint.into()),
            generated_at: Some(now()),
            confidence: Some(0.8),
            status: Some(AnalysisStatus::Ready),
            error_message: None,
        }
    }

    fn visual_fingerprint(asset_id: &MediaAssetId, input_fingerprint: &str) -> VisualFingerprint {
        VisualFingerprint {
            media_asset_id: asset_id.clone(),
            input_fingerprint: input_fingerprint.into(),
            provider: "deterministic-test".into(),
            provider_version: "v1".into(),
            perceptual_hash: "0123456789abcdef".into(),
            difference_hash: "fedcba9876543210".into(),
            color_signature: vec![10, 20, 30],
            embedding: vec![1, -2, 3, -4],
            embedding_dimensions: 4,
            bucket_keys: vec!["bucket-a".into()],
            generated_at: now(),
            status: AnalysisStatus::Ready,
            error_message: None,
        }
    }

    fn technical_evidence(
        asset_id: &MediaAssetId,
        input_fingerprint: &str,
    ) -> TechnicalQualityEvidence {
        TechnicalQualityEvidence {
            media_asset_id: asset_id.clone(),
            input_fingerprint: input_fingerprint.into(),
            provider: "deterministic-test".into(),
            provider_version: "v1".into(),
            global_sharpness: Some(42.0),
            sharpness_band: "moderate".into(),
            directional_blur_ratio: Some(1.0),
            blur_level: BlurEvidenceLevel::Low,
            mean_luminance: Some(0.5),
            median_luminance: Some(0.5),
            highlight_clipping_percent: Some(0.0),
            shadow_clipping_percent: Some(0.0),
            channel_clipping_percent: Some(0.0),
            technical_quality_score: Some(75.0),
            technical_quality_band: TechnicalQualityBand::Good,
            confidence: 0.8,
            generated_at: now(),
            status: AnalysisStatus::Ready,
            error_message: None,
        }
    }

    #[test]
    fn migration_is_idempotent_and_tracks_version() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        repository.migrate().unwrap();
        assert_eq!(repository.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn production_manifest_refuses_a_snapshot_when_a_human_decision_arrives_concurrently() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        let asset = culling_asset(&repository, &project.id, 8_001);
        repository
            .update_culling_decision(
                &project.id,
                &asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Concurrent client delivery".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules::client_delivery(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        let (_, _, _, _, revision_before_human_change, _) = repository
            .production_manifest_build_input(&project.id, &plan.id)
            .unwrap();
        repository
            .update_culling_decision(
                &project.id,
                &asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Review),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        let error = repository
            .create_export_manifest(
                &plan,
                revision_before_human_change,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "fixture-checksum",
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("changed while this manifest was being prepared"));
        assert_eq!(
            repository.production_workspace(&project.id).unwrap().plans[0].status,
            ProductionPlanStatus::Draft
        );
        let decision = repository.culling_progress(&project.id).unwrap();
        assert_eq!(
            decision.review, 1,
            "the newer explicit human decision remains authoritative"
        );
    }

    #[test]
    fn production_manifest_refuses_a_snapshot_when_plan_configuration_changes_concurrently() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Configuration race".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules::client_delivery(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        let (_, _, _, _, revision, _) = repository
            .production_manifest_build_input(&project.id, &plan.id)
            .unwrap();
        repository
            .update_production_plan_configuration(
                &project.id,
                &plan.id,
                &ProductionPlanInput {
                    name: "Configuration race".into(),
                    plan_type: ProductionPlanType::EditorWorkset,
                    selection_rules: SelectionRules::editor_workset(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();

        let error = repository
            .create_export_manifest(
                &plan,
                revision,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "fixture-checksum",
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("configuration changed while this manifest was being prepared"));
        let current = repository
            .production_plan(&project.id, &plan.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.plan_type, ProductionPlanType::EditorWorkset);
        assert_eq!(current.selection_rules, SelectionRules::editor_workset());
        assert!(current.current_manifest_id.is_none());
    }

    #[test]
    fn first_human_source_event_after_a_revision_zero_manifest_marks_it_stale() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let asset = culling_asset(&repository, &project.id, 8_050);
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Revision zero client delivery".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules::client_delivery(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        let manifest = repository
            .create_export_manifest(
                &plan,
                0,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "revision-zero-checksum",
            )
            .unwrap();
        repository
            .update_culling_decision(
                &project.id,
                &asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        assert_eq!(
            repository.production_workspace(&project.id).unwrap().plans[0].status,
            ProductionPlanStatus::Stale
        );
        assert_eq!(
            repository
                .export_manifest(&project.id, &manifest.id)
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
    }

    #[test]
    fn plan_edit_marks_its_prior_current_manifest_stale_before_replacing_intent() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Mutable delivery plan".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules::client_delivery(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        let manifest = repository
            .create_export_manifest(
                &plan,
                0,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "prior-manifest-checksum",
            )
            .unwrap();
        let updated = repository
            .update_production_plan_destination(
                &project.id,
                &plan.id,
                Some("/fixture/new-destination"),
            )
            .unwrap();
        assert_eq!(updated.status, ProductionPlanStatus::Draft);
        assert_eq!(updated.current_manifest_id, None);
        assert_eq!(
            repository
                .export_manifest(&project.id, &manifest.id)
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
    }

    #[test]
    fn plan_safety_reserve_is_configurable_and_stales_the_prior_manifest() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Reserve-aware delivery".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules::client_delivery(),
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        let manifest = repository
            .create_export_manifest(
                &plan,
                0,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "reserve-checksum",
            )
            .unwrap();
        let reserve = delivery_brain::MIN_DESTINATION_RESERVE_BYTES * 2;
        let updated = repository
            .update_production_plan_destination_reserve(&project.id, &plan.id, reserve)
            .unwrap();
        assert_eq!(updated.destination_reserve_bytes, reserve);
        assert_eq!(updated.status, ProductionPlanStatus::Draft);
        assert!(updated.current_manifest_id.is_none());
        assert_eq!(
            repository
                .export_manifest(&project.id, &manifest.id)
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
        assert!(repository
            .update_production_plan_destination_reserve(
                &project.id,
                &plan.id,
                delivery_brain::MIN_DESTINATION_RESERVE_BYTES - 1,
            )
            .is_err());
    }

    #[test]
    fn virtual_collections_and_plan_overrides_are_scoped_references_not_culling_mutations() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let first = culling_asset(&repository, &project.id, 8_101);
        let second = culling_asset(&repository, &project.id, 8_102);
        repository
            .update_culling_decision(
                &project.id,
                &first.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: Some(5),
                    starred: Some(true),
                    note: Some("Must remain private".into()),
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        let static_collection = repository
            .create_virtual_collection(
                &project.id,
                &VirtualCollectionInput {
                    name: "Manual client choices".into(),
                    kind: VirtualCollectionKind::Static,
                    rules: SelectionRules::default(),
                },
            )
            .unwrap();
        repository
            .set_static_virtual_collection_members(
                &project.id,
                &static_collection.id,
                std::slice::from_ref(&second.id),
            )
            .unwrap();
        assert_eq!(
            repository
                .virtual_collection_assets(&project.id, &static_collection.id)
                .unwrap(),
            vec![second.id.to_string()]
        );
        repository
            .set_static_virtual_collection_member(
                &project.id,
                &static_collection.id,
                &first.id,
                true,
            )
            .unwrap();
        assert_eq!(
            repository
                .virtual_collection_assets(&project.id, &static_collection.id)
                .unwrap(),
            vec![first.id.to_string(), second.id.to_string()]
        );
        repository
            .set_static_virtual_collection_member(
                &project.id,
                &static_collection.id,
                &first.id,
                false,
            )
            .unwrap();
        let dynamic_collection = repository
            .create_virtual_collection(
                &project.id,
                &VirtualCollectionInput {
                    name: "Human keeps".into(),
                    kind: VirtualCollectionKind::Dynamic,
                    rules: SelectionRules::client_delivery(),
                },
            )
            .unwrap();
        assert_eq!(
            repository
                .virtual_collection_assets(&project.id, &dynamic_collection.id)
                .unwrap(),
            vec![first.id.to_string()]
        );
        let plan = repository
            .create_production_plan(
                &project.id,
                &ProductionPlanInput {
                    name: "Client plan".into(),
                    plan_type: ProductionPlanType::ClientDelivery,
                    selection_rules: SelectionRules {
                        virtual_collection_id: Some(static_collection.id.clone()),
                        ..SelectionRules::client_delivery()
                    },
                    organization: OrganizationStrategy::SingleFolder,
                    filename_strategy: FilenameStrategy::PreserveOriginal,
                },
            )
            .unwrap();
        repository
            .set_production_plan_override(
                &project.id,
                &plan.id,
                &first.id,
                Some(PlanOverrideKind::ForceExclude),
            )
            .unwrap();
        let (_, overrides, _, _, _, collection_asset_ids) = repository
            .production_manifest_build_input(&project.id, &plan.id)
            .unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].kind, PlanOverrideKind::ForceExclude);
        assert_eq!(collection_asset_ids, Some(vec![second.id.to_string()]));
        let (_, _, _, _, source_revision, _) = repository
            .production_manifest_build_input(&project.id, &plan.id)
            .unwrap();
        let manifest = repository
            .create_export_manifest(
                &plan,
                source_revision,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "collection-staleness-checksum",
            )
            .unwrap();
        repository
            .set_static_virtual_collection_member(
                &project.id,
                &static_collection.id,
                &second.id,
                false,
            )
            .unwrap();
        let error = repository
            .create_export_manifest(
                &plan,
                source_revision,
                "/fixture/destination",
                &serde_json::json!({}),
                &[],
                "stale-collection-snapshot",
            )
            .unwrap_err();
        assert!(error.to_string().contains(
            "Production selection sources changed while this manifest was being prepared"
        ));
        assert_eq!(
            repository.production_workspace(&project.id).unwrap().plans[0].status,
            ProductionPlanStatus::Stale
        );
        assert_eq!(
            repository
                .export_manifest(&project.id, &manifest.id)
                .unwrap()
                .unwrap()
                .status,
            "stale"
        );
        let current = repository.culling_progress(&project.id).unwrap();
        assert_eq!(current.keep, 1);
        assert_eq!(current.review, 0);
        let current_decision: String = repository.connection.query_row(
            "SELECT decision FROM media_decisions WHERE project_id = ?1 AND media_asset_id = ?2",
            params![project.id.to_string(), first.id.to_string()],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(current_decision, "keep");
    }

    #[test]
    fn m1_volume_repair_preserves_roots_and_replaces_folder_derived_metadata() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection.execute_batch(MIGRATION_001).unwrap();
        connection.execute_batch(MIGRATION_002).unwrap();
        connection
            .pragma_update(None, "user_version", 2_i64)
            .unwrap();
        let repository = SqliteRepository { connection };
        let project = project();
        let volume = StorageVolume {
            display_name: "mixed-photo-video-audio".into(),
            mount_location: Some("/fixtures/index/mixed-photo-video-audio".into()),
            ..volume()
        };
        let root = IndexRoot {
            id: id(80, IndexRootId::from_uuid),
            project_id: project.id.clone(),
            selected_path: "/fixtures/index/mixed-photo-video-audio".into(),
            storage_volume_id: volume.id.clone(),
            added_at: now(),
            last_indexed_at: Some(now()),
            status: IndexRootStatus::Ready,
        };
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.register_index_root(&root).unwrap();

        repository.migrate().unwrap();

        let repaired = repository.get_storage_volume(&volume.id).unwrap().unwrap();
        assert_eq!(repaired.display_name, "Volume (fixture-master)");
        assert_eq!(repaired.mount_location, None);
        assert_eq!(
            repository.index_roots_for_project(&project.id).unwrap(),
            vec![root]
        );
        assert_eq!(repository.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn persists_asset_and_multiple_file_instances_separately() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        let backup = StorageVolume {
            id: id(4, StorageVolumeId::from_uuid),
            display_name: "Backup HDD".into(),
            mount_state: MountState::Offline,
            filesystem_identity: None,
            capacity_bytes: None,
            mount_location: None,
            last_seen_at: Some(now()),
            filesystem_type: None,
            classification: StorageClassification::Removable,
        };
        let asset = asset(project.id.clone());
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.insert_storage_volume(&backup).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        for (number, volume_id, path) in [
            (5, volume.id.clone(), "RAW/IMG_0001.ARW"),
            (6, backup.id.clone(), "Golden/IMG_0001.ARW"),
        ] {
            repository
                .insert_file_instance(&FileInstance {
                    id: id(number, FileInstanceId::from_uuid),
                    media_asset_id: asset.id.clone(),
                    storage_volume_id: volume_id,
                    index_root_id: None,
                    relative_path: path.into(),
                    observed_at: now(),
                    is_available: true,
                })
                .unwrap();
        }
        assert_eq!(
            repository
                .file_instances_for_asset(&asset.id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(repository.counts().unwrap().media_assets, 1);
    }

    #[test]
    fn volume_can_be_offline_without_losing_identity() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let volume = volume();
        repository.insert_storage_volume(&volume).unwrap();
        repository
            .update_volume_state(&volume.id, MountState::Offline, None)
            .unwrap();
        let stored = repository.get_storage_volume(&volume.id).unwrap().unwrap();
        assert_eq!(stored.mount_state, MountState::Offline);
        assert_eq!(stored.filesystem_identity, volume.filesystem_identity);
    }

    #[test]
    fn project_read_and_derivative_lineage_round_trip() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let source = asset(project.id.clone());
        let derived = MediaAsset {
            id: id(16, MediaAssetId::from_uuid),
            display_name: "Final_0001.jpg".into(),
            media_type: MediaType::Jpeg,
            ..asset(project.id.clone())
        };
        let lineage = Derivative {
            id: id(17, DerivativeId::from_uuid),
            source_asset_id: source.id.clone(),
            derived_asset_id: derived.id.clone(),
            recipe_hint: Some("fixture export".into()),
            created_at: now(),
        };
        repository.insert_project(&project).unwrap();
        repository.insert_media_asset(&source).unwrap();
        repository.insert_media_asset(&derived).unwrap();
        repository.insert_derivative(&lineage).unwrap();
        assert_eq!(repository.get_project(&project.id).unwrap(), Some(project));
        assert_eq!(
            repository.derivatives_from(&source.id).unwrap(),
            vec![lineage]
        );
    }

    #[test]
    fn project_library_uses_stable_ids_and_preserves_project_scoped_counts_after_reopen() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("captureos.sqlite3");
        let (first_project_id, second_project_id) = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let first = repository.create_project("Test Wedding").unwrap();
            let second = repository.create_project("Test Wedding").unwrap();
            assert_ne!(first.id, second.id);

            let volume = volume();
            let root = IndexRoot {
                id: id(81, IndexRootId::from_uuid),
                project_id: first.id.clone(),
                selected_path: "/fixture/test-wedding".into(),
                storage_volume_id: volume.id.clone(),
                added_at: now(),
                last_indexed_at: Some(now()),
                status: IndexRootStatus::Ready,
            };
            let first_asset = MediaAsset {
                id: id(82, MediaAssetId::from_uuid),
                project_id: first.id.clone(),
                ..asset(first.id.clone())
            };
            repository.insert_storage_volume(&volume).unwrap();
            repository.register_index_root(&root).unwrap();
            repository.insert_media_asset(&first_asset).unwrap();
            repository
                .insert_file_instance(&FileInstance {
                    id: id(83, FileInstanceId::from_uuid),
                    media_asset_id: first_asset.id,
                    storage_volume_id: volume.id,
                    index_root_id: Some(root.id),
                    relative_path: "RAW/IMG_0001.ARW".into(),
                    observed_at: now(),
                    is_available: true,
                })
                .unwrap();

            let library = repository.project_library().unwrap();
            assert_eq!(library.len(), 2);
            let first_card = library
                .iter()
                .find(|item| item.id == first.id.to_string())
                .unwrap();
            assert_eq!(first_card.name, "Test Wedding");
            assert_eq!(first_card.media_asset_count, 1);
            assert_eq!(first_card.storage_volume_count, 1);
            assert_eq!(first_card.protection_state, "not_recorded");
            let second_card = library
                .iter()
                .find(|item| item.id == second.id.to_string())
                .unwrap();
            assert_eq!(second_card.name, "Test Wedding");
            assert_eq!(second_card.media_asset_count, 0);
            assert_eq!(second_card.storage_volume_count, 0);
            (first.id, second.id)
        };

        let reopened = SqliteRepository::open(&catalog).unwrap();
        let library = reopened.project_library().unwrap();
        assert_eq!(library.len(), 2);
        assert!(library.iter().any(|item| {
            item.id == first_project_id.to_string()
                && item.name == "Test Wedding"
                && item.media_asset_count == 1
                && item.storage_volume_count == 1
        }));
        assert!(library.iter().any(|item| {
            item.id == second_project_id.to_string()
                && item.name == "Test Wedding"
                && item.media_asset_count == 0
                && item.storage_volume_count == 0
        }));
    }

    #[test]
    fn foreign_keys_reject_orphan_file_instances() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let result = repository.insert_file_instance(&FileInstance {
            id: id(9, FileInstanceId::from_uuid),
            media_asset_id: id(10, MediaAssetId::from_uuid),
            storage_volume_id: id(11, StorageVolumeId::from_uuid),
            index_root_id: None,
            relative_path: "missing/file".into(),
            observed_at: now(),
            is_available: false,
        });
        assert!(matches!(result, Err(PersistenceError::Database(_))));
    }

    #[test]
    fn relationship_correction_and_job_round_trip() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let relationship = Relationship {
            id: id(12, RelationshipId::from_uuid),
            source: EntityRef {
                kind: EntityKind::MediaAsset,
                id: "asset".into(),
            },
            target: EntityRef {
                kind: EntityKind::Moment,
                id: "moment".into(),
            },
            kind: RelationshipKind::BelongsToMoment,
            confidence: Some(0.87),
            provenance: Provenance {
                source: "ai".into(),
                algorithm_id: Some("test".into()),
                algorithm_version: Some("1".into()),
                produced_at: now(),
                human_confirmed: false,
            },
            created_at: now(),
        };
        repository.insert_relationship(&relationship).unwrap();
        assert_eq!(repository.relationships().unwrap()[0], relationship);
        let correction = Correction {
            id: id(13, CorrectionId::from_uuid),
            subject_kind: "relationship".into(),
            subject_id: relationship.id.to_string(),
            field_name: "moment".into(),
            previous_value: Some(serde_json::json!("ceremony")),
            corrected_value: serde_json::json!("first_dance"),
            corrected_at: now(),
            author_hint: Some("fixture user".into()),
        };
        repository.insert_correction(&correction).unwrap();
        assert_eq!(
            repository
                .corrections_for("relationship", &relationship.id.to_string())
                .unwrap(),
            vec![correction]
        );
        let job = BackgroundJob {
            id: id(14, JobId::from_uuid),
            state: WorkflowRunState::Paused,
            stage: JobStage::Fingerprint,
            items_completed: 7,
            items_total: Some(12),
            files_discovered: 12,
            files_processed: 7,
            error_count: 0,
            project_id: None,
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({"cursor": 7})),
            created_at: now(),
            updated_at: now(),
            finished_at: None,
        };
        repository.insert_background_job(&job).unwrap();
        assert_eq!(repository.get_background_job(&job.id).unwrap(), Some(job));
    }

    #[test]
    fn running_ingest_is_marked_interrupted_and_audited_on_recovery() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let job = IngestJob {
            id: id(71, IngestJobId::from_uuid),
            project_id: project.id,
            start_request_id: Some("00000000-0000-0000-0000-000000000071".into()),
            state: IngestJobState::Running,
            policy: IngestProtectionPolicy::Standard,
            guardian_state: CaptureGuardianState::Unprotected,
            safe_to_eject: false,
            created_at: now(),
            updated_at: now(),
            finished_at: None,
        };
        repository.insert_ingest_job(&job).unwrap();

        assert_eq!(repository.recover_interrupted_ingests().unwrap(), 1);
        assert_eq!(
            repository.ingest_job(&job.id).unwrap().unwrap().state,
            IngestJobState::Interrupted
        );
        assert!(repository
            .ingest_audit_events(&job.id)
            .unwrap()
            .iter()
            .any(|event| event.event_type == "JOB_INTERRUPTED"));
    }

    #[test]
    fn running_visual_preparation_is_marked_interrupted_on_recovery() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let job = BackgroundJob {
            id: id(72, JobId::from_uuid),
            state: WorkflowRunState::Running,
            stage: JobStage::Thumbnail,
            items_completed: 1,
            items_total: Some(3),
            files_discovered: 3,
            files_processed: 1,
            error_count: 0,
            project_id: Some(project.id),
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({"pipeline": "visual-preparation"})),
            created_at: now(),
            updated_at: now(),
            finished_at: None,
        };
        repository.insert_background_job(&job).unwrap();

        assert_eq!(
            repository
                .recover_interrupted_visual_preparations()
                .unwrap(),
            1
        );
        let recovered = repository.get_background_job(&job.id).unwrap().unwrap();
        assert_eq!(recovered.state, WorkflowRunState::Interrupted);
        assert_eq!(recovered.stage, JobStage::Finalize);
        assert!(recovered.finished_at.is_some());
        assert!(recovered.error_message.unwrap().contains("interrupted"));
    }

    #[test]
    fn visual_page_is_logical_paged_and_preserves_cached_offline_media() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        let root = repository
            .register_index_root(&IndexRoot {
                id: id(90, IndexRootId::from_uuid),
                project_id: project.id.clone(),
                selected_path: "/fixture/media".into(),
                storage_volume_id: volume.id.clone(),
                added_at: now(),
                last_indexed_at: Some(now()),
                status: IndexRootStatus::Ready,
            })
            .unwrap();
        let first = MediaAsset {
            media_type: MediaType::Jpeg,
            display_name: "IMG_0001.JPG".into(),
            extension: Some("jpg".into()),
            ..asset(project.id.clone())
        };
        let second = MediaAsset {
            id: id(91, MediaAssetId::from_uuid),
            media_type: MediaType::Video,
            display_name: "C0001.MOV".into(),
            extension: Some("mov".into()),
            fingerprint: MediaFingerprint {
                fast_fingerprint: Some("fast-2".into()),
                byte_size: Some(20),
                ..Default::default()
            },
            ..asset(project.id.clone())
        };
        repository.insert_media_asset(&first).unwrap();
        repository.insert_media_asset(&second).unwrap();
        let primary = FileInstance {
            id: id(92, FileInstanceId::from_uuid),
            media_asset_id: first.id.clone(),
            storage_volume_id: volume.id.clone(),
            index_root_id: Some(root.id.clone()),
            relative_path: "IMG_0001.JPG".into(),
            observed_at: now(),
            is_available: false,
        };
        let second_instance = FileInstance {
            id: id(93, FileInstanceId::from_uuid),
            media_asset_id: second.id.clone(),
            storage_volume_id: volume.id.clone(),
            index_root_id: Some(root.id),
            relative_path: "C0001.MOV".into(),
            observed_at: now(),
            is_available: true,
        };
        repository.insert_file_instance(&primary).unwrap();
        repository.insert_file_instance(&second_instance).unwrap();
        repository
            .upsert_media_metadata(&metadata(&first.id, &primary.id))
            .unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: "artifact-1".into(),
                media_asset_id: first.id.to_string(),
                source_file_instance_id: primary.id.to_string(),
                artifact_type: "thumbnail".into(),
                size_class: "small".into(),
                cache_relative_path: "m3.1/asset/thumb.jpg".into(),
                provider: "test".into(),
                generator_version: "1".into(),
                source_fingerprint: "fast-1".into(),
                created_at: timestamp(&now()),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        repository
            .upsert_media_metadata(&metadata(&second.id, &second_instance.id))
            .unwrap();
        let mut failed_small_poster = PreviewArtifactRecord {
            id: "poster-provider-a".into(),
            media_asset_id: second.id.to_string(),
            source_file_instance_id: second_instance.id.to_string(),
            artifact_type: "poster".into(),
            size_class: "small".into(),
            cache_relative_path: "m3.1/video/poster-small.png".into(),
            provider: "provider-a".into(),
            generator_version: "1".into(),
            source_fingerprint: "fast-2".into(),
            created_at: timestamp(&now()),
            status: "failed".into(),
            failure_reason: Some("first poster attempt failed".into()),
        };
        let mut ready_medium_poster = PreviewArtifactRecord {
            id: "poster-provider-b".into(),
            media_asset_id: second.id.to_string(),
            source_file_instance_id: second_instance.id.to_string(),
            artifact_type: "poster".into(),
            size_class: "medium".into(),
            cache_relative_path: "m3.1/video/poster-medium.png".into(),
            provider: "provider-b".into(),
            generator_version: "1".into(),
            source_fingerprint: "fast-2".into(),
            created_at: timestamp(&now()),
            status: "ready".into(),
            failure_reason: None,
        };
        repository
            .upsert_preview_artifact(&failed_small_poster)
            .unwrap();
        repository
            .upsert_preview_artifact(&ready_medium_poster)
            .unwrap();

        let page = repository
            .visual_media_page(
                &project.id,
                &VisualMediaQuery {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.has_more);
        let offline = repository
            .visual_media_page(
                &project.id,
                &VisualMediaQuery {
                    filter: VisualMediaFilter::Offline,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(offline.items.len(), 1);
        assert_eq!(offline.items[0].asset_id, first.id.to_string());
        assert_eq!(
            offline.items[0].thumbnail_preview_url.as_deref(),
            Some("artifact-1")
        );
        assert_eq!(
            repository.preview_render_artifact("artifact-1").unwrap(),
            Some(PreviewRenderArtifact {
                id: "artifact-1".into(),
                cache_relative_path: "m3.1/asset/thumb.jpg".into(),
            })
        );
        assert!(repository
            .preview_render_artifact("not-a-registered-artifact")
            .unwrap()
            .is_none());
        let detail = repository.media_asset_detail(&first.id).unwrap().unwrap();
        assert_eq!(detail.copies.len(), 1);
        assert_eq!(
            detail.metadata.unwrap().camera_model.as_deref(),
            Some("A7 IV")
        );

        let video_query = VisualMediaQuery {
            filter: VisualMediaFilter::Video,
            ..Default::default()
        };
        let ready_video = repository
            .visual_media_page(&project.id, &video_query)
            .unwrap()
            .items
            .pop()
            .unwrap();
        assert_eq!(ready_video.preview_status, "ready");
        assert_eq!(ready_video.preview_failure_reason, None);
        assert_eq!(
            ready_video.medium_preview_url.as_deref(),
            Some("poster-provider-b")
        );
        let ready_video_detail = repository.media_asset_detail(&second.id).unwrap().unwrap();
        assert_eq!(ready_video_detail.item.preview_status, "ready");
        assert_eq!(ready_video_detail.item.preview_failure_reason, None);

        // With no usable rendition left, real failures remain visible rather than being hidden.
        ready_medium_poster.status = "failed".into();
        ready_medium_poster.failure_reason = Some("all poster providers failed".into());
        repository
            .upsert_preview_artifact(&ready_medium_poster)
            .unwrap();
        assert_eq!(
            repository
                .visual_media_page(&project.id, &video_query)
                .unwrap()
                .items[0]
                .preview_status,
            "failed"
        );

        failed_small_poster.status = "corrupt".into();
        failed_small_poster.failure_reason = Some("Invalid video container signature".into());
        ready_medium_poster.status = "corrupt".into();
        ready_medium_poster.failure_reason = Some("Invalid video container signature".into());
        repository
            .upsert_preview_artifact(&failed_small_poster)
            .unwrap();
        repository
            .upsert_preview_artifact(&ready_medium_poster)
            .unwrap();
        assert_eq!(
            repository
                .visual_media_page(&project.id, &video_query)
                .unwrap()
                .items[0]
                .preview_status,
            "corrupt"
        );

        failed_small_poster.status = "unsupported".into();
        ready_medium_poster.status = "unsupported".into();
        repository
            .upsert_preview_artifact(&failed_small_poster)
            .unwrap();
        repository
            .upsert_preview_artifact(&ready_medium_poster)
            .unwrap();
        assert_eq!(
            repository
                .visual_media_page(&project.id, &video_query)
                .unwrap()
                .items[0]
                .preview_status,
            "unsupported"
        );
    }

    #[test]
    fn visual_pagination_does_not_load_a_large_catalog() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        for number in 0..2_000_u128 {
            let asset = MediaAsset {
                id: id(10_000 + number, MediaAssetId::from_uuid),
                project_id: project.id.clone(),
                media_type: if (number & 1) == 0 {
                    MediaType::Jpeg
                } else {
                    MediaType::Video
                },
                display_name: format!("IMG_{number:05}.jpg"),
                extension: Some("jpg".into()),
                captured_at: None,
                fingerprint: MediaFingerprint {
                    fast_fingerprint: Some(format!("fast-{number}")),
                    byte_size: Some(number as u64 + 1),
                    ..Default::default()
                },
                created_at: now(),
            };
            repository.insert_media_asset(&asset).unwrap();
            repository
                .insert_file_instance(&FileInstance {
                    id: id(20_000 + number, FileInstanceId::from_uuid),
                    media_asset_id: asset.id,
                    storage_volume_id: volume.id.clone(),
                    index_root_id: None,
                    relative_path: format!("IMG_{number:05}.jpg"),
                    observed_at: now(),
                    is_available: true,
                })
                .unwrap();
        }
        let page = repository
            .visual_media_page(
                &project.id,
                &VisualMediaQuery {
                    limit: 75,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(page.items.len(), 75);
        assert!(page.has_more);
        let photos = repository
            .visual_media_page(
                &project.id,
                &VisualMediaQuery {
                    filter: VisualMediaFilter::Photos,
                    limit: 75,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(photos.items.iter().all(|item| item.media_type == "jpeg"));
    }

    #[test]
    fn associates_only_an_unambiguous_xmp_sidecar_in_the_same_root() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        let root = repository
            .register_index_root(&IndexRoot {
                id: id(30_000, IndexRootId::from_uuid),
                project_id: project.id.clone(),
                selected_path: "/fixture/root".into(),
                storage_volume_id: volume.id.clone(),
                added_at: now(),
                last_indexed_at: Some(now()),
                status: IndexRootStatus::Ready,
            })
            .unwrap();
        let photo = MediaAsset {
            media_type: MediaType::RawPhoto,
            display_name: "DSC_1001.ARW".into(),
            extension: Some("arw".into()),
            ..asset(project.id.clone())
        };
        let sidecar = MediaAsset {
            id: id(30_001, MediaAssetId::from_uuid),
            media_type: MediaType::Sidecar,
            display_name: "DSC_1001.XMP".into(),
            extension: Some("xmp".into()),
            fingerprint: MediaFingerprint {
                fast_fingerprint: Some("sidecar".into()),
                byte_size: Some(4),
                ..Default::default()
            },
            ..asset(project.id.clone())
        };
        repository.insert_media_asset(&photo).unwrap();
        repository.insert_media_asset(&sidecar).unwrap();
        repository
            .insert_file_instance(&FileInstance {
                id: id(30_002, FileInstanceId::from_uuid),
                media_asset_id: photo.id.clone(),
                storage_volume_id: volume.id.clone(),
                index_root_id: Some(root.id.clone()),
                relative_path: "DCIM/DSC_1001.ARW".into(),
                observed_at: now(),
                is_available: true,
            })
            .unwrap();
        repository
            .insert_file_instance(&FileInstance {
                id: id(30_003, FileInstanceId::from_uuid),
                media_asset_id: sidecar.id.clone(),
                storage_volume_id: volume.id.clone(),
                index_root_id: Some(root.id.clone()),
                relative_path: "DCIM/DSC_1001.XMP".into(),
                observed_at: now(),
                is_available: true,
            })
            .unwrap();
        assert_eq!(repository.associate_sidecars_for_root(&root.id).unwrap(), 1);
        assert_eq!(repository.associate_sidecars_for_root(&root.id).unwrap(), 0);
        assert!(repository
            .relationships()
            .unwrap()
            .iter()
            .any(
                |relationship| relationship.kind == RelationshipKind::SidecarOf
                    && relationship.source.id == sidecar.id.to_string()
                    && relationship.target.id == photo.id.to_string()
            ));
    }

    #[test]
    fn m3_migration_preserves_an_existing_m2_catalog() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection.execute_batch(MIGRATION_001).unwrap();
        connection.execute_batch(MIGRATION_002).unwrap();
        connection.execute_batch(MIGRATION_003).unwrap();
        connection.execute_batch(MIGRATION_004).unwrap();
        connection.execute_batch(MIGRATION_005).unwrap();
        connection
            .pragma_update(None, "user_version", 5_i64)
            .unwrap();
        let repository = SqliteRepository { connection };
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.migrate().unwrap();
        assert_eq!(repository.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(repository.get_project(&project.id).unwrap(), Some(project));
        assert!(repository
            .connection
            .query_row("SELECT COUNT(*) FROM media_metadata", [], |row| row
                .get::<_, i64>(0))
            .is_ok());
    }

    #[test]
    fn m4_migration_preserves_v7_projects_and_legacy_analysis_artifacts() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        for migration in [
            MIGRATION_001,
            MIGRATION_002,
            MIGRATION_003,
            MIGRATION_004,
            MIGRATION_005,
            MIGRATION_006,
            MIGRATION_007,
        ] {
            connection.execute_batch(migration).unwrap();
        }
        connection
            .pragma_update(None, "user_version", 7_i64)
            .unwrap();
        let repository = SqliteRepository { connection };
        let project = project();
        let volume = volume();
        let asset = asset(project.id.clone());
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository.connection.execute(
            "INSERT INTO analysis_artifacts (id, media_asset_id, artifact_type, payload_json, provenance_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id(90, AnalysisArtifactId::from_uuid).to_string(), asset.id.to_string(), "legacy",
                "{}", serde_json::to_string(&Provenance { source: "legacy".into(), algorithm_id: None, algorithm_version: None, produced_at: now(), human_confirmed: false }).unwrap(),
            ],
        ).unwrap();

        repository.migrate().unwrap();
        assert_eq!(repository.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(repository.get_project(&project.id).unwrap(), Some(project));
        let preserved: (String, Option<String>, Option<String>) = repository.connection.query_row(
            "SELECT artifact_type, provider, status FROM analysis_artifacts WHERE media_asset_id = ?1",
            params![asset.id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(preserved, ("legacy".into(), None, None));
        assert!(repository
            .connection
            .query_row("SELECT COUNT(*) FROM visual_fingerprints", [], |row| row
                .get::<_, i64>(0))
            .is_ok());
        assert!(repository
            .connection
            .query_row("SELECT COUNT(*) FROM similarity_groups", [], |row| row
                .get::<_, i64>(0))
            .is_ok());
    }

    #[test]
    fn evidence_history_migration_preserves_v8_rows_and_decision_foreign_keys() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        for migration in [
            MIGRATION_001,
            MIGRATION_002,
            MIGRATION_003,
            MIGRATION_004,
            MIGRATION_005,
            MIGRATION_006,
            MIGRATION_007,
            MIGRATION_008,
        ] {
            connection.execute_batch(migration).unwrap();
        }
        connection
            .pragma_update(None, "user_version", 8_i64)
            .unwrap();
        let repository = SqliteRepository { connection };
        let project = project();
        let asset = asset(project.id.clone());
        let recommendation_id = id(280, RecommendationId::from_uuid).to_string();
        repository.insert_project(&project).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository.connection.execute(
            "INSERT INTO visual_fingerprints (media_asset_id, input_fingerprint, provider, provider_version, perceptual_hash, difference_hash, color_signature, bucket_keys_json, generated_at, status, error_message)
             VALUES (?1, 'legacy-input', 'legacy-provider', '1', 'aa', 'bb', ?2, '[]', ?3, 'ready', NULL)",
            params![asset.id.to_string(), vec![1_u8, 2, 3], timestamp(&now())],
        ).unwrap();
        repository.connection.execute(
            "INSERT INTO visual_embeddings (media_asset_id, input_fingerprint, provider, provider_version, dimensions, embedding, generated_at, status, error_message)
             VALUES (?1, 'legacy-input', 'legacy-provider', '1', 2, ?2, ?3, 'ready', NULL)",
            params![asset.id.to_string(), vec![1_u8, 255], timestamp(&now())],
        ).unwrap();
        repository.connection.execute(
            "INSERT INTO technical_quality (media_asset_id, input_fingerprint, provider, provider_version, global_sharpness, sharpness_band, directional_blur_ratio, blur_level, mean_luminance, median_luminance, highlight_clipping_percent, shadow_clipping_percent, channel_clipping_percent, technical_quality_score, technical_quality_band, confidence, generated_at, status, error_message)
             VALUES (?1, 'legacy-input', 'legacy-provider', '1', 1.0, 'low', 1.0, 'low', 0.5, 0.5, 0.0, 0.0, 0.0, 55.0, 'review', 0.7, ?2, 'ready', NULL)",
            params![asset.id.to_string(), timestamp(&now())],
        ).unwrap();
        repository.connection.execute(
            "INSERT INTO analysis_recommendations (id, media_asset_id, similarity_group_id, input_fingerprint, provider, provider_version, label, confidence, reasons_json, generated_at, status)
             VALUES (?1, ?2, NULL, 'legacy-input', 'legacy-provider', '1', 'review', 0.7, '[\"original recommendation\"]', ?3, 'ready')",
            params![recommendation_id, asset.id.to_string(), timestamp(&now())],
        ).unwrap();
        repository.connection.execute(
            "INSERT INTO intelligence_overrides (id, media_asset_id, recommendation_id, decision, decided_at, note)
             VALUES (?1, ?2, ?3, 'keep', ?4, 'preserve this link')",
            params![
                id(281, HumanDecisionId::from_uuid).to_string(),
                asset.id.to_string(),
                recommendation_id,
                timestamp(&now()),
            ],
        ).unwrap();

        repository.migrate().unwrap();
        assert_eq!(repository.schema_version().unwrap(), SCHEMA_VERSION);
        for table in [
            "visual_fingerprints",
            "visual_embeddings",
            "technical_quality",
            "analysis_recommendations",
        ] {
            let rows: i64 = repository
                .connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE stale = 0"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 1, "{table} row must survive migration");
        }
        let preserved = repository.connection.query_row(
            "SELECT recommendation_id, reasons_json FROM intelligence_overrides override
             JOIN analysis_recommendations recommendation ON recommendation.id = override.recommendation_id
             WHERE override.media_asset_id = ?1",
            params![asset.id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).unwrap();
        assert_eq!(preserved.0, recommendation_id);
        assert_eq!(preserved.1, "[\"original recommendation\"]");
        let foreign_key_issue: Option<String> = repository
            .connection
            .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
            .optional()
            .unwrap();
        assert!(foreign_key_issue.is_none());
    }

    #[test]
    fn changed_preview_stales_current_views_but_preserves_historical_evidence_and_decisions() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        let asset = asset(project.id.clone());
        let instance = FileInstance {
            id: id(290, FileInstanceId::from_uuid),
            media_asset_id: asset.id.clone(),
            storage_volume_id: volume.id.clone(),
            index_root_id: None,
            relative_path: "DCIM/IMG_0001.ARW".into(),
            observed_at: now(),
            is_available: true,
        };
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository.insert_file_instance(&instance).unwrap();

        let input_v1 = "fast-1|preview:generator-v1:preview";
        let input_v2 = "fast-2|preview:generator-v2:preview";
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: "history-preview".into(),
                media_asset_id: asset.id.to_string(),
                source_file_instance_id: instance.id.to_string(),
                artifact_type: "thumbnail".into(),
                size_class: "preview".into(),
                cache_relative_path: "history/preview-v1.ppm".into(),
                provider: "test".into(),
                generator_version: "generator-v1".into(),
                source_fingerprint: "fast-1".into(),
                created_at: timestamp(&now()),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        repository
            .upsert_analysis_artifact(&intelligence_artifact(&asset.id, 291, input_v1))
            .unwrap();
        repository
            .upsert_visual_fingerprint(&visual_fingerprint(&asset.id, input_v1))
            .unwrap();
        repository
            .upsert_technical_quality(&technical_evidence(&asset.id, input_v1))
            .unwrap();
        let original_recommendation = id(292, RecommendationId::from_uuid);
        repository
            .upsert_ai_recommendation(&AiRecommendation {
                id: original_recommendation.clone(),
                media_asset_id: asset.id.clone(),
                similarity_group_id: None,
                input_fingerprint: input_v1.into(),
                provider: "technical-advisory".into(),
                provider_version: "v1".into(),
                label: RecommendationLabel::Review,
                confidence: 0.7,
                reasons: vec!["original recommendation".into()],
                generated_at: now(),
                status: AnalysisStatus::Ready,
            })
            .unwrap();
        repository
            .upsert_human_decision(&HumanDecision {
                id: id(293, HumanDecisionId::from_uuid),
                media_asset_id: asset.id.clone(),
                recommendation_id: Some(original_recommendation.clone()),
                decision: HumanDecisionValue::Keep,
                decided_at: now(),
                note: Some("keep original context".into()),
            })
            .unwrap();

        // A changed source fingerprint must not reuse the old dedicated/browsing cache input.
        // Indexing owns this field in production; the focused repository test models that fresh
        // observation directly before inserting its corresponding regenerated artifact.
        repository
            .connection
            .execute(
                "UPDATE media_assets SET fast_fingerprint = ?1 WHERE id = ?2",
                params!["fast-2", asset.id.to_string()],
            )
            .unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: "history-preview-regenerated".into(),
                media_asset_id: asset.id.to_string(),
                source_file_instance_id: instance.id.to_string(),
                artifact_type: "thumbnail".into(),
                size_class: "preview".into(),
                cache_relative_path: "history/preview-v2.ppm".into(),
                provider: "test".into(),
                generator_version: "generator-v2".into(),
                source_fingerprint: "fast-2".into(),
                created_at: timestamp(&now()),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        let candidates = repository
            .analysis_input_candidates(
                &project.id,
                "deterministic-test",
                "v1",
                "analysis-settings-v1",
                &unavailable_face_provider_config(),
            )
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].input_fingerprint, input_v2);
        for table in [
            "analysis_artifacts",
            "visual_fingerprints",
            "visual_embeddings",
            "technical_quality",
            "analysis_recommendations",
        ] {
            let stale: i64 = repository
                .connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} WHERE media_asset_id = ?1 AND stale = 1"
                    ),
                    params![asset.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stale, 1, "{table} must retain and stale v1 evidence");
        }
        assert!(repository
            .capture_intelligence_detail(&asset.id)
            .unwrap()
            .is_none());
        let stale_card = repository
            .visual_media_page(&project.id, &VisualMediaQuery::default())
            .unwrap()
            .items
            .pop()
            .unwrap();
        assert_eq!(stale_card.intelligence.status.as_deref(), Some("stale"));
        assert_eq!(
            repository
                .capture_intelligence_terminal_counts(&project.id)
                .unwrap()
                .stale,
            1
        );

        repository
            .upsert_analysis_artifact(&intelligence_artifact(&asset.id, 294, input_v2))
            .unwrap();
        repository
            .upsert_visual_fingerprint(&visual_fingerprint(&asset.id, input_v2))
            .unwrap();
        repository
            .upsert_technical_quality(&technical_evidence(&asset.id, input_v2))
            .unwrap();
        let current_recommendation = id(295, RecommendationId::from_uuid);
        repository
            .upsert_ai_recommendation(&AiRecommendation {
                id: current_recommendation.clone(),
                media_asset_id: asset.id.clone(),
                similarity_group_id: None,
                input_fingerprint: input_v2.into(),
                provider: "technical-advisory".into(),
                provider_version: "v1".into(),
                label: RecommendationLabel::StrongCandidate,
                confidence: 0.9,
                reasons: vec!["replacement recommendation".into()],
                generated_at: now(),
                status: AnalysisStatus::Ready,
            })
            .unwrap();
        repository
            .upsert_human_decision(&HumanDecision {
                id: id(296, HumanDecisionId::from_uuid),
                media_asset_id: asset.id.clone(),
                recommendation_id: None,
                decision: HumanDecisionValue::Keep,
                decided_at: now(),
                note: None,
            })
            .unwrap();

        for table in [
            "visual_fingerprints",
            "visual_embeddings",
            "technical_quality",
            "analysis_recommendations",
        ] {
            let rows: i64 = repository
                .connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE media_asset_id = ?1"),
                    params![asset.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 2, "{table} must retain v1 and v2 history");
        }
        let original = repository
            .connection
            .query_row(
                "SELECT reasons_json FROM analysis_recommendations WHERE id = ?1",
                params![original_recommendation.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(original, "[\"original recommendation\"]");
        let linked_recommendations = repository.connection.prepare(
            "SELECT recommendation_id FROM intelligence_overrides WHERE media_asset_id = ?1 ORDER BY id ASC",
        ).unwrap().query_map(params![asset.id.to_string()], |row| row.get::<_, String>(0)).unwrap()
            .collect::<std::result::Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            linked_recommendations,
            vec![
                original_recommendation.to_string(),
                current_recommendation.to_string(),
            ]
        );
        let detail = repository
            .capture_intelligence_detail(&asset.id)
            .unwrap()
            .unwrap();
        assert_eq!(detail.input_fingerprint, input_v2);
        assert_eq!(
            detail.recommendation_reasons,
            vec!["replacement recommendation"]
        );
    }

    #[test]
    fn cached_ready_preview_replaces_an_offline_needs_original_result() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        let asset = asset(project.id.clone());
        let instance = FileInstance {
            id: id(92, FileInstanceId::from_uuid),
            media_asset_id: asset.id.clone(),
            storage_volume_id: volume.id.clone(),
            index_root_id: None,
            relative_path: "offline/IMG_0001.ARW".into(),
            observed_at: now(),
            is_available: false,
        };
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository.insert_file_instance(&instance).unwrap();

        let provider = "captureos-deterministic-image";
        let version = "m4.det.v1";
        let settings = "m4.analysis-preview-1600.v1";
        let initial = repository
            .analysis_input_candidates(
                &project.id,
                provider,
                version,
                settings,
                &unavailable_face_provider_config(),
            )
            .unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].preview_status, "needs_original");

        repository
            .upsert_analysis_artifact(&AnalysisArtifact {
                id: id(93, AnalysisArtifactId::from_uuid),
                media_asset_id: asset.id.clone(),
                artifact_type: "capture_intelligence".into(),
                payload: serde_json::json!({}),
                provenance: Provenance {
                    source: "test".into(),
                    algorithm_id: Some(provider.into()),
                    algorithm_version: Some(version.into()),
                    produced_at: now(),
                    human_confirmed: false,
                },
                provider: Some(provider.into()),
                provider_version: Some(version.into()),
                model_version: None,
                settings_version: Some(settings.into()),
                input_fingerprint: Some("fast-1".into()),
                generated_at: Some(now()),
                confidence: None,
                status: Some(AnalysisStatus::NeedsOriginal),
                error_message: Some("No cache yet".into()),
            })
            .unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: Uuid::new_v4().to_string(),
                media_asset_id: asset.id.to_string(),
                source_file_instance_id: instance.id.to_string(),
                artifact_type: "thumbnail".into(),
                size_class: "preview".into(),
                cache_relative_path: "m4/offline/preview.ppm".into(),
                provider: "test".into(),
                generator_version: "test".into(),
                source_fingerprint: "fast-1".into(),
                created_at: timestamp(&now()),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();

        let recovered = repository
            .analysis_input_candidates(
                &project.id,
                provider,
                version,
                settings,
                &unavailable_face_provider_config(),
            )
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].preview_status, "ready");
        assert_eq!(
            recovered[0].preview_relative_path.as_deref(),
            Some("m4/offline/preview.ppm")
        );
        assert!(!recovered[0].is_available);

        // A ready result is reused for the same managed preview, but regenerating that preview
        // with a different generator version changes the input identity and requeues analysis.
        let cached_preview_input = recovered[0].input_fingerprint.clone();
        repository
            .upsert_analysis_artifact(&AnalysisArtifact {
                id: id(96, AnalysisArtifactId::from_uuid),
                media_asset_id: asset.id.clone(),
                artifact_type: "capture_intelligence".into(),
                payload: serde_json::json!({ "face_provider": "none" }),
                provenance: Provenance {
                    source: "test".into(),
                    algorithm_id: Some(provider.into()),
                    algorithm_version: Some(version.into()),
                    produced_at: now(),
                    human_confirmed: false,
                },
                provider: Some(provider.into()),
                provider_version: Some(version.into()),
                model_version: None,
                settings_version: Some(settings.into()),
                input_fingerprint: Some(cached_preview_input.clone()),
                generated_at: Some(now()),
                confidence: Some(0.8),
                status: Some(AnalysisStatus::Ready),
                error_message: None,
            })
            .unwrap();
        repository
            .upsert_analysis_artifact(&AnalysisArtifact {
                id: id(97, AnalysisArtifactId::from_uuid),
                media_asset_id: asset.id.clone(),
                artifact_type: "face_detection".into(),
                payload: serde_json::json!({
                    "input_preview_fingerprint": cached_preview_input,
                    "face_provider_cache_identity": "test-face-provider-v1",
                }),
                provenance: Provenance {
                    source: "test".into(),
                    algorithm_id: Some("none".into()),
                    algorithm_version: Some("no-approved-local-provider.v1".into()),
                    produced_at: now(),
                    human_confirmed: false,
                },
                provider: Some("none".into()),
                provider_version: Some("no-approved-local-provider.v1".into()),
                model_version: None,
                settings_version: Some("m4.face-analysis.v1".into()),
                input_fingerprint: Some("face-cache-v1".into()),
                generated_at: Some(now()),
                confidence: None,
                status: Some(AnalysisStatus::NotApplicable),
                error_message: Some("No approved local face provider is installed".into()),
            })
            .unwrap();
        assert!(repository
            .analysis_input_candidates(
                &project.id,
                provider,
                version,
                settings,
                &unavailable_face_provider_config(),
            )
            .unwrap()
            .is_empty());

        // A face-provider revision invalidates only face/eye evidence. The primary technical
        // artifact remains current and is deliberately absent from the full-analysis queue;
        // the separate face queue uses the existing CaptureOS-managed preview.
        let revised_face_provider = FaceAnalysisProviderConfig::new(
            "different-local-face-provider",
            "v2",
            "m4.face-analysis.v1",
        );
        assert_eq!(
            repository
                .analysis_input_candidates(
                    &project.id,
                    provider,
                    version,
                    settings,
                    &revised_face_provider,
                )
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            repository
                .face_analysis_input_candidates(
                    &project.id,
                    provider,
                    version,
                    settings,
                    &revised_face_provider,
                )
                .unwrap()
                .len(),
            1
        );
        let primary_stale: i64 = repository
            .connection
            .query_row(
                "SELECT stale FROM analysis_artifacts WHERE id = ?1",
                params![id(96, AnalysisArtifactId::from_uuid).to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let face_stale: i64 = repository
            .connection
            .query_row(
                "SELECT stale FROM analysis_artifacts WHERE id = ?1",
                params![id(97, AnalysisArtifactId::from_uuid).to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(primary_stale, 0);
        assert_eq!(face_stale, 1);
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: Uuid::new_v4().to_string(),
                media_asset_id: asset.id.to_string(),
                source_file_instance_id: instance.id.to_string(),
                artifact_type: "thumbnail".into(),
                size_class: "preview".into(),
                cache_relative_path: "m4/offline/preview-v2.ppm".into(),
                provider: "test".into(),
                generator_version: "test-v2".into(),
                source_fingerprint: "fast-1".into(),
                created_at: timestamp(&now()),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        let after_generator_change = repository
            .analysis_input_candidates(
                &project.id,
                provider,
                version,
                settings,
                &unavailable_face_provider_config(),
            )
            .unwrap();
        assert_eq!(after_generator_change.len(), 1);
        assert_ne!(
            after_generator_change[0].input_fingerprint,
            cached_preview_input
        );
    }

    #[test]
    fn face_evidence_uses_its_independent_cache_input_in_details_and_filters() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let volume = volume();
        let asset = asset(project.id.clone());
        let instance = FileInstance {
            id: id(281, FileInstanceId::from_uuid),
            media_asset_id: asset.id.clone(),
            storage_volume_id: volume.id.clone(),
            index_root_id: None,
            relative_path: "faces/IMG_0001.ARW".into(),
            observed_at: now(),
            is_available: true,
        };
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository.insert_file_instance(&instance).unwrap();

        let primary_input = "preview-input-v1";
        let face_input = "face-cache-input-v1";
        repository
            .upsert_analysis_artifact(&AnalysisArtifact {
                id: id(282, AnalysisArtifactId::from_uuid),
                media_asset_id: asset.id.clone(),
                artifact_type: "capture_intelligence".into(),
                payload: serde_json::json!({ "face_provider": "test-local-face" }),
                provenance: Provenance {
                    source: "test".into(),
                    algorithm_id: Some("deterministic-test".into()),
                    algorithm_version: Some("v1".into()),
                    produced_at: now(),
                    human_confirmed: false,
                },
                provider: Some("deterministic-test".into()),
                provider_version: Some("v1".into()),
                model_version: None,
                settings_version: Some("analysis-settings-v1".into()),
                input_fingerprint: Some(primary_input.into()),
                generated_at: Some(now()),
                confidence: None,
                status: Some(AnalysisStatus::Ready),
                error_message: None,
            })
            .unwrap();
        repository
            .upsert_analysis_artifact(&AnalysisArtifact {
                id: id(283, AnalysisArtifactId::from_uuid),
                media_asset_id: asset.id.clone(),
                artifact_type: "face_detection".into(),
                payload: serde_json::json!({
                    "input_preview_fingerprint": primary_input,
                    "face_provider_cache_identity": "test-local-face-v1",
                    "resolved_face_provider": "ultraface-rfb-320",
                    "resolved_face_provider_version": "test-v1",
                    "provider_attempt_error": "Apple Vision host service unavailable",
                    "landmark_status": "not_applicable",
                    "landmark_error": "No landmark provider enabled",
                }),
                provenance: Provenance {
                    source: "test".into(),
                    algorithm_id: Some("test-local-face".into()),
                    algorithm_version: Some("v1".into()),
                    produced_at: now(),
                    human_confirmed: false,
                },
                provider: Some("test-local-face".into()),
                provider_version: Some("v1".into()),
                model_version: None,
                settings_version: Some("face-settings-v1".into()),
                input_fingerprint: Some(face_input.into()),
                generated_at: Some(now()),
                confidence: None,
                status: Some(AnalysisStatus::Ready),
                error_message: None,
            })
            .unwrap();
        repository
            .replace_face_analyses(
                &asset.id,
                face_input,
                &[FaceAnalysis {
                    id: id(284, FaceAnalysisId::from_uuid),
                    media_asset_id: asset.id.clone(),
                    input_fingerprint: face_input.into(),
                    provider: "test-local-face".into(),
                    provider_version: "v1".into(),
                    x: 0.2,
                    y: 0.2,
                    width: 0.3,
                    height: 0.3,
                    detection_confidence: 0.9,
                    relative_size: 0.09,
                    visibility: Some("good".into()),
                    pose: None,
                    face_sharpness: Some(70.0),
                    eye_state: EyeState::Closed,
                    eye_confidence: Some(0.75),
                    generated_at: now(),
                }],
            )
            .unwrap();

        let detail = repository
            .capture_intelligence_detail(&asset.id)
            .unwrap()
            .unwrap();
        assert_eq!(detail.face_provider, "test-local-face");
        assert_eq!(detail.face_resolved_provider, "ultraface-rfb-320");
        assert_eq!(
            detail.face_provider_attempt_error.as_deref(),
            Some("Apple Vision host service unavailable")
        );
        assert_eq!(detail.face_landmark_status, "not_applicable");
        assert_eq!(detail.faces.len(), 1);
        assert_eq!(detail.faces[0].eye_state, "closed");
        assert_eq!(detail.summary.face_count, 1);
        assert_eq!(detail.summary.possible_closed_eyes_count, 1);

        for filter in [
            VisualMediaFilter::Faces,
            VisualMediaFilter::PossibleClosedEyes,
        ] {
            let page = repository
                .visual_media_page(
                    &project.id,
                    &VisualMediaQuery {
                        filter,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].asset_id, asset.id.to_string());
        }
    }

    #[test]
    fn unchanged_recommendation_does_not_create_history_churn() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let asset = asset(project.id.clone());
        repository.insert_project(&project).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        for identifier in [285, 286] {
            repository
                .upsert_ai_recommendation(&AiRecommendation {
                    id: id(identifier, RecommendationId::from_uuid),
                    media_asset_id: asset.id.clone(),
                    similarity_group_id: None,
                    input_fingerprint: "preview-input-v1".into(),
                    provider: "technical-advisory".into(),
                    provider_version: "v1".into(),
                    label: RecommendationLabel::Review,
                    confidence: 0.7,
                    reasons: vec!["Measured evidence is unchanged".into()],
                    generated_at: now(),
                    status: AnalysisStatus::Ready,
                })
                .unwrap();
        }
        let rows: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*) FROM analysis_recommendations WHERE media_asset_id = ?1",
                params![asset.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn human_intelligence_decision_references_the_visible_ai_recommendation() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        let asset = asset(project.id.clone());
        let recommendation_id = id(94, RecommendationId::from_uuid);
        repository.insert_project(&project).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository
            .upsert_ai_recommendation(&AiRecommendation {
                id: recommendation_id.clone(),
                media_asset_id: asset.id.clone(),
                similarity_group_id: None,
                input_fingerprint: "fast-1".into(),
                provider: "test".into(),
                provider_version: "1".into(),
                label: RecommendationLabel::Review,
                confidence: 0.5,
                reasons: vec!["Technical evidence is incomplete".into()],
                generated_at: now(),
                status: AnalysisStatus::Ready,
            })
            .unwrap();
        repository
            .upsert_human_decision(&HumanDecision {
                id: id(95, HumanDecisionId::from_uuid),
                media_asset_id: asset.id.clone(),
                recommendation_id: None,
                decision: HumanDecisionValue::Keep,
                decided_at: now(),
                note: Some("Intentional low key frame".into()),
            })
            .unwrap();
        let linked: Option<String> = repository
            .connection
            .query_row(
                "SELECT recommendation_id FROM intelligence_overrides WHERE media_asset_id = ?1",
                params![asset.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let recommendation_id_text = recommendation_id.to_string();
        assert_eq!(linked.as_deref(), Some(recommendation_id_text.as_str()));
    }

    #[test]
    fn culling_decisions_are_project_scoped_append_history_and_survive_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("captureos.sqlite3");
        let repository = SqliteRepository::open(&path).unwrap();
        let first = project();
        let second = Project {
            id: id(401, ProjectId::from_uuid),
            name: "Second project".into(),
            created_at: now(),
        };
        repository.insert_project(&first).unwrap();
        repository.insert_project(&second).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let first_asset = culling_asset(&repository, &first.id, 402);
        let second_asset = culling_asset(&repository, &second.id, 403);
        let session = repository
            .start_or_resume_review_session(&first.id, "all_photos", Some("all"))
            .unwrap();
        let keep = CullingDecisionUpdate {
            decision: Some(CullingDecisionValue::Keep),
            clear_decision: false,
            rating: Some(5),
            starred: Some(true),
            note: Some("Album candidate".into()),
            flags: Some(vec!["album".into()]),
            session_id: Some(session.id.clone()),
        };
        let stored = repository
            .update_culling_decision(&first.id, &first_asset.id, &keep)
            .unwrap();
        assert_eq!(stored.decision.as_deref(), Some("keep"));
        assert_eq!(stored.rating, 5);
        assert!(stored.starred);
        let review = CullingDecisionUpdate {
            decision: Some(CullingDecisionValue::Review),
            clear_decision: false,
            rating: None,
            starred: None,
            note: None,
            flags: None,
            session_id: Some(session.id),
        };
        repository
            .update_culling_decision(&first.id, &first_asset.id, &review)
            .unwrap();
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM decision_history WHERE media_asset_id = ?1",
                    params![first_asset.id.to_string()],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            6
        );
        assert!(repository
            .update_culling_decision(&first.id, &second_asset.id, &review)
            .is_err());
        drop(repository);
        let reopened = SqliteRepository::open(&path).unwrap();
        let workspace = reopened
            .culling_workspace(&first.id, &CullingQuery::default())
            .unwrap();
        assert_eq!(workspace.progress.reviewed, 1);
        assert_eq!(workspace.progress.review, 1);
        assert_eq!(
            workspace.items[0].decision.note.as_deref(),
            Some("Album candidate")
        );
        assert_eq!(
            reopened
                .culling_workspace(&second.id, &CullingQuery::default())
                .unwrap()
                .progress
                .reviewed,
            0
        );
    }

    #[test]
    fn human_group_representative_keeps_ai_representative_and_records_private_preference_data() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let ai_choice = culling_asset(&repository, &project.id, 410);
        let human_choice = culling_asset(&repository, &project.id, 411);
        let group = SimilarityGroup {
            id: id(412, SimilarityGroupId::from_uuid),
            project_id: project.id.clone(),
            kind: SimilarityGroupKind::SimilarSet,
            representative_asset_id: ai_choice.id.clone(),
            grouping_method: "test".into(),
            grouping_version: "1".into(),
            similarity_confidence: 0.9,
            time_proximity_seconds: Some(1),
            visual_similarity: Some(0.9),
            created_at: now(),
        };
        repository
            .replace_similarity_groups(
                &project.id,
                std::slice::from_ref(&group),
                &[
                    SimilarityGroupMember {
                        group_id: group.id.clone(),
                        media_asset_id: ai_choice.id.clone(),
                        ordinal: 0,
                        similarity_confidence: 1.0,
                        time_proximity_seconds: Some(0),
                        is_representative: true,
                    },
                    SimilarityGroupMember {
                        group_id: group.id.clone(),
                        media_asset_id: human_choice.id.clone(),
                        ordinal: 1,
                        similarity_confidence: 0.9,
                        time_proximity_seconds: Some(1),
                        is_representative: false,
                    },
                ],
            )
            .unwrap();
        let session = repository
            .start_or_resume_review_session(&project.id, "similar_sets", Some("all"))
            .unwrap();
        repository
            .set_group_human_representative(
                &project.id,
                &group.id.to_string(),
                &human_choice.id,
                Some(&session.id),
            )
            .unwrap();
        let summary = culling_group_summaries(&repository.connection, &project.id).unwrap();
        assert_eq!(
            summary[0].ai_representative_asset_id,
            ai_choice.id.to_string()
        );
        assert_eq!(
            summary[0].human_representative_asset_id.as_deref(),
            Some(human_choice.id.to_string().as_str())
        );
        let examples = repository.preference_examples(&project.id).unwrap();
        assert_eq!(examples.len(), 1);
        assert_eq!(examples[0].chosen_asset_id, human_choice.id.to_string());
        let profile_id = repository.ensure_default_studio_profile().unwrap();
        assert!(
            !repository
                .studio_training_source_state(&profile_id)
                .unwrap()
                .materialization_pending,
            "a successful Similar Set representative capture clears its atomic source guard"
        );
        let snapshot: String = repository
            .connection
            .query_row(
                "SELECT technical_evidence_json FROM preference_examples WHERE id = ?1",
                params![examples[0].id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !snapshot.contains("RAW/")
                && !snapshot.contains("imageBytes")
                && !snapshot.contains("faceCrop")
        );
    }

    #[test]
    fn culling_group_completion_is_decision_based_and_survives_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("captureos.sqlite3");
        let repository = SqliteRepository::open(&path).unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let first = culling_asset(&repository, &project.id, 620);
        let second = culling_asset(&repository, &project.id, 621);
        let partial_first = culling_asset(&repository, &project.id, 622);
        let partial_second = culling_asset(&repository, &project.id, 623);
        let automatic = SimilarityGroup {
            id: id(624, SimilarityGroupId::from_uuid),
            project_id: project.id.clone(),
            kind: SimilarityGroupKind::SimilarSet,
            representative_asset_id: first.id.clone(),
            grouping_method: "test".into(),
            grouping_version: "1".into(),
            similarity_confidence: 0.9,
            time_proximity_seconds: Some(1),
            visual_similarity: Some(0.9),
            created_at: now(),
        };
        let explicit = SimilarityGroup {
            id: id(625, SimilarityGroupId::from_uuid),
            project_id: project.id.clone(),
            kind: SimilarityGroupKind::SimilarSet,
            representative_asset_id: partial_first.id.clone(),
            grouping_method: "test".into(),
            grouping_version: "1".into(),
            similarity_confidence: 0.9,
            time_proximity_seconds: Some(1),
            visual_similarity: Some(0.9),
            created_at: now(),
        };
        repository
            .replace_similarity_groups(
                &project.id,
                &[automatic.clone(), explicit.clone()],
                &[
                    SimilarityGroupMember {
                        group_id: automatic.id.clone(),
                        media_asset_id: first.id.clone(),
                        ordinal: 0,
                        similarity_confidence: 1.0,
                        time_proximity_seconds: Some(0),
                        is_representative: true,
                    },
                    SimilarityGroupMember {
                        group_id: automatic.id.clone(),
                        media_asset_id: second.id.clone(),
                        ordinal: 1,
                        similarity_confidence: 0.9,
                        time_proximity_seconds: Some(1),
                        is_representative: false,
                    },
                    SimilarityGroupMember {
                        group_id: explicit.id.clone(),
                        media_asset_id: partial_first.id.clone(),
                        ordinal: 0,
                        similarity_confidence: 1.0,
                        time_proximity_seconds: Some(0),
                        is_representative: true,
                    },
                    SimilarityGroupMember {
                        group_id: explicit.id.clone(),
                        media_asset_id: partial_second.id.clone(),
                        ordinal: 1,
                        similarity_confidence: 0.9,
                        time_proximity_seconds: Some(1),
                        is_representative: false,
                    },
                ],
            )
            .unwrap();
        let session = repository
            .start_or_resume_review_session(&project.id, "similar_sets", Some("all"))
            .unwrap();
        for update in [
            CullingDecisionUpdate {
                decision: None,
                clear_decision: false,
                rating: Some(5),
                starred: None,
                note: None,
                flags: None,
                session_id: Some(session.id.clone()),
            },
            CullingDecisionUpdate {
                decision: None,
                clear_decision: false,
                rating: None,
                starred: Some(true),
                note: None,
                flags: None,
                session_id: Some(session.id.clone()),
            },
            CullingDecisionUpdate {
                decision: None,
                clear_decision: false,
                rating: None,
                starred: None,
                note: Some("Album candidate".into()),
                flags: None,
                session_id: Some(session.id.clone()),
            },
        ] {
            repository
                .update_culling_decision(&project.id, &first.id, &update)
                .unwrap();
        }
        let summaries = culling_group_summaries(&repository.connection, &project.id).unwrap();
        let automatic_summary = summaries
            .iter()
            .find(|group| group.id == automatic.id.to_string())
            .unwrap();
        assert_eq!(automatic_summary.reviewed_count, 0);
        assert!(!automatic_summary.completed);
        assert_eq!(
            automatic_summary.ai_representative_filename,
            first.display_name
        );

        for (asset_id, decision) in [
            (&first.id, CullingDecisionValue::Keep),
            (&second.id, CullingDecisionValue::Review),
        ] {
            repository
                .update_culling_decision(
                    &project.id,
                    asset_id,
                    &CullingDecisionUpdate {
                        decision: Some(decision),
                        clear_decision: false,
                        rating: None,
                        starred: None,
                        note: None,
                        flags: None,
                        session_id: Some(session.id.clone()),
                    },
                )
                .unwrap();
        }
        repository
            .update_culling_decision(
                &project.id,
                &partial_first.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: Some(session.id.clone()),
                },
            )
            .unwrap();
        repository
            .mark_group_review_complete(&project.id, &explicit.id.to_string(), Some(&session.id))
            .unwrap();
        let summaries = culling_group_summaries(&repository.connection, &project.id).unwrap();
        let automatic_summary = summaries
            .iter()
            .find(|group| group.id == automatic.id.to_string())
            .unwrap();
        assert_eq!(automatic_summary.reviewed_count, 2);
        assert!(automatic_summary.completed);
        assert_eq!(
            automatic_summary.completion_kind.as_deref(),
            Some("auto_all_reviewed")
        );
        let explicit_summary = summaries
            .iter()
            .find(|group| group.id == explicit.id.to_string())
            .unwrap();
        assert_eq!(explicit_summary.reviewed_count, 1);
        assert!(explicit_summary.completed);
        assert_eq!(
            explicit_summary.completion_kind.as_deref(),
            Some("explicit_user_completion")
        );
        assert_eq!(
            repository
                .culling_progress(&project.id)
                .unwrap()
                .sets_reviewed,
            2
        );

        drop(repository);
        let reopened = SqliteRepository::open(&path).unwrap();
        let summaries = culling_group_summaries(&reopened.connection, &project.id).unwrap();
        assert!(summaries
            .iter()
            .any(|group| group.id == automatic.id.to_string()
                && group.completion_kind.as_deref() == Some("auto_all_reviewed")));
        assert!(summaries
            .iter()
            .any(|group| group.id == explicit.id.to_string()
                && group.completion_kind.as_deref() == Some("explicit_user_completion")));
    }

    #[test]
    fn culling_queries_stay_bounded_for_a_ten_thousand_asset_catalog() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        for value in 1_000_u128..11_000 {
            culling_asset(&repository, &project.id, value);
        }
        for value in 1_000_u128..1_100 {
            let asset_id = id(value, MediaAssetId::from_uuid);
            repository
                .update_culling_decision(
                    &project.id,
                    &asset_id,
                    &CullingDecisionUpdate {
                        decision: Some(CullingDecisionValue::Keep),
                        clear_decision: false,
                        rating: None,
                        starred: Some(value % 2 == 0),
                        note: None,
                        flags: None,
                        session_id: None,
                    },
                )
                .unwrap();
        }
        for value in 1_000_u128..1_200 {
            let asset_id = id(value, MediaAssetId::from_uuid);
            repository
                .upsert_ai_recommendation(&AiRecommendation {
                    id: id(value + 20_000, RecommendationId::from_uuid),
                    media_asset_id: asset_id,
                    similarity_group_id: None,
                    input_fingerprint: "ten-thousand-culling-fixture".into(),
                    provider: "test".into(),
                    provider_version: "1".into(),
                    label: if value % 3 == 0 {
                        RecommendationLabel::TechnicalIssue
                    } else {
                        RecommendationLabel::StrongCandidate
                    },
                    confidence: 0.8,
                    reasons: vec!["Synthetic technical queue fixture".into()],
                    generated_at: now(),
                    status: AnalysisStatus::Ready,
                })
                .unwrap();
        }
        let decision_started = Instant::now();
        repository
            .update_culling_decision(
                &project.id,
                &id(1_100, MediaAssetId::from_uuid),
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Review),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        eprintln!(
            "M5 measured local decision persistence (SQLite, debug test): {} ms",
            decision_started.elapsed().as_millis()
        );
        let ai_queue_started = Instant::now();
        let ai_queue_page = repository
            .culling_workspace(
                &project.id,
                &CullingQuery {
                    mode: "ai_review_queue".into(),
                    filter: "all".into(),
                    group_id: None,
                    moment_id: None,
                    limit: 60,
                    offset: 0,
                },
            )
            .unwrap();
        eprintln!(
            "M5 measured 10k-catalog AI review queue page (60 rows, SQLite, debug test): {} ms",
            ai_queue_started.elapsed().as_millis()
        );
        assert_eq!(ai_queue_page.items.len(), 60);
        assert!(ai_queue_page.has_more);
        let unreviewed_pages_started = Instant::now();
        let first_page = repository
            .culling_workspace(
                &project.id,
                &CullingQuery {
                    mode: "all_photos".into(),
                    filter: "unreviewed".into(),
                    group_id: None,
                    moment_id: None,
                    limit: 60,
                    offset: 0,
                },
            )
            .unwrap();
        eprintln!(
            "M5 measured 10k-catalog unreviewed pages (2 × 60 rows, SQLite, debug test): {} ms",
            unreviewed_pages_started.elapsed().as_millis()
        );
        let second_page = repository
            .culling_workspace(
                &project.id,
                &CullingQuery {
                    mode: "all_photos".into(),
                    filter: "unreviewed".into(),
                    group_id: None,
                    moment_id: None,
                    limit: 60,
                    offset: 60,
                },
            )
            .unwrap();
        assert_eq!(first_page.progress.total, 10_000);
        assert_eq!(first_page.progress.keep, 100);
        assert_eq!(first_page.items.len(), 60);
        assert!(first_page.has_more);
        assert_eq!(second_page.items.len(), 60);
        assert_ne!(
            first_page.items[0].media.asset_id,
            second_page.items[0].media.asset_id
        );
    }

    #[test]
    fn running_capture_intelligence_is_marked_interrupted_on_recovery() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let job = BackgroundJob {
            id: id(91, JobId::from_uuid),
            state: WorkflowRunState::Running,
            stage: JobStage::AiAnalysis,
            items_completed: 3,
            items_total: Some(8),
            files_discovered: 8,
            files_processed: 3,
            error_count: 0,
            project_id: Some(project.id.clone()),
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({ "pipeline": "capture-intelligence" })),
            created_at: now(),
            updated_at: now(),
            finished_at: None,
        };
        repository.insert_background_job(&job).unwrap();
        assert_eq!(
            repository
                .recover_interrupted_capture_intelligence()
                .unwrap(),
            1
        );
        let recovered = repository.get_background_job(&job.id).unwrap().unwrap();
        assert_eq!(recovered.state, WorkflowRunState::Interrupted);
        assert_eq!(recovered.stage, JobStage::Finalize);
        assert!(recovered.error_message.unwrap().contains("interrupted"));
    }

    fn semantic_model(dimensions: usize, version: &str) -> SemanticModelConfig {
        SemanticModelConfig {
            model_id: "local-test-model".into(),
            provider: "test-provider".into(),
            model_version: version.into(),
            embedding_version: format!("embedding-{version};dimensions={dimensions}"),
            preprocessing_version: "preview-v1".into(),
            metric: "normalized-dot-v1".into(),
            dimensions,
        }
    }

    fn m7_test_timeline(
        project_id: &ProjectId,
        timeline_id: &str,
        active_run_id: &str,
    ) -> MomentTimelineStatusRecord {
        MomentTimelineStatusRecord {
            timeline_id: timeline_id.into(),
            project_id: project_id.to_string(),
            state: "ready".into(),
            analyzer_id: "local-moment-brain".into(),
            analyzer_version: "test-v1".into(),
            boundary_algorithm_version: "test-boundaries-v1".into(),
            semantic_model_key: None,
            input_catalog_version: "test-input-v1".into(),
            active_run_id: Some(active_run_id.into()),
            moment_count: 0,
            eligible_count: 0,
            ungrouped_count: 0,
            updated_at: timestamp(&now()),
        }
    }

    fn m7_test_run(
        project_id: &ProjectId,
        timeline_id: &str,
        run_id: &str,
        item_count: u64,
    ) -> MomentAnalysisRunRecord {
        MomentAnalysisRunRecord {
            id: run_id.into(),
            timeline_id: timeline_id.into(),
            project_id: project_id.to_string(),
            state: "ready".into(),
            analyzer_id: "local-moment-brain".into(),
            analyzer_version: "test-v1".into(),
            boundary_algorithm_version: "test-boundaries-v1".into(),
            semantic_model_key: None,
            input_catalog_version: "test-input-v1".into(),
            items_total: item_count,
            items_completed: item_count,
            error_count: 0,
            started_at: timestamp(&now()),
            finished_at: Some(timestamp(&now())),
        }
    }

    fn m7_test_segment(
        project_id: &ProjectId,
        run_id: &str,
        id: &str,
        ordinal: u64,
        asset_count: u64,
    ) -> TimelineSegmentRecord {
        TimelineSegmentRecord {
            id: id.into(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            ordinal,
            started_at: None,
            ended_at: None,
            asset_count,
            boundary_category: None,
            boundary_evidence: serde_json::json!({"test": true}),
            created_at: timestamp(&now()),
            stale: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn m7_test_moment(
        project_id: &ProjectId,
        timeline_id: &str,
        run_id: &str,
        segment_id: &str,
        id: &str,
        anchor_asset_id: &str,
        ordinal: u64,
        asset_count: u64,
    ) -> MomentRecord {
        MomentRecord {
            id: id.into(),
            project_id: project_id.to_string(),
            timeline_id: timeline_id.into(),
            run_id: run_id.into(),
            segment_id: segment_id.into(),
            anchor_asset_id: anchor_asset_id.into(),
            ordinal,
            started_at: None,
            ended_at: None,
            asset_count,
            ai_representative_asset_id: Some(anchor_asset_id.into()),
            centroid: None,
            centroid_dimensions: None,
            suggested_label: None,
            label_confidence: None,
            label_evidence: serde_json::json!({"state": "abstained"}),
            label_state: "abstained".into(),
            created_at: timestamp(&now()),
            stale: false,
        }
    }

    fn m7_test_membership(
        project_id: &ProjectId,
        run_id: &str,
        id: &str,
        moment_id: Option<&str>,
        media_asset_id: &str,
        ordinal: u64,
        membership_state: &str,
    ) -> MomentMembershipRecord {
        MomentMembershipRecord {
            id: id.into(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            moment_id: moment_id.map(str::to_owned),
            media_asset_id: media_asset_id.into(),
            ordinal,
            membership_state: membership_state.into(),
            created_at: timestamp(&now()),
            active: true,
        }
    }

    fn active_moment_ordinals(
        repository: &SqliteRepository,
        project_id: &ProjectId,
    ) -> Vec<(String, i64)> {
        repository
            .connection
            .prepare(
                "SELECT id, ordinal
                 FROM moment_records
                 WHERE project_id = ?1 AND stale = 0
                 ORDER BY ordinal ASC, id ASC",
            )
            .unwrap()
            .query_map(params![project_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn assert_m7_projection_integrity(repository: &SqliteRepository, project_id: &ProjectId) {
        let active_ordinals = active_moment_ordinals(repository, project_id);
        assert_eq!(
            active_ordinals
                .iter()
                .map(|(_, ordinal)| *ordinal)
                .collect::<Vec<_>>(),
            (0..i64::try_from(active_ordinals.len()).unwrap()).collect::<Vec<_>>(),
            "active Moment ordinals must be contiguous and deterministic"
        );
        let active_membership_ordinals = repository
            .connection
            .prepare(
                "SELECT ordinal
                 FROM moment_memberships
                 WHERE project_id = ?1 AND active = 1
                 ORDER BY ordinal ASC, id ASC",
            )
            .unwrap()
            .query_map(params![project_id.to_string()], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            active_membership_ordinals,
            (0..i64::try_from(active_membership_ordinals.len()).unwrap()).collect::<Vec<_>>(),
            "active Moment membership ordinals must be contiguous and deterministic"
        );
        let duplicate_ordinal_count: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM (
                     SELECT run_id, ordinal
                     FROM moment_records
                     WHERE project_id = ?1
                     GROUP BY run_id, ordinal
                     HAVING COUNT(*) > 1
                 )",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(duplicate_ordinal_count, 0);
        let invalid_active_memberships: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM moment_memberships membership
                 LEFT JOIN moment_records moment ON moment.id = membership.moment_id
                 WHERE membership.project_id = ?1
                   AND membership.active = 1
                   AND membership.membership_state = 'member'
                   AND (moment.id IS NULL OR moment.stale <> 0 OR membership.run_id <> moment.run_id)",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalid_active_memberships, 0);
        let orphaned_active_segments: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM timeline_segments segment
                 WHERE segment.project_id = ?1
                   AND segment.stale = 0
                   AND NOT EXISTS (
                       SELECT 1
                       FROM moment_records moment
                       WHERE moment.project_id = segment.project_id
                         AND moment.segment_id = segment.id
                         AND moment.stale = 0
                   )",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphaned_active_segments, 0);
        let foreign_key_issue: Option<String> = repository
            .connection
            .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
            .optional()
            .unwrap();
        assert!(foreign_key_issue.is_none());
        let integrity: String = repository
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn m7_incremental_window_and_tail_replacement_preserve_active_projection_and_human_history() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("m7-tail-replacement.sqlite3");
        let repository = SqliteRepository::open(&catalog).unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let assets = (0_u128..7)
            .map(|offset| culling_asset(&repository, &project.id, 80_000 + offset))
            .collect::<Vec<_>>();
        let asset_ids = assets
            .iter()
            .map(|asset| asset.id.to_string())
            .collect::<Vec<_>>();
        for (ordinal, asset_id) in asset_ids.iter().enumerate() {
            repository
                .connection
                .execute(
                    "UPDATE media_assets SET captured_at = ?1 WHERE id = ?2",
                    params![format!("2025-01-01T10:00:0{ordinal}+00:00"), asset_id],
                )
                .unwrap();
        }

        let timeline_id = "m7-tail-timeline";
        let initial_run_id = "m7-tail-initial-run";
        let initial_timeline = m7_test_timeline(&project.id, timeline_id, initial_run_id);
        let initial_run = m7_test_run(&project.id, timeline_id, initial_run_id, 5);
        let initial_segments = vec![
            m7_test_segment(&project.id, initial_run_id, "m7-segment-0", 0, 2),
            m7_test_segment(&project.id, initial_run_id, "m7-segment-1", 1, 2),
            m7_test_segment(&project.id, initial_run_id, "m7-segment-2", 2, 1),
        ];
        let initial_moments = vec![
            m7_test_moment(
                &project.id,
                timeline_id,
                initial_run_id,
                "m7-segment-0",
                "m7-moment-0",
                &asset_ids[0],
                0,
                2,
            ),
            m7_test_moment(
                &project.id,
                timeline_id,
                initial_run_id,
                "m7-segment-1",
                "m7-moment-1",
                &asset_ids[2],
                1,
                2,
            ),
            m7_test_moment(
                &project.id,
                timeline_id,
                initial_run_id,
                "m7-segment-2",
                "m7-moment-2",
                &asset_ids[4],
                2,
                1,
            ),
        ];
        let initial_memberships = vec![
            m7_test_membership(
                &project.id,
                initial_run_id,
                "m7-member-0",
                Some("m7-moment-0"),
                &asset_ids[0],
                0,
                "member",
            ),
            m7_test_membership(
                &project.id,
                initial_run_id,
                "m7-member-1",
                Some("m7-moment-0"),
                &asset_ids[1],
                1,
                "member",
            ),
            m7_test_membership(
                &project.id,
                initial_run_id,
                "m7-member-2",
                Some("m7-moment-1"),
                &asset_ids[2],
                2,
                "member",
            ),
            m7_test_membership(
                &project.id,
                initial_run_id,
                "m7-member-3",
                Some("m7-moment-1"),
                &asset_ids[3],
                3,
                "member",
            ),
            m7_test_membership(
                &project.id,
                initial_run_id,
                "m7-member-4",
                Some("m7-moment-2"),
                &asset_ids[4],
                4,
                "member",
            ),
        ];
        repository
            .replace_active_moment_analysis(
                &initial_timeline,
                &initial_run,
                &initial_segments,
                &initial_moments,
                &initial_memberships,
                &[],
            )
            .unwrap();
        // This durable ungrouped row must be included in a later update rather than silently
        // falling out of the active projection. Asset 6 is a newly catalogued non-member.
        repository
            .connection
            .execute(
                "INSERT INTO moment_memberships (id, project_id, run_id, moment_id, media_asset_id, ordinal, membership_state, created_at, active)
                 VALUES (?1, ?2, ?3, NULL, ?4, 5, 'ungrouped', ?5, 1)",
                params![
                    "m7-initial-ungrouped-5",
                    project.id.to_string(),
                    initial_run_id,
                    asset_ids[5],
                    timestamp(&now()),
                ],
            )
            .unwrap();

        let window = repository
            .moment_incremental_analysis_window(&project.id, None)
            .unwrap()
            .unwrap();
        assert_eq!(window.timeline_id, timeline_id);
        assert_eq!(window.active_run_id, initial_run_id);
        assert_eq!(
            window.previous_latest_captured_at.as_deref(),
            Some("2025-01-01T10:00:04+00:00")
        );
        assert_eq!(window.global_ordinal_base, 6);
        assert_eq!(window.affected_tail_start_ordinal, 2);
        assert_eq!(window.moment_ordinal_base, 1);
        assert_eq!(
            window
                .preceding_context
                .iter()
                .map(|input| input.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                asset_ids[2].as_str(),
                asset_ids[3].as_str(),
                asset_ids[4].as_str()
            ]
        );
        assert_eq!(
            window
                .pending_inputs
                .iter()
                .map(|input| input.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec![asset_ids[5].as_str(), asset_ids[6].as_str()]
        );

        repository
            .rename_moment(&project.id, "m7-moment-1", "Human tail label")
            .unwrap();
        repository
            .set_moment_human_representative(&project.id, "m7-moment-1", &asset_ids[3])
            .unwrap();
        let profile_id = repository.ensure_default_studio_profile().unwrap();
        assert!(
            !repository
                .studio_training_source_state(&profile_id)
                .unwrap()
                .materialization_pending,
            "a successful Moment representative capture clears its atomic source guard"
        );
        repository
            .create_coverage_checklist_item(&CoverageChecklistItemRecord {
                id: "m7-tail-checklist".into(),
                project_id: project.id.to_string(),
                text: "Photographer-confirmed tail coverage".into(),
                created_at: timestamp(&now()),
            })
            .unwrap();
        repository
            .update_coverage_confirmation(
                &project.id,
                "m7-tail-checklist",
                "confirmed_covered",
                Some("m7-moment-1"),
                Some(&asset_ids[2]),
            )
            .unwrap();
        repository
            .connection
            .execute(
                "INSERT INTO moment_override_operations (id, project_id, operation, left_asset_id, right_asset_id, created_at, active)
                 VALUES (?1, ?2, 'split', ?3, ?4, ?5, 1)",
                params![
                    "m7-active-split",
                    project.id.to_string(),
                    asset_ids[2],
                    asset_ids[3],
                    timestamp(&now()),
                ],
            )
            .unwrap();

        let tail_run_id = "m7-tail-replacement-run";
        let tail_timeline = m7_test_timeline(&project.id, timeline_id, initial_run_id);
        let tail_run = m7_test_run(&project.id, timeline_id, tail_run_id, 5);
        let tail_segments = vec![
            m7_test_segment(&project.id, tail_run_id, "m7-tail-segment-1", 1, 1),
            m7_test_segment(&project.id, tail_run_id, "m7-tail-segment-2", 2, 4),
        ];
        let tail_moments = vec![
            m7_test_moment(
                &project.id,
                timeline_id,
                tail_run_id,
                "m7-tail-segment-1",
                "m7-tail-moment-1",
                &asset_ids[2],
                1,
                1,
            ),
            m7_test_moment(
                &project.id,
                timeline_id,
                tail_run_id,
                "m7-tail-segment-2",
                "m7-tail-moment-2",
                &asset_ids[3],
                2,
                4,
            ),
        ];
        let tail_memberships = vec![
            m7_test_membership(
                &project.id,
                tail_run_id,
                "m7-tail-member-2",
                Some("m7-tail-moment-1"),
                &asset_ids[2],
                2,
                "member",
            ),
            m7_test_membership(
                &project.id,
                tail_run_id,
                "m7-tail-member-3",
                Some("m7-tail-moment-2"),
                &asset_ids[3],
                3,
                "member",
            ),
            m7_test_membership(
                &project.id,
                tail_run_id,
                "m7-tail-member-4",
                Some("m7-tail-moment-2"),
                &asset_ids[4],
                4,
                "member",
            ),
            m7_test_membership(
                &project.id,
                tail_run_id,
                "m7-tail-member-5",
                Some("m7-tail-moment-2"),
                &asset_ids[5],
                5,
                "member",
            ),
            m7_test_membership(
                &project.id,
                tail_run_id,
                "m7-tail-member-6",
                Some("m7-tail-moment-2"),
                &asset_ids[6],
                6,
                "member",
            ),
        ];
        let tail_boundaries = vec![MomentBoundaryEvidenceRecord {
            id: "m7-tail-boundary-4".into(),
            project_id: project.id.to_string(),
            run_id: tail_run_id.into(),
            left_asset_id: asset_ids[2].clone(),
            right_asset_id: asset_ids[3].clone(),
            ordinal: 0,
            category: "strong".into(),
            components: serde_json::json!({"test": "tail boundary"}),
            explanation: "Local structural evidence supports this boundary.".into(),
            created_at: timestamp(&now()),
        }];
        // A direct persistence caller cannot leave a valid-looking tail with a gap between the
        // preserved prefix and its replacement cards. The local tail ordinals are contiguous,
        // so this specifically exercises the combined active-projection check.
        let mut malformed_tail_segments = tail_segments.clone();
        malformed_tail_segments[0].ordinal = 3;
        malformed_tail_segments[1].ordinal = 4;
        let mut malformed_tail_moments = tail_moments.clone();
        malformed_tail_moments[0].ordinal = 3;
        malformed_tail_moments[1].ordinal = 4;
        let malformed_tail_error = repository
            .replace_active_moment_analysis_tail(
                &tail_timeline,
                &tail_run,
                window.affected_tail_start_ordinal,
                &malformed_tail_segments,
                &malformed_tail_moments,
                &tail_memberships,
                &tail_boundaries,
            )
            .unwrap_err();
        assert!(malformed_tail_error
            .to_string()
            .contains("active Moment ordinals must be contiguous from 0"));
        assert_eq!(
            repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(initial_run_id),
            "a rejected direct tail payload must leave the prior timeline active"
        );
        let mut duplicate_head_tail_memberships = tail_memberships.clone();
        duplicate_head_tail_memberships.push(m7_test_membership(
            &project.id,
            tail_run_id,
            "m7-tail-duplicate-preserved-head",
            Some("m7-tail-moment-2"),
            &asset_ids[0],
            7,
            "member",
        ));
        let mut duplicate_head_tail_moments = tail_moments.clone();
        duplicate_head_tail_moments[1].asset_count = 5;
        let duplicate_head_error = repository
            .replace_active_moment_analysis_tail(
                &tail_timeline,
                &tail_run,
                window.affected_tail_start_ordinal,
                &tail_segments,
                &duplicate_head_tail_moments,
                &duplicate_head_tail_memberships,
                &tail_boundaries,
            )
            .unwrap_err();
        assert!(duplicate_head_error
            .to_string()
            .contains("repeats a preserved active media asset"));
        assert_eq!(
            repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(initial_run_id),
            "a duplicate preserved asset must roll back the attempted tail replacement"
        );
        repository
            .replace_active_moment_analysis_tail(
                &tail_timeline,
                &tail_run,
                window.affected_tail_start_ordinal,
                &tail_segments,
                &tail_moments,
                &tail_memberships,
                &tail_boundaries,
            )
            .unwrap();

        let status = repository
            .moment_timeline_status(&project.id)
            .unwrap()
            .unwrap();
        assert_eq!(status.timeline_id, timeline_id);
        assert_eq!(status.active_run_id.as_deref(), Some(tail_run_id));
        assert_eq!(status.moment_count, 3);
        assert_eq!(status.eligible_count, 7);
        assert_eq!(status.ungrouped_count, 0);
        let stale_initial_tail_moment: bool = repository
            .connection
            .query_row(
                "SELECT stale FROM moment_records WHERE id = 'm7-moment-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let preserved_initial_moment: bool = repository
            .connection
            .query_row(
                "SELECT stale FROM moment_records WHERE id = 'm7-moment-0'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let old_ungrouped_active: bool = repository
            .connection
            .query_row(
                "SELECT active FROM moment_memberships WHERE id = 'm7-initial-ungrouped-5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stale_initial_tail_moment);
        assert!(!preserved_initial_moment);
        assert!(!old_ungrouped_active);

        let page = repository.moment_timeline_page(&project.id, 10, 0).unwrap();
        assert!(page.moments.iter().any(|row| row.id == "m7-moment-0"));
        let renamed_tail = page
            .moments
            .iter()
            .find(|row| row.id == "m7-tail-moment-1")
            .unwrap();
        assert_eq!(renamed_tail.display_label, "Human tail label");
        assert!(renamed_tail.human_override_present);
        assert_eq!(renamed_tail.human_representative_asset_id, None);
        let remapped_tail_confirmation = repository.coverage_checklist_items(&project.id).unwrap();
        assert_eq!(
            remapped_tail_confirmation[0].moment_id.as_deref(),
            Some("m7-tail-moment-1")
        );
        assert_eq!(
            remapped_tail_confirmation[0].media_asset_id.as_deref(),
            Some(asset_ids[2].as_str())
        );
        let boundary_tail = page
            .moments
            .iter()
            .find(|row| row.id == "m7-tail-moment-2")
            .unwrap();
        assert_eq!(boundary_tail.boundary_category.as_deref(), Some("strong"));
        assert_eq!(
            boundary_tail.boundary_explanation.as_deref(),
            Some("Local structural evidence supports this boundary.")
        );
        assert_eq!(
            boundary_tail.human_representative_asset_id.as_deref(),
            Some(asset_ids[3].as_str()),
            "a generated tail replacement follows the photographer-selected asset to its new card"
        );

        // A local tail can preserve an older-run prefix. Editing that prefix must atomically
        // shift the newer tail block too; reindexing only the old run would leave 0, 2, 3 or
        // collide with the tail on a subsequent split.
        repository
            .split_moment(&project.id, "m7-moment-0", &asset_ids[0])
            .unwrap();
        let split_prefix_moment_id: String = repository
            .connection
            .query_row(
                "SELECT id FROM moment_records
                 WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                params![project.id.to_string(), asset_ids[1]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![
                ("m7-moment-0".into(), 0),
                (split_prefix_moment_id.clone(), 1),
                ("m7-tail-moment-1".into(), 2),
                ("m7-tail-moment-2".into(), 3),
            ]
        );
        repository
            .merge_adjacent_moments(&project.id, "m7-moment-0", &split_prefix_moment_id)
            .unwrap();
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![
                ("m7-moment-0".into(), 0),
                ("m7-tail-moment-1".into(), 1),
                ("m7-tail-moment-2".into(), 2),
            ]
        );
        assert_m7_projection_integrity(&repository, &project.id);

        // The active projection is deliberately composed from preserved `m7-moment-0` in the
        // original run and this new tail run. Reordering the tail must retain its display base
        // of 1; resetting it to 0 would collide with the preserved prefix in the timeline UI.
        let cross_run_error = repository
            .merge_adjacent_moments(&project.id, "m7-moment-0", "m7-tail-moment-1")
            .unwrap_err()
            .to_string();
        assert!(cross_run_error.contains("different local analysis runs"));
        repository
            .merge_adjacent_moments(&project.id, "m7-tail-moment-1", "m7-tail-moment-2")
            .unwrap();
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![("m7-moment-0".into(), 0), ("m7-tail-moment-1".into(), 1),]
        );
        assert_m7_projection_integrity(&repository, &project.id);

        // This was the production failure shape: a stale absorbed tail row still occupies the
        // run's old ordinal, then a split has to insert immediately after the surviving card.
        // The two-phase reindex keeps the tail range at 1.. rather than causing duplicate 0s.
        repository
            .split_moment(&project.id, "m7-tail-moment-1", &asset_ids[3])
            .unwrap();
        let split_tail_moment_id: String = repository
            .connection
            .query_row(
                "SELECT id FROM moment_records
                 WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                params![project.id.to_string(), asset_ids[4]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![
                ("m7-moment-0".into(), 0),
                ("m7-tail-moment-1".into(), 1),
                (split_tail_moment_id, 2),
            ]
        );
        assert_m7_projection_integrity(&repository, &project.id);
        let post_edit_window = repository
            .moment_incremental_analysis_window(&project.id, None)
            .unwrap()
            .unwrap();
        assert_eq!(post_edit_window.affected_tail_start_ordinal, 2);
        assert_eq!(post_edit_window.moment_ordinal_base, 1);
        let rename_event_count: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1 AND event_type = 'MOMENT_RENAMED'",
                params![project.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rename_event_count, 1);

        // Reopen before a real second tail replacement: the persisted tail base, label,
        // representative, and coverage confirmation must all remain usable after restart.
        drop(repository);
        let repository = SqliteRepository::open(&catalog).unwrap();
        let reopened_window = repository
            .moment_incremental_analysis_window(&project.id, None)
            .unwrap()
            .unwrap();
        assert_eq!(reopened_window.affected_tail_start_ordinal, 2);
        assert_eq!(reopened_window.moment_ordinal_base, 1);
        let second_tail_run_id = "m7-tail-replacement-run-2";
        let second_tail_timeline = m7_test_timeline(&project.id, timeline_id, tail_run_id);
        let second_tail_run = m7_test_run(&project.id, timeline_id, second_tail_run_id, 5);
        let second_tail_segments = vec![
            m7_test_segment(&project.id, second_tail_run_id, "m7-tail-2-segment-1", 1, 2),
            m7_test_segment(&project.id, second_tail_run_id, "m7-tail-2-segment-2", 2, 3),
        ];
        let second_tail_moments = vec![
            m7_test_moment(
                &project.id,
                timeline_id,
                second_tail_run_id,
                "m7-tail-2-segment-1",
                "m7-tail-2-moment-1",
                &asset_ids[2],
                1,
                2,
            ),
            m7_test_moment(
                &project.id,
                timeline_id,
                second_tail_run_id,
                "m7-tail-2-segment-2",
                "m7-tail-2-moment-2",
                &asset_ids[4],
                2,
                3,
            ),
        ];
        let second_tail_memberships = vec![
            m7_test_membership(
                &project.id,
                second_tail_run_id,
                "m7-tail-2-member-2",
                Some("m7-tail-2-moment-1"),
                &asset_ids[2],
                2,
                "member",
            ),
            m7_test_membership(
                &project.id,
                second_tail_run_id,
                "m7-tail-2-member-3",
                Some("m7-tail-2-moment-1"),
                &asset_ids[3],
                3,
                "member",
            ),
            m7_test_membership(
                &project.id,
                second_tail_run_id,
                "m7-tail-2-member-4",
                Some("m7-tail-2-moment-2"),
                &asset_ids[4],
                4,
                "member",
            ),
            m7_test_membership(
                &project.id,
                second_tail_run_id,
                "m7-tail-2-member-5",
                Some("m7-tail-2-moment-2"),
                &asset_ids[5],
                5,
                "member",
            ),
            m7_test_membership(
                &project.id,
                second_tail_run_id,
                "m7-tail-2-member-6",
                Some("m7-tail-2-moment-2"),
                &asset_ids[6],
                6,
                "member",
            ),
        ];
        repository
            .replace_active_moment_analysis_tail(
                &second_tail_timeline,
                &second_tail_run,
                reopened_window.affected_tail_start_ordinal,
                &second_tail_segments,
                &second_tail_moments,
                &second_tail_memberships,
                &[MomentBoundaryEvidenceRecord {
                    id: "m7-tail-2-boundary-4".into(),
                    project_id: project.id.to_string(),
                    run_id: second_tail_run_id.into(),
                    left_asset_id: asset_ids[3].clone(),
                    right_asset_id: asset_ids[4].clone(),
                    ordinal: 0,
                    category: "strong".into(),
                    components: serde_json::json!({"test": "second tail boundary"}),
                    explanation: "Local structural evidence supports this boundary.".into(),
                    created_at: timestamp(&now()),
                }],
            )
            .unwrap();
        assert_eq!(
            repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(second_tail_run_id)
        );
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![
                ("m7-moment-0".into(), 0),
                ("m7-tail-2-moment-1".into(), 1),
                ("m7-tail-2-moment-2".into(), 2),
            ]
        );
        let second_tail_page = repository.moment_timeline_page(&project.id, 10, 0).unwrap();
        let second_tail_first = second_tail_page
            .moments
            .iter()
            .find(|moment| moment.id == "m7-tail-2-moment-1")
            .unwrap();
        assert_eq!(second_tail_first.display_label, "Human tail label");
        assert_eq!(
            second_tail_first.human_representative_asset_id.as_deref(),
            Some(asset_ids[3].as_str())
        );
        let second_tail_confirmation = repository.coverage_checklist_items(&project.id).unwrap();
        assert_eq!(
            second_tail_confirmation[0].moment_id.as_deref(),
            Some("m7-tail-2-moment-1")
        );
        assert_eq!(
            second_tail_confirmation[0].media_asset_id.as_deref(),
            Some(asset_ids[2].as_str())
        );
        let stale_confirmation_refs: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM coverage_confirmations confirmation
                 JOIN moment_records moment ON moment.id = confirmation.moment_id
                 WHERE confirmation.project_id = ?1 AND moment.stale = 1",
                params![project.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_confirmation_refs, 0);
        assert_m7_projection_integrity(&repository, &project.id);

        drop(repository);
        let reopened = SqliteRepository::open(&catalog).unwrap();
        assert_eq!(
            reopened
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(second_tail_run_id)
        );
        assert_m7_projection_integrity(&reopened, &project.id);
    }

    #[test]
    fn m7_moment_semantic_search_does_not_stop_after_ten_thousand_moments() {
        const MOMENT_COUNT: usize = 10_001;

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        let timeline_id = "m7-search-large-timeline";
        let run_id = "m7-search-large-run";
        let timeline = m7_test_timeline(&project.id, timeline_id, run_id);
        let run = m7_test_run(&project.id, timeline_id, run_id, MOMENT_COUNT as u64);
        let mut segments = Vec::with_capacity(MOMENT_COUNT);
        let mut moments = Vec::with_capacity(MOMENT_COUNT);
        let mut memberships = Vec::with_capacity(MOMENT_COUNT);
        for ordinal in 0..MOMENT_COUNT {
            let segment_id = format!("m7-search-segment-{ordinal:05}");
            let moment_id = format!("m7-search-moment-{ordinal:05}");
            let timeline_asset = MediaAsset {
                id: id(1_000_000 + ordinal as u128, MediaAssetId::from_uuid),
                project_id: project.id.clone(),
                media_type: MediaType::RawPhoto,
                display_name: format!("SEARCH_{ordinal:05}.ARW"),
                extension: Some("arw".into()),
                captured_at: Some(now()),
                fingerprint: MediaFingerprint {
                    fast_fingerprint: Some(format!("search-fast-{ordinal}")),
                    byte_size: Some(10),
                    ..Default::default()
                },
                created_at: now(),
            };
            repository.insert_media_asset(&timeline_asset).unwrap();
            let asset_id = timeline_asset.id.to_string();
            segments.push(m7_test_segment(
                &project.id,
                run_id,
                &segment_id,
                ordinal as u64,
                1,
            ));
            let mut moment = m7_test_moment(
                &project.id,
                timeline_id,
                run_id,
                &segment_id,
                &moment_id,
                &asset_id,
                ordinal as u64,
                1,
            );
            moment.centroid = Some(vec![1.0, 0.0]);
            moment.centroid_dimensions = Some(2);
            moments.push(moment);
            memberships.push(m7_test_membership(
                &project.id,
                run_id,
                &format!("m7-search-membership-{ordinal:05}"),
                Some(&moment_id),
                &asset_id,
                ordinal as u64,
                "member",
            ));
        }
        repository
            .replace_active_moment_analysis(&timeline, &run, &segments, &moments, &memberships, &[])
            .unwrap();

        let candidates = repository
            .moment_search_candidates(&project.id, &semantic_model(2, "m7-search"))
            .unwrap();
        assert_eq!(candidates.len(), MOMENT_COUNT);
        assert_eq!(
            candidates.first().unwrap().moment_id,
            "m7-search-moment-00000"
        );
        assert_eq!(
            candidates.last().unwrap().moment_id,
            "m7-search-moment-10000"
        );
    }

    #[test]
    fn m7_human_moment_edits_and_checklist_confirmation_survive_file_backed_restart() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("m7-restart.sqlite3");
        let (project_id, anchor_asset_id, representative_asset_id) = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let project = project();
            repository.insert_project(&project).unwrap();
            repository.insert_storage_volume(&volume()).unwrap();
            let assets = (0_u128..4)
                .map(|offset| culling_asset(&repository, &project.id, 90_000 + offset))
                .collect::<Vec<_>>();
            let asset_ids = assets
                .iter()
                .map(|asset| asset.id.to_string())
                .collect::<Vec<_>>();
            let timeline_id = "m7-restart-timeline";
            let run_id = "m7-restart-run";
            let timeline = m7_test_timeline(&project.id, timeline_id, run_id);
            let run = m7_test_run(&project.id, timeline_id, run_id, 4);
            let segments = vec![
                m7_test_segment(&project.id, run_id, "m7-restart-segment-0", 0, 2),
                m7_test_segment(&project.id, run_id, "m7-restart-segment-1", 1, 2),
            ];
            let moments = vec![
                m7_test_moment(
                    &project.id,
                    timeline_id,
                    run_id,
                    "m7-restart-segment-0",
                    "m7-restart-moment-0",
                    &asset_ids[0],
                    0,
                    2,
                ),
                m7_test_moment(
                    &project.id,
                    timeline_id,
                    run_id,
                    "m7-restart-segment-1",
                    "m7-restart-moment-1",
                    &asset_ids[2],
                    1,
                    2,
                ),
            ];
            let memberships = vec![
                m7_test_membership(
                    &project.id,
                    run_id,
                    "m7-restart-member-0",
                    Some("m7-restart-moment-0"),
                    &asset_ids[0],
                    0,
                    "member",
                ),
                m7_test_membership(
                    &project.id,
                    run_id,
                    "m7-restart-member-1",
                    Some("m7-restart-moment-0"),
                    &asset_ids[1],
                    1,
                    "member",
                ),
                m7_test_membership(
                    &project.id,
                    run_id,
                    "m7-restart-member-2",
                    Some("m7-restart-moment-1"),
                    &asset_ids[2],
                    2,
                    "member",
                ),
                m7_test_membership(
                    &project.id,
                    run_id,
                    "m7-restart-member-3",
                    Some("m7-restart-moment-1"),
                    &asset_ids[3],
                    3,
                    "member",
                ),
            ];
            repository
                .replace_active_moment_analysis(
                    &timeline,
                    &run,
                    &segments,
                    &moments,
                    &memberships,
                    &[],
                )
                .unwrap();
            repository
                .rename_moment(&project.id, "m7-restart-moment-0", "Photographer label")
                .unwrap();
            repository
                .set_moment_human_representative(&project.id, "m7-restart-moment-0", &asset_ids[1])
                .unwrap();
            repository
                .split_moment(&project.id, "m7-restart-moment-0", &asset_ids[0])
                .unwrap();
            let split_moment_id: String = repository
                .connection
                .query_row(
                    "SELECT id FROM moment_records
                     WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                    params![project.id.to_string(), asset_ids[1]],
                    |row| row.get(0),
                )
                .unwrap();
            repository
                .merge_adjacent_moments(&project.id, "m7-restart-moment-0", &split_moment_id)
                .unwrap();
            repository
                .create_coverage_checklist_item(&CoverageChecklistItemRecord {
                    id: "m7-restart-checklist".into(),
                    project_id: project.id.to_string(),
                    text: "Photographer-provided checklist phrase".into(),
                    created_at: timestamp(&now()),
                })
                .unwrap();
            repository
                .update_coverage_confirmation(
                    &project.id,
                    "m7-restart-checklist",
                    "confirmed_covered",
                    Some("m7-restart-moment-0"),
                    Some(&asset_ids[0]),
                )
                .unwrap();
            (project.id, asset_ids[0].clone(), asset_ids[1].clone())
        };

        let reopened = SqliteRepository::open(&catalog).unwrap();
        let page = reopened.moment_timeline_page(&project_id, 10, 0).unwrap();
        let edited = page
            .moments
            .iter()
            .find(|row| row.id == "m7-restart-moment-0")
            .unwrap();
        assert_eq!(edited.display_label, "Photographer label");
        assert_eq!(
            edited.human_representative_asset_id.as_deref(),
            Some(representative_asset_id.as_str())
        );
        let operations = reopened
            .active_moment_override_operations(&project_id)
            .unwrap();
        assert!(operations
            .iter()
            .any(|operation| operation.operation == "split"));
        assert!(operations
            .iter()
            .any(|operation| operation.operation == "merge"));
        let checklist = reopened.coverage_checklist_items(&project_id).unwrap();
        assert_eq!(checklist.len(), 1);
        assert_eq!(
            checklist[0].confirmation_state.as_deref(),
            Some("confirmed_covered")
        );
        assert_eq!(
            checklist[0].moment_id.as_deref(),
            Some("m7-restart-moment-0")
        );
        assert_eq!(
            checklist[0].media_asset_id.as_deref(),
            Some(anchor_asset_id.as_str())
        );
        let human_event_count: i64 = reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM moment_events
                 WHERE project_id = ?1
                   AND event_type IN (
                       'MOMENT_RENAMED',
                       'MOMENT_REPRESENTATIVE_CHANGED',
                       'MOMENT_SPLIT',
                       'MOMENT_MERGED',
                       'COVERAGE_CONFIRMED'
                   )",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        // The representative follows the selected asset through split and merge. Each rebind is
        // preserved as an additional append-only human event rather than rewriting the original
        // representative choice out of history.
        assert_eq!(human_event_count, 7);
    }

    #[test]
    fn m7_structural_edits_rebind_human_representatives_and_stale_absorbed_segments() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let asset_ids = (0_u128..4)
            .map(|offset| {
                culling_asset(&repository, &project.id, 94_000 + offset)
                    .id
                    .to_string()
            })
            .collect::<Vec<_>>();
        let timeline_id = "m7-representative-timeline";
        let run_id = "m7-representative-run";
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&project.id, timeline_id, run_id),
                &m7_test_run(&project.id, timeline_id, run_id, 4),
                &[
                    m7_test_segment(&project.id, run_id, "m7-representative-segment-0", 0, 2),
                    m7_test_segment(&project.id, run_id, "m7-representative-segment-1", 1, 2),
                ],
                &[
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        run_id,
                        "m7-representative-segment-0",
                        "m7-representative-moment-0",
                        &asset_ids[0],
                        0,
                        2,
                    ),
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        run_id,
                        "m7-representative-segment-1",
                        "m7-representative-moment-1",
                        &asset_ids[2],
                        1,
                        2,
                    ),
                ],
                &[
                    m7_test_membership(
                        &project.id,
                        run_id,
                        "m7-representative-member-0",
                        Some("m7-representative-moment-0"),
                        &asset_ids[0],
                        0,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        run_id,
                        "m7-representative-member-1",
                        Some("m7-representative-moment-0"),
                        &asset_ids[1],
                        1,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        run_id,
                        "m7-representative-member-2",
                        Some("m7-representative-moment-1"),
                        &asset_ids[2],
                        2,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        run_id,
                        "m7-representative-member-3",
                        Some("m7-representative-moment-1"),
                        &asset_ids[3],
                        3,
                        "member",
                    ),
                ],
                &[],
            )
            .unwrap();

        repository
            .rename_moment(
                &project.id,
                "m7-representative-moment-0",
                "Left-side human label",
            )
            .unwrap();
        repository
            .rename_moment(
                &project.id,
                "m7-representative-moment-1",
                "Right-side human label",
            )
            .unwrap();
        let before_conflicting_label_merge_events: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                params![project.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let conflicting_label_merge = repository
            .merge_adjacent_moments(
                &project.id,
                "m7-representative-moment-0",
                "m7-representative-moment-1",
            )
            .unwrap_err();
        assert!(conflicting_label_merge
            .to_string()
            .contains("Cannot merge Moments with different human labels"));
        assert_eq!(
            active_moment_ordinals(&repository, &project.id),
            vec![
                ("m7-representative-moment-0".into(), 0),
                ("m7-representative-moment-1".into(), 1),
            ],
            "a conflicting label merge must not change the active projection"
        );
        let after_conflicting_label_merge_events: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                params![project.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            after_conflicting_label_merge_events, before_conflicting_label_merge_events,
            "a rejected label merge must not append an override or event"
        );

        repository
            .set_moment_human_representative(
                &project.id,
                "m7-representative-moment-0",
                &asset_ids[1],
            )
            .unwrap();
        repository
            .split_moment(&project.id, "m7-representative-moment-0", &asset_ids[0])
            .unwrap();
        let split_moment_id: String = repository
            .connection
            .query_row(
                "SELECT id FROM moment_records
                 WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                params![project.id.to_string(), asset_ids[1]],
                |row| row.get(0),
            )
            .unwrap();
        let after_split = repository.moment_timeline_page(&project.id, 10, 0).unwrap();
        assert_eq!(
            after_split
                .moments
                .iter()
                .find(|moment| moment.id == "m7-representative-moment-0")
                .unwrap()
                .human_representative_asset_id,
            None
        );
        assert_eq!(
            after_split
                .moments
                .iter()
                .find(|moment| moment.id == split_moment_id)
                .unwrap()
                .human_representative_asset_id
                .as_deref(),
            Some(asset_ids[1].as_str())
        );

        // The destination currently has no representative, so a human choice on the absorbed
        // right card must rebind to the surviving left card during merge.
        repository
            .connection
            .execute(
                "DELETE FROM moment_human_representatives WHERE project_id = ?1 AND anchor_asset_id = ?2",
                params![project.id.to_string(), asset_ids[1]],
            )
            .unwrap();
        repository
            .set_moment_human_representative(
                &project.id,
                "m7-representative-moment-1",
                &asset_ids[3],
            )
            .unwrap();
        repository
            .rename_moment(
                &project.id,
                "m7-representative-moment-1",
                "Right-side human label",
            )
            .unwrap();
        repository
            .merge_adjacent_moments(&project.id, &split_moment_id, "m7-representative-moment-1")
            .unwrap();
        let after_merge = repository.moment_timeline_page(&project.id, 10, 0).unwrap();
        assert_eq!(
            after_merge
                .moments
                .iter()
                .find(|moment| moment.id == split_moment_id)
                .unwrap()
                .human_representative_asset_id
                .as_deref(),
            Some(asset_ids[3].as_str())
        );
        assert_eq!(
            after_merge
                .moments
                .iter()
                .find(|moment| moment.id == split_moment_id)
                .unwrap()
                .display_label,
            "Right-side human label",
            "a right-side human label must move to the surviving card during a merge"
        );
        let absorbed_segment_stale: bool = repository
            .connection
            .query_row(
                "SELECT stale FROM timeline_segments WHERE id = 'm7-representative-segment-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(absorbed_segment_stale);
        assert_m7_projection_integrity(&repository, &project.id);
    }

    #[test]
    fn m7_full_replacement_rebinds_a_resegmented_human_representative() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = project();
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let asset_ids = (0_u128..4)
            .map(|offset| {
                culling_asset(&repository, &project.id, 95_000 + offset)
                    .id
                    .to_string()
            })
            .collect::<Vec<_>>();
        let timeline_id = "m7-full-representative-timeline";
        let initial_run_id = "m7-full-representative-run-1";
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&project.id, timeline_id, initial_run_id),
                &m7_test_run(&project.id, timeline_id, initial_run_id, 4),
                &[
                    m7_test_segment(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-segment-0",
                        0,
                        2,
                    ),
                    m7_test_segment(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-segment-1",
                        1,
                        2,
                    ),
                ],
                &[
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        initial_run_id,
                        "m7-full-representative-segment-0",
                        "m7-full-representative-moment-0",
                        &asset_ids[0],
                        0,
                        2,
                    ),
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        initial_run_id,
                        "m7-full-representative-segment-1",
                        "m7-full-representative-moment-1",
                        &asset_ids[2],
                        1,
                        2,
                    ),
                ],
                &[
                    m7_test_membership(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-member-0",
                        Some("m7-full-representative-moment-0"),
                        &asset_ids[0],
                        0,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-member-1",
                        Some("m7-full-representative-moment-0"),
                        &asset_ids[1],
                        1,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-member-2",
                        Some("m7-full-representative-moment-1"),
                        &asset_ids[2],
                        2,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        initial_run_id,
                        "m7-full-representative-member-3",
                        Some("m7-full-representative-moment-1"),
                        &asset_ids[3],
                        3,
                        "member",
                    ),
                ],
                &[],
            )
            .unwrap();
        repository
            .set_moment_human_representative(
                &project.id,
                "m7-full-representative-moment-0",
                &asset_ids[1],
            )
            .unwrap();
        repository
            .rename_moment(
                &project.id,
                "m7-full-representative-moment-1",
                "Resegmented human label",
            )
            .unwrap();
        repository
            .rename_moment(
                &project.id,
                "m7-full-representative-moment-0",
                "Conflicting human label",
            )
            .unwrap();
        let conflicting_rebuild_run_id = "m7-full-representative-conflicting-run";
        let conflicting_rebuild_error = repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&project.id, timeline_id, conflicting_rebuild_run_id),
                &m7_test_run(&project.id, timeline_id, conflicting_rebuild_run_id, 4),
                &[m7_test_segment(
                    &project.id,
                    conflicting_rebuild_run_id,
                    "m7-full-representative-conflicting-segment",
                    0,
                    4,
                )],
                &[m7_test_moment(
                    &project.id,
                    timeline_id,
                    conflicting_rebuild_run_id,
                    "m7-full-representative-conflicting-segment",
                    "m7-full-representative-conflicting-moment",
                    &asset_ids[0],
                    0,
                    4,
                )],
                &[
                    m7_test_membership(
                        &project.id,
                        conflicting_rebuild_run_id,
                        "m7-full-representative-conflicting-member-0",
                        Some("m7-full-representative-conflicting-moment"),
                        &asset_ids[0],
                        0,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        conflicting_rebuild_run_id,
                        "m7-full-representative-conflicting-member-1",
                        Some("m7-full-representative-conflicting-moment"),
                        &asset_ids[1],
                        1,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        conflicting_rebuild_run_id,
                        "m7-full-representative-conflicting-member-2",
                        Some("m7-full-representative-conflicting-moment"),
                        &asset_ids[2],
                        2,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        conflicting_rebuild_run_id,
                        "m7-full-representative-conflicting-member-3",
                        Some("m7-full-representative-conflicting-moment"),
                        &asset_ids[3],
                        3,
                        "member",
                    ),
                ],
                &[],
            )
            .unwrap_err();
        assert!(conflicting_rebuild_error
            .to_string()
            .contains("multiple human Moment labels would converge"));
        assert_eq!(
            repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(initial_run_id),
            "a conflicting generated resegmentation must preserve the previous active run"
        );
        repository
            .rename_moment(
                &project.id,
                "m7-full-representative-moment-0",
                "Resegmented human label",
            )
            .unwrap();
        repository
            .create_coverage_checklist_item(&CoverageChecklistItemRecord {
                id: "m7-full-representative-checklist".into(),
                project_id: project.id.to_string(),
                text: "Anchor-only photographer coverage".into(),
                created_at: timestamp(&now()),
            })
            .unwrap();
        repository
            .update_coverage_confirmation(
                &project.id,
                "m7-full-representative-checklist",
                "confirmed_covered",
                Some("m7-full-representative-moment-0"),
                None,
            )
            .unwrap();

        // The selected asset moves away from the source anchor when a full local rebuild
        // resegments the cards. The photographer's current representative must follow it.
        let replacement_run_id = "m7-full-representative-run-2";
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&project.id, timeline_id, replacement_run_id),
                &m7_test_run(&project.id, timeline_id, replacement_run_id, 4),
                &[
                    m7_test_segment(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-segment-0",
                        0,
                        1,
                    ),
                    m7_test_segment(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-segment-1",
                        1,
                        3,
                    ),
                ],
                &[
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        replacement_run_id,
                        "m7-full-representative-replacement-segment-0",
                        "m7-full-representative-replacement-moment-0",
                        &asset_ids[0],
                        0,
                        1,
                    ),
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        replacement_run_id,
                        "m7-full-representative-replacement-segment-1",
                        "m7-full-representative-replacement-moment-1",
                        &asset_ids[1],
                        1,
                        3,
                    ),
                ],
                &[
                    m7_test_membership(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-member-0",
                        Some("m7-full-representative-replacement-moment-0"),
                        &asset_ids[0],
                        0,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-member-1",
                        Some("m7-full-representative-replacement-moment-1"),
                        &asset_ids[1],
                        1,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-member-2",
                        Some("m7-full-representative-replacement-moment-1"),
                        &asset_ids[2],
                        2,
                        "member",
                    ),
                    m7_test_membership(
                        &project.id,
                        replacement_run_id,
                        "m7-full-representative-replacement-member-3",
                        Some("m7-full-representative-replacement-moment-1"),
                        &asset_ids[3],
                        3,
                        "member",
                    ),
                ],
                &[],
            )
            .unwrap();
        let page = repository.moment_timeline_page(&project.id, 10, 0).unwrap();
        assert_eq!(
            page.moments
                .iter()
                .find(|moment| moment.id == "m7-full-representative-replacement-moment-0")
                .unwrap()
                .human_representative_asset_id,
            None
        );
        assert_eq!(
            page.moments
                .iter()
                .find(|moment| moment.id == "m7-full-representative-replacement-moment-1")
                .unwrap()
                .human_representative_asset_id
                .as_deref(),
            Some(asset_ids[1].as_str())
        );
        assert_eq!(
            page.moments
                .iter()
                .find(|moment| moment.id == "m7-full-representative-replacement-moment-1")
                .unwrap()
                .display_label,
            "Resegmented human label",
            "a human label must follow its anchor asset through a full resegmentation"
        );
        let source_anchor_row_count: i64 = repository
            .connection
            .query_row(
                "SELECT COUNT(*) FROM moment_human_representatives
                 WHERE project_id = ?1 AND anchor_asset_id = ?2",
                params![project.id.to_string(), asset_ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_anchor_row_count, 0);
        let anchor_only_confirmation = repository.coverage_checklist_items(&project.id).unwrap();
        assert_eq!(
            anchor_only_confirmation[0].moment_id.as_deref(),
            Some("m7-full-representative-replacement-moment-0"),
            "a confirmation without a selected asset uses the stale card anchor conservatively"
        );
        assert_eq!(anchor_only_confirmation[0].media_asset_id, None);
        assert_m7_projection_integrity(&repository, &project.id);
    }

    #[test]
    fn m7_manual_structural_sequences_resequence_ordinals_and_rollback_safely() {
        let directory = tempdir().unwrap();
        let catalog = directory.path().join("m7-ordinal-repair.sqlite3");
        let (project_id, asset_ids, timeline_id, run_id) = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let project = project();
            repository.insert_project(&project).unwrap();
            repository.insert_storage_volume(&volume()).unwrap();
            let assets = (0_u128..6)
                .map(|offset| culling_asset(&repository, &project.id, 93_000 + offset))
                .collect::<Vec<_>>();
            let asset_ids = assets
                .iter()
                .map(|asset| asset.id.to_string())
                .collect::<Vec<_>>();
            let timeline_id = "m7-ordinal-timeline";
            let run_id = "m7-ordinal-run";
            let timeline = m7_test_timeline(&project.id, timeline_id, run_id);
            let run = m7_test_run(&project.id, timeline_id, run_id, 6);
            let segments = (0..3)
                .map(|ordinal| {
                    m7_test_segment(
                        &project.id,
                        run_id,
                        &format!("m7-ordinal-segment-{ordinal}"),
                        ordinal,
                        2,
                    )
                })
                .collect::<Vec<_>>();
            let moments = (0..3)
                .map(|ordinal| {
                    m7_test_moment(
                        &project.id,
                        timeline_id,
                        run_id,
                        &format!("m7-ordinal-segment-{ordinal}"),
                        &format!("m7-ordinal-moment-{ordinal}"),
                        &asset_ids[ordinal * 2],
                        ordinal as u64,
                        2,
                    )
                })
                .collect::<Vec<_>>();
            let memberships = (0..6)
                .map(|ordinal| {
                    m7_test_membership(
                        &project.id,
                        run_id,
                        &format!("m7-ordinal-membership-{ordinal}"),
                        Some(&format!("m7-ordinal-moment-{}", ordinal / 2)),
                        &asset_ids[ordinal],
                        ordinal as u64,
                        "member",
                    )
                })
                .collect::<Vec<_>>();
            repository
                .replace_active_moment_analysis(
                    &timeline,
                    &run,
                    &segments,
                    &moments,
                    &memberships,
                    &[],
                )
                .unwrap();
            repository
                .rename_moment(&project.id, "m7-ordinal-moment-0", "Photographer first")
                .unwrap();
            repository
                .set_moment_human_representative(&project.id, "m7-ordinal-moment-0", &asset_ids[0])
                .unwrap();
            repository
                .create_coverage_checklist_item(&CoverageChecklistItemRecord {
                    id: "m7-ordinal-checklist".into(),
                    project_id: project.id.to_string(),
                    text: "Photographer-provided moment".into(),
                    created_at: timestamp(&now()),
                })
                .unwrap();
            repository
                .update_coverage_confirmation(
                    &project.id,
                    "m7-ordinal-checklist",
                    "confirmed_covered",
                    Some("m7-ordinal-moment-1"),
                    Some(&asset_ids[2]),
                )
                .unwrap();

            // A + B + C becomes AB + C. The right record is preserved as stale history, but
            // the active display ordinal space is immediately compacted to 0, 1.
            repository
                .merge_adjacent_moments(&project.id, "m7-ordinal-moment-0", "m7-ordinal-moment-1")
                .unwrap();
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                vec![
                    ("m7-ordinal-moment-0".into(), 0),
                    ("m7-ordinal-moment-2".into(), 1),
                ]
            );
            assert_m7_projection_integrity(&repository, &project.id);

            // This is the production regression: the stale absorbed record formerly retained
            // ordinal 1 and caused the next split to violate UNIQUE(run_id, ordinal).
            repository
                .split_moment(&project.id, "m7-ordinal-moment-0", &asset_ids[0])
                .unwrap();
            let split_moment_id: String = repository
                .connection
                .query_row(
                    "SELECT id FROM moment_records
                     WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                    params![project.id.to_string(), asset_ids[1]],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                vec![
                    ("m7-ordinal-moment-0".into(), 0),
                    (split_moment_id.clone(), 1),
                    ("m7-ordinal-moment-2".into(), 2),
                ]
            );
            let checklist = repository.coverage_checklist_items(&project.id).unwrap();
            assert_eq!(
                checklist[0].moment_id.as_deref(),
                Some(split_moment_id.as_str()),
                "the existing human confirmation follows its selected asset through a split"
            );
            assert_m7_projection_integrity(&repository, &project.id);

            // Exercise both structural orderings: split -> merge -> split. Every pass must keep
            // active ordinals contiguous while the old generated rows remain available as stale
            // provenance in the same unique run namespace.
            repository
                .merge_adjacent_moments(&project.id, "m7-ordinal-moment-0", &split_moment_id)
                .unwrap();
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                vec![
                    ("m7-ordinal-moment-0".into(), 0),
                    ("m7-ordinal-moment-2".into(), 1),
                ]
            );
            repository
                .split_moment(&project.id, "m7-ordinal-moment-0", &asset_ids[0])
                .unwrap();
            assert_eq!(
                active_moment_ordinals(&repository, &project.id)
                    .into_iter()
                    .map(|(_, ordinal)| ordinal)
                    .collect::<Vec<_>>(),
                vec![0, 1, 2]
            );
            assert_m7_projection_integrity(&repository, &project.id);

            // Abort after merge has recorded its override and reassigned memberships, but before
            // the absorbed Moment can be marked stale. The immediate transaction must roll every
            // preceding write back together.
            let fresh_split_moment_id: String = repository
                .connection
                .query_row(
                    "SELECT id FROM moment_records
                     WHERE project_id = ?1 AND stale = 0 AND anchor_asset_id = ?2",
                    params![project.id.to_string(), asset_ids[1]],
                    |row| row.get(0),
                )
                .unwrap();
            let before_failed_merge = active_moment_ordinals(&repository, &project.id);
            let before_failed_merge_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let before_failed_merge_overrides: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_override_operations WHERE project_id = ?1 AND active = 1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let before_failed_merge_asset_moment: String = repository
                .connection
                .query_row(
                    "SELECT moment_id FROM moment_memberships
                     WHERE project_id = ?1 AND media_asset_id = ?2 AND active = 1",
                    params![project.id.to_string(), asset_ids[4]],
                    |row| row.get(0),
                )
                .unwrap();
            repository
                .connection
                .execute_batch(
                    "CREATE TRIGGER m7_force_merge_failure
                     BEFORE UPDATE OF stale ON moment_records
                     WHEN NEW.id = 'm7-ordinal-moment-2' AND NEW.stale = 1
                     BEGIN
                       SELECT RAISE(ABORT, 'forced Moment merge transaction failure');
                     END;",
                )
                .unwrap();
            assert!(repository
                .merge_adjacent_moments(&project.id, &fresh_split_moment_id, "m7-ordinal-moment-2",)
                .is_err());
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                before_failed_merge
            );
            let after_failed_merge_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let after_failed_merge_overrides: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_override_operations WHERE project_id = ?1 AND active = 1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let after_failed_merge_asset_moment: String = repository
                .connection
                .query_row(
                    "SELECT moment_id FROM moment_memberships
                     WHERE project_id = ?1 AND media_asset_id = ?2 AND active = 1",
                    params![project.id.to_string(), asset_ids[4]],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(after_failed_merge_events, before_failed_merge_events);
            assert_eq!(after_failed_merge_overrides, before_failed_merge_overrides);
            assert_eq!(
                after_failed_merge_asset_moment,
                before_failed_merge_asset_moment
            );
            repository
                .connection
                .execute_batch("DROP TRIGGER m7_force_merge_failure")
                .unwrap();
            assert_m7_projection_integrity(&repository, &project.id);

            let before_failed_split = active_moment_ordinals(&repository, &project.id);
            let before_failed_split_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            repository
                .connection
                .execute_batch(
                    "CREATE TRIGGER m7_force_split_failure
                     BEFORE INSERT ON moment_records
                     WHEN NEW.run_id = 'm7-ordinal-run' AND NEW.stale = 0
                     BEGIN
                       SELECT RAISE(ABORT, 'forced Moment split transaction failure');
                     END;",
                )
                .unwrap();
            assert!(repository
                .split_moment(&project.id, "m7-ordinal-moment-2", &asset_ids[4])
                .is_err());
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                before_failed_split
            );
            let after_failed_split_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(after_failed_split_events, before_failed_split_events);
            repository
                .connection
                .execute_batch("DROP TRIGGER m7_force_split_failure")
                .unwrap();
            assert_m7_projection_integrity(&repository, &project.id);

            let previous_status = repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap();
            let failed_run_id = "m7-ordinal-rollback-run";
            let failed_timeline = m7_test_timeline(&project.id, timeline_id, failed_run_id);
            let failed_run = m7_test_run(&project.id, timeline_id, failed_run_id, 2);
            let failed_segments = vec![
                m7_test_segment(&project.id, failed_run_id, "m7-rollback-segment-0", 0, 1),
                m7_test_segment(&project.id, failed_run_id, "m7-rollback-segment-1", 1, 1),
            ];
            let failed_moments = vec![
                m7_test_moment(
                    &project.id,
                    timeline_id,
                    failed_run_id,
                    "m7-rollback-segment-0",
                    "m7-rollback-moment-0",
                    &asset_ids[0],
                    0,
                    1,
                ),
                m7_test_moment(
                    &project.id,
                    timeline_id,
                    failed_run_id,
                    "m7-rollback-segment-1",
                    "m7-rollback-moment-1",
                    &asset_ids[1],
                    1,
                    1,
                ),
            ];
            let failed_memberships = vec![
                m7_test_membership(
                    &project.id,
                    failed_run_id,
                    "m7-rollback-membership-0",
                    Some("m7-rollback-moment-0"),
                    &asset_ids[0],
                    0,
                    "member",
                ),
                m7_test_membership(
                    &project.id,
                    failed_run_id,
                    "m7-rollback-membership-1",
                    Some("m7-rollback-moment-1"),
                    &asset_ids[1],
                    1,
                    "member",
                ),
            ];
            // Full replacement callers must likewise provide a complete 0..n Moment
            // projection. Reject a malformed direct payload before it can stale the previous
            // timeline or consume its analysis-run ID.
            let mut malformed_full_moments = failed_moments.clone();
            malformed_full_moments[1].ordinal = 2;
            let mut malformed_full_segments = failed_segments.clone();
            malformed_full_segments[1].ordinal = 2;
            let malformed_full_error = repository
                .replace_active_moment_analysis(
                    &failed_timeline,
                    &failed_run,
                    &malformed_full_segments,
                    &malformed_full_moments,
                    &failed_memberships,
                    &[],
                )
                .unwrap_err();
            assert!(malformed_full_error
                .to_string()
                .contains("ordinals must be contiguous from 0"));
            assert_eq!(
                repository
                    .moment_timeline_status(&project.id)
                    .unwrap()
                    .unwrap()
                    .active_run_id,
                previous_status.active_run_id,
                "a rejected direct full payload must leave the prior timeline active"
            );
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                before_failed_split
            );
            let mut malformed_anchor_moments = failed_moments.clone();
            malformed_anchor_moments[0].anchor_asset_id = asset_ids[2].clone();
            let malformed_anchor_error = repository
                .replace_active_moment_analysis(
                    &failed_timeline,
                    &failed_run,
                    &failed_segments,
                    &malformed_anchor_moments,
                    &failed_memberships,
                    &[],
                )
                .unwrap_err();
            assert!(malformed_anchor_error
                .to_string()
                .contains("Moment anchor asset must be a member"));
            let mut duplicate_segment_ordinals = failed_segments.clone();
            duplicate_segment_ordinals[1].ordinal = 0;
            let duplicate_segment_error = repository
                .replace_active_moment_analysis(
                    &failed_timeline,
                    &failed_run,
                    &duplicate_segment_ordinals,
                    &failed_moments,
                    &failed_memberships,
                    &[],
                )
                .unwrap_err();
            assert!(duplicate_segment_error
                .to_string()
                .contains("duplicate timeline segment IDs or ordinals"));
            let before_failed_replacement_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            repository
                .connection
                .execute_batch(
                    "CREATE TRIGGER m7_force_rebuild_failure
                     BEFORE INSERT ON moment_records
                     WHEN NEW.id = 'm7-rollback-moment-1'
                     BEGIN
                       SELECT RAISE(ABORT, 'forced Moment rebuild transaction failure');
                     END;",
                )
                .unwrap();
            assert!(repository
                .replace_active_moment_analysis(
                    &failed_timeline,
                    &failed_run,
                    &failed_segments,
                    &failed_moments,
                    &failed_memberships,
                    &[],
                )
                .is_err());
            repository
                .connection
                .execute_batch("DROP TRIGGER m7_force_rebuild_failure")
                .unwrap();
            let status_after_failed_replacement = repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                status_after_failed_replacement.active_run_id,
                previous_status.active_run_id
            );
            assert_eq!(
                active_moment_ordinals(&repository, &project.id),
                before_failed_split
            );
            let failed_run_count: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_analysis_runs WHERE id = ?1",
                    params![failed_run_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(failed_run_count, 0);
            let after_failed_replacement_events: i64 = repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM moment_events WHERE project_id = ?1",
                    params![project.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                after_failed_replacement_events, before_failed_replacement_events,
                "the first staged Moment event must roll back with the failed replacement"
            );
            assert_m7_projection_integrity(&repository, &project.id);

            (
                project.id,
                asset_ids,
                timeline_id.to_owned(),
                run_id.to_owned(),
            )
        };

        let reopened = SqliteRepository::open(&catalog).unwrap();
        assert_eq!(
            active_moment_ordinals(&reopened, &project_id)
                .into_iter()
                .map(|(_, ordinal)| ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            reopened
                .moment_timeline_status(&project_id)
                .unwrap()
                .unwrap()
                .active_run_id
                .as_deref(),
            Some(run_id.as_str())
        );
        let first = reopened
            .moment_timeline_page(&project_id, 10, 0)
            .unwrap()
            .moments
            .into_iter()
            .find(|moment| moment.id == "m7-ordinal-moment-0")
            .unwrap();
        assert_eq!(first.display_label, "Photographer first");
        assert_eq!(
            first.human_representative_asset_id.as_deref(),
            Some(asset_ids[0].as_str())
        );
        assert_eq!(timeline_id, "m7-ordinal-timeline");
        assert_m7_projection_integrity(&reopened, &project_id);
    }

    #[test]
    fn m7_clock_diagnostics_are_project_scoped_and_tied_to_the_active_run() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let first_project = project();
        let second_project = Project {
            id: id(91_001, ProjectId::from_uuid),
            name: "Second M7 project".into(),
            created_at: now(),
        };
        repository.insert_project(&first_project).unwrap();
        repository.insert_project(&second_project).unwrap();

        let first_timeline_id = "m7-clock-first-timeline";
        let first_run_id = "m7-clock-first-run";
        let second_timeline_id = "m7-clock-second-timeline";
        let second_run_id = "m7-clock-second-run";
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&first_project.id, first_timeline_id, first_run_id),
                &m7_test_run(&first_project.id, first_timeline_id, first_run_id, 0),
                &[],
                &[],
                &[],
                &[],
            )
            .unwrap();
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&second_project.id, second_timeline_id, second_run_id),
                &m7_test_run(&second_project.id, second_timeline_id, second_run_id, 0),
                &[],
                &[],
                &[],
                &[],
            )
            .unwrap();

        let first_diagnostic = CameraClockOffsetDiagnosticRecord {
            id: "m7-clock-first-diagnostic".into(),
            project_id: first_project.id.to_string(),
            run_id: first_run_id.into(),
            camera_a: "Camera A".into(),
            camera_b: "Camera B".into(),
            possible_offset_seconds: Some(120),
            evidence_json: serde_json::json!({
                "state": "advisory",
                "supportingPairCount": 8,
            }),
            created_at: timestamp(&now()),
        };
        let second_diagnostic = CameraClockOffsetDiagnosticRecord {
            id: "m7-clock-second-diagnostic".into(),
            project_id: second_project.id.to_string(),
            run_id: second_run_id.into(),
            camera_a: "Camera C".into(),
            camera_b: "Camera D".into(),
            // Inconclusive evidence stays explicit and is not coerced to zero seconds.
            possible_offset_seconds: None,
            evidence_json: serde_json::json!({"state": "inconclusive"}),
            created_at: timestamp(&now()),
        };
        repository
            .record_camera_clock_offset_diagnostics(std::slice::from_ref(&first_diagnostic))
            .unwrap();
        repository
            .record_camera_clock_offset_diagnostics(std::slice::from_ref(&second_diagnostic))
            .unwrap();
        assert_eq!(
            repository
                .latest_camera_clock_offset_diagnostics(&first_project.id)
                .unwrap(),
            vec![first_diagnostic]
        );
        assert_eq!(
            repository
                .latest_camera_clock_offset_diagnostics(&second_project.id)
                .unwrap(),
            vec![second_diagnostic]
        );

        // A later active run with no diagnostic must not make a stale diagnostic appear current
        // or be converted into a claim that there is no clock offset.
        let replacement_run_id = "m7-clock-first-replacement-run";
        repository
            .replace_active_moment_analysis(
                &m7_test_timeline(&first_project.id, first_timeline_id, replacement_run_id),
                &m7_test_run(&first_project.id, first_timeline_id, replacement_run_id, 0),
                &[],
                &[],
                &[],
                &[],
            )
            .unwrap();
        assert!(repository
            .latest_camera_clock_offset_diagnostics(&first_project.id)
            .unwrap()
            .is_empty());
        assert_eq!(
            repository
                .latest_camera_clock_offset_diagnostics(&second_project.id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn m6_embeddings_and_history_are_project_scoped_and_model_changes_stale_prior_vectors() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let first_project = project();
        let second_project = Project {
            id: id(200, ProjectId::from_uuid),
            name: "Second project".into(),
            created_at: now(),
        };
        let first_asset = asset(first_project.id.clone());
        repository.insert_project(&first_project).unwrap();
        repository.insert_project(&second_project).unwrap();
        repository.insert_media_asset(&first_asset).unwrap();
        let mut video = first_asset.clone();
        video.id = id(201, MediaAssetId::from_uuid);
        video.media_type = MediaType::Video;
        video.display_name = "clip.mov".into();
        video.extension = Some("mov".into());
        repository.insert_media_asset(&video).unwrap();
        let metadata_only = repository
            .semantic_metadata_candidates(
                &first_project.id,
                &SemanticMetadataQuery {
                    limit: 10,
                    ..SemanticMetadataQuery::default()
                },
            )
            .unwrap();
        assert_eq!(
            metadata_only
                .iter()
                .map(|candidate| candidate.asset_id.as_str())
                .collect::<Vec<_>>(),
            vec![first_asset.id.to_string()]
        );
        let model = semantic_model(3, "v1");

        let wrong_project = SemanticEmbeddingRecord {
            media_asset_id: first_asset.id.clone(),
            project_id: second_project.id.clone(),
            input_fingerprint: "input-v1".into(),
            model: model.clone(),
            embedding: Some(vec![1.0, 0.0, 0.0]),
            generated_at: now(),
            status: AnalysisStatus::Ready,
            error_message: None,
        };
        assert!(repository
            .upsert_semantic_embedding(&wrong_project)
            .is_err());

        repository
            .upsert_semantic_embedding(&SemanticEmbeddingRecord {
                project_id: first_project.id.clone(),
                ..wrong_project
            })
            .unwrap();
        assert_eq!(
            repository
                .semantic_embeddings_for_index(&first_project.id, &model)
                .unwrap()
                .len(),
            1
        );

        let replacement = semantic_model(4, "v2");
        assert_eq!(
            repository
                .mark_other_semantic_embeddings_stale(&first_project.id, &replacement)
                .unwrap(),
            1
        );
        assert!(repository
            .semantic_embeddings_for_index(&first_project.id, &model)
            .unwrap()
            .is_empty());

        repository
            .record_magic_search_history(
                &first_project.id,
                "yellow boat",
                "yellow boat",
                &serde_json::json!({ "chips": [] }),
            )
            .unwrap();
        repository
            .record_magic_search_history(
                &second_project.id,
                "woman in red",
                "woman in red",
                &serde_json::json!({ "chips": [] }),
            )
            .unwrap();
        assert_eq!(
            repository
                .magic_search_history(&first_project.id, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository
                .clear_magic_search_history(&first_project.id)
                .unwrap(),
            1
        );
        assert!(repository
            .magic_search_history(&first_project.id, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            repository
                .magic_search_history(&second_project.id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn studio_brain_materializes_only_explicit_human_signals_and_opt_out_preserves_decisions() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = repository.create_project("Studio fixture").unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let asset = culling_asset(&repository, &project.id, 910_001);
        let initial = repository.studio_brain_project_status(&project.id).unwrap();
        assert_eq!(initial.training_status, "not_ready");
        assert_eq!(initial.eligible_decision_count, 0);
        let profile_id = initial.profile_id.clone();
        let initial_source_revision = repository
            .studio_training_source_revision(&profile_id)
            .unwrap();

        repository
            .update_culling_decision(
                &project.id,
                &asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: Some(5),
                    starred: Some(true),
                    note: Some("Client requested this one".into()),
                    flags: Some(vec!["client".into()]),
                    session_id: None,
                },
            )
            .unwrap();
        assert_eq!(
            repository.ensure_default_studio_profile().unwrap(),
            profile_id
        );
        let examples = repository.studio_training_examples(&profile_id).unwrap();
        assert_eq!(
            examples.len(),
            3,
            "decision, rating, and star are explicit signals"
        );
        assert!(examples
            .iter()
            .all(|example| example.feature_snapshot_json.get("note").is_none()));
        assert!(examples
            .iter()
            .all(|example| example.decision_type != "note"));
        assert!(examples
            .iter()
            .all(|example| example.decision_type != "flags"));
        assert!(examples
            .iter()
            .all(|example| example.recommendation_shown == "unknown"));
        assert!(examples.iter().all(|example| {
            example.feature_snapshot_json.get("anonymousFaceCount")
                == Some(&serde_json::Value::Null)
                && example.feature_snapshot_json.get("similarityGroupSize")
                    == Some(&serde_json::Value::Null)
                && example.feature_snapshot_json.get("momentSize") == Some(&serde_json::Value::Null)
        }));
        assert_eq!(
            repository
                .studio_training_source_revision(&profile_id)
                .unwrap(),
            initial_source_revision + 4,
            "the authoritative human action and each newly materialized explicit source advance the activation guard"
        );
        assert!(
            !repository
                .studio_training_source_state(&profile_id)
                .unwrap()
                .materialization_pending,
            "a successful M5 decision/rating/star capture clears its single atomic source guard"
        );

        let individually_excluded = examples[0].id.clone();
        let before_decision_exclusion = repository
            .studio_training_source_revision(&profile_id)
            .unwrap();
        repository
            .set_studio_training_example_excluded(&profile_id, &individually_excluded, true)
            .unwrap();
        assert!(repository
            .studio_training_examples(&profile_id)
            .unwrap()
            .iter()
            .any(|example| example.id == individually_excluded && !example.training_eligible));
        assert_eq!(
            repository
                .studio_training_source_revision(&profile_id)
                .unwrap(),
            before_decision_exclusion + 1
        );
        repository
            .set_studio_training_example_excluded(&profile_id, &individually_excluded, false)
            .unwrap();

        let before_project_opt_out = repository
            .studio_training_source_revision(&profile_id)
            .unwrap();
        repository
            .set_project_training_included(&profile_id, &project.id, false)
            .unwrap();
        assert_eq!(
            repository
                .studio_training_source_revision(&profile_id)
                .unwrap(),
            before_project_opt_out + 1
        );
        assert!(repository
            .studio_training_examples(&profile_id)
            .unwrap()
            .iter()
            .all(|example| !example.training_eligible));
        let opted_out_asset = culling_asset(&repository, &project.id, 910_002);
        repository
            .update_culling_decision(
                &project.id,
                &opted_out_asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Reject),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        assert_eq!(
            repository
                .studio_training_examples(&profile_id)
                .unwrap()
                .len(),
            3,
            "an excluded project cannot materialize a new live Studio source"
        );
        // Existing legacy history is retained as a normal human decision, but explicit
        // backfill must also honor the current opt-out rather than materializing it later.
        repository
            .connection
            .execute(
                "DELETE FROM studio_training_examples WHERE studio_profile_id = ?1",
                params![profile_id],
            )
            .unwrap();
        assert_eq!(
            repository
                .materialize_historical_studio_training_examples(&profile_id)
                .unwrap(),
            0,
            "historical opt-out prevents source-row creation as well as fitting"
        );
        assert!(repository
            .studio_training_examples(&profile_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            repository
                .culling_workspace(&project.id, &CullingQuery::default())
                .unwrap()
                .items
                .first()
                .and_then(|row| row.decision.decision.as_deref()),
            Some("keep"),
            "training opt-out must not alter a human culling decision"
        );

        repository
            .reset_studio_personalization(&profile_id)
            .unwrap();
        assert_eq!(
            culling_decision_for_asset(&repository.connection, &project.id, &asset.id.to_string())
                .unwrap()
                .decision
                .as_deref(),
            Some("keep"),
            "reset removes only derived Studio Brain artifacts"
        );
        let foreign_key_issues: i64 = repository
            .connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_issues, 0);
    }

    #[test]
    fn studio_activation_refuses_a_candidate_when_explicit_sources_changed() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = repository
            .create_project("Studio activation race fixture")
            .unwrap();
        let profile_id = repository.ensure_default_studio_profile().unwrap();
        let source_revision = repository
            .studio_training_source_revision(&profile_id)
            .unwrap();
        let created_at = now();
        let mut job = BackgroundJob {
            id: JobId::new(),
            state: WorkflowRunState::Running,
            stage: JobStage::StudioTraining,
            items_completed: 0,
            items_total: Some(0),
            files_discovered: 0,
            files_processed: 0,
            error_count: 0,
            project_id: Some(project.id.clone()),
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({"pipeline":"studio-training"})),
            created_at,
            updated_at: created_at,
            finished_at: None,
        };
        repository.insert_background_job(&job).unwrap();
        let run_id = "m8-stale-source-run";
        let model_id = "m8-stale-source-candidate";
        let now_text = timestamp(&created_at);
        repository.connection.execute(
            "INSERT INTO studio_training_runs (id, studio_profile_id, background_job_id, algorithm, algorithm_version, feature_schema_version, parameters_json, snapshot_hash, snapshot_count, previous_active_model_id, state, error_message, created_at, updated_at, finished_at) VALUES (?1, ?2, ?3, 'test', 'test', 'studio-feature-v1', '{}', 'snapshot', 0, NULL, 'training', NULL, ?4, ?4, NULL)",
            params![run_id, profile_id, job.id.to_string(), now_text],
        ).unwrap();
        repository.connection.execute(
            "INSERT INTO studio_models (id, studio_profile_id, studio_training_run_id, algorithm, model_version, feature_schema_version, artifact_json, checksum, artifact_size_bytes, state, metrics_json, created_at, activated_at) VALUES (?1, ?2, ?3, 'test', 'test', 'studio-feature-v1', '{}', 'not-read-because-source-is-stale', 0, 'candidate', '{}', ?4, NULL)",
            params![model_id, profile_id, run_id, timestamp(&created_at)],
        ).unwrap();

        // This preference update stands in for a human source arriving after the candidate's
        // snapshot. Its trigger advances the profile generation before activation can swap
        // visibility.
        repository
            .set_project_training_included(&profile_id, &project.id, false)
            .unwrap();
        job.state = WorkflowRunState::Completed;
        job.stage = JobStage::Finalize;
        job.finished_at = Some(created_at);
        assert_eq!(
            repository
                .activate_studio_model(&profile_id, model_id, source_revision, &job)
                .unwrap(),
            StudioModelActivationOutcome::SourceSnapshotStale
        );
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT state FROM studio_models WHERE id = ?1",
                    params![model_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "candidate"
        );
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT state FROM studio_training_runs WHERE id = ?1",
                    params![run_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "training"
        );
        assert_eq!(
            repository
                .get_background_job(&job.id)
                .unwrap()
                .unwrap()
                .state,
            WorkflowRunState::Running
        );
    }

    #[test]
    fn studio_action_scoped_guards_survive_an_unrelated_opted_out_review() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let included_project = repository
            .create_project("Included Studio project")
            .unwrap();
        let opted_out_project = repository
            .create_project("Opted-out Studio project")
            .unwrap();
        repository.insert_storage_volume(&volume()).unwrap();
        let opted_out_asset = culling_asset(&repository, &opted_out_project.id, 920_001);
        let profile_id = repository.ensure_default_studio_profile().unwrap();
        repository
            .set_project_training_included(&profile_id, &opted_out_project.id, false)
            .unwrap();

        // This models an included action whose live compact capture failed after its authority
        // row committed. The guard belongs to that action alone until explicit reconciliation.
        repository
            .connection
            .execute(
                "INSERT INTO studio_source_materialization_guards (studio_profile_id, source_kind, source_record_id, created_at) VALUES (?1, 'm5_culling', 'deferred-included-action', ?2)",
                params![profile_id, timestamp(&now())],
            )
            .unwrap();
        assert!(
            repository
                .studio_training_source_state(&profile_id)
                .unwrap()
                .materialization_pending
        );

        repository
            .update_culling_decision(
                &opted_out_project.id,
                &opted_out_asset.id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Keep),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: None,
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();
        assert!(repository
            .studio_training_source_state(&profile_id)
            .unwrap()
            .materialization_pending,
            "an opted-out action has no matching guard and cannot clear another action's deferred source"
        );
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM studio_source_materialization_guards WHERE studio_profile_id = ?1 AND source_record_id = 'deferred-included-action'",
                    params![profile_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            repository
                .culling_workspace(&opted_out_project.id, &CullingQuery::default())
                .unwrap()
                .items
                .first()
                .and_then(|item| item.decision.decision.as_deref()),
            Some("keep"),
            "Studio guard state never prevents the photographer's authoritative M5 decision"
        );
        assert!(repository
            .get_project(&included_project.id)
            .unwrap()
            .is_some());
    }
}
