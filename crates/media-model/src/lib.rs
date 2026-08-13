//! Stable, storage-agnostic CaptureOS domain types.
//! This crate deliberately knows nothing about SQLite, Tauri, or a camera vendor.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentifierError {
    #[error("{kind} identifier must be a UUID: {value}")]
    InvalidUuid { kind: &'static str, value: String },
}

macro_rules! id_type {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Uuid::parse_str(value)
                    .map(Self)
                    .map_err(|_| IdentifierError::InvalidUuid {
                        kind: $kind,
                        value: value.to_owned(),
                    })
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }
    };
}

id_type!(ProjectId, "project");
id_type!(ShootId, "shoot");
id_type!(MomentId, "moment");
id_type!(SceneId, "scene");
id_type!(PersonClusterId, "person cluster");
id_type!(MediaAssetId, "media asset");
id_type!(FileInstanceId, "file instance");
id_type!(StorageVolumeId, "storage volume");
id_type!(CaptureDeviceId, "capture device");
id_type!(DerivativeId, "derivative");
id_type!(BackupCopyId, "backup copy");
id_type!(AnalysisArtifactId, "analysis artifact");
id_type!(RelationshipId, "relationship");
id_type!(WorkflowRunId, "workflow run");
id_type!(CorrectionId, "correction");
id_type!(ProvenanceRecordId, "provenance record");
id_type!(JobId, "job");
id_type!(OperationId, "operation");
id_type!(IndexRootId, "index root");
id_type!(IndexIssueId, "index issue");
id_type!(IngestJobId, "ingest job");
id_type!(IngestSourceId, "ingest source");
id_type!(IngestDestinationId, "ingest destination");
id_type!(IngestItemId, "ingest item");
id_type!(IngestAuditEventId, "ingest audit event");
id_type!(SimilarityGroupId, "similarity group");
id_type!(FaceAnalysisId, "face analysis");
id_type!(RecommendationId, "recommendation");
id_type!(HumanDecisionId, "human decision");
id_type!(LocalModelId, "local model");
id_type!(ReviewSessionId, "review session");
id_type!(MediaDecisionId, "media decision");
id_type!(ReviewEventId, "review event");
id_type!(PreferenceExampleId, "preference example");
id_type!(SemanticIndexVersionId, "semantic index version");
id_type!(MagicSearchHistoryId, "magic search history");
id_type!(SavedSearchId, "saved search");
id_type!(StudioProfileId, "studio profile");
id_type!(StudioTrainingExampleId, "studio training example");
id_type!(StudioTrainingRunId, "studio training run");
id_type!(StudioModelId, "studio model");
id_type!(StudioRecommendationId, "studio recommendation");
id_type!(ProductionPlanId, "production plan");
id_type!(VirtualCollectionId, "virtual collection");
id_type!(ExportManifestId, "export manifest");
id_type!(ExportJobId, "export job");
id_type!(DeliveryReportId, "delivery report");
id_type!(EditSessionId, "edit session");
id_type!(EditWorkItemId, "edit work item");
id_type!(EditHandoffPackageId, "edit handoff package");
id_type!(EditOutputRootId, "edit output root");
id_type!(EditOutputId, "edit output");
id_type!(EditVersionId, "edit version");
id_type!(EditEventId, "edit event");

pub type Timestamp = DateTime<Utc>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    RawPhoto,
    Jpeg,
    Heif,
    Png,
    Tiff,
    Video,
    Audio,
    Sidecar,
    Proxy,
    Thumbnail,
    ProjectDocument,
    Unknown,
}

