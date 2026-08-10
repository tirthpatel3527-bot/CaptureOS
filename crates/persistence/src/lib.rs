//! SQLite catalog persistence. SQL stays behind this repository boundary.

use capture_graph::{EntityRef, Relationship, RelationshipKind};
use chrono::{DateTime, Utc};
use media_model::*;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use thiserror::Error;

const SCHEMA_VERSION: i64 = 11;

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
    pub limit: u32,
    pub offset: u32,
}

impl Default for CullingQuery {
    fn default() -> Self {
        Self {
            mode: "all_photos".into(),
            filter: "all".into(),
            group_id: None,
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
    pub technical_quality_score: Option<f64>,
    pub camera_model: Option<String>,
    pub captured_at: Option<String>,
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
        limit: u32,
        offset: u32,
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
    fn latest_semantic_indexing_job(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<BackgroundJob>>;
    fn semantic_index_terminal_counts(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<SemanticIndexTerminalCounts>;
    fn recover_interrupted_semantic_indexing(&self) -> Result<u64>;
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

    fn count(&self, table: &str) -> Result<u64> {
        // Only literal internal table names are used by `counts`; never accept UI input here.
        self.connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }
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
        let filter_sql = visual_filter_sql(&query.filter);
        let order_sql = visual_sort_sql(query.sort);
        let direction = if query.descending { "DESC" } else { "ASC" };
        let resolved_preview_status = resolved_preview_status_sql();
        let resolved_preview_failure_reason = resolved_preview_failure_reason_sql();
        // `filter_sql`, `order_sql`, and `direction` come exclusively from enums; UI text is
        // bound below and never interpolated into SQL.
        let sql = format!(
            "WITH ranked_instances AS (SELECT fi.id, fi.media_asset_id, fi.relative_path, fi.observed_at, fi.is_available, fi.index_root_id, fi.storage_volume_id, roots.selected_path, volumes.display_name AS volume_name, volumes.id AS volume_id, ROW_NUMBER() OVER (PARTITION BY fi.media_asset_id ORDER BY fi.is_available DESC, CASE WHEN EXISTS (SELECT 1 FROM media_metadata chosen WHERE chosen.media_asset_id = fi.media_asset_id AND chosen.source_file_instance_id = fi.id AND chosen.status = 'ready') THEN 1 ELSE 0 END DESC, fi.observed_at DESC, fi.id ASC) AS instance_rank FROM file_instances fi LEFT JOIN index_roots roots ON roots.id = fi.index_root_id JOIN storage_volumes volumes ON volumes.id = fi.storage_volume_id), primary_instances AS (SELECT * FROM ranked_instances WHERE instance_rank = 1) SELECT a.id, pi.id, a.display_name, a.media_type, a.extension, a.byte_size, COALESCE(m.captured_at_local, a.captured_at, a.observed_modified_at), pi.observed_at, pi.relative_path, pi.selected_path, pi.volume_name, pi.volume_id, pi.is_available, CASE WHEN small.status = 'ready' THEN small.id END, CASE WHEN medium.status = 'ready' THEN medium.id END, CASE WHEN preview.status = 'ready' THEN preview.id END, {resolved_preview_status}, {resolved_preview_failure_reason}, m.width, m.height, m.duration_ms, m.camera_model, m.lens_model, m.codec FROM media_assets a JOIN primary_instances pi ON pi.media_asset_id = a.id LEFT JOIN media_metadata m ON m.media_asset_id = a.id LEFT JOIN preview_artifacts small ON small.media_asset_id = a.id AND small.source_file_instance_id = pi.id AND small.size_class = 'small' AND small.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts medium ON medium.media_asset_id = a.id AND medium.source_file_instance_id = pi.id AND medium.size_class = 'medium' AND medium.artifact_type IN ('thumbnail', 'poster') LEFT JOIN preview_artifacts preview ON preview.media_asset_id = a.id AND preview.source_file_instance_id = pi.id AND preview.size_class = 'preview' AND preview.artifact_type IN ('thumbnail', 'poster') WHERE a.project_id = ?1 AND {filter_sql} AND (?2 IS NULL OR lower(a.display_name) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.camera_model, '')) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(m.lens_model, '')) LIKE '%' || lower(?2) || '%') AND (?3 IS NULL OR lower(COALESCE(m.camera_model, '')) = lower(?3)) AND (?4 IS NULL OR lower(COALESCE(m.lens_model, '')) = lower(?4)) AND (?5 IS NULL OR COALESCE(m.captured_at_local, a.captured_at) >= ?5) AND (?6 IS NULL OR COALESCE(m.captured_at_local, a.captured_at) <= ?6) ORDER BY {order_sql} {direction}, a.id ASC LIMIT ?7 OFFSET ?8"
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
                "SELECT media_asset_id, source_file_instance_id, source_fingerprint, extractor, extractor_version, status, failure_reason, extracted_at, mime_type, byte_size, captured_at_raw, captured_at_local, capture_timezone, capture_time_source, width, height, orientation, camera_make, camera_model, lens_make, lens_model, focal_length_mm, focal_length_equivalent_mm, aperture, shutter_speed, iso, exposure_compensation, flash, white_balance, color_space, gps_present, duration_ms, frame_rate, codec, pixel_format, bitrate, audio_streams, video_streams, sample_rate, bit_depth, channels, raw_metadata_json FROM media_metadata WHERE media_asset_id = ?1",
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

    fn upsert_media_metadata(&self, metadata: &MediaMetadataRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO media_metadata (media_asset_id, source_file_instance_id, source_fingerprint, extractor, extractor_version, status, failure_reason, extracted_at, mime_type, byte_size, captured_at_raw, captured_at_local, capture_timezone, capture_time_source, width, height, orientation, camera_make, camera_model, lens_make, lens_model, focal_length_mm, focal_length_equivalent_mm, aperture, shutter_speed, iso, exposure_compensation, flash, white_balance, color_space, gps_present, duration_ms, frame_rate, codec, pixel_format, bitrate, audio_streams, video_streams, sample_rate, bit_depth, channels, raw_metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42) ON CONFLICT(media_asset_id) DO UPDATE SET source_file_instance_id = excluded.source_file_instance_id, source_fingerprint = excluded.source_fingerprint, extractor = excluded.extractor, extractor_version = excluded.extractor_version, status = excluded.status, failure_reason = excluded.failure_reason, extracted_at = excluded.extracted_at, mime_type = excluded.mime_type, byte_size = excluded.byte_size, captured_at_raw = excluded.captured_at_raw, captured_at_local = excluded.captured_at_local, capture_timezone = excluded.capture_timezone, capture_time_source = excluded.capture_time_source, width = excluded.width, height = excluded.height, orientation = excluded.orientation, camera_make = excluded.camera_make, camera_model = excluded.camera_model, lens_make = excluded.lens_make, lens_model = excluded.lens_model, focal_length_mm = excluded.focal_length_mm, focal_length_equivalent_mm = excluded.focal_length_equivalent_mm, aperture = excluded.aperture, shutter_speed = excluded.shutter_speed, iso = excluded.iso, exposure_compensation = excluded.exposure_compensation, flash = excluded.flash, white_balance = excluded.white_balance, color_space = excluded.color_space, gps_present = excluded.gps_present, duration_ms = excluded.duration_ms, frame_rate = excluded.frame_rate, codec = excluded.codec, pixel_format = excluded.pixel_format, bitrate = excluded.bitrate, audio_streams = excluded.audio_streams, video_streams = excluded.video_streams, sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth, channels = excluded.channels, raw_metadata_json = excluded.raw_metadata_json",
            params![metadata.media_asset_id, metadata.source_file_instance_id, metadata.source_fingerprint, metadata.extractor, metadata.extractor_version, metadata.status, metadata.failure_reason, metadata.extracted_at, metadata.mime_type, metadata.byte_size.map(|value| value as i64), metadata.captured_at_raw, metadata.captured_at_local, metadata.capture_timezone, metadata.capture_time_source, metadata.width.map(|value| value as i64), metadata.height.map(|value| value as i64), metadata.orientation, metadata.camera_make, metadata.camera_model, metadata.lens_make, metadata.lens_model, metadata.focal_length_mm, metadata.focal_length_equivalent_mm, metadata.aperture, metadata.shutter_speed, metadata.iso.map(|value| value as i64), metadata.exposure_compensation, metadata.flash, metadata.white_balance, metadata.color_space, metadata.gps_present, metadata.duration_ms.map(|value| value as i64), metadata.frame_rate, metadata.codec, metadata.pixel_format, metadata.bitrate.map(|value| value as i64), metadata.audio_streams.map(|value| value as i64), metadata.video_streams.map(|value| value as i64), metadata.sample_rate.map(|value| value as i64), metadata.bit_depth.map(|value| value as i64), metadata.channels.map(|value| value as i64), json(&metadata.raw_metadata)?],
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
        let filter_sql = culling_filter_sql(&query.filter)?;
        let session =
            self.start_or_resume_review_session(project_id, &query.mode, Some(&query.filter))?;
        let mode_queue_sql = "(?3 <> 'ai_review_queue' OR EXISTS (SELECT 1 FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 AND r.status = 'ready' AND r.label IN ('strong_candidate', 'strong_alternative', 'review', 'probable_duplicate', 'technical_issue')))";
        let group_sql = "(?3 <> 'similar_sets' OR EXISTS (SELECT 1 FROM similarity_group_members gm JOIN similarity_groups g ON g.id = gm.group_id WHERE gm.media_asset_id = a.id AND g.project_id = ?1 AND g.stale = 0 AND g.id = COALESCE(?2, (SELECT current_group.id FROM similarity_groups current_group WHERE current_group.project_id = ?1 AND current_group.stale = 0 ORDER BY current_group.created_at ASC, current_group.id ASC LIMIT 1))))";
        let sql = format!(
            "SELECT a.id FROM media_assets a LEFT JOIN media_decisions md ON md.media_asset_id = a.id AND md.project_id = ?1 WHERE a.project_id = ?1 AND a.media_type IN ('raw_photo', 'jpeg', 'heif', 'png', 'tiff') AND {mode_queue_sql} AND {group_sql} AND {filter_sql} ORDER BY CASE WHEN ?3 = 'ai_review_queue' THEN CASE (SELECT r.label FROM analysis_recommendations r WHERE r.media_asset_id = a.id AND r.stale = 0 ORDER BY r.generated_at DESC, r.id DESC LIMIT 1) WHEN 'technical_issue' THEN 0 WHEN 'review' THEN 1 WHEN 'probable_duplicate' THEN 2 WHEN 'strong_candidate' THEN 3 WHEN 'strong_alternative' THEN 4 ELSE 5 END ELSE 0 END, COALESCE(a.captured_at, a.created_at), a.id ASC LIMIT ?4 OFFSET ?5"
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
        let before =
            culling_decision_for_asset(&self.connection, project_id, &asset_id.to_string())?;
        let flags = update.flags.as_ref().map(json).transpose()?;
        let now = timestamp(&Utc::now());
        let decision_id = self
            .connection
            .query_row(
                "SELECT id FROM media_decisions WHERE project_id = ?1 AND media_asset_id = ?2",
                params![project_id.to_string(), asset_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| MediaDecisionId::new().to_string());
        self.connection.execute(
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
        let after =
            culling_decision_for_asset(&self.connection, project_id, &asset_id.to_string())?;
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
        for (event_type, previous, current, changed) in changes {
            if !changed {
                continue;
            }
            self.connection.execute(
                "INSERT INTO decision_history (id, project_id, media_asset_id, decision_id, session_id, event_type, previous_value_json, current_value_json, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'human', ?9)",
                params![ReviewEventId::new().to_string(), project_id.to_string(), asset_id.to_string(), decision_id, update.session_id.as_deref(), event_type, json(&previous)?, json(&current)?, now],
            )?;
            if matches!(
                event_type,
                "DECISION_CHANGED" | "RATING_CHANGED" | "STAR_CHANGED"
            ) {
                self.connection.execute(
                    "INSERT INTO review_events (id, project_id, session_id, media_asset_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![ReviewEventId::new().to_string(), project_id.to_string(), update.session_id.as_deref(), asset_id.to_string(), event_type, json(&serde_json::json!({ "previous": previous, "current": current }))?, now],
                )?;
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
        let now = timestamp(&Utc::now());
        self.connection.execute(
            "INSERT INTO group_human_representatives (group_id, project_id, media_asset_id, session_id, selected_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(group_id) DO UPDATE SET media_asset_id = excluded.media_asset_id, session_id = excluded.session_id, selected_at = excluded.selected_at",
            params![group_id, project_id.to_string(), asset_id.to_string(), session_id, now],
        )?;
        let compared = self.connection.prepare(
            "SELECT media_asset_id FROM similarity_group_members WHERE group_id = ?1 AND media_asset_id <> ?2 ORDER BY ordinal ASC, media_asset_id ASC"
        )?.query_map(params![group_id, asset_id.to_string()], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        // The snapshot contains only normalized technical/recommendation metadata. It intentionally
        // excludes byte data, face crops, source paths, and private image content.
        let evidence = preference_evidence_snapshot(&self.connection, group_id, asset_id)?;
        let ai_recommendation = self.connection.query_row(
            "SELECT json_object('assetId', g.representative_asset_id, 'recommendation', r.label, 'confidence', r.confidence) FROM similarity_groups g LEFT JOIN analysis_recommendations r ON r.media_asset_id = g.representative_asset_id AND r.stale = 0 WHERE g.id = ?1",
            params![group_id], |row| row.get::<_, String>(0),
        ).optional()?.unwrap_or_else(|| "{}".into());
        self.connection.execute(
            "INSERT INTO preference_examples (id, project_id, similarity_group_id, chosen_asset_id, compared_asset_ids_json, technical_evidence_json, ai_recommendation_json, human_decision_context, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'group_representative', ?8)",
            params![PreferenceExampleId::new().to_string(), project_id.to_string(), group_id, asset_id.to_string(), json(&compared)?, evidence, ai_recommendation, now],
        )?;
        self.connection.execute(
            "INSERT INTO review_events (id, project_id, session_id, media_asset_id, similarity_group_id, event_type, details_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'GROUP_REPRESENTATIVE_CHANGED', ?6, ?7)",
            params![ReviewEventId::new().to_string(), project_id.to_string(), session_id, asset_id.to_string(), group_id, json(&serde_json::json!({ "previousAssetId": prior, "chosenAssetId": asset_id.to_string() }))?, now],
        )?;
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
                   candidate.input_fingerprint, candidate.cache_relative_path,
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
                ],
                |row| {
                    Ok(SemanticInputCandidate {
                        asset_id: row.get(0)?,
                        project_id: row.get(1)?,
                        file_instance_id: row.get(2)?,
                        filename: row.get(3)?,
                        input_fingerprint: row.get(4)?,
                        preview_relative_path: row.get(5)?,
                        preview_status: row.get(6)?,
                        preview_failure_reason: row.get(7)?,
                        is_available: row.get(8)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn upsert_semantic_embedding(&self, embedding: &SemanticEmbeddingRecord) -> Result<()> {
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
        Ok(())
    }

    fn semantic_embeddings_for_index(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<Vec<StoredSemanticVector>> {
        let mut statement = self.connection.prepare(
            "SELECT media_asset_id, embedding_blob, dimensions FROM semantic_embeddings WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND stale = 0 AND status = 'ready' ORDER BY media_asset_id ASC",
        )?;
        let vectors = statement
            .query_map(
                params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric],
                |row| {
                    let dimensions = row.get::<_, i64>(2)? as usize;
                    let blob = row.get::<_, Option<Vec<u8>>>(1)?.ok_or_else(|| rusqlite::Error::InvalidColumnType(1, "embedding_blob".into(), rusqlite::types::Type::Null))?;
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
        let placeholders = (0..asset_ids.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
        let select_vector = if model.is_some() {
            "stored.embedding_blob, stored.dimensions"
        } else {
            "NULL AS embedding_blob, NULL AS dimensions"
        };
        let join_vector = if model.is_some() {
            "JOIN semantic_embeddings stored ON stored.media_asset_id = a.id AND stored.project_id = a.project_id AND stored.stale = 0 AND stored.status = 'ready' AND stored.model_id = ?2 AND stored.provider = ?3 AND stored.model_version = ?4 AND stored.embedding_version = ?5 AND stored.preprocessing_version = ?6 AND stored.metric = ?7"
        } else {
            ""
        };
        let sql = format!(
            "SELECT a.id, {select_vector},
                CASE WHEN EXISTS (SELECT 1 FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready') THEN (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = a.id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready' ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)) ELSE NULL END,
                COALESCE(decision.rating, 0), decision.decision,
                (SELECT quality.sharpness_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.blur_level FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
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
            ]);
        }
        bound.extend(asset_ids.iter().cloned().map(Into::into));
        let mut statement = self.connection.prepare(&sql)?;
        let candidates = statement
            .query_map(rusqlite::params_from_iter(bound), |row| {
                let dimensions = row.get::<_, Option<i64>>(2)?;
                let vector = match (row.get::<_, Option<Vec<u8>>>(1)?, dimensions) {
                    (Some(blob), Some(dimensions)) => Some(decode_semantic_vector(&blob, dimensions as usize).map_err(to_sql_error)?),
                    (None, None) => None,
                    _ => return Err(rusqlite::Error::InvalidColumnType(1, "embedding_blob".into(), rusqlite::types::Type::Blob)),
                };
                Ok(SemanticSearchCandidate {
                    asset_id: row.get(0)?,
                    vector,
                    face_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    rating: row.get::<_, i64>(4)? as u8,
                    decision: row.get(5)?,
                    sharpness_band: row.get(6)?,
                    blur_level: row.get(7)?,
                    technical_quality_score: row.get(8)?,
                    camera_model: row.get(9)?,
                    captured_at: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        candidates
    }

    fn semantic_metadata_candidates(
        &self,
        project_id: &ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SemanticSearchCandidate>> {
        let mut statement = self.connection.prepare(
            "SELECT a.id, NULL AS embedding_blob, NULL AS dimensions,
                CASE WHEN EXISTS (SELECT 1 FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready') THEN (SELECT COUNT(*) FROM face_analyses face WHERE face.media_asset_id = a.id AND face.input_fingerprint = (SELECT face_artifact.input_fingerprint FROM analysis_artifacts face_artifact WHERE face_artifact.media_asset_id = a.id AND face_artifact.artifact_type = 'face_detection' AND face_artifact.stale = 0 AND face_artifact.status = 'ready' ORDER BY face_artifact.generated_at DESC, face_artifact.id DESC LIMIT 1)) ELSE NULL END,
                COALESCE(decision.rating, 0), decision.decision,
                (SELECT quality.sharpness_band FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.blur_level FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                (SELECT quality.technical_quality_score FROM technical_quality quality WHERE quality.media_asset_id = a.id AND quality.stale = 0 AND quality.status = 'ready' ORDER BY quality.generated_at DESC LIMIT 1),
                metadata.camera_model, COALESCE(metadata.captured_at_local, a.captured_at)
             FROM media_assets a
             LEFT JOIN media_decisions decision ON decision.media_asset_id = a.id AND decision.project_id = a.project_id
             LEFT JOIN media_metadata metadata ON metadata.media_asset_id = a.id
             WHERE a.project_id = ?1
             ORDER BY COALESCE(metadata.captured_at_local, a.captured_at, a.created_at) DESC, a.id ASC LIMIT ?2 OFFSET ?3",
        )?;
        let candidates = statement
            .query_map(params![project_id.to_string(), limit as i64, offset as i64], |row| {
                Ok(SemanticSearchCandidate {
                    asset_id: row.get(0)?,
                    vector: None,
                    face_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    rating: row.get::<_, i64>(4)? as u8,
                    decision: row.get(5)?,
                    sharpness_band: row.get(6)?,
                    blur_level: row.get(7)?,
                    technical_quality_score: row.get(8)?,
                    camera_model: row.get(9)?,
                    captured_at: row.get(10)?,
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
            "SELECT id, project_id, model_id, provider, model_version, embedding_version, preprocessing_version, metric, dimensions, index_format, index_relative_path, index_checksum, embedding_count, status, stale, created_at, rebuilt_at FROM semantic_index_versions WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7 AND stale = 0 ORDER BY rebuilt_at DESC, id DESC LIMIT 1",
            params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric],
            semantic_index_version_from_row,
        ).optional()?.map(Ok).transpose()
    }

    fn mark_other_semantic_index_versions_stale(
        &self,
        project_id: &ProjectId,
        model: &SemanticModelConfig,
    ) -> Result<u64> {
        let changed = self.connection.execute(
            "UPDATE semantic_index_versions SET stale = 1, status = 'stale' WHERE project_id = ?1 AND stale = 0 AND (model_id <> ?2 OR provider <> ?3 OR model_version <> ?4 OR embedding_version <> ?5 OR preprocessing_version <> ?6 OR metric <> ?7)",
            params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric],
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
        let history = statement.query_map(params![project_id.to_string(), limit.clamp(1, 100) as i64], |row| {
            let plan: String = row.get(4)?;
            let plan = serde_json::from_str(&plan).map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error)))?;
            Ok(MagicSearchHistoryEntry { id: row.get(0)?, project_id: row.get(1)?, query_text: row.get(2)?, normalized_query: row.get(3)?, plan, used_at: row.get(5)?, use_count: row.get::<_, i64>(6)? as u64 })
        })?.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into);
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
            "WITH ranked AS (SELECT status, stale, ROW_NUMBER() OVER (PARTITION BY media_asset_id ORDER BY generated_at DESC) AS position FROM semantic_embeddings WHERE project_id = ?1 AND model_id = ?2 AND provider = ?3 AND model_version = ?4 AND embedding_version = ?5 AND preprocessing_version = ?6 AND metric = ?7) SELECT CASE WHEN stale = 1 THEN 'stale' ELSE status END, COUNT(*) FROM ranked WHERE position = 1 GROUP BY CASE WHEN stale = 1 THEN 'stale' ELSE status END",
        )?;
        let mut counts = SemanticIndexTerminalCounts::default();
        for row in statement.query_map(params![project_id.to_string(), model.model_id, model.provider, model.model_version, model.embedding_version, model.preprocessing_version, model.metric], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)))? {
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
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
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
    let raw_metadata: String = row.get(41)?;
    let raw_metadata = serde_json::from_str(&raw_metadata).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(41, rusqlite::types::Type::Text, Box::new(error))
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
        width: row.get::<_, Option<i64>>(14)?.map(|value| value as u32),
        height: row.get::<_, Option<i64>>(15)?.map(|value| value as u32),
        orientation: row.get(16)?,
        camera_make: row.get(17)?,
        camera_model: row.get(18)?,
        lens_make: row.get(19)?,
        lens_model: row.get(20)?,
        focal_length_mm: row.get(21)?,
        focal_length_equivalent_mm: row.get(22)?,
        aperture: row.get(23)?,
        shutter_speed: row.get(24)?,
        iso: row.get::<_, Option<i64>>(25)?.map(|value| value as u32),
        exposure_compensation: row.get(26)?,
        flash: row.get(27)?,
        white_balance: row.get(28)?,
        color_space: row.get(29)?,
        gps_present: row.get(30)?,
        duration_ms: row.get::<_, Option<i64>>(31)?.map(|value| value as u64),
        frame_rate: row.get(32)?,
        codec: row.get(33)?,
        pixel_format: row.get(34)?,
        bitrate: row.get::<_, Option<i64>>(35)?.map(|value| value as u64),
        audio_streams: row.get::<_, Option<i64>>(36)?.map(|value| value as u32),
        video_streams: row.get::<_, Option<i64>>(37)?.map(|value| value as u32),
        sample_rate: row.get::<_, Option<i64>>(38)?.map(|value| value as u32),
        bit_depth: row.get::<_, Option<i64>>(39)?.map(|value| value as u32),
        channels: row.get::<_, Option<i64>>(40)?.map(|value| value as u32),
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
    if values.len() != dimensions || values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(PersistenceError::InvalidData(
            "semantic embedding dimensions or values were invalid".into(),
        ));
    }
    let mut encoded = Vec::with_capacity(values.len() * std::mem::size_of::<f32>());
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
}