impl MediaType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RawPhoto => "raw_photo",
            Self::Jpeg => "jpeg",
            Self::Heif => "heif",
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Sidecar => "sidecar",
            Self::Proxy => "proxy",
            Self::Thumbnail => "thumbnail",
            Self::ProjectDocument => "project_document",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shoot {
    pub id: ShootId,
    pub project_id: ProjectId,
    pub name: String,
    pub captured_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moment {
    pub id: MomentId,
    pub shoot_id: ShootId,
    pub name: String,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub id: SceneId,
    pub shoot_id: ShootId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonCluster {
    pub id: PersonClusterId,
    pub project_id: ProjectId,
    pub display_name: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MediaFingerprint {
    pub cryptographic_hash: Option<String>,
    pub fast_fingerprint: Option<String>,
    pub byte_size: Option<u64>,
    pub observed_modified_at: Option<Timestamp>,
    pub perceptual_fingerprint: Option<String>,
    pub metadata_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: MediaAssetId,
    pub project_id: ProjectId,
    pub media_type: MediaType,
    pub display_name: String,
    /// Lowercase extension observed during basic classification. It is not treated as identity alone.
    pub extension: Option<String>,
    pub captured_at: Option<Timestamp>,
    pub fingerprint: MediaFingerprint,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInstance {
    pub id: FileInstanceId,
    pub media_asset_id: MediaAssetId,
    pub storage_volume_id: StorageVolumeId,
    pub index_root_id: Option<IndexRootId>,
    /// A sanitized relative path, never a volume mount path.
    pub relative_path: String,
    pub observed_at: Timestamp,
    pub is_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestJobState {
    Preflight,
    Ready,
    Running,
    Interrupted,
    Completed,
    NeedsAttention,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestItemState {
    Discovered,
    Queued,
    Copying,
    Copied,
    Verifying,
    Verified,
    Failed,
    Cancelled,
    Conflict,
    SourceChanged,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    NotVerified,
    Verified,
    Failed,
    SourceChanged,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestDestinationRole {
    Master,
    Backup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestProtectionPolicy {
    Basic,
    Standard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureGuardianState {
    Unprotected,
    MasterCopy,
    PartiallyProtected,
    Protected,
    AtRisk,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestJob {
    pub id: IngestJobId,
    pub project_id: ProjectId,
    /// UUID supplied by the initiating pre-flight session. It makes one user
    /// start action idempotent without collapsing deliberate later runs.
    pub start_request_id: Option<String>,
    pub state: IngestJobState,
    pub policy: IngestProtectionPolicy,
    pub guardian_state: CaptureGuardianState,
    pub safe_to_eject: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestSource {
    pub id: IngestSourceId,
    pub ingest_job_id: IngestJobId,
    pub label: String,
    pub selected_path: String,
    pub storage_volume_id: StorageVolumeId,
    pub file_count: u64,
    pub total_bytes: u64,
    pub status: IngestItemState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestDestination {
    pub id: IngestDestinationId,
    pub ingest_job_id: IngestJobId,
    pub role: IngestDestinationRole,
    pub selected_path: String,
    pub storage_volume_id: StorageVolumeId,
    pub available_bytes: Option<u64>,
    pub required_bytes: u64,
    pub writable: bool,
    pub status: IngestItemState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexRootStatus {
    Ready,
    Indexing,
    Unavailable,
    Failed,
}

/// A user-selected local folder. Its path is a local observation, while the volume ID
/// retains the conceptual storage association when the folder is disconnected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRoot {
    pub id: IndexRootId,
    pub project_id: ProjectId,
    pub selected_path: String,
    pub storage_volume_id: StorageVolumeId,
    pub added_at: Timestamp,
    pub last_indexed_at: Option<Timestamp>,
    pub status: IndexRootStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageClassification {
    Internal,
    Removable,
    Network,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageVolume {
    pub id: StorageVolumeId,
    pub display_name: String,
    pub filesystem_identity: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub mount_state: MountState,
    pub mount_location: Option<String>,
    pub last_seen_at: Option<Timestamp>,
    pub filesystem_type: Option<String>,
    pub classification: StorageClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountState {
    Online,
    Offline,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureDevice {
    pub id: CaptureDeviceId,
    pub project_id: ProjectId,
    pub display_name: String,
    pub device_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraMetadata {
    pub capture_device_id: CaptureDeviceId,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensMetadata {
    pub capture_device_id: CaptureDeviceId,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub focal_length_mm: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSource {
    pub capture_device_id: CaptureDeviceId,
    pub channel_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derivative {
    pub id: DerivativeId,
    pub source_asset_id: MediaAssetId,
    pub derived_asset_id: MediaAssetId,
    pub recipe_hint: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupCopy {
    pub id: BackupCopyId,
    pub source_file_instance_id: FileInstanceId,
    pub backup_file_instance_id: FileInstanceId,
    pub verified_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisArtifact {
    pub id: AnalysisArtifactId,
    pub media_asset_id: MediaAssetId,
    pub artifact_type: String,
    pub payload: serde_json::Value,
    pub provenance: Provenance,
    /// The local provider that produced the artifact. Legacy rows may not have it.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_version: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub settings_version: Option<String>,
    #[serde(default)]
    pub input_fingerprint: Option<String>,
    #[serde(default)]
    pub generated_at: Option<Timestamp>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub status: Option<AnalysisStatus>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Lifecycle state for a local intelligence result. `not_applicable` means a provider
/// intentionally cannot make a valid claim; it is never a disguised success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Pending,
    Ready,
    Unsupported,
    Corrupt,
    NeedsOriginal,
    Failed,
    NotApplicable,
    Stale,
}

impl AnalysisStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Unsupported => "unsupported",
            Self::Corrupt => "corrupt",
            Self::NeedsOriginal => "needs_original",
            Self::Failed => "failed",
            Self::NotApplicable => "not_applicable",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisResourceMode {
    Eco,
    Balanced,
    Fast,
}

impl AnalysisResourceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eco => "eco",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityGroupKind {
    ExactDuplicateSet,
    NearDuplicateSet,
    SimilarSet,
    Burst,
}

impl SimilarityGroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactDuplicateSet => "exact_duplicate_set",
            Self::NearDuplicateSet => "near_duplicate_set",
            Self::SimilarSet => "similar_set",
            Self::Burst => "burst",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TechnicalQualityBand {
    Strong,
    Good,
    Review,
    TechnicalIssue,
    NotApplicable,
}

impl TechnicalQualityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Good => "good",
            Self::Review => "review",
            Self::TechnicalIssue => "technical_issue",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlurEvidenceLevel {
    Low,
    Moderate,
    High,
    Uncertain,
    NotApplicable,
}

impl BlurEvidenceLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Uncertain => "uncertain",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EyeState {
    Open,
    Closed,
    Uncertain,
    NotAnalyzable,
}

impl EyeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Uncertain => "uncertain",
            Self::NotAnalyzable => "not_analyzable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationLabel {
    StrongCandidate,
    StrongAlternative,
    Review,
    ProbableDuplicate,
    TechnicalIssue,
    NotApplicable,
}

impl RecommendationLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StrongCandidate => "strong_candidate",
            Self::StrongAlternative => "strong_alternative",
            Self::Review => "review",
            Self::ProbableDuplicate => "probable_duplicate",
            Self::TechnicalIssue => "technical_issue",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanDecisionValue {
    Keep,
    Review,
    Reject,
}

/// The explicit, non-destructive state recorded by a photographer while culling. `Unreviewed`
/// is a query/UI state only and is deliberately never written as a synthetic decision event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CullingDecisionValue {
    Keep,
    Review,
    Reject,
}

impl CullingDecisionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Review => "review",
            Self::Reject => "reject",
        }
    }
}

impl HumanDecisionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Review => "review",
            Self::Reject => "reject",
        }
    }
}

/// Queryable deterministic visual evidence. Embeddings are kept as compact signed bytes,
/// never an enormous JSON array of floats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualFingerprint {
    pub media_asset_id: MediaAssetId,
    pub input_fingerprint: String,
    pub provider: String,
    pub provider_version: String,
    pub perceptual_hash: String,
    pub difference_hash: String,
    pub color_signature: Vec<u8>,
    pub embedding: Vec<i8>,
    pub embedding_dimensions: u32,
    pub bucket_keys: Vec<String>,
    pub generated_at: Timestamp,
    pub status: AnalysisStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarityGroup {
    pub id: SimilarityGroupId,
    pub project_id: ProjectId,
    pub kind: SimilarityGroupKind,
    pub representative_asset_id: MediaAssetId,
    pub grouping_method: String,
    pub grouping_version: String,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub visual_similarity: Option<f64>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarityGroupMember {
    pub group_id: SimilarityGroupId,
    pub media_asset_id: MediaAssetId,
    pub ordinal: u32,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub is_representative: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceAnalysis {
    pub id: FaceAnalysisId,
    pub media_asset_id: MediaAssetId,
    pub input_fingerprint: String,
    pub provider: String,
    pub provider_version: String,
    /// Normalized image coordinates, each constrained to 0.0 through 1.0 by providers.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub detection_confidence: f64,
    pub relative_size: f64,
    pub visibility: Option<String>,
    pub pose: Option<String>,
    pub face_sharpness: Option<f64>,
    pub eye_state: EyeState,
    pub eye_confidence: Option<f64>,
    pub generated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TechnicalQualityEvidence {
    pub media_asset_id: MediaAssetId,
    pub input_fingerprint: String,
    pub provider: String,
    pub provider_version: String,
    pub global_sharpness: Option<f64>,
    pub sharpness_band: String,
    pub directional_blur_ratio: Option<f64>,
    pub blur_level: BlurEvidenceLevel,
    pub mean_luminance: Option<f64>,
    pub median_luminance: Option<f64>,
    pub highlight_clipping_percent: Option<f64>,
    pub shadow_clipping_percent: Option<f64>,
    pub channel_clipping_percent: Option<f64>,
    pub technical_quality_score: Option<f64>,
    pub technical_quality_band: TechnicalQualityBand,
    pub confidence: f64,
    pub generated_at: Timestamp,
    pub status: AnalysisStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiRecommendation {
    pub id: RecommendationId,
    pub media_asset_id: MediaAssetId,
    pub similarity_group_id: Option<SimilarityGroupId>,
    pub input_fingerprint: String,
    pub provider: String,
    pub provider_version: String,
    pub label: RecommendationLabel,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub generated_at: Timestamp,
    pub status: AnalysisStatus,
}

/// A human choice is append-only and never changes or removes the recommendation that
/// motivated it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HumanDecision {
    pub id: HumanDecisionId,
    pub media_asset_id: MediaAssetId,
    pub recommendation_id: Option<RecommendationId>,
    pub decision: HumanDecisionValue,
    pub decided_at: Timestamp,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelRecord {
    pub id: LocalModelId,
    pub model_id: String,
    /// A stable architecture family such as `siglip`. This is metadata only; executable
    /// loading remains allow-listed in the semantic provider boundary.
    pub model_family: Option<String>,
    pub provider: String,
    pub version: String,
    pub local_relative_path: Option<String>,
    pub checksum: Option<String>,
    pub capability: String,
    /// The pack's declared image input long edge when it has one.
    pub input_size: Option<u32>,
    /// The shared image/text vector dimension when this is a semantic model.
    pub embedding_dimension: Option<u32>,
    pub status: String,
    pub license: String,
    pub license_url: Option<String>,
    pub source_url: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub hardware_requirements: Option<String>,
    pub registered_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub algorithm_id: Option<String>,
    pub algorithm_version: Option<String>,
    pub produced_at: Timestamp,
    pub human_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub id: ProvenanceRecordId,
    pub subject_kind: String,
    pub subject_id: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    pub id: CorrectionId,
    pub subject_kind: String,
    pub subject_id: String,
    pub field_name: String,
    pub previous_value: Option<serde_json::Value>,
    pub corrected_value: serde_json::Value,
    pub corrected_at: Timestamp,
    pub author_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: WorkflowRunId,
    pub workflow_name: String,
    pub state: WorkflowRunState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    Discover,
    Inspect,
    Classify,
    Metadata,
    Fingerprint,
    Persist,
    Finalize,
    Thumbnail,
    Preview,
    AiAnalysis,
    Verification,
    Proxy,
    Transcription,
    /// Local image-to-vector work for Magic Search. It intentionally does not imply a
    /// Capture Intelligence technical-analysis rerun.
    SemanticEmbedding,
    /// Rebuild of the derived, replaceable local Magic Search vector index.
    SemanticIndex,
    /// Local, evidence-based still-photo timeline segmentation. This is distinct from both
    /// Capture Intelligence analysis and Magic Search indexing; it never implies an event,
    /// identity, or culling decision.
    MomentAnalysis,
    /// Explicit, local-only Studio Brain preference-model training. This never changes a
    /// photographer's decisions or source media.
    StudioTraining,
    /// Validation and safe activation of a candidate Studio Brain artifact.
    StudioEvaluation,
    /// Deterministic local Production Plan dry-run and immutable manifest construction.
    ProductionPlanning,
    /// Streaming local-folder copy and strong destination verification for an Export Job.
    ProductionExport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundJob {
    pub id: JobId,
    pub state: WorkflowRunState,
    pub stage: JobStage,
    pub items_completed: u64,
    pub items_total: Option<u64>,
    pub files_discovered: u64,
    pub files_processed: u64,
    pub error_count: u64,
    pub project_id: Option<ProjectId>,
    pub index_root_id: Option<IndexRootId>,
    pub error_message: Option<String>,
    pub resume_metadata: Option<serde_json::Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexIssueSeverity {
    Warning,
    Error,
}

/// A non-fatal file or traversal concern captured during a read-only index run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexIssue {
    pub id: IndexIssueId,
    pub job_id: JobId,
    pub relative_path: Option<String>,
    pub severity: IndexIssueSeverity,
    pub message: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    IndexStarted,
    IndexCompleted,
    CopyStarted,
    CopyVerified,
    AnalysisCompleted,
    UserCorrectedAiResult,
    BackupVerified,
    FileUnavailable,
    VolumeConnected,
    VolumeDisconnected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: OperationId,
    pub kind: OperationKind,
    pub subject_kind: String,
    pub subject_id: String,
    pub occurred_at: Timestamp,
    pub details: serde_json::Value,
}

/// Manifest-only contract for a future portable CaptureOS project sidecar.
/// It records catalog knowledge and references; it never embeds original media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSidecarManifest {
    pub format_version: u32,
    pub project_id: ProjectId,
    pub includes_graph: bool,
    pub includes_fingerprints: bool,
    pub includes_analysis: bool,
    pub includes_transcript_references: bool,
    pub includes_thumbnail_references: bool,
    pub includes_corrections: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    #[test]
    fn media_asset_serializes_without_losing_fingerprint() {
        let id = MediaAssetId::from_uuid(Uuid::from_u128(1));
        let project_id = ProjectId::from_uuid(Uuid::from_u128(2));
        let asset = MediaAsset {
            id,
            project_id,
            media_type: MediaType::RawPhoto,
            display_name: "IMG_0001.ARW".into(),
            extension: Some("arw".into()),
            captured_at: None,
            fingerprint: MediaFingerprint {
                cryptographic_hash: Some("abc".into()),
                byte_size: Some(42),
                ..Default::default()
            },
            created_at: Utc.timestamp_opt(0, 0).single().unwrap(),
        };
        let encoded = serde_json::to_string(&asset).unwrap();
        assert_eq!(serde_json::from_str::<MediaAsset>(&encoded).unwrap(), asset);
    }

    proptest! {
        #[test]
        fn malformed_ids_are_rejected(value in "[a-zA-Z0-9]{1,64}") {
            if Uuid::parse_str(&value).is_err() {
                prop_assert!(MediaAssetId::try_from(value.as_str()).is_err());
            }
        }
    }
}
