//! Application service for the deterministic Phase 0 fixture.

use capture_graph::{EntityKind, EntityRef, Relationship, RelationshipKind};
use capture_intelligence::{
    analyze_preview, build_similarity_groups_with_diagnostics, face_sharpness,
    platform_face_provider_identity, primary_group_for_asset, recommend,
    unavailable_face_provider_identity, AnalysisImageDecoder, DeterministicImageAnalyzer,
    FaceDetector, FaceProviderResult, FingerprintEvidence, ImageAnalyzer, LocalPreviewDecoder,
    PlatformFaceDetector, RecommendationInput, TechnicalEvidence, DETERMINISTIC_PROVIDER,
    DETERMINISTIC_VERSION, FACE_ANALYSIS_SETTINGS_VERSION,
};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use delivery_brain::{
    build_manifest, ManifestBuildInput, ManifestDraft, PlanOverride, PlanOverrideKind,
};
use edit_bridge::{
    CanonicalEditManifest, EditAdapter, EditAdapterIdentity, EditManifestPrivacy,
    EditManifestProject, EditManifestSource, EditManifestWorkItem, ExpectedOutputPolicy,
    HandoffMode, MatchConfidence, NativeManifestAdapter, OutputCandidateIndex, OutputMatchInput,
    OutputMatchState, EDIT_MANIFEST_SCHEMA_VERSION,
};
use ingest::{
    copy_and_verify, hash_file, preflight, CopyVerificationOutcome, DefaultDestinationLayout,
    DestinationLayout, IngestRequest, LocalAvailableSpace, PreflightIssue, PreflightReport,
    PreflightSeverity,
};
pub use magic_search::SiglipProviderCache;
use magic_search::{
    normalize_embedding, plan_query, rank_normalized_vectors, IndexCandidates,
    PersistentVectorIndex, SemanticEmbeddingProvider, SemanticProviderIdentity, SiglipOnnxProvider,
    SUPPORTED_SIGLIP_PACK_DIRECTORY,
};
use media_index::{classify_extension, scan_read_only, IndexCandidate, IndexEvent};
use media_model::*;
use media_visual::{
    capture_time_priority, clear_cache, extract_metadata, prepare_analysis_preview,
    prepare_previews, ArtifactStatus, CaptureTimeCandidate, LocalVisualAdapters, ThumbnailProvider,
    ANALYSIS_PREVIEW_GENERATOR_VERSION, ANALYSIS_PREVIEW_LONG_EDGE, GENERATOR_VERSION,
    METADATA_EXTRACTOR_VERSION,
};
use moment_brain::{
    analyze_append_only_tail, analyze_timeline, derive_clock_offset_diagnostics,
    AppendOnlyTailAnalysisRequest, ExistingHumanDecision, GenericLabelConcept,
    HumanPresentationSignals, IncrementalDisposition, LabelCandidate, LabelCandidateKind,
    Orientation as MomentOrientation, SemanticVector, TimelineAnalysis, TimelineAnalysisConfig,
    TimelineAnalysisRequest, TimelineAssetInput,
};
use persistence::{
    AnalysisInputCandidate, CameraClockOffsetDiagnosticRecord, CaptureIntelligenceTerminalCounts,
    CaptureTimeObservationRecord, CatalogCounts, CatalogRepository, CoverageChecklistItemRecord,
    CoverageChecklistItemView as PersistedCoverageChecklistItemView, CreateEditSessionInput,
    CullingDecisionUpdate, CullingDecisionView, CullingProgress, CullingQuery, CullingReportRow,
    CullingWorkspaceView, DeliveryReportRecord, EditHandoffInput, EditOutputRegistration,
    EditSessionPage, EditSessionRecord, EditVersionRecord, EligibleEditSource,
    ExportJobEntryUpdate, ExportJobRecord, ExportManifestRecord, FaceAnalysisProviderConfig,
    IndexedMediaRow, IngestAuditEvent, IngestItemRecord, IngestReport,
    MagicSearchHistoryEntry as PersistedMagicSearchHistoryEntry, MediaAssetDetail,
    MediaBrowserFilter, MediaMetadataRecord, MomentAnalysisInput, MomentAnalysisRunRecord,
    MomentBoundaryEvidenceRecord, MomentIncrementalAnalysisWindow, MomentMembershipRecord,
    MomentOverrideOperation, MomentRecord, MomentTimelineStatusRecord, PersistenceError,
    PreviewArtifactRecord, ProductionPlanInput, ProductionPlanRecord, ProductionPreflight,
    ProductionWorkspaceView, ProjectIndexSummary,
    ProjectLibraryItem as PersistedProjectLibraryItem, Result as PersistenceResult,
    ReviewSessionView, SemanticEmbeddingRecord, SemanticIndexVersion, SemanticInputCandidate,
    SemanticMetadataQuery, SemanticMetadataSort, SemanticModelConfig, SemanticSearchCandidate,
    SimilarityGroupView, StudioBrainProjectStatus, StudioModelActivationOutcome, StudioModelRecord,
    StudioPairwisePreferenceRecord, StudioRecommendationRecord, StudioTrainingExampleRecord,
    StudioTrainingRunRecord, TimelineSegmentRecord, VirtualCollectionInput,
    VirtualCollectionRecord, VisualMediaPage, VisualMediaQuery, VisualMediaRow,
    VisualPreparationTerminalCounts,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    panic::AssertUnwindSafe,
    path::{Component, Path, PathBuf},
    sync::Arc,
    thread,
    time::Instant,
};
use storage::{LocalVolumeInspector, VolumeInspector, VolumeObservation};
use studio_brain::{
    decode_verified_model_artifact, encode_verified_model_artifact,
    evaluate_studio_model_on_observations, predict, rank_similar_set, train_studio_model,
    AuxiliaryHumanSignals, ExplicitHumanActionKind, GenericAgreement, GenericRecommendation,
    HumanDecision as StudioHumanDecision, PairwisePreference, SimilarSetCandidate,
    StudioFeatureInput, StudioTrainingConfig, StudioTrainingStatus, TrainingObservation,
    VerifiedModelArtifact, STUDIO_BRAIN_ALGORITHM_VERSION, STUDIO_BRAIN_FEATURE_SCHEMA_VERSION,
};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
struct GoldenShootManifest {
    fixture_version: u32,
    project_name: String,
    description: String,
    expected: ExpectedCounts,
}

#[derive(Debug, Clone, Deserialize)]
struct ExpectedCounts {
    media_assets: u64,
    file_instances: u64,
    storage_volumes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStatus {
    pub foundation_ready: bool,
    pub sample_project: String,
    pub media_assets: u64,
    pub file_instances: u64,
    pub storage_volumes: u64,
    pub local_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
    pub name: String,
}

/// The global, small-footprint library projection. Its identity is always the stable Project ID;
/// display names intentionally remain non-unique.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLibraryItem {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub media_asset_count: u64,
    pub storage_volume_count: u64,
    pub protection_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRootView {
    pub id: String,
    pub selected_path: String,
    pub status: String,
    pub last_indexed_at: Option<String>,
    pub storage_volume_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub state: String,
    pub stage: String,
    pub files_discovered: u64,
    pub files_processed: u64,
    pub error_count: u64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHome {
    pub project: ProjectView,
    pub summary: ProjectIndexSummary,
    pub roots: Vec<IndexRootView>,
    pub latest_job: Option<JobView>,
    pub media: Vec<IndexedMediaRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestPreflightView {
    pub report: PreflightReport,
    pub can_start: bool,
    pub policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPreparationProgress {
    pub state: String,
    pub stage: String,
    pub items_completed: u64,
    pub items_total: u64,
    pub error_count: u64,
    pub ready_count: u64,
    pub unsupported_count: u64,
    pub corrupt_count: u64,
    pub offline_count: u64,
    pub failed_count: u64,
    pub timeout_count: u64,
    pub cancelled_count: u64,
    pub current_asset_id: Option<String>,
    pub current_file_instance_id: Option<String>,
    pub current_provider: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
}

/// Progress for an explicit, metadata-only refresh. It never creates previews, opens a semantic
/// model, or alters customer media; the desktop runs it on a dedicated background connection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataRefreshProgress {
    pub state: String,
    pub items_completed: u64,
    pub items_total: u64,
    pub error_count: u64,
    pub resolved_capture_time_count: u64,
    pub high_confidence_capture_time_count: u64,
    pub copy_conflict_count: u64,
    pub current_asset_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureIntelligenceProgress {
    pub state: String,
    pub stage: String,
    pub resource_mode: String,
    pub items_completed: u64,
    pub items_total: u64,
    pub error_count: u64,
    pub ready_count: u64,
    pub unsupported_count: u64,
    pub corrupt_count: u64,
    pub needs_original_count: u64,
    pub failed_count: u64,
    pub not_applicable_count: u64,
    pub stale_count: u64,
    pub current_asset_id: Option<String>,
    pub current_stage_detail: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
}

/// Durable terminal counts for the separate M6 semantic embedding pipeline. These do not reuse
/// Capture Intelligence counts because a semantic model revision can be rebuilt independently.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticIndexCounts {
    pub total: u64,
    pub ready: u64,
    pub unsupported: u64,
    pub corrupt: u64,
    pub needs_original: u64,
    pub failed: u64,
    pub pending: u64,
    pub stale: u64,
}

/// UI-safe metadata for the one validated local image/text provider. Raw model paths, weights,
/// checksums, embeddings, and source paths remain backend-only derived data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelIdentityView {
    pub model_id: String,
    pub model_version: String,
    pub provider: String,
    pub license_url: Option<String>,
    pub embedding_dimension: Option<usize>,
    pub installed_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelStatus {
    pub installed: bool,
    pub message: Option<String>,
    pub identity: Option<SemanticModelIdentityView>,
}

/// Project-scoped, local Magic Search indexing state. The model capability is explicit so the
/// UI can offer honest structured-filter fallback rather than fabricated semantic matches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticIndexProgress {
    pub state: String,
    pub stage: String,
    pub resource_mode: String,
    pub completed: u64,
    pub total: u64,
    pub error_count: u64,
    pub counts: SemanticIndexCounts,
    pub model: SemanticModelStatus,
    pub active: bool,
    pub paused: bool,
    pub index_ready: bool,
    pub index_embedding_count: u64,
    pub last_error: Option<String>,
    pub current_asset_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
}

/// Durable, UI-safe status for M7 structural analysis. This deliberately omits raw embeddings,
/// model paths, source paths, boundary scores, and any person/identity interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentAnalysisProgress {
    pub state: String,
    pub active: bool,
    pub paused: bool,
    pub stage: String,
    /// The existing local scheduler mode selected for this explicitly requested analysis.
    /// Project loading never creates an M7 job merely to obtain this value.
    pub resource_mode: String,
    pub completed: u64,
    pub total: u64,
    pub error_count: u64,
    pub timeline_ready: bool,
    pub moment_count: u64,
    pub ungrouped_asset_count: u64,
    pub last_error: Option<String>,
    pub message: Option<String>,
}

/// UI-safe progress for an explicitly requested, local Studio Brain training run. It contains no
/// model coefficients, source paths, raw feature snapshots, notes, or probability values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioBrainProgress {
    pub profile_id: String,
    pub state: String,
    pub active: bool,
    pub stage: String,
    pub completed: u64,
    pub total: u64,
    pub error_count: u64,
    pub active_model_version: Option<String>,
    pub message: Option<String>,
    /// Developer-details only. Normal UI should show a recovery message rather than surface
    /// raw SQLite/Rust diagnostics directly.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentBoundaryEvidenceView {
    pub strength: String,
    pub summary: String,
    pub signals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentLabelView {
    pub display_label: String,
    pub ai_suggested_label: Option<String>,
    pub human_label: Option<String>,
    pub source: String,
    pub strength: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentRepresentativeView {
    pub asset_id: String,
    pub filename: String,
    pub thumbnail_preview_url: Option<String>,
    pub source: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentSummaryView {
    pub id: String,
    pub ordinal: u64,
    /// Whether this card can be safely merged with the immediately preceding card in the current
    /// timeline page. Incremental timelines can intentionally retain an older-run prefix, so a
    /// visually adjacent cross-run pair must fail closed until a full local rebuild materializes
    /// one complete run.
    pub can_merge_with_previous: bool,
    pub label: MomentLabelView,
    pub captured_from: Option<String>,
    pub captured_to: Option<String>,
    pub capture_time_state: String,
    pub asset_count: u64,
    /// Factual project-local summaries only. These values do not change culling, Similar Set
    /// membership, or any human decision; they make a Moment card useful for review context.
    pub similar_set_count: u64,
    pub keep_count: u64,
    pub reject_count: u64,
    pub review_count: u64,
    pub unreviewed_count: u64,
    pub starred_count: u64,
    pub technical_issue_count: u64,
    pub representative: Option<MomentRepresentativeView>,
    pub boundary_before: Option<MomentBoundaryEvidenceView>,
    pub has_human_structure_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentClockDiagnosticView {
    pub camera_label: String,
    pub summary: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentTimelineGapView {
    pub started_at: String,
    pub ended_at: String,
    pub duration_seconds: u64,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentTimelineView {
    pub progress: Option<MomentAnalysisProgress>,
    pub moments: Vec<MomentSummaryView>,
    pub has_more: bool,
    pub total_moments: u64,
    pub ungrouped_asset_count: u64,
    pub clock_diagnostics: Vec<MomentClockDiagnosticView>,
    pub timeline_gaps: Vec<MomentTimelineGapView>,
}

/// A bounded text request for current-project Moment cards. Unlike Magic Search asset retrieval,
/// this ranks only durable Moment centroids built from compatible locally persisted embeddings.
/// It neither reads original media nor makes a semantic result stand in for a factual detection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentSearchRequest {
    pub query: String,
    #[serde(default = "default_moment_search_limit")]
    pub limit: u32,
}

const fn default_moment_search_limit() -> u32 {
    24
}

impl Default for MomentSearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            limit: default_moment_search_limit(),
        }
    }
}

/// UI-safe Moment-card semantic retrieval. Numeric similarity values, centroids, source paths,
/// and raw label evidence remain local implementation details.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentSearchResponse {
    pub query: String,
    pub results: Vec<MomentSummaryView>,
    pub has_more: bool,
    pub total_results: u64,
    pub semantic_available: bool,
    pub semantic_applied: bool,
    pub semantic_unavailable_reason: Option<String>,
    pub identity_search_blocked: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentDetailView {
    pub moment: MomentSummaryView,
    pub boundary_evidence: Vec<MomentBoundaryEvidenceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageChecklistItem {
    pub id: String,
    pub phrase: String,
    pub state: String,
    pub confirmed_moment_id: Option<String>,
    pub confirmed_asset_id: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentChecklistView {
    pub id: String,
    pub name: String,
    pub items: Vec<CoverageChecklistItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCoverageChecklistItemInput {
    pub checklist_id: Option<String>,
    pub phrase: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCoverageConfirmationInput {
    pub checklist_item_id: String,
    pub state: String,
    pub moment_id: Option<String>,
    pub asset_id: Option<String>,
}

const MOMENT_ANALYZER_ID: &str = "captureos-moment-brain";
const MOMENT_BOUNDARY_ALGORITHM_VERSION: &str = moment_brain::MOMENT_BRAIN_ALGORITHM_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchRequest {
    pub query: String,
    #[serde(default = "default_magic_search_sort")]
    pub sort: String,
    #[serde(default)]
    pub descending: bool,
    #[serde(default = "default_magic_search_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    /// Optional M7 current-Moment scope. Core validates project ownership before every semantic
    /// or deterministic retrieval path; this is never a browser-provided asset collection.
    #[serde(default)]
    pub moment_id: Option<String>,
}

fn default_magic_search_sort() -> String {
    "relevance".into()
}

const fn default_magic_search_limit() -> u32 {
    60
}

impl Default for MagicSearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            sort: default_magic_search_sort(),
            descending: false,
            limit: default_magic_search_limit(),
            offset: 0,
            moment_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchFilterView {
    pub chips: Vec<String>,
}

/// A result card stays a logical MediaAsset card. The optional score is a local normalized-dot
/// ranking signal only, never a confidence, object/identity assertion, or human decision.
/// Raw embeddings are never sent to the frontend or written into a human decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchResult {
    pub item: VisualMediaRow,
    pub score_label: Option<String>,
    pub semantic_score: Option<f32>,
    pub explanation: String,
    pub matched_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchResponse {
    pub query: String,
    pub results: Vec<MagicSearchResult>,
    pub semantic_available: bool,
    pub semantic_applied: bool,
    pub semantic_unavailable_reason: Option<String>,
    pub parsed_filters: MagicSearchFilterView,
    pub has_more: bool,
    pub total_results: u64,
    pub identity_search_blocked: bool,
    pub message: Option<String>,
}

/// A local history entry intentionally keeps a query and its displayed deterministic chips, but
/// never exposes raw vector data, model filesystem paths, or another project's history.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchHistoryEntry {
    pub id: String,
    pub query: String,
    pub created_at: String,
    pub parsed_filters: MagicSearchFilterView,
}

const SEMANTIC_INDEX_FORMAT: &str = "captureos-m6-lsh.v1";
const MAGIC_SEARCH_PAGE_LIMIT: u32 = 120;

/// Roots owned by CaptureOS for managed semantic artifacts. These paths deliberately exclude
/// customer originals: image analysis receives only a cache preview, while model packs and the
/// derived vector index have their own local roots.
#[derive(Debug, Clone, Copy)]
pub struct SemanticStorageRoots<'a> {
    pub preview_cache_root: &'a Path,
    pub index_root: &'a Path,
}

/// A Find Similar request stays separate from a Magic Search text request because it has no
/// prompt and uses a persisted image embedding from one project-owned MediaAsset.
#[derive(Debug, Clone, Copy)]
pub struct FindSimilarRequest<'a> {
    pub asset_id: &'a MediaAssetId,
    pub limit: u32,
    pub offset: u32,
}

struct SemanticProgressDetails {
    counts: SemanticIndexCounts,
    model: SemanticModelStatus,
    index_ready: bool,
    index_embedding_count: u64,
    resource_mode: AnalysisResourceMode,
    current_asset_id: Option<String>,
    message: Option<String>,
}

struct SemanticIndexExecution<'a> {
    resource_mode: AnalysisResourceMode,
    provider: &'a dyn SemanticEmbeddingProvider,
    model_status: SemanticModelStatus,
    should_pause: &'a dyn Fn() -> bool,
    on_progress: &'a mut dyn FnMut(&SemanticIndexProgress),
}

struct SemanticPauseDetails {
    counts: SemanticIndexCounts,
    current_asset_id: Option<String>,
    message: String,
}

struct SemanticResponseContext<'a> {
    request: &'a MagicSearchRequest,
    plan: &'a magic_search::QueryPlan,
    preview_cache_root: &'a Path,
    semantic_available: bool,
    semantic_applied: bool,
    semantic_unavailable_reason: Option<String>,
    explanation: &'a str,
}

struct MagicResultContext<'a> {
    project_id: &'a ProjectId,
    preview_cache_root: &'a Path,
    plan: &'a magic_search::QueryPlan,
    semantic_applied: bool,
    explanation: &'a str,
}

struct FindSimilarExecution<'a> {
    provider: &'a dyn SemanticEmbeddingProvider,
    index_root: &'a Path,
    preview_cache_root: &'a Path,
}

fn semantic_model_config(identity: &SemanticProviderIdentity) -> SemanticModelConfig {
    // The database schema predates M6 and its natural unique tuple does not have a dimensions
    // column. Include dimensions in the versioned embedding identity as well as in the derived
    // index cache key, so a provider cannot silently compare or overwrite a differently-sized
    // space under the same revision label.
    SemanticModelConfig {
        model_id: identity.model_id.clone(),
        provider: identity.provider.clone(),
        model_version: identity.model_version.clone(),
        embedding_version: format!(
            "{};dimensions={}",
            identity.embedding_version, identity.dimensions
        ),
        preprocessing_version: identity.preprocessing_version.clone(),
        metric: identity.metric.clone(),
        dimensions: identity.dimensions,
    }
}

fn semantic_model_status_for_provider(provider: &SiglipOnnxProvider) -> SemanticModelStatus {
    let identity = provider.identity();
    let metadata = provider.pack_metadata();
    SemanticModelStatus {
        installed: true,
        message: Some("Local semantic search model is ready.".into()),
        identity: Some(SemanticModelIdentityView {
            model_id: identity.model_id.clone(),
            model_version: identity.model_version.clone(),
            provider: identity.provider.clone(),
            license_url: Some(metadata.license_url),
            embedding_dimension: Some(identity.dimensions),
            installed_bytes: Some(metadata.installed_bytes),
        }),
    }
}

fn unavailable_semantic_model_status(message: impl Into<String>) -> SemanticModelStatus {
    SemanticModelStatus {
        installed: false,
        message: Some(message.into()),
        identity: None,
    }
}

/// Acquires only the fixed, static M6 pack and mirrors a successfully reference-validated pack
/// into the local model registry. This is not a downloader, installer, or generic model loader:
/// a missing, invalid, or unreviewed pack remains unavailable. The supplied cache revalidates a
/// closed lightweight pack stamp before reuse and performs full admission on a changed pack.
fn inspect_semantic_provider(
    repository: &impl CatalogRepository,
    provider_cache: &SiglipProviderCache,
) -> PersistenceResult<(SemanticModelStatus, Option<Arc<SiglipOnnxProvider>>)> {
    match provider_cache.acquire() {
        Ok(Some(provider)) => {
            register_validated_semantic_model(repository, &provider)?;
            let status = semantic_model_status_for_provider(&provider);
            Ok((status, Some(provider)))
        }
        Ok(None) => Ok((
            unavailable_semantic_model_status(
                "Semantic model not installed. Metadata and technical filters remain available.",
            ),
            None,
        )),
        Err(error) => Ok((
            unavailable_semantic_model_status(format!("Semantic model unavailable: {error}")),
            None,
        )),
    }
}

fn register_validated_semantic_model(
    repository: &impl CatalogRepository,
    provider: &SiglipOnnxProvider,
) -> PersistenceResult<()> {
    let identity = provider.identity();
    let metadata = provider.pack_metadata();
    repository.upsert_local_model(&LocalModelRecord {
        id: LocalModelId::new(),
        model_id: identity.model_id.clone(),
        model_family: Some(identity.model_family.clone()),
        provider: identity.provider.clone(),
        version: identity.model_version.clone(),
        local_relative_path: Some(SUPPORTED_SIGLIP_PACK_DIRECTORY.into()),
        checksum: Some(metadata.source_weights_sha256),
        capability: "shared_image_text_embedding".into(),
        input_size: Some(metadata.input_size),
        embedding_dimension: Some(metadata.embedding_dimension as u32),
        status: "available".into(),
        license: metadata.license,
        license_url: Some(metadata.license_url),
        source_url: Some("https://huggingface.co/google/siglip-base-patch16-224".into()),
        file_size_bytes: Some(metadata.installed_bytes),
        hardware_requirements: Some("Local CPU via tract-onnx; no network service".into()),
        registered_at: Utc::now(),
    })
}

fn semantic_counts(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    model: &SemanticModelConfig,
    pending: u64,
) -> PersistenceResult<SemanticIndexCounts> {
    let terminal = repository.semantic_index_terminal_counts(project_id, model)?;
    Ok(SemanticIndexCounts {
        total: terminal.total.saturating_add(pending),
        ready: terminal.ready,
        unsupported: terminal.unsupported,
        corrupt: terminal.corrupt,
        needs_original: terminal.needs_original,
        failed: terminal.failed,
        pending,
        stale: terminal.stale,
    })
}

fn semantic_resource_mode_from_job(job: Option<&BackgroundJob>) -> AnalysisResourceMode {
    match job
        .and_then(|job| job.resume_metadata.as_ref())
        .and_then(|metadata| metadata.get("resource_mode"))
        .and_then(serde_json::Value::as_str)
    {
        Some("eco") => AnalysisResourceMode::Eco,
        Some("fast") => AnalysisResourceMode::Fast,
        _ => AnalysisResourceMode::Balanced,
    }
}

fn semantic_progress(
    job: Option<&BackgroundJob>,
    details: SemanticProgressDetails,
) -> SemanticIndexProgress {
    let state = job
        .map(|job| workflow_state_label(&job.state))
        .unwrap_or_else(|| "idle".into());
    let stage = job
        .map(|job| job_stage_label(&job.stage))
        .unwrap_or_else(|| "semantic_index".into());
    SemanticIndexProgress {
        state: state.clone(),
        stage,
        resource_mode: details.resource_mode.as_str().into(),
        completed: job
            .map(|job| job.items_completed)
            .unwrap_or(details.counts.total),
        total: job
            .and_then(|job| job.items_total)
            .unwrap_or(details.counts.total),
        error_count: job
            .map(|job| job.error_count)
            .unwrap_or(details.counts.failed),
        counts: details.counts,
        model: details.model,
        active: state == "running",
        paused: state == "paused",
        index_ready: details.index_ready,
        index_embedding_count: details.index_embedding_count,
        last_error: job.and_then(|job| job.error_message.clone()),
        current_asset_id: details.current_asset_id,
        started_at: job
            .map(|job| job.created_at.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        finished_at: job.and_then(|job| job.finished_at.map(|value| value.to_rfc3339())),
        message: details.message,
    }
}

fn load_active_semantic_index(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    model: &SemanticModelConfig,
    model_cache_key: &str,
    index_root: &Path,
) -> PersistenceResult<Option<(PersistentVectorIndex, u64)>> {
    let Some(version) = repository.active_semantic_index_version(project_id, model)? else {
        return Ok(None);
    };
    let index = PersistentVectorIndex::load(
        index_root,
        &version.index_relative_path,
        &project_id.to_string(),
        model_cache_key,
        model.dimensions,
    )
    .map_err(|error| {
        PersistenceError::InvalidData(format!("local semantic index requires rebuild: {error}"))
    })?;
    Ok(index.map(|index| (index, version.embedding_count)))
}

/// Returns a durable, project-scoped M6 status. It never starts a model download or a scan; a
/// corrupt/missing derived index simply reports not-ready so the next local rebuild can replace it.
pub fn load_semantic_index_status(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    provider_cache: &SiglipProviderCache,
    index_root: &Path,
) -> PersistenceResult<SemanticIndexProgress> {
    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let Some(provider) = provider else {
        return Ok(semantic_progress(
            None,
            SemanticProgressDetails {
                counts: SemanticIndexCounts::default(),
                model: model_status,
                index_ready: false,
                index_embedding_count: 0,
                resource_mode: AnalysisResourceMode::Balanced,
                current_asset_id: None,
                message: None,
            },
        ));
    };
    let model = semantic_model_config(provider.identity());
    let model_cache_key = provider.identity().cache_key();
    let job = repository.latest_semantic_indexing_job(project_id)?;
    let mode = semantic_resource_mode_from_job(job.as_ref());
    let counts = semantic_counts(repository, project_id, &model, 0)?;
    let (index_ready, index_embedding_count) = match load_active_semantic_index(
        repository,
        project_id,
        &model,
        &model_cache_key,
        index_root,
    ) {
        Ok(Some((_, count))) => (true, count),
        Ok(None) | Err(_) => (false, 0),
    };
    Ok(semantic_progress(
        job.as_ref(),
        SemanticProgressDetails {
            counts,
            model: model_status,
            index_ready,
            index_embedding_count,
            resource_mode: mode,
            current_asset_id: None,
            message: job.as_ref().and_then(|job| job.error_message.clone()),
        },
    ))
}

pub fn recover_interrupted_semantic_indexing(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_semantic_indexing()
}

pub fn load_magic_search_history(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    limit: u32,
) -> PersistenceResult<Vec<MagicSearchHistoryEntry>> {
    repository
        .magic_search_history(project_id, limit)
        .map(|entries| entries.into_iter().map(magic_search_history_view).collect())
}

pub fn clear_magic_search_history(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<u64> {
    repository.clear_magic_search_history(project_id)
}

/// Reads a compact, durable M7 status. It intentionally does not acquire a model, scan media,
/// or start analysis, so Project Home and the timeline remain responsive on startup.
pub fn load_moment_timeline_status(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<Option<MomentAnalysisProgress>> {
    let timeline = repository.moment_timeline_status(project_id)?;
    let job = repository.latest_moment_analysis_job(project_id)?;
    if timeline.is_none() && job.is_none() {
        return Ok(None);
    }
    Ok(Some(moment_progress(job.as_ref(), timeline.as_ref(), None)))
}

/// Loads only one bounded Moment-card page. No original paths, raw embeddings, or numeric
/// boundary/semantic scores leave this application-service layer.
pub fn load_moment_timeline(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    limit: u32,
    offset: u32,
    preview_cache_root: &Path,
) -> PersistenceResult<MomentTimelineView> {
    let page = repository.moment_timeline_page(project_id, limit, offset)?;
    let progress = load_moment_timeline_status(repository, project_id)?;
    let total_moments = page
        .timeline
        .as_ref()
        .map(|timeline| timeline.moment_count)
        .unwrap_or(0);
    let ungrouped_asset_count = page
        .timeline
        .as_ref()
        .map(|timeline| timeline.ungrouped_count)
        .unwrap_or(0);
    let mut previous_run_id: Option<&str> = None;
    let moments = page
        .moments
        .iter()
        .map(|row| {
            let can_merge_with_previous = previous_run_id == Some(row.run_id.as_str());
            previous_run_id = Some(row.run_id.as_str());
            moment_summary_view(
                repository,
                project_id,
                row,
                preview_cache_root,
                can_merge_with_previous,
            )
        })
        .collect::<PersistenceResult<Vec<_>>>()?;
    let clock_diagnostics = repository
        .latest_camera_clock_offset_diagnostics(project_id)?
        .into_iter()
        .filter_map(moment_clock_diagnostic_view)
        .collect();
    Ok(MomentTimelineView {
        progress,
        moments,
        has_more: page.has_more,
        total_moments,
        ungrouped_asset_count,
        // Only completed-run diagnostics satisfying Moment Brain's strict local evidence rule
        // are shown. This stays advisory: CaptureOS never writes a timestamp correction.
        clock_diagnostics,
        timeline_gaps: page
            .gaps
            .into_iter()
            .map(|gap| MomentTimelineGapView {
                started_at: gap.started_at,
                ended_at: gap.ended_at,
                duration_seconds: gap.duration_seconds,
                explanation: gap.explanation,
            })
            .collect(),
    })
}

/// Searches current-project Moment cards with the same explicit, locally installed M6 text
/// provider used by Magic Search. Retrieval is available only when the active Moment timeline
/// declares the exact same embedding-space cache key as the provider; a matching dimensionality
/// alone is never enough to mix model revisions or preprocessing schemes.
pub fn search_moments(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: &MomentSearchRequest,
    provider_cache: &SiglipProviderCache,
    preview_cache_root: &Path,
) -> PersistenceResult<MomentSearchResponse> {
    let plan = plan_query(&request.query)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    if plan.identity_search_blocked {
        return Ok(MomentSearchResponse {
            query: plan.normalized_query,
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: false,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "Identity recognition and person search are not available in Moment search."
                    .into(),
            ),
            identity_search_blocked: true,
            message: Some(
                "Moment search does not identify or match people. Use a neutral visual description instead."
                    .into(),
            ),
        });
    }

    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let Some(provider) = provider else {
        return Ok(moment_search_unavailable(
            plan.normalized_query,
            model_status.message,
        ));
    };
    let Some(semantic_query) = plan.semantic_query.as_deref() else {
        return Ok(MomentSearchResponse {
            query: plan.normalized_query,
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: true,
            semantic_applied: false,
            semantic_unavailable_reason: None,
            identity_search_blocked: false,
            message: Some(
                "Use a concise local visual description to search Moment cards. Deterministic photo filters remain available in Magic Search."
                    .into(),
            ),
        });
    };

    search_moments_with_provider(
        repository,
        project_id,
        request,
        &plan.normalized_query,
        semantic_query,
        provider.as_ref(),
        preview_cache_root,
    )
}

fn moment_search_unavailable(query: String, reason: Option<String>) -> MomentSearchResponse {
    MomentSearchResponse {
        query,
        results: Vec::new(),
        has_more: false,
        total_results: 0,
        semantic_available: false,
        semantic_applied: false,
        semantic_unavailable_reason: reason.or_else(|| {
            Some("A locally installed semantic model is required to search Moment cards.".into())
        }),
        identity_search_blocked: false,
        message: Some(
            "Moment-card semantic retrieval is unavailable until an approved local semantic model is installed and compatible local embeddings exist."
                .into(),
        ),
    }
}

fn search_moments_with_provider(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: &MomentSearchRequest,
    normalized_query: &str,
    semantic_query: &str,
    provider: &dyn SemanticEmbeddingProvider,
    preview_cache_root: &Path,
) -> PersistenceResult<MomentSearchResponse> {
    let provider_model_key = provider.identity().cache_key();
    let Some(timeline) = repository.moment_timeline_status(project_id)? else {
        return Ok(MomentSearchResponse {
            query: normalized_query.into(),
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: true,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "No local Moment timeline is ready for this project. Analyze the timeline after local embeddings are indexed."
                    .into(),
            ),
            identity_search_blocked: false,
            message: Some(
                "Moment-card retrieval has no compatible local timeline evidence to rank yet."
                    .into(),
            ),
        });
    };
    if timeline.state != "ready"
        || timeline.semantic_model_key.as_deref() != Some(provider_model_key.as_str())
    {
        return Ok(MomentSearchResponse {
            query: normalized_query.into(),
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: true,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "The active Moment timeline has no centroids compatible with the installed local semantic model. Update or rebuild the timeline after indexing compatible local embeddings."
                    .into(),
            ),
            identity_search_blocked: false,
            message: Some(
                "Moment-card retrieval is paused rather than comparing incompatible local embedding spaces."
                    .into(),
            ),
        });
    }

    let query = match provider
        .embed_text(semantic_query)
        .and_then(normalize_embedding)
    {
        Ok(query) => query,
        Err(error) => {
            return Ok(MomentSearchResponse {
                query: normalized_query.into(),
                results: Vec::new(),
                has_more: false,
                total_results: 0,
                semantic_available: false,
                semantic_applied: false,
                semantic_unavailable_reason: Some(format!(
                    "Local Moment text embedding is unavailable: {error}"
                )),
                identity_search_blocked: false,
                message: Some(
                    "No Moment cards were ranked because the local text embedding could not be produced."
                        .into(),
                ),
            });
        }
    };
    let model = semantic_model_config(provider.identity());
    if query.len() != model.dimensions {
        return Ok(MomentSearchResponse {
            query: normalized_query.into(),
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: false,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "Local Moment text embedding dimensions do not match the installed model.".into(),
            ),
            identity_search_blocked: false,
            message: Some(
                "No Moment cards were ranked from an incompatible local text embedding.".into(),
            ),
        });
    }

    let candidates = repository.moment_search_candidates(project_id, &model)?;
    if candidates.is_empty() {
        return Ok(MomentSearchResponse {
            query: normalized_query.into(),
            results: Vec::new(),
            has_more: false,
            total_results: 0,
            semantic_available: true,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "No compatible local Moment centroids are stored for this project."
                    .into(),
            ),
            identity_search_blocked: false,
            message: Some(
                "No Moment cards were ranked because this timeline has no compatible locally derived centroid evidence."
                    .into(),
            ),
        });
    }

    let mut candidates_by_id = candidates
        .into_iter()
        .map(|candidate| (candidate.moment_id.clone(), candidate))
        .collect::<HashMap<_, _>>();
    let ranked = rank_normalized_vectors(
        &query,
        candidates_by_id
            .values()
            .map(|candidate| (candidate.moment_id.clone(), candidate.centroid.clone())),
        candidates_by_id.len(),
    )
    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let total_results = ranked.len() as u64;
    let limit = request.limit.clamp(1, 60) as usize;
    let has_more = ranked.len() > limit;
    let results = ranked
        .into_iter()
        .take(limit)
        .filter_map(|ranked| candidates_by_id.remove(&ranked.asset_id))
        .map(|candidate| {
            moment_summary_view(
                repository,
                project_id,
                &candidate.row,
                preview_cache_root,
                false,
            )
        })
        .collect::<PersistenceResult<Vec<_>>>()?;
    Ok(MomentSearchResponse {
        query: normalized_query.into(),
        results,
        has_more,
        total_results,
        semantic_available: true,
        semantic_applied: true,
        semantic_unavailable_reason: None,
        identity_search_blocked: false,
        message: Some(
            "Moment cards are ranked only by local image/text embedding similarity of compatible stored centroids. This is retrieval, not proof of an object, identity, relationship, or event."
                .into(),
        ),
    })
}

/// Loads a selected Moment summary plus qualitative local boundary explanations. Its member
/// photos stay on the existing bounded `visual_media_page` endpoint with a validated `moment_id`.
pub fn load_moment_detail(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    moment_id: &str,
    preview_cache_root: &Path,
) -> PersistenceResult<Option<MomentDetailView>> {
    let Some(detail) = repository.moment_detail(project_id, moment_id)? else {
        return Ok(None);
    };
    let mut summary = moment_summary_view(
        repository,
        project_id,
        &detail.moment,
        preview_cache_root,
        false,
    )?;
    summary.label.evidence = detail.label_evidence;
    let boundaries = summary
        .boundary_before
        .clone()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(Some(MomentDetailView {
        moment: summary,
        boundary_evidence: boundaries,
    }))
}

pub fn rename_moment(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    moment_id: &str,
    label: &str,
) -> PersistenceResult<()> {
    repository.rename_moment(project_id, moment_id, label)
}

pub fn set_moment_human_representative(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    moment_id: &str,
    asset_id: &MediaAssetId,
) -> PersistenceResult<()> {
    ensure_asset_project(repository, project_id, asset_id)?;
    repository.set_moment_human_representative(project_id, moment_id, &asset_id.to_string())
}

pub fn merge_adjacent_moments(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    left_moment_id: &str,
    right_moment_id: &str,
) -> PersistenceResult<()> {
    repository.merge_adjacent_moments(project_id, left_moment_id, right_moment_id)
}

pub fn split_moment(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    moment_id: &str,
    after_asset_id: &MediaAssetId,
) -> PersistenceResult<()> {
    ensure_asset_project(repository, project_id, after_asset_id)?;
    repository.split_moment(project_id, moment_id, &after_asset_id.to_string())
}

pub fn load_moment_checklists(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<Vec<MomentChecklistView>> {
    let items = repository
        .coverage_checklist_items(project_id)?
        .into_iter()
        .map(coverage_checklist_item_view)
        .collect::<Vec<_>>();
    // M7 I has a single optional project-local checklist. The explicit stable projection leaves
    // room for later user-managed checklist collections without assigning hidden categories.
    Ok(vec![MomentChecklistView {
        id: format!("project:{}:coverage", project_id),
        name: "Project checklist".into(),
        items,
    }])
}

pub fn create_coverage_checklist_item(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    input: &CreateCoverageChecklistItemInput,
) -> PersistenceResult<()> {
    let phrase = input.phrase.trim();
    if phrase.is_empty() {
        return Err(PersistenceError::InvalidData(
            "checklist phrase cannot be empty".into(),
        ));
    }
    repository.create_coverage_checklist_item(&CoverageChecklistItemRecord {
        id: Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        text: phrase.into(),
        created_at: Utc::now().to_rfc3339(),
    })
}

pub fn update_coverage_confirmation(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    input: &UpdateCoverageConfirmationInput,
) -> PersistenceResult<()> {
    repository.update_coverage_confirmation(
        project_id,
        &input.checklist_item_id,
        &input.state,
        input.moment_id.as_deref(),
        input.asset_id.as_deref(),
    )
}

/// Runs an explicitly requested, local Moment Brain analysis. This is deliberately separate
/// from project loading: it reads only durable catalog evidence and M6 embeddings, and it never
/// opens originals, mutates decisions, rewrites Similar Sets, or requires a network service.
pub fn start_moment_analysis(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    provider_cache: &SiglipProviderCache,
    resource_mode: AnalysisResourceMode,
    rebuild: bool,
    should_pause: impl Fn() -> bool,
    on_progress: impl FnMut(&MomentAnalysisProgress),
) -> PersistenceResult<MomentAnalysisProgress> {
    let mut on_progress = on_progress;
    let existing_timeline = repository.moment_timeline_status(project_id)?;
    // Model inspection is permitted here because the photographer explicitly started analysis.
    // A missing pack is not a structural-analysis failure: stored compatible vectors may still
    // support boundaries, while labels correctly abstain without a local text embedder.
    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let provider_identity = provider
        .as_deref()
        .map(|provider| provider.identity().clone());
    let persisted_model = existing_timeline
        .as_ref()
        .and_then(|timeline| timeline.semantic_model_key.as_deref())
        .and_then(semantic_model_config_from_cache_key);
    let model = provider_identity
        .as_ref()
        .map(semantic_model_config)
        .or(persisted_model);
    let semantic_model_key = provider_identity
        .as_ref()
        .map(SemanticProviderIdentity::cache_key)
        .or_else(|| {
            existing_timeline
                .as_ref()
                .and_then(|timeline| timeline.semantic_model_key.clone())
        });
    let model_changed = existing_timeline
        .as_ref()
        .and_then(|timeline| timeline.semantic_model_key.as_deref())
        .zip(semantic_model_key.as_deref())
        .is_some_and(|(previous, current)| previous != current);
    // A normal update asks persistence for just two whole trailing Moments plus new/unresolved
    // stills. A model/version change or explicit rebuild deliberately falls back to a full
    // projection because semantic spaces must never be mixed silently.
    let mut incremental_window = if !rebuild && !model_changed {
        repository.moment_incremental_analysis_window(project_id, model.as_ref())?
    } else {
        None
    }
    .filter(|window| window.active_semantic_model_key == semantic_model_key);
    let mut inputs = if let Some(window) = incremental_window.as_ref() {
        let mut inputs = window.preceding_context.clone();
        inputs.extend(window.pending_inputs.clone());
        inputs
    } else {
        repository.moment_analysis_inputs(project_id, model.as_ref())?
    };
    let now = Utc::now();
    let timeline_id = existing_timeline
        .as_ref()
        .map(|timeline| timeline.timeline_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::MomentAnalysis,
        items_completed: 0,
        items_total: Some(inputs.len() as u64),
        files_discovered: inputs.len() as u64,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({
            "pipeline": "moment-analysis",
            "resource_mode": resource_mode.as_str(),
            "requested_rebuild": rebuild,
            "semantic_model_key": semantic_model_key.clone(),
        })),
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    let initial_message = if rebuild || model_changed {
        "Rebuilding the local Moment timeline. AI-derived boundaries and suggestions may change; human labels, representatives, merges, and splits remain separate."
    } else {
        "Analyzing local shoot structure from durable timestamps, existing evidence, and compatible cached embeddings."
    };
    let initial_progress = moment_progress(
        Some(&job),
        existing_timeline.as_ref(),
        Some(initial_message.into()),
    );
    on_progress(&initial_progress);
    if should_pause() {
        return pause_moment_analysis(
            repository,
            &mut job,
            existing_timeline.as_ref(),
            &mut on_progress,
            "Moment analysis paused before any local projection was replaced.",
        );
    }

    let checklist_items = repository.coverage_checklist_items(project_id)?;
    let label_candidates = moment_label_candidates(
        provider.as_deref(),
        provider_identity.as_ref(),
        &checklist_items,
    );
    let mut timeline_assets =
        materialize_moment_timeline_assets(&inputs, semantic_model_key.as_deref(), resource_mode);
    let config = TimelineAnalysisConfig::default();
    let mut tail_window = None::<MomentIncrementalAnalysisWindow>;
    let mut incremental_fallback_reason = None::<String>;
    let analysis = if let Some(window) = incremental_window.take() {
        let previous_latest = window
            .previous_latest_captured_at
            .as_deref()
            .and_then(moment_unix_millis);
        if let Some(previous_latest) = previous_latest {
            let context_length = window.preceding_context.len();
            match analyze_append_only_tail(&AppendOnlyTailAnalysisRequest {
                previous_latest_captured_at_unix_ms: previous_latest,
                preceding_context_assets: timeline_assets[..context_length].to_vec(),
                appended_assets: timeline_assets[context_length..].to_vec(),
                label_candidates: label_candidates.clone(),
                config: config.clone(),
            }) {
                Ok(result)
                    if matches!(
                        result.disposition,
                        IncrementalDisposition::TailReanalyzed
                            | IncrementalDisposition::UngroupedAssetsUpdated
                    ) =>
                {
                    let Some(analysis) = result.analysis else {
                        return fail_moment_analysis(
                            repository,
                            &mut job,
                            existing_timeline.as_ref(),
                            &mut on_progress,
                            "Bounded Moment update returned no analysis projection.".into(),
                        );
                    };
                    tail_window = Some(window);
                    analysis
                }
                Ok(result) if matches!(result.disposition, IncrementalDisposition::NoChanges) => {
                    job.state = WorkflowRunState::Completed;
                    job.updated_at = Utc::now();
                    job.finished_at = Some(job.updated_at);
                    let progress = moment_progress(
                        Some(&job),
                        existing_timeline.as_ref(),
                        Some("No new local still-photo timeline evidence was found; the existing Moment projection remains unchanged.".into()),
                    );
                    job.resume_metadata = Some(serde_json::json!({
                        "pipeline": "moment-analysis",
                        "resource_mode": resource_mode.as_str(),
                        "incremental": true,
                        "summary": &progress,
                    }));
                    repository.update_background_job(&job)?;
                    on_progress(&progress);
                    return Ok(progress);
                }
                Ok(result) => {
                    incremental_fallback_reason = Some(result.reason);
                    inputs = repository.moment_analysis_inputs(project_id, model.as_ref())?;
                    timeline_assets = materialize_moment_timeline_assets(
                        &inputs,
                        semantic_model_key.as_deref(),
                        resource_mode,
                    );
                    job.items_total = Some(inputs.len() as u64);
                    job.files_discovered = inputs.len() as u64;
                    job.updated_at = Utc::now();
                    repository.update_background_job(&job)?;
                    analyze_timeline(&TimelineAnalysisRequest {
                        assets: timeline_assets.clone(),
                        label_candidates,
                        config,
                    })
                    .map_err(|error| {
                        PersistenceError::InvalidData(format!(
                            "Local Moment full rebuild could not safely process the project: {error}"
                        ))
                    })?
                }
                Err(error) => {
                    return fail_moment_analysis(
                        repository,
                        &mut job,
                        existing_timeline.as_ref(),
                        &mut on_progress,
                        format!("Bounded local Moment update could not safely run: {error}"),
                    )
                }
            }
        } else {
            incremental_fallback_reason = Some(
                "The prior Moment tail has no valid capture timestamp, so CaptureOS conservatively rebuilt the local projection.".into(),
            );
            inputs = repository.moment_analysis_inputs(project_id, model.as_ref())?;
            timeline_assets = materialize_moment_timeline_assets(
                &inputs,
                semantic_model_key.as_deref(),
                resource_mode,
            );
            job.items_total = Some(inputs.len() as u64);
            job.files_discovered = inputs.len() as u64;
            job.updated_at = Utc::now();
            repository.update_background_job(&job)?;
            analyze_timeline(&TimelineAnalysisRequest {
                assets: timeline_assets.clone(),
                label_candidates,
                config,
            })
            .map_err(|error| {
                PersistenceError::InvalidData(format!(
                    "Local Moment full rebuild could not safely process the project: {error}"
                ))
            })?
        }
    } else {
        analyze_timeline(&TimelineAnalysisRequest {
            assets: timeline_assets.clone(),
            label_candidates,
            config,
        })
        .map_err(|error| {
            PersistenceError::InvalidData(format!(
                "Local Moment analysis could not safely process the project: {error}"
            ))
        })?
    };
    if should_pause() {
        return pause_moment_analysis(
            repository,
            &mut job,
            existing_timeline.as_ref(),
            &mut on_progress,
            "Moment analysis paused before its new local projection was committed.",
        );
    }

    let overrides = repository.active_moment_override_operations(project_id)?;
    let run_id = Uuid::new_v4().to_string();
    let finished_at = Utc::now();
    let moment_ordinal_offset = tail_window
        .as_ref()
        .map(|window| window.moment_ordinal_base)
        .unwrap_or(0);
    let membership_ordinal_offset = tail_window
        .as_ref()
        .map(|window| window.affected_tail_start_ordinal)
        .unwrap_or(0);
    let capture_time_by_asset = inputs
        .iter()
        .filter_map(|input| {
            input
                .captured_at
                .as_ref()
                .map(|captured_at| (input.asset_id.clone(), captured_at.clone()))
        })
        .collect::<HashMap<_, _>>();
    let projection = materialize_moment_projection(MomentProjectionRequest {
        project_id,
        timeline_id: &timeline_id,
        run_id: &run_id,
        analysis: &analysis,
        assets: &timeline_assets,
        capture_time_by_asset: &capture_time_by_asset,
        overrides: &overrides,
        created_at: &finished_at,
        moment_ordinal_offset,
        membership_ordinal_offset,
    })?;
    let input_catalog_version = tail_window.as_ref().map_or_else(
        || moment_input_catalog_version(&inputs, semantic_model_key.as_deref()),
        |window| {
            moment_incremental_catalog_version(
                existing_timeline
                    .as_ref()
                    .map(|timeline| timeline.input_catalog_version.as_str()),
                window,
                &inputs,
                semantic_model_key.as_deref(),
            )
        },
    );
    let timeline = MomentTimelineStatusRecord {
        timeline_id: timeline_id.clone(),
        project_id: project_id.to_string(),
        state: "ready".into(),
        analyzer_id: MOMENT_ANALYZER_ID.into(),
        analyzer_version: analysis.algorithm_version.clone(),
        boundary_algorithm_version: MOMENT_BOUNDARY_ALGORITHM_VERSION.into(),
        semantic_model_key: semantic_model_key.clone(),
        input_catalog_version,
        active_run_id: Some(run_id.clone()),
        moment_count: projection.moments.len() as u64,
        eligible_count: inputs.len() as u64,
        ungrouped_count: projection
            .memberships
            .iter()
            .filter(|membership| membership.membership_state == "ungrouped")
            .count() as u64,
        updated_at: finished_at.to_rfc3339(),
    };
    let run = MomentAnalysisRunRecord {
        id: run_id,
        timeline_id,
        project_id: project_id.to_string(),
        state: "completed".into(),
        analyzer_id: MOMENT_ANALYZER_ID.into(),
        analyzer_version: analysis.algorithm_version.clone(),
        boundary_algorithm_version: MOMENT_BOUNDARY_ALGORITHM_VERSION.into(),
        semantic_model_key,
        input_catalog_version: timeline.input_catalog_version.clone(),
        items_total: inputs.len() as u64,
        items_completed: inputs.len() as u64,
        // Diagnostics retain individual unavailable/invalid evidence without pretending every
        // non-fatal diagnostic was a failed asset.
        error_count: 0,
        started_at: job.created_at.to_rfc3339(),
        finished_at: Some(finished_at.to_rfc3339()),
    };
    let persisted = if let Some(window) = tail_window.as_ref() {
        // The tail writer uses the `active_run_id` carried by this record as an optimistic
        // concurrency guard for the *previous* durable projection. The new run is carried by
        // `run` and becomes active atomically inside the repository. Passing the planned new
        // run here would make a valid append-only update look like an in-place rewrite.
        let mut tail_timeline = timeline.clone();
        tail_timeline.active_run_id = Some(window.active_run_id.clone());
        repository.replace_active_moment_analysis_tail(
            &tail_timeline,
            &run,
            window.affected_tail_start_ordinal,
            &projection.segments,
            &projection.moments,
            &projection.memberships,
            &projection.boundaries,
        )
    } else {
        repository.replace_active_moment_analysis(
            &timeline,
            &run,
            &projection.segments,
            &projection.moments,
            &projection.memberships,
            &projection.boundaries,
        )
    };
    if let Err(error) = persisted {
        return fail_moment_analysis(
            repository,
            &mut job,
            existing_timeline.as_ref(),
            &mut on_progress,
            format!("Local Moment projection could not be saved: {error}"),
        );
    }

    // A full local projection may derive a narrow camera-time advisory from existing Similar
    // Set co-membership. A bounded tail update intentionally abstains instead of carrying a
    // prior full-run advisory into a new run: newly appended related frames could conflict with
    // it, and Moment Brain must never imply that an unexamined diagnostic remains current.
    let mut clock_diagnostic_persistence_failed = false;
    let clock_diagnostic_count = if tail_window.is_none() {
        let diagnostics =
            moment_clock_diagnostic_records(project_id, &run.id, &timeline_assets, finished_at);
        let count = diagnostics.len();
        if !diagnostics.is_empty()
            && repository
                .record_camera_clock_offset_diagnostics(&diagnostics)
                .is_err()
        {
            // The timeline is already durable and must remain usable. A clock observation is
            // optional advisory evidence, so a storage failure never rolls back, blocks, or
            // converts the structural analysis into a fabricated successful diagnostic.
            clock_diagnostic_persistence_failed = true;
            job.error_count = job.error_count.saturating_add(1);
        }
        count
    } else {
        0
    };

    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::MomentAnalysis;
    job.items_completed = inputs.len() as u64;
    job.files_processed = inputs.len() as u64;
    job.updated_at = finished_at;
    job.finished_at = Some(finished_at);
    let persisted_timeline = repository.moment_timeline_status(project_id)?;
    let mut completed_message = if provider.is_some() {
        "Local Moment timeline is ready. Suggested labels use only the reviewed vocabulary or exact photographer checklist phrases; uncertain labels remain Untitled Moment."
    } else {
        model_status.message.as_deref().unwrap_or(
            "Local structural timeline is ready; semantic label suggestions were unavailable.",
        )
    }
    .to_owned();
    if clock_diagnostic_persistence_failed {
        completed_message.push_str(
            " A possible camera-time advisory could not be saved; no timestamps were changed.",
        );
        job.error_message = Some(
            "A local camera-time advisory could not be saved; the Moment timeline remains ready and no timestamps were changed."
                .into(),
        );
    }
    let completed_progress = moment_progress(
        Some(&job),
        persisted_timeline.as_ref(),
        Some(completed_message),
    );
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "moment-analysis",
        "resource_mode": resource_mode.as_str(),
        "requested_rebuild": rebuild,
        "incremental": tail_window.is_some(),
        "incremental_fallback_reason": incremental_fallback_reason,
        "clock_offset_diagnostic_count": clock_diagnostic_count,
        "summary": &completed_progress,
        "diagnostic_count": analysis.diagnostics.len(),
    }));
    repository.update_background_job(&job)?;
    on_progress(&completed_progress);
    Ok(completed_progress)
}

/// Marks interrupted local Moment jobs safely at desktop startup. It does not start analysis,
/// load a model, or change the last durable timeline projection.
pub fn recover_interrupted_moment_analysis(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_moment_analysis()
}

fn pause_moment_analysis(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    timeline: Option<&MomentTimelineStatusRecord>,
    on_progress: &mut impl FnMut(&MomentAnalysisProgress),
    message: &str,
) -> PersistenceResult<MomentAnalysisProgress> {
    job.state = WorkflowRunState::Paused;
    job.updated_at = Utc::now();
    let progress = moment_progress(Some(job), timeline, Some(message.into()));
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "moment-analysis",
        "summary": &progress,
    }));
    repository.update_background_job(job)?;
    on_progress(&progress);
    Ok(progress)
}

fn fail_moment_analysis(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    timeline: Option<&MomentTimelineStatusRecord>,
    on_progress: &mut impl FnMut(&MomentAnalysisProgress),
    message: String,
) -> PersistenceResult<MomentAnalysisProgress> {
    let persistence_failed = message.starts_with("Local Moment projection could not be saved:");
    let user_message = if persistence_failed {
        if timeline.is_some_and(|timeline| timeline.state == "ready") {
            "Timeline update could not be saved. Your previous timeline is still available."
                .to_owned()
        } else {
            "Timeline update could not be saved. No previous timeline was available.".to_owned()
        }
    } else {
        message.clone()
    };
    job.state = WorkflowRunState::Failed;
    job.error_count = job.error_count.saturating_add(1);
    // Keep the exact storage failure in durable local developer diagnostics, but never make a
    // raw SQLite exception the primary timeline message. The prior projection is untouched
    // because its replacement transaction has already rolled back.
    job.error_message = Some(message.clone());
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    let progress = moment_progress(Some(job), timeline, Some(user_message.clone()));
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "moment-analysis",
        "user_message": user_message,
        "developer_details": message,
        "summary": &progress,
    }));
    repository.update_background_job(job)?;
    on_progress(&progress);
    Ok(progress)
}

fn semantic_model_config_from_cache_key(value: &str) -> Option<SemanticModelConfig> {
    let parts = value.split('|').collect::<Vec<_>>();
    let [model_id, provider, model_version, embedding_version, preprocessing_version, metric, dimensions] =
        parts.as_slice()
    else {
        return None;
    };
    let dimensions = dimensions.parse::<usize>().ok()?;
    if dimensions == 0
        || [
            *model_id,
            *provider,
            *model_version,
            *embedding_version,
            *preprocessing_version,
            *metric,
        ]
        .iter()
        .any(|part| part.trim().is_empty())
    {
        return None;
    }
    Some(SemanticModelConfig {
        model_id: (*model_id).into(),
        provider: (*provider).into(),
        model_version: (*model_version).into(),
        // Persisted M6 rows use the full embedding-version tuple including dimensions. Rebuild
        // the exact tuple from the cache key so an installed provider can reuse durable local
        // embeddings offline instead of treating its own compatible vectors as unavailable.
        embedding_version: format!("{embedding_version};dimensions={dimensions}"),
        preprocessing_version: (*preprocessing_version).into(),
        metric: (*metric).into(),
        dimensions,
    })
}

fn moment_input_catalog_version(
    inputs: &[MomentAnalysisInput],
    semantic_model_key: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"captureos-m7-moment-inputs-v1\0");
    hasher.update(
        semantic_model_key
            .unwrap_or("no-compatible-semantic-model")
            .as_bytes(),
    );
    for input in inputs {
        for value in [
            input.asset_id.as_str(),
            input.captured_at.as_deref().unwrap_or(""),
            input.capture_time_source.as_deref().unwrap_or(""),
            input.camera_model.as_deref().unwrap_or(""),
            input.lens_model.as_deref().unwrap_or(""),
            input.orientation.as_deref().unwrap_or(""),
            input.technical_quality_band.as_deref().unwrap_or(""),
            input.decision.as_deref().unwrap_or(""),
        ] {
            hasher.update(value.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(&input.face_count.unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(
            &input
                .technical_quality_score
                .unwrap_or(f64::NAN)
                .to_le_bytes(),
        );
        hasher.update(&[input.rating, u8::from(input.starred)]);
        for group_id in &input.similar_set_ids {
            hasher.update(group_id.as_bytes());
            hasher.update(&[0]);
        }
        if let Some(embedding) = &input.embedding {
            for value in embedding {
                hasher.update(&value.to_le_bytes());
            }
        }
        hasher.update(&[0xff]);
    }
    format!("m7:{}", hasher.finalize().to_hex())
}

/// A durable lineage version for a bounded append-only replacement. It records the previous
/// full/tail version plus the exact bounded evidence handed to the analyzer; callers do not
/// misrepresent it as a fresh all-catalog checksum.
fn moment_incremental_catalog_version(
    previous_version: Option<&str>,
    window: &MomentIncrementalAnalysisWindow,
    inputs: &[MomentAnalysisInput],
    semantic_model_key: Option<&str>,
) -> String {
    let bounded = moment_input_catalog_version(inputs, semantic_model_key);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"captureos-m7-moment-incremental-lineage-v1\0");
    hasher.update(previous_version.unwrap_or("no-prior-version").as_bytes());
    hasher.update(&[0]);
    hasher.update(window.timeline_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(window.active_run_id.as_bytes());
    hasher.update(&window.affected_tail_start_ordinal.to_le_bytes());
    hasher.update(&window.moment_ordinal_base.to_le_bytes());
    hasher.update(bounded.as_bytes());
    format!("m7-incremental:{}", hasher.finalize().to_hex())
}

fn moment_unix_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc).timestamp_millis())
        .or_else(|| {
            // Unknown-offset EXIF values remain local wall-clock strings in persistence and UI.
            // Moment Brain needs only a stable within-project ordering coordinate, so map the
            // naive value onto an internal epoch without ever serializing it as claimed UTC.
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
                .ok()
                .map(|value| Utc.from_utc_datetime(&value).timestamp_millis())
        })
}

fn moment_label_candidates(
    provider: Option<&SiglipOnnxProvider>,
    identity: Option<&SemanticProviderIdentity>,
    checklist_items: &[PersistedCoverageChecklistItemView],
) -> Vec<LabelCandidate> {
    let mut candidates = GenericLabelConcept::REVIEWED
        .into_iter()
        .map(|concept| LabelCandidate {
            candidate_id: format!("reviewed-generic:{}", concept.display_name()),
            kind: LabelCandidateKind::ReviewedGeneric { concept },
            semantic_embedding: moment_label_embedding(provider, identity, concept.display_name()),
        })
        .collect::<Vec<_>>();
    candidates.extend(checklist_items.iter().filter_map(|item| {
        let phrase = item.text.trim();
        (!phrase.is_empty()).then(|| LabelCandidate {
            candidate_id: format!("project-checklist:{}", item.id),
            kind: LabelCandidateKind::HumanProjectPhrase {
                phrase: phrase.into(),
            },
            semantic_embedding: moment_label_embedding(provider, identity, phrase),
        })
    }));
    candidates
}

fn moment_label_embedding(
    provider: Option<&SiglipOnnxProvider>,
    identity: Option<&SemanticProviderIdentity>,
    phrase: &str,
) -> Option<SemanticVector> {
    let provider = provider?;
    let identity = identity?;
    let values = provider.embed_text(phrase).ok()?;
    let values = normalize_embedding(values).ok()?;
    (values.len() == identity.dimensions).then(|| SemanticVector {
        compatibility_key: identity.cache_key(),
        values,
    })
}

// Moment Brain remains a deterministic sequential structural analyzer, but transforming a large
// durable input page into its pure engine form must not monopolize the background worker. These
// modes alter only batch/yield cadence; they never change evidence inputs, ordering, thresholds,
// candidate vocabulary, or labels.
const MOMENT_ECO_TRANSFORM_BATCH_SIZE: usize = 16;
const MOMENT_BALANCED_TRANSFORM_BATCH_SIZE: usize = 64;
const MOMENT_FAST_TRANSFORM_BATCH_SIZE: usize = 256;

fn moment_transform_batch_size(resource_mode: AnalysisResourceMode) -> usize {
    match resource_mode {
        AnalysisResourceMode::Eco => MOMENT_ECO_TRANSFORM_BATCH_SIZE,
        AnalysisResourceMode::Balanced => MOMENT_BALANCED_TRANSFORM_BATCH_SIZE,
        AnalysisResourceMode::Fast => MOMENT_FAST_TRANSFORM_BATCH_SIZE,
    }
}

fn materialize_moment_timeline_assets(
    inputs: &[MomentAnalysisInput],
    semantic_model_key: Option<&str>,
    resource_mode: AnalysisResourceMode,
) -> Vec<TimelineAssetInput> {
    let batch_size = moment_transform_batch_size(resource_mode);
    let batch_count = (inputs.len().saturating_add(batch_size - 1)) / batch_size;
    let mut assets = Vec::with_capacity(inputs.len());
    for (batch_index, batch) in inputs.chunks(batch_size).enumerate() {
        assets.extend(
            batch
                .iter()
                .map(|input| moment_timeline_asset(input, semantic_model_key)),
        );
        // `yield_now` is intentionally the only mode-dependent behavior: the same vector of
        // inputs is passed to the analyzer in the same chronological/catalog order in all modes.
        if batch_index.saturating_add(1) < batch_count {
            thread::yield_now();
        }
    }
    assets
}

fn moment_timeline_asset(
    input: &MomentAnalysisInput,
    semantic_model_key: Option<&str>,
) -> TimelineAssetInput {
    let orientation = input.orientation.as_deref().and_then(|value| {
        let value = value.trim().to_ascii_lowercase();
        match value.as_str() {
            "landscape" | "horizontal" => Some(MomentOrientation::Landscape),
            "portrait" | "vertical" => Some(MomentOrientation::Portrait),
            "square" => Some(MomentOrientation::Square),
            _ => None,
        }
    });
    let decision = input.decision.as_deref().and_then(|value| match value {
        "keep" => Some(ExistingHumanDecision::Keep),
        "reject" => Some(ExistingHumanDecision::Reject),
        "review" => Some(ExistingHumanDecision::Review),
        _ => None,
    });
    let semantic_embedding = semantic_model_key.and_then(|compatibility_key| {
        input
            .embedding
            .clone()
            .and_then(|values| normalize_embedding(values).ok())
            .map(|values| SemanticVector {
                compatibility_key: compatibility_key.into(),
                values,
            })
    });
    TimelineAssetInput {
        asset_id: input.asset_id.clone(),
        captured_at_unix_ms: input.captured_at.as_deref().and_then(moment_unix_millis),
        camera_model: input.camera_model.clone(),
        lens_model: input.lens_model.clone(),
        orientation,
        anonymous_face_count: input.face_count.and_then(|count| u32::try_from(count).ok()),
        similar_set_ids: Some(input.similar_set_ids.clone()),
        // CaptureOS deliberately does not parse source filenames for Moment Brain. A future
        // sequence field may be supplied by a dedicated, tested metadata adapter.
        filename_sequence: None,
        semantic_embedding,
        technical_presentation_score: input
            .technical_quality_score
            .filter(|score| score.is_finite())
            .map(|score| (score / 100.0).clamp(0.0, 1.0) as f32),
        human_presentation: HumanPresentationSignals {
            decision,
            rating: (input.rating > 0).then_some(input.rating),
            // A missing star and an explicit false star are both non-positive existing state;
            // neither is used to downgrade a representative.
            starred: input.starred.then_some(true),
        },
    }
}

/// Converts the pure, conservative Moment Brain result into a current-run local persistence
/// record. The signed offset remains private evidence; normal UI presentation is intentionally
/// direction-free because this is not a claim that either camera's timestamp is correct.
fn moment_clock_diagnostic_records(
    project_id: &ProjectId,
    run_id: &str,
    assets: &[TimelineAssetInput],
    created_at: DateTime<Utc>,
) -> Vec<CameraClockOffsetDiagnosticRecord> {
    derive_clock_offset_diagnostics(assets)
        .into_iter()
        .map(|diagnostic| CameraClockOffsetDiagnosticRecord {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            camera_a: diagnostic.camera_a,
            camera_b: diagnostic.camera_b,
            possible_offset_seconds: Some(round_milliseconds_to_seconds(
                diagnostic.median_offset_ms,
            )),
            evidence_json: serde_json::json!({
                "state": "possible_offset",
                "method": diagnostic.method,
                "medianOffsetMilliseconds": diagnostic.median_offset_ms,
                "medianAbsoluteDeviationMilliseconds": diagnostic.median_absolute_deviation_ms,
                "independentSimilarSetComparisons": diagnostic.comparison_count,
                "supportingSimilarSetIds": diagnostic.supporting_similar_set_ids,
                "scope": "current_project_existing_similar_sets",
            }),
            created_at: created_at.to_rfc3339(),
        })
        .collect()
}

/// Round rather than truncate the signed local observation so a stored `possible_offset_seconds`
/// remains a faithful, compact representation of its derived millisecond evidence.
fn round_milliseconds_to_seconds(value: i64) -> i64 {
    let seconds = value / 1_000;
    let remainder = value % 1_000;
    if remainder.unsigned_abs() >= 500 {
        seconds.saturating_add(value.signum())
    } else {
        seconds
    }
}

fn moment_clock_diagnostic_view(
    diagnostic: CameraClockOffsetDiagnosticRecord,
) -> Option<MomentClockDiagnosticView> {
    let seconds = diagnostic.possible_offset_seconds?;
    // A database row is not necessarily M7 engine output (for example, an interrupted or older
    // implementation may have written incomplete evidence). Keep the normal UI stricter than
    // storage and abstain unless it still meets the documented one-minute minimum.
    if seconds.unsigned_abs() < 60 {
        return None;
    }
    Some(MomentClockDiagnosticView {
        camera_label: format!("{} ↔ {}", diagnostic.camera_a, diagnostic.camera_b),
        summary: format!(
            "Possible camera time offset: related local frames consistently differ by about {}. CaptureOS did not change any timestamps.",
            concise_clock_offset_duration(seconds.unsigned_abs())
        ),
        state: "possible_offset".into(),
    })
}

fn concise_clock_offset_duration(seconds: u64) -> String {
    let minutes = (seconds.saturating_add(30) / 60).max(1);
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    match (hours, remaining_minutes) {
        (0, minutes) => format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" }),
        (hours, 0) => format!("{hours} hour{}", if hours == 1 { "" } else { "s" }),
        (hours, minutes) => format!(
            "{hours} hour{} {minutes} minute{}",
            if hours == 1 { "" } else { "s" },
            if minutes == 1 { "" } else { "s" },
        ),
    }
}

struct MaterializedMomentProjection {
    segments: Vec<TimelineSegmentRecord>,
    moments: Vec<MomentRecord>,
    memberships: Vec<MomentMembershipRecord>,
    boundaries: Vec<MomentBoundaryEvidenceRecord>,
}

/// All inputs necessary to materialize one derived Moment projection. Grouping them preserves a
/// legible boundary between analysis and persistence without an error-prone positional argument
/// list; no user-facing data or original-media reference crosses this boundary.
struct MomentProjectionRequest<'a> {
    project_id: &'a ProjectId,
    timeline_id: &'a str,
    run_id: &'a str,
    analysis: &'a TimelineAnalysis,
    assets: &'a [TimelineAssetInput],
    /// Persist the original normalized capture string for presentation. This is especially
    /// important for unknown-timezone camera clocks, whose internal ordering coordinate must
    /// never be written back as an invented UTC value.
    capture_time_by_asset: &'a HashMap<String, String>,
    overrides: &'a [MomentOverrideOperation],
    created_at: &'a DateTime<Utc>,
    moment_ordinal_offset: u64,
    membership_ordinal_offset: u64,
}

fn materialize_moment_projection(
    request: MomentProjectionRequest<'_>,
) -> PersistenceResult<MaterializedMomentProjection> {
    let MomentProjectionRequest {
        project_id,
        timeline_id,
        run_id,
        analysis,
        assets,
        capture_time_by_asset,
        overrides,
        created_at,
        moment_ordinal_offset,
        membership_ordinal_offset,
    } = request;
    let assets_by_id = assets
        .iter()
        .map(|asset| (asset.asset_id.clone(), asset))
        .collect::<HashMap<_, _>>();
    let automatic_segments = analysis
        .moments
        .iter()
        .map(|segment| (segment.asset_ids.clone(), segment))
        .collect::<HashMap<_, _>>();
    let automatic_boundaries = analysis
        .boundaries
        .iter()
        .map(|boundary| (boundary.right_asset_id.clone(), boundary))
        .collect::<HashMap<_, _>>();
    let mut automatic_breaks = HashSet::new();
    for boundary in &analysis.boundaries {
        if matches!(
            boundary.category,
            moment_brain::BoundaryCategory::Moderate | moment_brain::BoundaryCategory::Strong
        ) {
            automatic_breaks.insert((
                boundary.left_asset_id.clone(),
                boundary.right_asset_id.clone(),
            ));
        }
    }
    // Later active events win deterministically if a user has explicitly changed the same
    // boundary more than once. Both operations stay in durable history; only the current event
    // is applied to the new AI projection.
    let mut overrides_by_boundary = HashMap::new();
    for operation in overrides.iter().filter(|operation| operation.active) {
        overrides_by_boundary.insert(
            (
                operation.left_asset_id.clone(),
                operation.right_asset_id.clone(),
            ),
            operation,
        );
    }
    let mut groups = Vec::<Vec<String>>::new();
    if let Some(first_asset_id) = analysis.ordered_asset_ids.first() {
        let mut current = vec![first_asset_id.clone()];
        for asset_id in analysis.ordered_asset_ids.iter().skip(1) {
            let left_asset_id = current
                .last()
                .expect("a non-empty local Moment group has a last asset")
                .clone();
            let boundary = (left_asset_id.clone(), asset_id.clone());
            let mut split = automatic_breaks.contains(&boundary);
            if let Some(operation) = overrides_by_boundary.get(&boundary) {
                split = match operation.operation.as_str() {
                    "split" => true,
                    "merge" => false,
                    _ => split,
                };
            }
            if split {
                groups.push(current);
                current = vec![asset_id.clone()];
            } else {
                current.push(asset_id.clone());
            }
        }
        groups.push(current);
    }

    let created_at_text = created_at.to_rfc3339();
    let mut segments = Vec::with_capacity(groups.len());
    let mut moments = Vec::with_capacity(groups.len());
    let mut membership_to_moment = HashMap::<String, String>::new();
    for (relative_ordinal, group) in groups.iter().enumerate() {
        let ordinal = moment_ordinal_offset.saturating_add(relative_ordinal as u64);
        let automatic = automatic_segments.get(group).copied();
        let segment_id = Uuid::new_v4().to_string();
        let moment_id = Uuid::new_v4().to_string();
        let first = assets_by_id.get(&group[0]).copied().ok_or_else(|| {
            PersistenceError::InvalidData(
                "Moment output referenced an unknown timeline asset".into(),
            )
        })?;
        let last = assets_by_id
            .get(group.last().expect("non-empty Moment group"))
            .copied()
            .ok_or_else(|| {
                PersistenceError::InvalidData(
                    "Moment output referenced an unknown timeline asset".into(),
                )
            })?;
        let boundary_before = automatic_boundaries.get(&group[0]).copied();
        let (suggested_label, label_confidence, label_state, label_evidence, ai_representative) =
            if let Some(segment) = automatic {
                let state = moment_enum_label(&segment.ai_label.state);
                let source = segment.ai_label.source.as_ref().map(moment_enum_label);
                let label_state = match (state.as_str(), source.as_deref()) {
                    ("suggested", Some("human_project_phrase")) => {
                        "project_checklist_semantic_candidate"
                    }
                    ("suggested", Some("reviewed_generic_concepts")) => "generic_visual_vocabulary",
                    ("semantic_evidence_unavailable", _) => "semantic_evidence_unavailable",
                    _ => "abstained",
                }
                .to_owned();
                let representative = if group.contains(&segment.ai_representative.asset_id) {
                    segment.ai_representative.asset_id.clone()
                } else {
                    group[0].clone()
                };
                (
                    (state == "suggested").then(|| segment.ai_label.display_label.clone()),
                    segment.ai_label.semantic_similarity.map(f64::from),
                    label_state,
                    serde_json::json!({
                        "state": state,
                        "source": source,
                        "concepts": segment.ai_label.supporting_generic_concepts.iter().map(|concept| concept.display_name()).collect::<Vec<_>>(),
                        "evidence": segment.ai_label.evidence,
                        "compatibilityKey": segment.ai_label.compatibility_key,
                        "supportingVectorCount": segment.ai_label.supporting_vector_count,
                        "representativeEvidence": segment.ai_representative.explanation,
                    }),
                    representative,
                )
            } else {
                (
                    None,
                    None,
                    "abstained_human_structure_override".into(),
                    serde_json::json!({
                        "state": "abstained",
                        "evidence": ["The photographer's protected merge or split changed this automatic segment. CaptureOS abstained from remapping a semantic label across the revised human structure."],
                        "concepts": [],
                    }),
                    group[0].clone(),
                )
            };
        let compatible_key =
            automatic.and_then(|segment| segment.semantic_summary.compatibility_key.as_deref());
        let centroid = moment_centroid(group, &assets_by_id, compatible_key);
        let boundary_evidence = boundary_before.map_or_else(
            || serde_json::json!({"automaticBoundary": false}),
            |boundary| {
                serde_json::json!({
                    "automaticBoundary": true,
                    "category": moment_enum_label(&boundary.category),
                    "explanation": boundary.explanation,
                })
            },
        );
        segments.push(TimelineSegmentRecord {
            id: segment_id.clone(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            ordinal,
            started_at: materialized_moment_capture_time(
                &first.asset_id,
                first.captured_at_unix_ms,
                capture_time_by_asset,
            ),
            ended_at: materialized_moment_capture_time(
                &last.asset_id,
                last.captured_at_unix_ms,
                capture_time_by_asset,
            ),
            asset_count: group.len() as u64,
            boundary_category: boundary_before
                .map(|boundary| moment_enum_label(&boundary.category)),
            boundary_evidence,
            created_at: created_at_text.clone(),
            stale: false,
        });
        moments.push(MomentRecord {
            id: moment_id.clone(),
            project_id: project_id.to_string(),
            timeline_id: timeline_id.into(),
            run_id: run_id.into(),
            segment_id,
            anchor_asset_id: group[0].clone(),
            ordinal,
            started_at: materialized_moment_capture_time(
                &first.asset_id,
                first.captured_at_unix_ms,
                capture_time_by_asset,
            ),
            ended_at: materialized_moment_capture_time(
                &last.asset_id,
                last.captured_at_unix_ms,
                capture_time_by_asset,
            ),
            asset_count: group.len() as u64,
            ai_representative_asset_id: Some(ai_representative),
            centroid_dimensions: centroid.as_ref().map(Vec::len),
            centroid,
            suggested_label,
            label_confidence,
            label_evidence,
            label_state,
            created_at: created_at_text.clone(),
            stale: false,
        });
        for asset_id in group {
            membership_to_moment.insert(asset_id.clone(), moment_id.clone());
        }
    }

    let mut memberships =
        Vec::with_capacity(analysis.ordered_asset_ids.len() + analysis.ungrouped_assets.len());
    for (relative_ordinal, asset_id) in analysis.ordered_asset_ids.iter().enumerate() {
        memberships.push(MomentMembershipRecord {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            moment_id: membership_to_moment.get(asset_id).cloned(),
            media_asset_id: asset_id.clone(),
            ordinal: membership_ordinal_offset.saturating_add(relative_ordinal as u64),
            membership_state: "member".into(),
            created_at: created_at_text.clone(),
            active: true,
        });
    }
    for (offset, asset) in analysis.ungrouped_assets.iter().enumerate() {
        memberships.push(MomentMembershipRecord {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            moment_id: None,
            media_asset_id: asset.asset_id.clone(),
            ordinal: membership_ordinal_offset
                .saturating_add((analysis.ordered_asset_ids.len() + offset) as u64),
            membership_state: "ungrouped".into(),
            created_at: created_at_text.clone(),
            active: true,
        });
    }
    let boundaries = analysis
        .boundaries
        .iter()
        .enumerate()
        .map(|(ordinal, boundary)| MomentBoundaryEvidenceRecord {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            run_id: run_id.into(),
            left_asset_id: boundary.left_asset_id.clone(),
            right_asset_id: boundary.right_asset_id.clone(),
            ordinal: ordinal as u64,
            category: moment_enum_label(&boundary.category),
            components: serde_json::to_value(boundary).unwrap_or_else(|_| {
                serde_json::json!({
                    "availability": "unavailable",
                    "detail": "Local boundary evidence could not be serialized.",
                })
            }),
            explanation: boundary.explanation.clone(),
            created_at: created_at_text.clone(),
        })
        .collect();
    Ok(MaterializedMomentProjection {
        segments,
        moments,
        memberships,
        boundaries,
    })
}

fn moment_centroid(
    asset_ids: &[String],
    assets_by_id: &HashMap<String, &TimelineAssetInput>,
    compatibility_key: Option<&str>,
) -> Option<Vec<f32>> {
    let compatibility_key = compatibility_key?;
    let mut values: Option<Vec<f32>> = None;
    let mut vector_count = 0usize;
    for asset_id in asset_ids {
        let Some(vector) = assets_by_id
            .get(asset_id)
            .and_then(|asset| asset.semantic_embedding.as_ref())
        else {
            continue;
        };
        if vector.compatibility_key != compatibility_key {
            continue;
        }
        match &mut values {
            Some(sum) if sum.len() == vector.values.len() => {
                for (target, value) in sum.iter_mut().zip(&vector.values) {
                    *target += *value;
                }
                vector_count += 1;
            }
            None => {
                values = Some(vector.values.clone());
                vector_count = 1;
            }
            Some(_) => {}
        }
    }
    let mut values = values?;
    if vector_count == 0 {
        return None;
    }
    for value in &mut values {
        *value /= vector_count as f32;
    }
    normalize_embedding(values).ok()
}

fn moment_timestamp(value: Option<i64>) -> Option<String> {
    value
        .and_then(|value| Utc.timestamp_millis_opt(value).single())
        .map(|value| value.to_rfc3339())
}

fn materialized_moment_capture_time(
    asset_id: &str,
    fallback_timestamp: Option<i64>,
    capture_time_by_asset: &HashMap<String, String>,
) -> Option<String> {
    capture_time_by_asset
        .get(asset_id)
        .cloned()
        .or_else(|| moment_timestamp(fallback_timestamp))
}

fn moment_enum_label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unavailable".into())
}

fn coverage_checklist_item_view(item: PersistedCoverageChecklistItemView) -> CoverageChecklistItem {
    CoverageChecklistItem {
        id: item.id,
        phrase: item.text,
        state: item
            .confirmation_state
            .unwrap_or_else(|| "unreviewed".into()),
        confirmed_moment_id: item.moment_id,
        confirmed_asset_id: item.media_asset_id,
        updated_at: item.confirmed_at,
    }
}

fn moment_progress(
    job: Option<&BackgroundJob>,
    timeline: Option<&MomentTimelineStatusRecord>,
    message: Option<String>,
) -> MomentAnalysisProgress {
    let state = job
        .map(|job| workflow_state_label(&job.state))
        .unwrap_or_else(|| {
            timeline
                .map(|timeline| timeline.state.clone())
                .unwrap_or_else(|| "idle".into())
        });
    let stage = job
        .map(|job| job_stage_label(&job.stage))
        .unwrap_or_else(|| "moment_analysis".into());
    let resource_mode = job
        .and_then(|job| job.resume_metadata.as_ref())
        .and_then(|metadata| metadata.get("resource_mode"))
        .and_then(serde_json::Value::as_str)
        .filter(|mode| matches!(*mode, "eco" | "balanced" | "fast"))
        .unwrap_or("balanced")
        .to_owned();
    let timeline_ready = timeline.is_some_and(|timeline| timeline.state == "ready");
    let persisted_user_message = job
        .and_then(|job| job.resume_metadata.as_ref())
        .and_then(|metadata| metadata.get("user_message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    MomentAnalysisProgress {
        active: state == "running" || state == "queued",
        paused: state == "paused",
        resource_mode,
        completed: job.map(|job| job.items_completed).unwrap_or(0),
        total: job.and_then(|job| job.items_total).unwrap_or(0),
        error_count: job.map(|job| job.error_count).unwrap_or(0),
        moment_count: timeline.map(|timeline| timeline.moment_count).unwrap_or(0),
        ungrouped_asset_count: timeline.map(|timeline| timeline.ungrouped_count).unwrap_or(0),
        last_error: job.and_then(|job| job.error_message.clone()),
        message: message.or(persisted_user_message).or_else(|| {
            if timeline_ready {
                Some("Local structural timeline is ready. Human overrides remain separate and protected.".into())
            } else if state == "idle" {
                Some("No local Moment timeline has been analyzed yet.".into())
            } else {
                None
            }
        }),
        state,
        stage,
        timeline_ready,
    }
}

fn moment_summary_view(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    row: &persistence::MomentTimelineRow,
    preview_cache_root: &Path,
    can_merge_with_previous: bool,
) -> PersistenceResult<MomentSummaryView> {
    let (source, strength) = if row.human_label.is_some() {
        ("human".into(), "strong".into())
    } else if row.suggested_label.is_some() {
        let source = if row.label_state.contains("project") {
            "project_checklist"
        } else {
            "generic_visual_vocabulary"
        };
        (source.into(), "moderate".into())
    } else {
        ("none".into(), "unavailable".into())
    };
    let representative_asset = row
        .human_representative_asset_id
        .as_ref()
        .map(|id| (id, "human"))
        .or_else(|| {
            row.ai_representative_asset_id
                .as_ref()
                .map(|id| (id, "ai_suggested"))
        });
    let representative = representative_asset
        .map(|(raw_id, source)| {
            let asset_id = MediaAssetId::try_from(raw_id.as_str())
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
            let detail = load_media_asset_detail(repository, project_id, &asset_id, preview_cache_root)?;
            let item = detail.ok_or_else(|| PersistenceError::InvalidData("moment representative does not belong to the selected project".into()))?.item;
            Ok::<MomentRepresentativeView, PersistenceError>(MomentRepresentativeView {
                asset_id: raw_id.clone(),
                filename: item.filename,
                thumbnail_preview_url: item.thumbnail_preview_url,
                source: source.into(),
                evidence: if source == "human" {
                    vec!["Photographer-selected representative.".into()]
                } else {
                    vec!["Local structural representative using available centrality and technical presentation evidence; not a creative-quality claim.".into()]
                },
            })
        })
        .transpose()?;
    let boundary_before =
        row.boundary_category
            .as_ref()
            .map(|category| MomentBoundaryEvidenceView {
                strength: boundary_strength(category).into(),
                summary: row
                    .boundary_explanation
                    .clone()
                    .unwrap_or_else(|| "Local structural boundary evidence is available.".into()),
                signals: vec![
                    "Only locally available capture, visual, and metadata evidence is considered."
                        .into(),
                ],
            });
    Ok(MomentSummaryView {
        id: row.id.clone(),
        ordinal: row.ordinal,
        can_merge_with_previous,
        label: MomentLabelView {
            display_label: row.display_label.clone(),
            ai_suggested_label: row.suggested_label.clone(),
            human_label: row.human_label.clone(),
            source,
            strength,
            evidence: Vec::new(),
        },
        captured_from: row.started_at.clone(),
        captured_to: row.ended_at.clone(),
        capture_time_state: match (&row.started_at, &row.ended_at) {
            (Some(_), Some(_)) => "observed".into(),
            (Some(_), None) | (None, Some(_)) => "partially_observed".into(),
            (None, None) => "unavailable".into(),
        },
        asset_count: row.asset_count,
        similar_set_count: row.similar_set_count,
        keep_count: row.keep_count,
        reject_count: row.reject_count,
        review_count: row.review_count,
        unreviewed_count: row.unreviewed_count,
        starred_count: row.starred_count,
        technical_issue_count: row.technical_issue_count,
        representative,
        boundary_before,
        has_human_structure_override: row.human_override_present,
    })
}

fn boundary_strength(category: &str) -> &str {
    match category {
        "strong" => "strong",
        "moderate" => "moderate",
        "continuous" => "continuous",
        _ => "unavailable",
    }
}

fn magic_search_history_view(entry: PersistedMagicSearchHistoryEntry) -> MagicSearchHistoryEntry {
    let chips = entry
        .plan
        .get("chips")
        .and_then(serde_json::Value::as_array)
        .map(|chips| {
            chips
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    MagicSearchHistoryEntry {
        id: entry.id,
        query: entry.query_text,
        created_at: entry.used_at,
        parsed_filters: MagicSearchFilterView { chips },
    }
}

#[derive(Debug)]
struct ComputedSemanticCandidate {
    candidate: SemanticInputCandidate,
    status: AnalysisStatus,
    embedding: Option<Vec<f32>>,
    error_message: Option<String>,
}

#[derive(Debug, Clone)]
struct ScoredSemanticCandidate {
    candidate: SemanticSearchCandidate,
    score: f32,
}

/// Builds or refreshes a project-owned, local semantic index. The provider receives only
/// CaptureOS-managed preview pixels; source originals are read only when a preview must first be
/// prepared. Every embedding result is durable before the derived index is atomically replaced.
pub fn index_semantic_embeddings(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    roots: SemanticStorageRoots<'_>,
    provider_cache: &SiglipProviderCache,
    resource_mode: AnalysisResourceMode,
    should_pause: impl Fn() -> bool,
    on_progress: impl FnMut(&SemanticIndexProgress),
) -> PersistenceResult<SemanticIndexProgress> {
    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let provider = provider.ok_or_else(|| {
        PersistenceError::InvalidData(
            model_status
                .message
                .clone()
                .unwrap_or_else(|| "Semantic search model is unavailable".into()),
        )
    })?;
    let mut on_progress = on_progress;
    let mut execution = SemanticIndexExecution {
        resource_mode,
        provider: provider.as_ref(),
        model_status,
        should_pause: &should_pause,
        on_progress: &mut on_progress,
    };
    index_semantic_embeddings_with_provider(repository, project_id, roots, &mut execution)
}

fn index_semantic_embeddings_with_provider(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    roots: SemanticStorageRoots<'_>,
    execution: &mut SemanticIndexExecution<'_>,
) -> PersistenceResult<SemanticIndexProgress> {
    let identity = execution.provider.identity().clone();
    let model = semantic_model_config(&identity);
    // Version replacement preserves previous vectors and indexes as stale audit/cache records.
    // A current model revision never silently mixes incomparable vector spaces.
    repository.mark_other_semantic_embeddings_stale(project_id, &model)?;
    repository.mark_other_semantic_index_versions_stale(project_id, &model)?;
    let candidates = repository.semantic_embedding_candidates(project_id, &model)?;
    let now = Utc::now();
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::SemanticEmbedding,
        items_completed: 0,
        items_total: Some(candidates.len() as u64),
        files_discovered: candidates.len() as u64,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({
            "pipeline": "semantic-indexing",
            "resource_mode": execution.resource_mode.as_str(),
            "model_cache_key": identity.cache_key(),
        })),
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    let mut counts = semantic_counts(repository, project_id, &model, candidates.len() as u64)?;
    let mut progress = semantic_progress(
        Some(&job),
        SemanticProgressDetails {
            counts: counts.clone(),
            model: execution.model_status.clone(),
            index_ready: false,
            index_embedding_count: 0,
            resource_mode: execution.resource_mode,
            current_asset_id: None,
            message: Some(
                "Preparing local still-photo embeddings from CaptureOS-managed previews.".into(),
            ),
        },
    );
    (execution.on_progress)(&progress);

    let worker_count = analysis_worker_count(execution.resource_mode);
    for batch in candidates.chunks(worker_count) {
        if (execution.should_pause)() {
            return pause_semantic_indexing(
                repository,
                &mut job,
                execution,
                SemanticPauseDetails {
                    counts,
                    current_asset_id: None,
                    message: "Indexing paused. Completed local embeddings are durable; resume continues remaining photos."
                        .into(),
                },
            );
        }
        job.stage = JobStage::SemanticEmbedding;
        job.updated_at = Utc::now();
        repository.update_background_job(&job)?;
        progress = semantic_progress(
            Some(&job),
            SemanticProgressDetails {
                counts: counts.clone(),
                model: execution.model_status.clone(),
                index_ready: false,
                index_embedding_count: 0,
                resource_mode: execution.resource_mode,
                current_asset_id: batch.first().map(|candidate| candidate.asset_id.clone()),
                message: Some(format!(
                    "Embedding up to {} local preview{} at a time ({}).",
                    worker_count,
                    if worker_count == 1 { "" } else { "s" },
                    execution.resource_mode.as_str(),
                )),
            },
        );
        (execution.on_progress)(&progress);

        let resolved = batch
            .iter()
            .cloned()
            .map(|candidate| {
                let fallback = candidate.clone();
                resolve_semantic_input(repository, roots.preview_cache_root, candidate)
                    .unwrap_or_else(|error| {
                        failed_semantic_input_candidate(fallback, error.to_string())
                    })
            })
            .collect::<Vec<_>>();
        for computed in
            compute_semantic_batch(&resolved, roots.preview_cache_root, execution.provider)
        {
            let current_asset_id = Some(computed.candidate.asset_id.clone());
            persist_semantic_embedding(repository, project_id, &model, computed)?;
            job.files_processed += 1;
            job.items_completed = job.files_processed;
            // Only provider/internal failures contribute to the job error count. Unsupported,
            // corrupt, and offline originals are truthful terminal states, not fake successes.
            counts = semantic_counts(
                repository,
                project_id,
                &model,
                candidates.len() as u64 - job.files_processed,
            )?;
            job.error_count = counts.failed;
            job.updated_at = Utc::now();
            repository.update_background_job(&job)?;
            progress = semantic_progress(
                Some(&job),
                SemanticProgressDetails {
                    counts: counts.clone(),
                    model: execution.model_status.clone(),
                    index_ready: false,
                    index_embedding_count: 0,
                    resource_mode: execution.resource_mode,
                    current_asset_id,
                    message: None,
                },
            );
            (execution.on_progress)(&progress);
        }
    }

    if (execution.should_pause)() {
        return pause_semantic_indexing(
            repository,
            &mut job,
            execution,
            SemanticPauseDetails {
                counts: semantic_counts(repository, project_id, &model, 0)?,
                current_asset_id: None,
                message: "Embedding is complete; local index rebuild is paused before replacing the derived index."
                    .into(),
            },
        );
    }

    job.stage = JobStage::SemanticIndex;
    job.updated_at = Utc::now();
    repository.update_background_job(&job)?;
    let rebuilding_message = "Rebuilding the local, replaceable semantic index.".to_owned();
    progress = semantic_progress(
        Some(&job),
        SemanticProgressDetails {
            counts: semantic_counts(repository, project_id, &model, 0)?,
            model: execution.model_status.clone(),
            index_ready: false,
            index_embedding_count: 0,
            resource_mode: execution.resource_mode,
            current_asset_id: None,
            message: Some(rebuilding_message),
        },
    );
    (execution.on_progress)(&progress);

    let vectors = repository.semantic_embeddings_for_index(project_id, &model)?;
    let build = match PersistentVectorIndex::build_and_store(
        roots.index_root,
        &project_id.to_string(),
        &identity.cache_key(),
        model.dimensions,
        vectors
            .into_iter()
            .map(|vector| (vector.asset_id, vector.vector)),
    ) {
        Ok(build) => build,
        Err(error) => {
            let message = format!("Local semantic index rebuild failed: {error}");
            job.state = WorkflowRunState::Failed;
            job.error_message = Some(message.clone());
            job.updated_at = Utc::now();
            job.finished_at = Some(job.updated_at);
            let failed_progress = semantic_progress(
                Some(&job),
                SemanticProgressDetails {
                    counts: semantic_counts(repository, project_id, &model, 0)?,
                    model: execution.model_status.clone(),
                    index_ready: false,
                    index_embedding_count: 0,
                    resource_mode: execution.resource_mode,
                    current_asset_id: None,
                    message: Some(message),
                },
            );
            job.resume_metadata = Some(serde_json::json!({
                "pipeline": "semantic-indexing",
                "resource_mode": execution.resource_mode.as_str(),
                "summary": &failed_progress,
            }));
            repository.update_background_job(&job)?;
            (execution.on_progress)(&failed_progress);
            return Err(PersistenceError::InvalidData(error.to_string()));
        }
    };
    let rebuilt_at = Utc::now();
    repository.upsert_semantic_index_version(&SemanticIndexVersion {
        id: SemanticIndexVersionId::new(),
        project_id: project_id.clone(),
        model: model.clone(),
        index_format: SEMANTIC_INDEX_FORMAT.into(),
        index_relative_path: build.relative_path,
        index_checksum: build.checksum,
        embedding_count: build.vector_count as u64,
        status: "ready".into(),
        stale: false,
        created_at: rebuilt_at,
        rebuilt_at,
    })?;
    let final_counts = semantic_counts(repository, project_id, &model, 0)?;
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::SemanticIndex;
    job.items_completed = job.items_total.unwrap_or(0);
    job.files_processed = job.items_completed;
    job.error_count = final_counts.failed;
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    let final_progress = semantic_progress(
        Some(&job),
        SemanticProgressDetails {
            counts: final_counts,
            model: execution.model_status.clone(),
            index_ready: true,
            index_embedding_count: build.vector_count as u64,
            resource_mode: execution.resource_mode,
            current_asset_id: None,
            message: Some(
                "Local semantic index rebuilt. Search remains project-scoped and offline-capable from cached embeddings."
                    .into(),
            ),
        },
    );
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "semantic-indexing",
        "resource_mode": execution.resource_mode.as_str(),
        "summary": &final_progress,
    }));
    repository.update_background_job(&job)?;
    (execution.on_progress)(&final_progress);
    Ok(final_progress)
}

fn pause_semantic_indexing(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    execution: &mut SemanticIndexExecution<'_>,
    details: SemanticPauseDetails,
) -> PersistenceResult<SemanticIndexProgress> {
    job.state = WorkflowRunState::Paused;
    job.updated_at = Utc::now();
    let progress = semantic_progress(
        Some(job),
        SemanticProgressDetails {
            counts: details.counts,
            model: execution.model_status.clone(),
            index_ready: false,
            index_embedding_count: 0,
            resource_mode: execution.resource_mode,
            current_asset_id: details.current_asset_id,
            message: Some(details.message),
        },
    );
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "semantic-indexing",
        "resource_mode": execution.resource_mode.as_str(),
        "summary": &progress,
    }));
    repository.update_background_job(job)?;
    (execution.on_progress)(&progress);
    Ok(progress)
}

/// Resolves a preview for M6 without widening the source-media boundary. Existing managed
/// previews remain usable while an original is offline; an available source is read only to make
/// a new managed preview, never passed to a model or modified in place.
fn resolve_semantic_input(
    repository: &impl CatalogRepository,
    cache_root: &Path,
    candidate: SemanticInputCandidate,
) -> PersistenceResult<SemanticInputCandidate> {
    if candidate.preview_status == "ready"
        && candidate
            .preview_relative_path
            .as_deref()
            .and_then(|relative| resolve_analysis_preview_path(cache_root, relative))
            .is_some()
    {
        return Ok(candidate);
    }
    if matches!(candidate.preview_status.as_str(), "corrupt" | "unsupported") {
        return Ok(candidate);
    }
    let copies = repository.preparation_file_instance_candidates(&candidate.asset_id)?;
    let mut terminal_failure: Option<(ArtifactStatus, String, SemanticInputCandidate)> = None;
    for copy in copies {
        if !copy.is_available {
            continue;
        }
        let media_type = match media_type_from_label(&copy.media_type) {
            Ok(media_type) => media_type,
            Err(error) => {
                return Ok(failed_semantic_input_candidate(
                    candidate,
                    error.to_string(),
                ));
            }
        };
        let Some(source) = source_path(copy.selected_root.as_deref(), &copy.relative_path) else {
            continue;
        };
        let generated = prepare_analysis_preview(
            &LocalVisualAdapters,
            cache_root,
            &copy.asset_id,
            &copy.file_instance_id,
            &source,
            &media_type,
            &copy.source_fingerprint,
        )
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
        repository.upsert_preview_artifact(&PreviewArtifactRecord {
            id: Uuid::new_v4().to_string(),
            media_asset_id: copy.asset_id.clone(),
            source_file_instance_id: copy.file_instance_id.clone(),
            artifact_type: generated.artifact_type.clone(),
            size_class: generated.size.as_str().into(),
            cache_relative_path: generated.cache_relative_path.clone(),
            provider: generated.provider.clone(),
            generator_version: ANALYSIS_PREVIEW_GENERATOR_VERSION.into(),
            source_fingerprint: generated.source_fingerprint.clone(),
            created_at: Utc::now().to_rfc3339(),
            status: generated.status.as_str().into(),
            failure_reason: generated.failure_reason.clone(),
        })?;
        let resolved = resolved_semantic_candidate(&candidate, &copy, &generated);
        if generated.status == ArtifactStatus::Ready
            && resolved
                .preview_relative_path
                .as_deref()
                .and_then(|relative| resolve_analysis_preview_path(cache_root, relative))
                .is_some()
        {
            return Ok(resolved);
        }
        if generated.status != ArtifactStatus::Offline {
            let message = generated.failure_reason.unwrap_or_else(|| {
                "CaptureOS could not prepare a local semantic-search preview".into()
            });
            if terminal_failure.as_ref().is_none_or(|(status, _, _)| {
                analysis_input_failure_rank(&generated.status) > analysis_input_failure_rank(status)
            }) {
                terminal_failure = Some((generated.status, message, resolved));
            }
        }
    }
    if let Some((status, message, candidate)) = terminal_failure {
        return Ok(resolved_terminal_semantic_candidate(
            candidate, status, message,
        ));
    }
    Ok(resolved_terminal_semantic_candidate(
        candidate,
        ArtifactStatus::Offline,
        "No sufficient cached semantic preview exists and no usable local FileInstance is currently available".into(),
    ))
}

fn resolved_semantic_candidate(
    base: &SemanticInputCandidate,
    copy: &persistence::MediaPreparationCandidate,
    generated: &media_visual::GeneratedPreview,
) -> SemanticInputCandidate {
    let mut resolved = base.clone();
    resolved.file_instance_id = copy.file_instance_id.clone();
    resolved.filename = copy.filename.clone();
    resolved.media_type = copy.media_type.clone();
    resolved.input_fingerprint = analysis_input_fingerprint(
        &generated.source_fingerprint,
        &generated.artifact_type,
        ANALYSIS_PREVIEW_GENERATOR_VERSION,
        generated.size.as_str(),
    );
    resolved.preview_relative_path =
        (generated.status == ArtifactStatus::Ready).then(|| generated.cache_relative_path.clone());
    resolved.preview_status = generated.status.as_str().into();
    resolved.preview_failure_reason = generated.failure_reason.clone();
    resolved.is_available = copy.is_available;
    resolved
}

fn resolved_terminal_semantic_candidate(
    mut candidate: SemanticInputCandidate,
    status: ArtifactStatus,
    message: String,
) -> SemanticInputCandidate {
    candidate.preview_relative_path = None;
    candidate.preview_status = match status {
        ArtifactStatus::Corrupt => "corrupt",
        ArtifactStatus::Unsupported => "unsupported",
        ArtifactStatus::Offline => "needs_original",
        _ => "failed",
    }
    .into();
    candidate.preview_failure_reason = Some(message);
    candidate
}

fn failed_semantic_input_candidate(
    candidate: SemanticInputCandidate,
    message: String,
) -> SemanticInputCandidate {
    resolved_terminal_semantic_candidate(candidate, ArtifactStatus::Failed, message)
}

fn compute_semantic_batch<P: SemanticEmbeddingProvider + ?Sized>(
    batch: &[SemanticInputCandidate],
    cache_root: &Path,
    provider: &P,
) -> Vec<ComputedSemanticCandidate> {
    let cache_root = cache_root.to_path_buf();
    thread::scope(|scope| {
        let workers = batch
            .iter()
            .cloned()
            .map(|candidate| {
                let fallback = candidate.clone();
                let cache_root = cache_root.clone();
                let worker = scope.spawn(move || {
                    let fallback_after_panic = candidate.clone();
                    std::panic::catch_unwind(AssertUnwindSafe(|| {
                        compute_semantic_candidate(candidate, &cache_root, provider)
                    }))
                    .unwrap_or_else(|_| {
                        ComputedSemanticCandidate {
                        candidate: fallback_after_panic,
                        status: AnalysisStatus::Failed,
                        embedding: None,
                        error_message: Some(
                            "A local semantic worker stopped unexpectedly; no embedding was stored."
                                .into(),
                        ),
                    }
                    })
                });
                (fallback, worker)
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|(fallback, worker)| {
                worker.join().unwrap_or_else(|_| ComputedSemanticCandidate {
                    candidate: fallback,
                    status: AnalysisStatus::Failed,
                    embedding: None,
                    error_message: Some(
                        "A local semantic worker could not be joined; no embedding was stored."
                            .into(),
                    ),
                })
            })
            .collect()
    })
}

fn compute_semantic_candidate<P: SemanticEmbeddingProvider + ?Sized>(
    candidate: SemanticInputCandidate,
    cache_root: &Path,
    provider: &P,
) -> ComputedSemanticCandidate {
    if candidate.preview_status == "ready" {
        let Some(path) = candidate
            .preview_relative_path
            .as_deref()
            .and_then(|relative| resolve_analysis_preview_path(cache_root, relative))
        else {
            return ComputedSemanticCandidate {
                candidate,
                status: AnalysisStatus::NeedsOriginal,
                embedding: None,
                error_message: Some(
                    "Cached semantic preview is no longer safely available.".into(),
                ),
            };
        };
        let image = match LocalPreviewDecoder.decode(&path) {
            Ok(image) => image,
            Err(error) => {
                return ComputedSemanticCandidate {
                    candidate,
                    status: error.status(),
                    embedding: None,
                    error_message: Some(error.to_string()),
                };
            }
        };
        return match provider.embed_image(&image).and_then(normalize_embedding) {
            Ok(embedding) if embedding.len() == provider.identity().dimensions => {
                ComputedSemanticCandidate {
                    candidate,
                    status: AnalysisStatus::Ready,
                    embedding: Some(embedding),
                    error_message: None,
                }
            }
            Ok(_) => ComputedSemanticCandidate {
                candidate,
                status: AnalysisStatus::Failed,
                embedding: None,
                error_message: Some(
                    "Local semantic provider returned an unexpected embedding dimension.".into(),
                ),
            },
            Err(error) => ComputedSemanticCandidate {
                candidate,
                status: AnalysisStatus::Failed,
                embedding: None,
                error_message: Some(error.to_string()),
            },
        };
    }
    let (status, message) = match candidate.preview_status.as_str() {
        "corrupt" => (
            AnalysisStatus::Corrupt,
            candidate
                .preview_failure_reason
                .clone()
                .unwrap_or_else(|| "The local semantic input is corrupt".into()),
        ),
        "unsupported" => (
            AnalysisStatus::Unsupported,
            candidate.preview_failure_reason.clone().unwrap_or_else(|| {
                "The available local source is unsupported by the current preview provider".into()
            }),
        ),
        "needs_original" => (
            AnalysisStatus::NeedsOriginal,
            candidate
                .preview_failure_reason
                .clone()
                .unwrap_or_else(|| "No usable local FileInstance is currently available".into()),
        ),
        _ => (
            AnalysisStatus::Failed,
            candidate.preview_failure_reason.clone().unwrap_or_else(|| {
                "The semantic input resolver did not produce a usable local preview".into()
            }),
        ),
    };
    ComputedSemanticCandidate {
        candidate,
        status,
        embedding: None,
        error_message: Some(message),
    }
}

fn persist_semantic_embedding(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    model: &SemanticModelConfig,
    computed: ComputedSemanticCandidate,
) -> PersistenceResult<()> {
    if computed.candidate.project_id != project_id.to_string() {
        return Err(PersistenceError::InvalidData(
            "semantic embedding candidate did not belong to the selected project".into(),
        ));
    }
    let media_asset_id = MediaAssetId::try_from(computed.candidate.asset_id.as_str())
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    repository.upsert_semantic_embedding(&SemanticEmbeddingRecord {
        media_asset_id,
        project_id: project_id.clone(),
        input_fingerprint: computed.candidate.input_fingerprint,
        model: model.clone(),
        embedding: computed.embedding,
        generated_at: Utc::now(),
        status: computed.status,
        error_message: computed.error_message,
    })
}

/// Runs a text search entirely through the selected local provider and a project-owned derived
/// index. When the provider/index is unavailable, only explicit deterministic filters run; text
/// is never substituted with filename, tag, or cloud lookup behavior.
pub fn search_magic(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: &MagicSearchRequest,
    provider_cache: &SiglipProviderCache,
    index_root: &Path,
    preview_cache_root: &Path,
) -> PersistenceResult<MagicSearchResponse> {
    if let Some(moment_id) = request.moment_id.as_deref() {
        if !repository.moment_belongs_to_project(project_id, moment_id)? {
            return Err(PersistenceError::InvalidData(
                "moment does not belong to the selected project".into(),
            ));
        }
    }
    let plan = plan_query(&request.query)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    repository.record_magic_search_history(
        project_id,
        &plan.normalized_query,
        &plan.normalized_query.to_lowercase(),
        &serde_json::json!({
            "chips": &plan.chips,
            "identitySearchBlocked": plan.identity_search_blocked,
        }),
    )?;
    if plan.identity_search_blocked {
        return Ok(MagicSearchResponse {
            query: plan.normalized_query,
            results: Vec::new(),
            semantic_available: false,
            semantic_applied: false,
            semantic_unavailable_reason: Some(
                "Identity recognition and person search are not available in Magic Search.".into(),
            ),
            parsed_filters: MagicSearchFilterView { chips: plan.chips },
            has_more: false,
            total_results: 0,
            identity_search_blocked: true,
            message: Some(
                "Magic Search does not perform person identity recognition or matching.".into(),
            ),
        });
    }
    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let filters_only = plan.semantic_query.is_none();
    let Some(provider) = provider else {
        return deterministic_magic_search(
            repository,
            project_id,
            request,
            &plan,
            preview_cache_root,
            false,
            model_status.message,
        );
    };
    let model = semantic_model_config(provider.identity());
    let index = match load_active_semantic_index(
        repository,
        project_id,
        &model,
        &provider.identity().cache_key(),
        index_root,
    ) {
        Ok(index) => index,
        Err(error) => {
            return deterministic_magic_search(
                repository,
                project_id,
                request,
                &plan,
                preview_cache_root,
                false,
                Some(format!(
                    "Semantic index unavailable and needs a local rebuild: {error}"
                )),
            );
        }
    };
    if filters_only {
        return deterministic_magic_search(
            repository,
            project_id,
            request,
            &plan,
            preview_cache_root,
            index.is_some(),
            (!index.is_some()).then_some(
                "Semantic index is not ready. Deterministic local filters remain available.".into(),
            ),
        );
    }
    let Some((index, _)) = index else {
        return deterministic_magic_search(
            repository,
            project_id,
            request,
            &plan,
            preview_cache_root,
            false,
            Some("Semantic index is not ready. Index local photos to enable local image/text matching.".into()),
        );
    };
    let query = match provider
        .embed_text(
            plan.semantic_query
                .as_deref()
                .expect("checked semantic query"),
        )
        .and_then(normalize_embedding)
    {
        Ok(query) => query,
        Err(error) => {
            return deterministic_magic_search(
                repository,
                project_id,
                request,
                &plan,
                preview_cache_root,
                false,
                Some(format!("Local text embedding unavailable: {error}")),
            );
        }
    };
    if query.len() != model.dimensions {
        return deterministic_magic_search(
            repository,
            project_id,
            request,
            &plan,
            preview_cache_root,
            false,
            Some("Local text provider returned an unexpected embedding dimension.".into()),
        );
    }
    let candidates = semantic_index_candidates(
        repository,
        project_id,
        &model,
        &index,
        &query,
        request.moment_id.as_deref(),
    )?;
    let scored =
        score_semantic_candidates(candidates, &plan, &query, &request.sort, request.descending)?;
    semantic_response(
        repository,
        project_id,
        scored,
        SemanticResponseContext {
            request,
            plan: &plan,
            preview_cache_root,
            semantic_available: true,
            semantic_applied: true,
            semantic_unavailable_reason: None,
            explanation:
                "Results are ranked by local image/text embedding similarity; this is not an object, identity, or localized-detection claim.",
        },
    )
}

/// Finds related still photos from the same project using the already persisted local embedding.
/// It never updates Similar Sets, decisions, ratings, notes, representatives, or review state.
pub fn find_similar(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: FindSimilarRequest<'_>,
    roots: SemanticStorageRoots<'_>,
    provider_cache: &SiglipProviderCache,
) -> PersistenceResult<MagicSearchResponse> {
    ensure_asset_project(repository, project_id, request.asset_id)?;
    let (model_status, provider) = inspect_semantic_provider(repository, provider_cache)?;
    let provider =
        provider.ok_or_else(|| {
            PersistenceError::InvalidData(model_status.message.unwrap_or_else(|| {
                "Find Similar requires an available local semantic model.".into()
            }))
        })?;
    find_similar_with_provider(
        repository,
        project_id,
        request,
        FindSimilarExecution {
            provider: provider.as_ref(),
            index_root: roots.index_root,
            preview_cache_root: roots.preview_cache_root,
        },
    )
}

fn find_similar_with_provider(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: FindSimilarRequest<'_>,
    execution: FindSimilarExecution<'_>,
) -> PersistenceResult<MagicSearchResponse> {
    let model = semantic_model_config(execution.provider.identity());
    let index = load_active_semantic_index(
        repository,
        project_id,
        &model,
        &execution.provider.identity().cache_key(),
        execution.index_root,
    )?
    .ok_or_else(|| {
        PersistenceError::InvalidData(
            "Find Similar requires a ready local semantic index for this project.".into(),
        )
    })?
    .0;
    let source = repository.semantic_search_candidates_for_assets(
        project_id,
        Some(&model),
        &[request.asset_id.to_string()],
    )?;
    let source_vector = source
        .into_iter()
        .next()
        .and_then(|candidate| candidate.vector)
        .ok_or_else(|| {
            PersistenceError::InvalidData(
                "This photo has no current local semantic embedding. Index local photos first."
                    .into(),
            )
        })?;
    let candidates =
        semantic_index_candidates(repository, project_id, &model, &index, &source_vector, None)?
            .into_iter()
            .filter(|candidate| candidate.asset_id != request.asset_id.to_string())
            .collect();
    let plan = magic_search::QueryPlan {
        normalized_query: format!("Find Similar: {}", request.asset_id),
        semantic_query: None,
        filters: magic_search::StructuredFilters::default(),
        chips: vec!["Find Similar".into()],
        identity_search_blocked: false,
    };
    let request = MagicSearchRequest {
        query: plan.normalized_query.clone(),
        sort: "relevance".into(),
        descending: true,
        limit: request.limit,
        offset: request.offset,
        moment_id: None,
    };
    let scored = score_semantic_candidates(candidates, &plan, &source_vector, "relevance", true)?;
    semantic_response(
        repository,
        project_id,
        scored,
        SemanticResponseContext {
            request: &request,
            plan: &plan,
            preview_cache_root: execution.preview_cache_root,
            semantic_available: true,
            semantic_applied: true,
            semantic_unavailable_reason: None,
            explanation:
                "Results are related local visual embeddings. Find Similar does not create or alter Similar Sets.",
        },
    )
}

fn deterministic_magic_search(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: &MagicSearchRequest,
    plan: &magic_search::QueryPlan,
    preview_cache_root: &Path,
    semantic_available: bool,
    unavailable_reason: Option<String>,
) -> PersistenceResult<MagicSearchResponse> {
    let limit = request.limit.clamp(1, MAGIC_SEARCH_PAGE_LIMIT);
    let semantic_requested = plan.semantic_query.is_some();
    let candidates = if semantic_requested && !has_structured_filters(&plan.filters) {
        // A natural-language-only query cannot honestly fall back to “all photos” when local
        // image/text matching is unavailable. Return no semantic result rather than fabricate
        // relevance from filename or catalog order.
        Vec::new()
    } else {
        repository.semantic_metadata_candidates(
            project_id,
            &SemanticMetadataQuery {
                face_count: plan.filters.face_count,
                rating_exact: plan.filters.rating_exact,
                rating_minimum: plan.filters.rating_minimum,
                decision: plan.filters.decision.clone(),
                require_sharp: plan.filters.require_sharp,
                require_blurry: plan.filters.require_blurry,
                require_technical_issue: plan.filters.require_technical_issue,
                camera_model: plan.filters.camera_model.clone(),
                moment_id: request.moment_id.clone(),
                sort: metadata_sort(&request.sort),
                descending: request.descending,
                limit: limit.saturating_add(1),
                offset: request.offset,
            },
        )?
    };
    let has_more = candidates.len() > limit as usize;
    let visible = candidates
        .into_iter()
        .take(limit as usize)
        .collect::<Vec<_>>();
    let results = visible
        .iter()
        .filter_map(|candidate| {
            magic_result(
                repository,
                candidate,
                None,
                MagicResultContext {
                    project_id,
                    preview_cache_root,
                    plan,
                    semantic_applied: false,
                    explanation: "Matched explicit local metadata or technical evidence.",
                },
            )
            .transpose()
        })
        .collect::<PersistenceResult<Vec<_>>>()?;
    Ok(MagicSearchResponse {
        query: plan.normalized_query.clone(),
        total_results: results.len() as u64,
        results,
        semantic_available,
        semantic_applied: false,
        semantic_unavailable_reason: unavailable_reason,
        parsed_filters: MagicSearchFilterView {
            chips: plan.chips.clone(),
        },
        has_more,
        identity_search_blocked: false,
        message: semantic_requested.then_some(
            "Text semantics were not applied because the local provider or project index is unavailable; only explicit local filters can return results.".into(),
        ),
    })
}

fn has_structured_filters(filters: &magic_search::StructuredFilters) -> bool {
    filters.face_count.is_some()
        || filters.rating_exact.is_some()
        || filters.rating_minimum.is_some()
        || filters.decision.is_some()
        || filters.require_sharp
        || filters.require_blurry
        || filters.require_technical_issue
        || filters.camera_model.is_some()
}

fn metadata_sort(sort: &str) -> SemanticMetadataSort {
    match sort {
        "technicalQuality" => SemanticMetadataSort::TechnicalQuality,
        "rating" => SemanticMetadataSort::Rating,
        _ => SemanticMetadataSort::CaptureTime,
    }
}

fn semantic_index_candidates(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    model: &SemanticModelConfig,
    index: &PersistentVectorIndex,
    query: &[f32],
    moment_id: Option<&str>,
) -> PersistenceResult<Vec<SemanticSearchCandidate>> {
    let mut asset_ids = match index
        .candidates(query)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?
    {
        IndexCandidates::AllStoredVectors => repository
            .semantic_embeddings_for_index(project_id, model)?
            .into_iter()
            .map(|vector| vector.asset_id)
            .collect::<Vec<_>>(),
        IndexCandidates::AssetIds(asset_ids) => asset_ids,
    };
    if let Some(moment_id) = moment_id {
        let allowed = repository
            .filter_active_moment_assets(project_id, moment_id, &asset_ids)?
            .into_iter()
            .collect::<HashSet<_>>();
        asset_ids.retain(|asset_id| allowed.contains(asset_id));
    }
    repository.semantic_search_candidates_for_assets(project_id, Some(model), &asset_ids)
}

fn score_semantic_candidates(
    candidates: Vec<SemanticSearchCandidate>,
    plan: &magic_search::QueryPlan,
    query: &[f32],
    sort: &str,
    descending: bool,
) -> PersistenceResult<Vec<ScoredSemanticCandidate>> {
    let filtered = candidates
        .into_iter()
        .filter(|candidate| semantic_candidate_matches_filters(candidate, &plan.filters))
        .collect::<Vec<_>>();
    let ranked = rank_normalized_vectors(
        query,
        filtered.iter().filter_map(|candidate| {
            candidate
                .vector
                .clone()
                .map(|vector| (candidate.asset_id.clone(), vector))
        }),
        filtered.len(),
    )
    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let scores = ranked
        .into_iter()
        .map(|score| (score.asset_id, score.score))
        .collect::<HashMap<_, _>>();
    let mut scored = filtered
        .into_iter()
        .filter_map(|candidate| {
            scores
                .get(&candidate.asset_id)
                .copied()
                .map(|score| ScoredSemanticCandidate { candidate, score })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        let order = match sort {
            "captureTime" => left.candidate.captured_at.cmp(&right.candidate.captured_at),
            "technicalQuality" => left
                .candidate
                .technical_quality_score
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(
                    &right
                        .candidate
                        .technical_quality_score
                        .unwrap_or(f64::NEG_INFINITY),
                ),
            "rating" => left.candidate.rating.cmp(&right.candidate.rating),
            _ => left.score.total_cmp(&right.score),
        };
        let order = if descending { order.reverse() } else { order };
        order.then_with(|| left.candidate.asset_id.cmp(&right.candidate.asset_id))
    });
    Ok(scored)
}

fn semantic_candidate_matches_filters(
    candidate: &SemanticSearchCandidate,
    filters: &magic_search::StructuredFilters,
) -> bool {
    filters
        .face_count
        .is_none_or(|value| candidate.face_count == Some(value))
        && filters
            .rating_exact
            .is_none_or(|value| candidate.rating == value)
        && filters
            .rating_minimum
            .is_none_or(|value| candidate.rating >= value)
        && filters
            .decision
            .as_deref()
            .is_none_or(|value| candidate.decision.as_deref() == Some(value))
        && (!filters.require_sharp
            || matches!(
                candidate.sharpness_band.as_deref(),
                Some("excellent" | "good")
            ))
        && (!filters.require_blurry
            || matches!(candidate.blur_level.as_deref(), Some("moderate" | "high")))
        && (!filters.require_technical_issue
            || candidate.technical_quality_band.as_deref() == Some("technical_issue"))
        && filters.camera_model.as_deref().is_none_or(|camera| {
            candidate
                .camera_model
                .as_deref()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(camera))
        })
}

fn semantic_response(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    scored: Vec<ScoredSemanticCandidate>,
    context: SemanticResponseContext<'_>,
) -> PersistenceResult<MagicSearchResponse> {
    let limit = context.request.limit.clamp(1, MAGIC_SEARCH_PAGE_LIMIT) as usize;
    let start = context.request.offset as usize;
    let total_results = scored.len() as u64;
    let has_more = start.saturating_add(limit) < scored.len();
    let results = scored
        .into_iter()
        .skip(start)
        .take(limit)
        .filter_map(|scored| {
            magic_result(
                repository,
                &scored.candidate,
                Some(scored.score),
                MagicResultContext {
                    project_id,
                    preview_cache_root: context.preview_cache_root,
                    plan: context.plan,
                    semantic_applied: context.semantic_applied,
                    explanation: context.explanation,
                },
            )
            .transpose()
        })
        .collect::<PersistenceResult<Vec<_>>>()?;
    Ok(MagicSearchResponse {
        query: context.plan.normalized_query.clone(),
        results,
        semantic_available: context.semantic_available,
        semantic_applied: context.semantic_applied,
        semantic_unavailable_reason: context.semantic_unavailable_reason,
        parsed_filters: MagicSearchFilterView {
            chips: context.plan.chips.clone(),
        },
        has_more,
        total_results,
        identity_search_blocked: context.plan.identity_search_blocked,
        message: None,
    })
}

fn magic_result(
    repository: &impl CatalogRepository,
    candidate: &SemanticSearchCandidate,
    semantic_score: Option<f32>,
    context: MagicResultContext<'_>,
) -> PersistenceResult<Option<MagicSearchResult>> {
    let asset_id = MediaAssetId::try_from(candidate.asset_id.as_str())
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let Some(detail) = load_media_asset_detail(
        repository,
        context.project_id,
        &asset_id,
        context.preview_cache_root,
    )?
    else {
        return Ok(None);
    };
    let mut matched_evidence = matched_filter_evidence(candidate, &context.plan.filters);
    if context.semantic_applied {
        matched_evidence.insert(
            0,
            if context.plan.chips.as_slice() == ["Find Similar"] {
                "Local visual embedding similarity"
            } else {
                "Local image/text embedding similarity"
            }
            .into(),
        );
    }
    Ok(Some(MagicSearchResult {
        item: detail.item,
        // A deliberately non-calibrated provenance label. `semantic_score` remains a local
        // ranking signal and the UI must not turn it into an object/identity/quality claim.
        score_label: semantic_score.map(|_| "Local".into()),
        semantic_score,
        explanation: context.explanation.into(),
        matched_evidence,
    }))
}

fn matched_filter_evidence(
    candidate: &SemanticSearchCandidate,
    filters: &magic_search::StructuredFilters,
) -> Vec<String> {
    let mut evidence = Vec::new();
    if filters.face_count.is_some() {
        if let Some(count) = candidate.face_count {
            evidence.push(format!("Anonymous face count: {count}"));
        }
    }
    if filters.rating_exact.is_some() || filters.rating_minimum.is_some() {
        evidence.push(format!("Rating: {}★", candidate.rating));
    }
    if filters.decision.is_some() {
        if let Some(decision) = &candidate.decision {
            evidence.push(format!("Decision: {decision}"));
        }
    }
    if filters.require_sharp {
        if let Some(sharpness) = &candidate.sharpness_band {
            evidence.push(format!("Sharpness: {sharpness}"));
        }
    }
    if filters.require_blurry {
        if let Some(blur) = &candidate.blur_level {
            evidence.push(format!("Blur evidence: {blur}"));
        }
    }
    if filters.require_technical_issue {
        evidence.push("Technical quality: issue".into());
    }
    if filters.camera_model.is_some() {
        if let Some(camera) = &candidate.camera_model {
            evidence.push(format!("Camera: {camera}"));
        }
    }
    evidence
}

pub fn create_local_project(
    repository: &impl CatalogRepository,
    name: &str,
) -> PersistenceResult<ProjectView> {
    repository.create_project(name).map(project_view)
}

pub fn list_local_projects(
    repository: &impl CatalogRepository,
) -> PersistenceResult<Vec<ProjectView>> {
    repository
        .projects()
        .map(|projects| projects.into_iter().map(project_view).collect())
}

pub fn load_project_library(
    repository: &impl CatalogRepository,
) -> PersistenceResult<Vec<ProjectLibraryItem>> {
    repository
        .project_library()
        .map(|items| items.into_iter().map(project_library_item).collect())
}

pub fn load_project_home(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    filter: MediaBrowserFilter,
    offset: u32,
    limit: u32,
) -> PersistenceResult<ProjectHome> {
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project not found".into()))?;
    let roots = repository.index_roots_for_project(project_id)?;
    refresh_known_volume_metadata(repository, &roots)?;
    Ok(ProjectHome {
        project: project_view(project),
        summary: repository.project_index_summary(project_id)?,
        roots: roots.into_iter().map(index_root_view).collect(),
        latest_job: repository.recent_background_job(project_id)?.map(job_view),
        media: repository.indexed_media(project_id, filter, limit, offset)?,
    })
}

/// Reads a bounded logical-media page. SQLite returns registered artifact identities; this
/// desktop-boundary layer turns them into opaque custom-protocol URLs. It never serializes an
/// absolute cache path or original-media path to the frontend.
pub fn load_visual_media_page(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    query: &VisualMediaQuery,
    cache_root: &Path,
) -> PersistenceResult<VisualMediaPage> {
    let mut page = repository.visual_media_page(project_id, query)?;
    for item in &mut page.items {
        item.thumbnail_preview_url = preview_url(item.thumbnail_preview_url.take());
        item.medium_preview_url = preview_url(item.medium_preview_url.take());
        item.preview_preview_url = preview_url(item.preview_preview_url.take());
        if item.preview_status == "ready"
            && item.thumbnail_preview_url.is_none()
            && item.medium_preview_url.is_none()
            && item.preview_preview_url.is_none()
        {
            item.preview_status = "stale".into();
        }
    }
    page.cache_bytes = cache_size(cache_root);
    Ok(page)
}

pub fn load_media_asset_detail(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    asset_id: &MediaAssetId,
    _cache_root: &Path,
) -> PersistenceResult<Option<MediaAssetDetail>> {
    ensure_asset_project(repository, project_id, asset_id)?;
    let mut detail = repository.media_asset_detail(asset_id)?;
    if let Some(detail) = &mut detail {
        detail.item.thumbnail_preview_url = preview_url(detail.item.thumbnail_preview_url.take());
        detail.item.medium_preview_url = preview_url(detail.item.medium_preview_url.take());
        detail.item.preview_preview_url = preview_url(detail.item.preview_preview_url.take());
    }
    Ok(detail)
}

/// Returns the durable, non-empty result of the latest completed logical-media preparation job.
/// This survives desktop restarts and is intentionally distinct from an active worker event.
pub fn load_visual_preparation_summary(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<Option<MediaPreparationProgress>> {
    let completed_job = repository.latest_visual_preparation_job(project_id)?;
    let needs_reconciliation_repair = completed_job
        .as_ref()
        .is_some_and(is_outdated_reconciled_summary);
    let persisted_summary = completed_job
        .as_ref()
        .and_then(job_preparation_summary)
        .transpose()
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    if let (false, Some(summary)) = (needs_reconciliation_repair, persisted_summary) {
        return Ok(Some(summary));
    }

    // Milestone 3 jobs written before M3.1 did not serialize their terminal counts. Reconcile
    // only terminal artifact/metadata evidence for the currently selected FileInstance; never
    // synthesize a result from MediaAsset or FileInstance catalog totals.
    let counts = repository.visual_preparation_terminal_counts(project_id)?;
    if counts.processed == 0 {
        return Ok(None);
    }
    let now = Utc::now();
    let summary = terminal_counts_summary(counts, now);
    if let Some(mut job) = completed_job {
        job.stage = JobStage::Finalize;
        job.items_completed = counts.processed;
        job.items_total = Some(counts.processed);
        job.files_discovered = counts.processed;
        job.files_processed = counts.processed;
        job.error_count = counts.failed + counts.timeout;
        job.resume_metadata = Some(serde_json::json!({
            "pipeline": "visual-preparation",
            "summary": &summary,
            "summary_origin": "reconciled-terminal-artifacts-v2",
        }));
        job.updated_at = now;
        repository.update_background_job(&job)?;
    } else {
        repository.insert_background_job(&BackgroundJob {
            id: JobId::new(),
            state: WorkflowRunState::Completed,
            stage: JobStage::Finalize,
            items_completed: counts.processed,
            items_total: Some(counts.processed),
            files_discovered: counts.processed,
            files_processed: counts.processed,
            error_count: counts.failed + counts.timeout,
            project_id: Some(project_id.clone()),
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({
                "pipeline": "visual-preparation",
                "summary": &summary,
                "summary_origin": "reconciled-terminal-artifacts-v2",
            })),
            created_at: now,
            updated_at: now,
            finished_at: Some(now),
        })?;
    }
    Ok(Some(summary))
}

fn job_preparation_summary(
    job: &BackgroundJob,
) -> Option<Result<MediaPreparationProgress, serde_json::Error>> {
    job.resume_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("summary"))
        .cloned()
        .map(serde_json::from_value)
}

fn is_outdated_reconciled_summary(job: &BackgroundJob) -> bool {
    job.resume_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("summary_origin"))
        .and_then(serde_json::Value::as_str)
        == Some("reconciled-terminal-artifacts")
}

fn terminal_counts_summary(
    counts: VisualPreparationTerminalCounts,
    completed_at: chrono::DateTime<Utc>,
) -> MediaPreparationProgress {
    MediaPreparationProgress {
        state: "completed".into(),
        stage: "finalize".into(),
        items_completed: counts.processed,
        items_total: counts.processed,
        error_count: counts.failed + counts.timeout,
        ready_count: counts.ready,
        unsupported_count: counts.unsupported,
        corrupt_count: counts.corrupt,
        offline_count: counts.offline,
        failed_count: counts.failed,
        timeout_count: counts.timeout,
        cancelled_count: counts.cancelled,
        current_asset_id: None,
        current_file_instance_id: None,
        current_provider: None,
        started_at: completed_at.to_rfc3339(),
        finished_at: Some(completed_at.to_rfc3339()),
        message: Some("Recovered from terminal preview artifacts".into()),
    }
}

/// Prepares only a requested page of logical media. The desktop calls this from a worker and
/// emits `MediaPreparationProgress`; browsing continues against the WAL-backed catalog.
pub fn prepare_visual_media(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    query: &VisualMediaQuery,
    cache_root: &Path,
    on_progress: impl FnMut(&MediaPreparationProgress),
) -> PersistenceResult<MediaPreparationProgress> {
    let candidates = repository.visual_preparation_candidates(project_id, query)?;
    prepare_visual_candidates(
        repository,
        project_id,
        candidates,
        cache_root,
        &LocalVisualAdapters,
        on_progress,
    )
}

/// Retries only terminal provider failures. Ready, unsupported, offline, and corrupt records are
/// intentionally left untouched until their source fingerprint changes or a later recovery flow.
pub fn retry_failed_visual_media(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    cache_root: &Path,
    on_progress: impl FnMut(&MediaPreparationProgress),
) -> PersistenceResult<MediaPreparationProgress> {
    let candidates = repository.failed_visual_preparation_candidates(project_id)?;
    prepare_visual_candidates(
        repository,
        project_id,
        candidates,
        cache_root,
        &LocalVisualAdapters,
        on_progress,
    )
}

const METADATA_REFRESH_ASSET_BATCH_SIZE: u32 = 64;

/// Re-reads locally available source metadata for an already indexed project without creating
/// previews, loading a semantic model, or changing any analysis or human decision. It is an
/// explicit worker operation because opening a project must remain nonblocking.
pub fn refresh_capture_metadata(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    mut on_progress: impl FnMut(&MetadataRefreshProgress),
) -> PersistenceResult<MetadataRefreshProgress> {
    let started_at = Utc::now().to_rfc3339();
    let total = repository.capture_time_refresh_asset_count(project_id)?;
    let mut progress = MetadataRefreshProgress {
        state: "running".into(),
        items_completed: 0,
        items_total: total,
        error_count: 0,
        resolved_capture_time_count: 0,
        high_confidence_capture_time_count: 0,
        copy_conflict_count: 0,
        current_asset_id: None,
        started_at: started_at.clone(),
        finished_at: None,
        message: Some(
            "Refreshing local capture-time metadata only. Existing previews, semantic embeddings, Similar Sets, and human decisions are unchanged.".into(),
        ),
    };
    on_progress(&progress);

    let mut after_asset_id = None::<String>;
    loop {
        let candidates = repository.capture_time_refresh_candidates(
            project_id,
            after_asset_id.as_deref(),
            METADATA_REFRESH_ASSET_BATCH_SIZE,
        )?;
        if candidates.is_empty() {
            break;
        }
        let mut candidate_groups = Vec::<Vec<persistence::MediaPreparationCandidate>>::new();
        for candidate in candidates {
            if candidate_groups
                .last()
                .and_then(|group| group.first())
                .is_some_and(|first| first.asset_id == candidate.asset_id)
            {
                candidate_groups
                    .last_mut()
                    .expect("the last metadata refresh candidate group exists")
                    .push(candidate);
            } else {
                candidate_groups.push(vec![candidate]);
            }
        }
        for group in candidate_groups {
            let Some(first) = group.first() else {
                continue;
            };
            let asset_id = first.asset_id.clone();
            progress.current_asset_id = Some(asset_id.clone());
            let result = media_type_from_label(&first.media_type).and_then(|media_type| {
                let inspections = inspect_metadata_candidates(group, &media_type);
                resolve_asset_metadata(inspections)
            });
            match result {
                Ok(resolved) => {
                    for observation in &resolved.observations {
                        repository.upsert_capture_time_observation(observation)?;
                    }
                    let record = metadata_record(&resolved.selected.candidate, resolved.metadata);
                    let has_capture_time = record.captured_at_local.is_some();
                    let high_confidence = record.capture_time_confidence.as_deref() == Some("high");
                    repository.upsert_media_metadata(&record)?;
                    progress.resolved_capture_time_count += u64::from(has_capture_time);
                    progress.high_confidence_capture_time_count += u64::from(high_confidence);
                    progress.copy_conflict_count += u64::from(resolved.copy_conflict);
                }
                Err(error) => {
                    // One unreadable/unsupported source must not prevent a project's other
                    // files from gaining their independently observed chronology.
                    progress.error_count += 1;
                    progress.message = Some(format!(
                        "Capture-time metadata refresh continued after one asset could not be resolved: {error}"
                    ));
                }
            }
            progress.items_completed += 1;
            after_asset_id = Some(asset_id);
            on_progress(&progress);
        }
    }
    progress.state = "completed".into();
    progress.current_asset_id = None;
    progress.finished_at = Some(Utc::now().to_rfc3339());
    progress.message = Some(
        "Capture-time metadata refresh completed. Rebuild Moments to use the refreshed chronology; no source media, previews, semantic embeddings, Similar Sets, or human decisions were changed."
            .into(),
    );
    on_progress(&progress);
    Ok(progress)
}

/// Runs the first local intelligence baseline over a resolved CaptureOS-managed analysis image.
/// The resolver can read an available source only to create a contained cache artifact; it never
/// creates, copies, edits, or deletes original media. Each result is cached by source
/// fingerprint/provider/version/settings, so reopening the grid does not trigger recomputation.
pub fn analyze_capture_intelligence(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    cache_root: &Path,
    resource_mode: AnalysisResourceMode,
    should_pause: impl Fn() -> bool,
    mut on_progress: impl FnMut(&CaptureIntelligenceProgress),
) -> PersistenceResult<CaptureIntelligenceProgress> {
    let analyzer = DeterministicImageAnalyzer;
    let face_provider = platform_face_provider_identity();
    let face_provider_config = FaceAnalysisProviderConfig::new(
        face_provider.provider,
        face_provider.provider_version,
        FACE_ANALYSIS_SETTINGS_VERSION,
    );
    let candidates = repository.analysis_input_candidates(
        project_id,
        analyzer.provider(),
        analyzer.version(),
        capture_intelligence::ANALYSIS_SETTINGS_VERSION,
        &face_provider_config,
    )?;
    let face_candidates = repository.face_analysis_input_candidates(
        project_id,
        analyzer.provider(),
        analyzer.version(),
        capture_intelligence::ANALYSIS_SETTINGS_VERSION,
        &face_provider_config,
    )?;
    let input_resolver = AnalysisInputResolver::new(
        repository,
        cache_root,
        &LocalVisualAdapters,
        AnalysisInputRequirement::default(),
    );
    let now = Utc::now();
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::AiAnalysis,
        items_completed: 0,
        items_total: Some((candidates.len() + face_candidates.len()) as u64),
        files_discovered: (candidates.len() + face_candidates.len()) as u64,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({
            "pipeline": "capture-intelligence",
            "resource_mode": resource_mode.as_str(),
            "analysis_settings_version": capture_intelligence::ANALYSIS_SETTINGS_VERSION,
        })),
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    let mut counts = CaptureIntelligenceTerminalCounts::default();
    let mut progress = intelligence_progress(&job, counts, resource_mode, None, None);
    on_progress(&progress);

    // Analysis may use a platform face provider in addition to deterministic visual evidence.
    // Keep those expensive reads bounded by the selected local resource mode, while retaining
    // all catalog writes and progress events on this thread. That preserves SQLite durability and
    // a deterministic visible progress order without making ECO/BALANCED/FAST only UI labels.
    let worker_count = analysis_worker_count(resource_mode);
    for batch in candidates.chunks(worker_count) {
        if should_pause() {
            job.state = WorkflowRunState::Paused;
            job.updated_at = Utc::now();
            progress = intelligence_progress(
                &job,
                counts,
                resource_mode,
                None,
                Some("Analysis paused. Completed local artifacts are durable; resume continues remaining media.".into()),
            );
            job.resume_metadata = Some(serde_json::json!({
                "pipeline": "capture-intelligence",
                "resource_mode": resource_mode.as_str(),
                "summary": &progress,
            }));
            repository.update_background_job(&job)?;
            on_progress(&progress);
            return Ok(progress);
        }
        job.stage = JobStage::Fingerprint;
        job.updated_at = Utc::now();
        repository.update_background_job(&job)?;
        progress = intelligence_progress(
            &job,
            counts,
            resource_mode,
            batch.first().map(|candidate| candidate.asset_id.clone()),
            Some(format!(
                "Analyzing up to {} local preview{} at a time ({})",
                worker_count,
                if worker_count == 1 { "" } else { "s" },
                resource_mode.as_str(),
            )),
        );
        on_progress(&progress);

        // A batch is deliberately no larger than the worker limit. Pause is observed before
        // every batch, so an active platform decoder is never forcefully interrupted and no
        // partially-written analysis state is created.
        let resolved_batch = batch
            .iter()
            .cloned()
            .map(|candidate| {
                let fallback = candidate.clone();
                input_resolver.resolve(candidate).unwrap_or_else(|error| {
                    failed_analysis_input_candidate(fallback, error.to_string())
                })
            })
            .collect::<Vec<_>>();
        let computed = compute_analysis_batch(&resolved_batch, cache_root);
        for computed_candidate in computed {
            let candidate_progress = Some(computed_candidate.candidate.asset_id.clone());
            let result = persist_candidate_analysis(
                repository,
                &computed_candidate.candidate,
                &analyzer,
                computed_candidate.analysis,
            )?;
            record_intelligence_status(&mut counts, &mut job, &result.status);
            job.files_processed += 1;
            job.items_completed = job.files_processed;
            job.updated_at = Utc::now();
            repository.update_background_job(&job)?;
            progress = intelligence_progress(
                &job,
                counts,
                resource_mode,
                candidate_progress,
                result.error_message,
            );
            on_progress(&progress);
        }
    }

    // Face artifacts have a separately versioned cache contract. Refreshing them reads only an
    // existing CaptureOS-managed preview and writes only face rows/artifacts; it deliberately
    // does not recalculate technical quality, visual descriptors, groups, recommendations, or
    // preview files.
    for candidate in &face_candidates {
        if should_pause() {
            job.state = WorkflowRunState::Paused;
            job.updated_at = Utc::now();
            progress = intelligence_progress(
                &job,
                counts,
                resource_mode,
                None,
                Some("Face analysis paused. Completed local face artifacts are durable; resume continues remaining media.".into()),
            );
            job.resume_metadata = Some(serde_json::json!({
                "pipeline": "capture-intelligence",
                "resource_mode": resource_mode.as_str(),
                "summary": &progress,
            }));
            repository.update_background_job(&job)?;
            on_progress(&progress);
            return Ok(progress);
        }
        job.stage = JobStage::AiAnalysis;
        job.updated_at = Utc::now();
        repository.update_background_job(&job)?;
        let candidate_progress = Some(candidate.asset_id.clone());
        let result = persist_face_candidate_analysis(
            repository,
            candidate,
            compute_face_candidate_analysis(candidate, cache_root),
        )?;
        job.files_processed += 1;
        job.items_completed = job.files_processed;
        job.updated_at = Utc::now();
        repository.update_background_job(&job)?;
        progress = intelligence_progress(
            &job,
            counts,
            resource_mode,
            candidate_progress,
            result.error_message,
        );
        on_progress(&progress);
    }

    // Build group membership only after all currently queued fingerprints have reached a durable
    // terminal state. This avoids unstable partial groups and keeps candidate generation bounded.
    if !candidates.is_empty() {
        job.stage = JobStage::AiAnalysis;
        job.updated_at = Utc::now();
        repository.update_background_job(&job)?;
        rebuild_similarity_groups_and_recommendations(repository, project_id)?;
    }

    let final_counts = repository.capture_intelligence_terminal_counts(project_id)?;
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::Finalize;
    job.items_completed = job.items_total.unwrap_or(0);
    job.files_processed = job.items_completed;
    job.error_count = final_counts.failed;
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    progress = intelligence_progress(
        &job,
        final_counts,
        resource_mode,
        None,
        Some("Capture Intelligence uses local, explainable technical evidence. It never makes creative or deletion decisions.".into()),
    );
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "capture-intelligence",
        "resource_mode": resource_mode.as_str(),
        "summary": &progress,
    }));
    repository.update_background_job(&job)?;
    on_progress(&progress);
    Ok(progress)
}

/// Loads the durable analysis summary and reconciles counts from artifacts when an older job did
/// not serialize its progress payload. This is read-only with respect to completed evidence.
pub fn load_capture_intelligence_summary(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<Option<CaptureIntelligenceProgress>> {
    let job = repository.latest_capture_intelligence_job(project_id)?;
    if let Some(summary) = job
        .as_ref()
        .and_then(|job| job.resume_metadata.as_ref())
        .and_then(|metadata| metadata.get("summary"))
        .cloned()
        .map(serde_json::from_value::<CaptureIntelligenceProgress>)
        .transpose()
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?
    {
        return Ok(Some(summary));
    }
    let counts = repository.capture_intelligence_terminal_counts(project_id)?;
    if counts.total == 0 {
        return Ok(None);
    }
    let now = Utc::now();
    Ok(Some(CaptureIntelligenceProgress {
        state: "completed".into(),
        stage: "finalize".into(),
        resource_mode: AnalysisResourceMode::Balanced.as_str().into(),
        items_completed: counts.total,
        items_total: counts.total,
        error_count: counts.failed,
        ready_count: counts.ready,
        unsupported_count: counts.unsupported,
        corrupt_count: counts.corrupt,
        needs_original_count: counts.needs_original,
        failed_count: counts.failed,
        not_applicable_count: counts.not_applicable,
        stale_count: counts.stale,
        current_asset_id: None,
        current_stage_detail: None,
        started_at: now.to_rfc3339(),
        finished_at: Some(now.to_rfc3339()),
        message: Some("Recovered from durable local analysis artifacts".into()),
    }))
}

pub fn recover_interrupted_capture_intelligence(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_capture_intelligence()
}

/// Returns a bounded, focused related-frame set. Preview identities are converted at the same
/// desktop boundary as the regular grid, never exposing cache or source paths to the webview.
pub fn load_similarity_group(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    asset_id: &MediaAssetId,
    limit: u32,
    offset: u32,
) -> PersistenceResult<Option<SimilarityGroupView>> {
    ensure_asset_project(repository, project_id, asset_id)?;
    let mut group = repository.similarity_group_for_asset(asset_id, limit, offset)?;
    if let Some(group) = &mut group {
        for member in &mut group.members {
            member.medium_preview_url = preview_url(member.medium_preview_url.take());
            if let Ok(asset_id) = MediaAssetId::try_from(member.asset_id.as_str()) {
                if let Some(detail) = repository.capture_intelligence_detail(&asset_id)? {
                    member.intelligence = detail.summary;
                    member.faces = detail.faces;
                }
            }
        }
    }
    Ok(group)
}

pub fn save_human_intelligence_decision(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    asset_id: &MediaAssetId,
    decision: HumanDecisionValue,
    note: Option<&str>,
) -> PersistenceResult<()> {
    ensure_asset_project(repository, project_id, asset_id)?;
    repository.upsert_human_decision(&HumanDecision {
        id: HumanDecisionId::new(),
        media_asset_id: asset_id.clone(),
        recommendation_id: None,
        decision,
        decided_at: Utc::now(),
        note: note
            .map(str::to_owned)
            .filter(|value| !value.trim().is_empty()),
    })
}

/// Opens a bounded local culling page and translates only CaptureOS-managed preview artifact
/// identities into runtime protocol URLs. The repository never exposes cache/source paths.
pub fn load_culling_workspace(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    query: &CullingQuery,
    cache_root: &Path,
) -> PersistenceResult<CullingWorkspaceView> {
    let mut workspace = repository.culling_workspace(project_id, query)?;
    for row in &mut workspace.items {
        row.media.thumbnail_preview_url = preview_url(row.media.thumbnail_preview_url.take());
        row.media.medium_preview_url = preview_url(row.media.medium_preview_url.take());
        row.media.preview_preview_url = preview_url(row.media.preview_preview_url.take());
        if row.media.preview_status == "ready"
            && row.media.thumbnail_preview_url.is_none()
            && row.media.medium_preview_url.is_none()
            && row.media.preview_preview_url.is_none()
        {
            row.media.preview_status = "stale".into();
            row.media.preview_failure_reason = Some(
                "The local preview cache artifact is no longer available; originals were not changed."
                    .into(),
            );
        }
    }
    // Keeping this parameter is intentional: it mirrors other media loaders and makes the
    // protocol/path boundary explicit even though culling itself never resolves source paths.
    let _ = cache_root;
    Ok(workspace)
}

/// Project-home summary without opening or changing a review session.
pub fn load_culling_progress(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<CullingProgress> {
    repository.culling_progress(project_id)
}

/// Reads only a compact persisted Studio Brain status. It does not materialize historical
/// examples, load an artifact, or start training, so opening a project remains nonblocking.
pub fn load_studio_brain_status(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<StudioBrainProjectStatus> {
    repository.studio_brain_project_status(project_id)
}

pub fn set_studio_brain_project_included(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    included: bool,
) -> PersistenceResult<StudioBrainProjectStatus> {
    let profile_id = repository.ensure_default_studio_profile()?;
    repository.set_project_training_included(&profile_id, project_id, included)?;
    repository.studio_brain_project_status(project_id)
}

pub fn set_studio_brain_enabled(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    enabled: bool,
) -> PersistenceResult<StudioBrainProjectStatus> {
    let profile_id = repository.ensure_default_studio_profile()?;
    repository.set_studio_personalization_enabled(&profile_id, enabled)?;
    repository.studio_brain_project_status(project_id)
}

/// Removes only derived local models and recommendations. This intentionally leaves every
/// culling decision, rating, star, representative, Moment, preview, and source file untouched.
pub fn reset_studio_brain_personalization(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<StudioBrainProjectStatus> {
    let profile_id = repository.ensure_default_studio_profile()?;
    repository.reset_studio_personalization(&profile_id)?;
    repository.studio_brain_project_status(project_id)
}

/// One explicit production-plan dry run. It is derived from local catalog state and performs no
/// destination writes. A frozen manifest is created only after this preview is free of blockers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPlanPreview {
    pub plan: ProductionPlanRecord,
    /// Kept in core for the immediate manifest transaction. It is deliberately never serialized
    /// across the desktop boundary, which prevents a large project preview from loading an
    /// entire 100k-entry candidate manifest into React memory.
    #[serde(skip_serializing)]
    pub manifest_draft: ManifestDraft,
    pub manifest_summary: ProductionManifestPreviewSummary,
    /// At most 120 local catalog records. This makes the human selection review useful without
    /// turning a large production plan into a 100k-item frontend payload.
    pub inspection: ProductionPlanInspection,
    pub destination_path: Option<String>,
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
    pub naming_examples: Vec<ProductionNamingExample>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionManifestPreviewSummary {
    pub selected_file_count: u64,
    pub estimated_bytes: u64,
    pub checksum: String,
    pub blocking_issue_count: u64,
    pub warning_issue_count: u64,
}

/// Bounded, read-only production selection review. Its asset identifiers are desktop-local
/// control references only; they are never written to client-facing delivery artifacts.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPlanInspection {
    pub included_count: u64,
    pub excluded_count: u64,
    pub blocked_count: u64,
    pub remaining_count: u64,
    pub items: Vec<ProductionPlanInspectionItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionPlanInspectionItem {
    pub asset_id: String,
    pub original_filename: String,
    pub human_decision: Option<String>,
    /// `included`, `excluded`, or `blocked`; this is a plan preview fact, never a culling state.
    pub state: String,
    pub destination_relative_path: Option<String>,
    pub reason: Option<String>,
    pub plan_override: Option<PlanOverrideKind>,
}

impl ProductionPlanPreview {
    pub fn can_create_manifest(&self) -> bool {
        self.blockers.is_empty() && self.manifest_draft.is_ready()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionNamingExample {
    pub original_filename: String,
    pub destination_relative_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionExportProgress {
    pub export_job_id: String,
    pub manifest_id: String,
    pub state: String,
    pub stage: String,
    pub items_completed: u64,
    pub items_total: u64,
    pub verified_count: u64,
    pub skipped_identical_count: u64,
    pub failed_count: u64,
    pub verified_bytes: u64,
    pub current_filename: Option<String>,
    pub message: Option<String>,
}

pub fn load_production_workspace(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<ProductionWorkspaceView> {
    repository.production_workspace(project_id)
}

pub fn create_production_plan(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    input: &ProductionPlanInput,
) -> PersistenceResult<ProductionPlanRecord> {
    repository.create_production_plan(project_id, input)
}

pub fn update_production_plan_destination(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
    destination_path: Option<&str>,
) -> PersistenceResult<ProductionPlanRecord> {
    repository.update_production_plan_destination(project_id, plan_id, destination_path)
}

/// Changes only a plan's local preflight headroom. It never creates an artificial catalog or
/// export-size limit, and any previous manifest becomes stale for an explicit fresh preview.
pub fn update_production_plan_destination_reserve(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
    reserve_bytes: u64,
) -> PersistenceResult<ProductionPlanRecord> {
    repository.update_production_plan_destination_reserve(project_id, plan_id, reserve_bytes)
}

pub fn update_production_plan_configuration(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
    input: &ProductionPlanInput,
) -> PersistenceResult<ProductionPlanRecord> {
    repository.update_production_plan_configuration(project_id, plan_id, input)
}

pub fn set_production_plan_override(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
    asset_id: &MediaAssetId,
    kind: Option<PlanOverrideKind>,
) -> PersistenceResult<()> {
    // This writes an organizational override only. The repository never receives a culling
    // mutation here, so Keep/Reject/Review, rating, star, note, and source media remain intact.
    repository.set_production_plan_override(project_id, plan_id, asset_id, kind)
}

pub fn create_virtual_collection(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    input: &VirtualCollectionInput,
) -> PersistenceResult<VirtualCollectionRecord> {
    repository.create_virtual_collection(project_id, input)
}

pub fn set_static_virtual_collection_members(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    collection_id: &str,
    media_asset_ids: &[MediaAssetId],
) -> PersistenceResult<()> {
    repository.set_static_virtual_collection_members(project_id, collection_id, media_asset_ids)
}

/// One static-collection membership is a local asset reference only. It neither copies media
/// nor changes Smart Cull; collection-backed manifests become stale for an explicit refresh.
pub fn set_static_virtual_collection_member(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    collection_id: &str,
    media_asset_id: &MediaAssetId,
    included: bool,
) -> PersistenceResult<()> {
    repository.set_static_virtual_collection_member(
        project_id,
        collection_id,
        media_asset_id,
        included,
    )
}

pub fn virtual_collection_assets(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    collection_id: &str,
) -> PersistenceResult<Vec<String>> {
    repository.virtual_collection_assets(project_id, collection_id)
}

pub fn preview_production_plan(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
) -> PersistenceResult<ProductionPlanPreview> {
    let (plan, overrides, assets, source_roots, source_revision, virtual_collection_asset_ids) =
        repository.production_manifest_build_input(project_id, plan_id)?;
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project does not exist".into()))?;
    let inspection_seed = production_plan_inspection_seed(
        &plan,
        &overrides,
        &assets,
        virtual_collection_asset_ids.as_deref(),
    );
    let manifest_draft = build_manifest(&ManifestBuildInput {
        plan_id: plan.id.clone(),
        project_id: plan.project_id.clone(),
        project_name: project.name,
        plan_type: plan.plan_type,
        selection_rules: plan.selection_rules.clone(),
        organization: plan.organization,
        filename_strategy: plan.filename_strategy.clone(),
        overrides,
        virtual_collection_asset_ids,
        assets,
    })
    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let inspection = production_plan_inspection_from_seed(inspection_seed, &manifest_draft);
    let mut preview =
        production_preview_from_draft(&plan, manifest_draft, &source_roots, inspection)?;
    // The source revision is not an advisory display field—it is rechecked inside the manifest
    // creation transaction. Recording it in the diagnostic lets advanced users understand why a
    // concurrent human decision asks them to refresh rather than silently changing a snapshot.
    preview.warnings.push(format!(
        "Local selection revision {source_revision}; manifest creation rechecks it transactionally."
    ));
    Ok(preview)
}

pub fn create_production_export_manifest(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    plan_id: &str,
) -> PersistenceResult<ExportManifestRecord> {
    let (plan, overrides, assets, source_roots, source_revision, virtual_collection_asset_ids) =
        repository.production_manifest_build_input(project_id, plan_id)?;
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project does not exist".into()))?;
    let draft = build_manifest(&ManifestBuildInput {
        plan_id: plan.id.clone(),
        project_id: plan.project_id.clone(),
        project_name: project.name,
        plan_type: plan.plan_type,
        selection_rules: plan.selection_rules.clone(),
        organization: plan.organization,
        filename_strategy: plan.filename_strategy.clone(),
        overrides,
        virtual_collection_asset_ids,
        assets,
    })
    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let preview = production_preview_from_draft(
        &plan,
        draft.clone(),
        &source_roots,
        ProductionPlanInspection::default(),
    )?;
    if !preview.can_create_manifest() {
        return Err(PersistenceError::InvalidData(format!(
            "Production Plan cannot create a manifest: {}",
            preview.blockers.join(" ")
        )));
    }
    let destination = preview
        .destination_path
        .as_deref()
        .ok_or_else(|| PersistenceError::InvalidData("a local destination is required".into()))?;
    repository.create_export_manifest(
        &plan,
        source_revision,
        destination,
        &serde_json::json!({
            "destinationWritable": preview.destination_writable,
            "availableBytes": preview.available_bytes,
            "requiredBytes": preview.required_bytes,
            "reserveBytes": preview.reserve_bytes,
            "headroomBytes": preview.headroom_bytes,
            "availableSourceCount": preview.available_source_count,
            "offlineSourceCount": preview.offline_source_count,
            "existingIdenticalCount": preview.existing_identical_count,
            "collisionCount": preview.collision_count,
            "blockers": preview.blockers,
            "warnings": preview.warnings,
        }),
        &draft.entries,
        &draft.checksum,
    )
}

pub fn preflight_production_export(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    manifest_id: &str,
) -> PersistenceResult<ProductionPreflight> {
    let manifest = repository
        .export_manifest(project_id, manifest_id)?
        .ok_or_else(|| {
            PersistenceError::InvalidData("export manifest does not belong to this project".into())
        })?;
    let entries = repository.export_manifest_entries(project_id, manifest_id)?;
    let execution_entries =
        repository.export_manifest_execution_entries(project_id, manifest_id)?;
    // An Export Manifest is an immutable safety snapshot. Export-time preflight must use the
    // reserve that was explicitly validated when this frozen manifest was created, not a mutable
    // plan setting or the generic default. This closes an M9 under/over-enforcement race.
    let reserve_bytes = manifest
        .validation
        .get("reserveBytes")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value >= delivery_brain::MIN_DESTINATION_RESERVE_BYTES)
        .ok_or_else(|| {
            PersistenceError::InvalidData(
                "This frozen Export Manifest is missing its validated destination reserve; refresh the Production Plan before exporting".into(),
            )
        })?;
    let preflight = production_preflight_from_entries(
        &manifest.destination_path,
        manifest.estimated_bytes,
        reserve_bytes,
        &entries,
        &execution_entries,
    )?;
    Ok(ProductionPreflight {
        manifest,
        entries,
        destination_writable: preflight.destination_writable,
        available_bytes: preflight.available_bytes,
        required_bytes: preflight.required_bytes,
        reserve_bytes: preflight.reserve_bytes,
        headroom_bytes: preflight.headroom_bytes,
        available_source_count: preflight.available_source_count,
        offline_source_count: preflight.offline_source_count,
        existing_identical_count: preflight.existing_identical_count,
        collision_count: preflight.collision_count,
        blockers: preflight.blockers,
        warnings: preflight.warnings,
    })
}

/// Executes one frozen local Export Manifest. It never reevaluates selection rules or follows a
/// Studio recommendation, and it uses the same streaming verified-copy primitive as ingest.
/// Calling it again on an interrupted/failed manifest safely resumes by recognizing verified
/// destination matches; it never overwrites a differing existing file.
pub fn export_production_manifest(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    manifest_id: &str,
    cancelled: impl Fn() -> bool,
    mut on_progress: impl FnMut(&ProductionExportProgress),
) -> PersistenceResult<ProductionExportProgress> {
    let preflight = preflight_production_export(repository, project_id, manifest_id)?;
    if !preflight.can_start() {
        return Err(PersistenceError::InvalidData(format!(
            "Export cannot start: {}",
            preflight.blockers.join(" ")
        )));
    }
    let manifest = preflight.manifest;
    let manifest_entries = preflight.entries;
    if manifest.status != "ready" {
        return Err(PersistenceError::InvalidData(
            "refresh this Production Plan before exporting; its manifest is no longer current"
                .into(),
        ));
    }
    let destination_root = canonical_local_destination(&manifest.destination_path)?;
    let execution_entries =
        repository.export_manifest_execution_entries(project_id, manifest_id)?;
    let started_at = Utc::now();
    let job_id = Uuid::new_v4().to_string();
    let background_job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::ProductionExport,
        items_completed: 0,
        items_total: Some(execution_entries.len() as u64),
        files_discovered: execution_entries.len() as u64,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({
            "pipeline": "production-export",
            "export_job_id": job_id,
            "manifest_id": manifest_id,
            "verification": "blake3_streaming",
        })),
        created_at: started_at,
        updated_at: started_at,
        finished_at: None,
    };
    let mut export_job = ExportJobRecord {
        id: job_id.clone(),
        plan_id: manifest.plan_id.clone(),
        manifest_id: manifest.id.clone(),
        background_job_id: background_job.id.to_string(),
        state: "running".into(),
        destination_path: destination_root.to_string_lossy().into_owned(),
        items_total: execution_entries.len() as u64,
        items_completed: 0,
        verified_count: 0,
        skipped_identical_count: 0,
        failed_count: 0,
        verified_bytes: 0,
        created_at: started_at.to_rfc3339(),
        updated_at: started_at.to_rfc3339(),
        finished_at: None,
        error_message: None,
    };
    let mut durable_job = background_job;
    repository.create_export_job(&export_job, &durable_job)?;
    let mut progress = production_export_progress(
        &export_job,
        "copying",
        None,
        Some("Verified local export started.".into()),
    );
    on_progress(&progress);

    for execution in execution_entries {
        if cancelled() {
            repository.cancel_pending_export_job_entries(
                &export_job.id,
                "Export cancelled before this entry began; it is not a completed export file.",
            )?;
            export_job.state = "cancelled".into();
            export_job.updated_at = Utc::now().to_rfc3339();
            export_job.finished_at = Some(export_job.updated_at.clone());
            durable_job.state = WorkflowRunState::Cancelled;
            durable_job.stage = JobStage::Finalize;
            durable_job.updated_at = Utc::now();
            durable_job.finished_at = Some(durable_job.updated_at);
            durable_job.error_message = Some(
                "Export cancelled. Previously verified destination files remain valid.".into(),
            );
            repository.update_export_job(&export_job, &durable_job)?;
            progress = production_export_progress(
                &export_job,
                "finalize",
                None,
                durable_job.error_message.clone(),
            );
            on_progress(&progress);
            return Ok(progress);
        }
        let entry = execution.entry.clone();
        let source = match safe_execution_source(&execution) {
            Ok(path) => path,
            Err(error) => {
                record_export_entry_failure(
                    repository,
                    &mut export_job,
                    &mut durable_job,
                    &entry.id,
                    &error.to_string(),
                )?;
                progress = production_export_progress(
                    &export_job,
                    "copying",
                    Some(entry.original_filename),
                    Some("A source is unavailable; continuing remaining entries.".into()),
                );
                on_progress(&progress);
                continue;
            }
        };
        let target =
            destination_relative_target(&destination_root, &entry.destination_relative_path)?;
        repository.update_export_job_entry(
            &export_job.id,
            &ExportJobEntryUpdate {
                manifest_entry_id: entry.id.clone(),
                state: "copying".into(),
                copied_bytes: 0,
                source_checksum: None,
                destination_checksum: None,
                error_message: None,
            },
        )?;
        let mut observed_copied = 0_u64;
        let outcome = copy_and_verify(&source, &destination_root, &target, &cancelled, |copied| {
            observed_copied = copied;
        });
        match outcome {
            Ok(CopyVerificationOutcome::Verified(result)) => {
                let state = if result.reused_existing {
                    "skipped_identical"
                } else {
                    "verified"
                };
                repository.update_export_job_entry(
                    &export_job.id,
                    &ExportJobEntryUpdate {
                        manifest_entry_id: entry.id.clone(),
                        state: state.into(),
                        copied_bytes: result.byte_size,
                        source_checksum: Some(result.source_hash),
                        destination_checksum: Some(result.destination_hash),
                        error_message: None,
                    },
                )?;
                export_job.items_completed += 1;
                export_job.verified_count += 1;
                export_job.verified_bytes = export_job
                    .verified_bytes
                    .checked_add(result.byte_size)
                    .ok_or_else(|| {
                        PersistenceError::InvalidData("verified byte count overflow".into())
                    })?;
                if result.reused_existing {
                    export_job.skipped_identical_count += 1;
                }
            }
            Ok(CopyVerificationOutcome::Cancelled) => {
                repository.update_export_job_entry(
                    &export_job.id,
                    &ExportJobEntryUpdate { manifest_entry_id: entry.id.clone(), state: "cancelled".into(), copied_bytes: observed_copied, source_checksum: None, destination_checksum: None, error_message: Some("Copy cancelled before verification; the CaptureOS partial is not a completed export file.".into()) },
                )?;
                repository.cancel_pending_export_job_entries(
                    &export_job.id,
                    "Export cancelled before this entry began; it is not a completed export file.",
                )?;
                export_job.state = "cancelled".into();
                export_job.updated_at = Utc::now().to_rfc3339();
                export_job.finished_at = Some(export_job.updated_at.clone());
                durable_job.state = WorkflowRunState::Cancelled;
                durable_job.stage = JobStage::Finalize;
                durable_job.updated_at = Utc::now();
                durable_job.finished_at = Some(durable_job.updated_at);
                durable_job.error_message = Some(
                    "Export cancelled. Previously verified destination files remain valid.".into(),
                );
                repository.update_export_job(&export_job, &durable_job)?;
                progress = production_export_progress(
                    &export_job,
                    "finalize",
                    Some(entry.original_filename),
                    durable_job.error_message.clone(),
                );
                on_progress(&progress);
                return Ok(progress);
            }
            Ok(CopyVerificationOutcome::Conflict { message })
            | Ok(CopyVerificationOutcome::VerificationFailed { message })
            | Ok(CopyVerificationOutcome::SourceChanged { message }) => {
                record_export_entry_failure(
                    repository,
                    &mut export_job,
                    &mut durable_job,
                    &entry.id,
                    &message,
                )?;
            }
            Err(error) => {
                record_export_entry_failure(
                    repository,
                    &mut export_job,
                    &mut durable_job,
                    &entry.id,
                    &error.to_string(),
                )?;
            }
        }
        export_job.updated_at = Utc::now().to_rfc3339();
        durable_job.items_completed = export_job.items_completed;
        durable_job.files_processed = export_job.items_completed;
        durable_job.error_count = export_job.failed_count;
        durable_job.updated_at = Utc::now();
        repository.update_export_job(&export_job, &durable_job)?;
        progress =
            production_export_progress(&export_job, "copying", Some(entry.original_filename), None);
        on_progress(&progress);
    }
    export_job.state = if export_job.failed_count == 0 {
        "completed"
    } else {
        "partially_completed"
    }
    .into();
    export_job.updated_at = Utc::now().to_rfc3339();
    export_job.finished_at = Some(export_job.updated_at.clone());
    let plan = repository
        .production_plan(project_id, &manifest.plan_id)?
        .ok_or_else(|| {
            PersistenceError::InvalidData(
                "Production Plan disappeared before report generation".into(),
            )
        })?;
    let delivery_reference = delivery_reference(&manifest, &export_job);
    if plan.plan_type == delivery_brain::ProductionPlanType::EditorWorkset {
        let handoff = editor_handoff_manifest_payload(
            &manifest,
            &export_job,
            &plan.name,
            &manifest_entries,
            &delivery_reference,
        )?;
        if let Err(error) =
            write_local_editor_handoff_manifest(&destination_root, &delivery_reference, &handoff)
        {
            export_job.state = "partially_completed".into();
            let message = format!(
                "Verified media copy completed, but the local Editor Handoff Manifest could not be written: {error}"
            );
            export_job.error_message = Some(match export_job.error_message.take() {
                Some(existing) => format!("{existing} {message}"),
                None => message,
            });
        }
    }
    let (report_json, report_text) =
        delivery_report_payload(&manifest, &export_job, &plan.name, &delivery_reference)?;
    // Reports are part of a professional handoff. If a destination cannot receive one, the
    // media copies remain verified but the execution must be visibly partial rather than falsely
    // reported as complete.
    let report_written = match write_local_delivery_report(
        &destination_root,
        &delivery_reference,
        &report_json,
        &report_text,
    ) {
        Ok(()) => true,
        Err(error) => {
            export_job.state = "partially_completed".into();
            let message = format!(
                "Verified media copy completed, but the local Delivery Report could not be written: {error}"
            );
            export_job.error_message = Some(match export_job.error_message.take() {
                Some(existing) => format!("{existing} {message}"),
                None => message,
            });
            false
        }
    };
    if report_written {
        if let Err(error) = repository.store_delivery_report(&DeliveryReportRecord {
            id: Uuid::new_v4().to_string(),
            export_job_id: export_job.id.clone(),
            manifest_checksum: manifest.checksum.clone(),
            report_json,
            report_text,
            created_at: export_job.updated_at.clone(),
        }) {
            export_job.state = "partially_completed".into();
            let message = format!(
                "Verified media copy and local Delivery Report completed, but its local catalog record could not be saved: {error}"
            );
            export_job.error_message = Some(match export_job.error_message.take() {
                Some(existing) => format!("{existing} {message}"),
                None => message,
            });
        }
    }
    durable_job.state = if export_job.state == "completed" {
        WorkflowRunState::Completed
    } else {
        WorkflowRunState::Failed
    };
    durable_job.stage = JobStage::Finalize;
    durable_job.items_completed = export_job.items_completed;
    durable_job.files_processed = export_job.items_completed;
    durable_job.error_count = export_job.failed_count;
    durable_job.updated_at = Utc::now();
    durable_job.finished_at = Some(durable_job.updated_at);
    if export_job.error_message.is_none() && export_job.failed_count > 0 {
        export_job.error_message = Some(format!(
            "{} entries could not be verified. Previously verified destination files remain valid.",
            export_job.failed_count
        ));
    }
    durable_job.error_message = export_job.error_message.clone();
    repository.update_export_job(&export_job, &durable_job)?;
    progress = production_export_progress(
        &export_job,
        "finalize",
        None,
        if export_job.failed_count == 0 {
            Some("All selected files were verified locally.".into())
        } else {
            durable_job.error_message.clone()
        },
    );
    on_progress(&progress);
    Ok(progress)
}

pub fn recover_interrupted_production_exports(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_production_exports()
}

// ─────────────────────────────────────────────────────────────────────────────
// Milestone 10 — Edit Bridge presentation and local orchestration
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWorkspace {
    pub sessions: Vec<EditSessionView>,
    pub eligible_manifests: Vec<EligibleEditManifestView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EligibleEditManifestView {
    pub id: String,
    pub plan_id: String,
    pub plan_name: String,
    pub manifest_version: u64,
    pub checksum: String,
    pub selected_file_count: u64,
    pub estimated_bytes: u64,
    /// A local workspace location is app UI metadata only. It is intentionally not copied into
    /// the external canonical edit manifest.
    pub destination_path: Option<String>,
    pub export_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditSessionView {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub template: String,
    pub state: String,
    pub export_manifest_id: String,
    pub source_plan_name: Option<String>,
    pub source_manifest_version: Option<u64>,
    pub source_manifest_checksum: Option<String>,
    pub expected_output_policy: String,
    pub work_item_count: u64,
    pub estimated_bytes: u64,
    pub handoff_state: Option<String>,
    pub returned_output_count: u64,
    pub approved_count: u64,
    pub needs_revision_count: u64,
    pub missing_output_count: u64,
    pub blocked_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWorkItemView {
    pub id: String,
    pub session_id: String,
    pub source_asset_id: String,
    pub source_filename: String,
    pub source_thumbnail_preview_url: Option<String>,
    pub source_available: bool,
    pub moment_label: Option<String>,
    pub rating: Option<u8>,
    pub starred: bool,
    pub state: String,
    pub handoff_relative_path: Option<String>,
    pub latest_output_id: Option<String>,
    pub latest_output_filename: Option<String>,
    pub latest_output_thumbnail_preview_url: Option<String>,
    pub latest_version_number: Option<u64>,
    pub review_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutputView {
    pub id: String,
    pub session_id: String,
    pub filename: String,
    pub thumbnail_preview_url: Option<String>,
    pub availability: String,
    pub state: String,
    pub match_state: String,
    pub match_evidence: Vec<String>,
    pub matched_work_item_id: Option<String>,
    pub suggested_work_item_id: Option<String>,
    pub latest_version_id: Option<String>,
    pub latest_version_number: Option<u64>,
    pub registered_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditVersionView {
    pub id: String,
    pub session_id: String,
    pub output_id: String,
    pub work_item_id: Option<String>,
    pub version_number: u64,
    pub filename: String,
    pub thumbnail_preview_url: Option<String>,
    pub availability: String,
    pub review_state: Option<String>,
    pub is_current: bool,
    pub byte_size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub media_type: Option<String>,
    pub registered_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditSessionPageView {
    pub session: EditSessionView,
    pub work_items: Vec<EditWorkItemView>,
    pub outputs: Vec<EditOutputView>,
    pub versions: Vec<EditVersionView>,
    pub has_more: bool,
    pub total_work_items: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditHandoffView {
    pub id: String,
    pub session_id: String,
    pub handoff_version: u64,
    pub state: String,
    pub manifest_checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EditOutputRegistrationSummary {
    pub discovered_count: u64,
    pub created_count: u64,
    pub matched_count: u64,
    pub ambiguous_count: u64,
    pub unmatched_count: u64,
    pub technically_unreadable_count: u64,
    pub skipped_symlink_count: u64,
}

/// Project opening calls only this compact projection; it never touches external output folders,
/// hashes edits, builds a handoff, generates previews, or re-runs culling.
pub fn edit_workspace(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<EditWorkspace> {
    let persisted = repository.edit_workspace(project_id)?;
    let sessions = persisted
        .sessions
        .iter()
        .map(|session| edit_session_view(repository, project_id, session, None))
        .collect::<PersistenceResult<Vec<_>>>()?;
    let eligible_manifests = persisted
        .eligible_sources
        .iter()
        .map(eligible_edit_manifest_view)
        .collect();
    Ok(EditWorkspace {
        sessions,
        eligible_manifests,
    })
}

pub fn create_edit_session(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    input: &CreateEditSessionInput,
) -> PersistenceResult<EditSessionView> {
    let session = repository.create_edit_session(project_id, input)?;
    edit_session_view(repository, project_id, &session, None)
}

pub fn edit_session_page(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session_id: &str,
    offset: u64,
    limit: u32,
) -> PersistenceResult<EditSessionPageView> {
    let page = repository.edit_session_page(project_id, session_id, offset, limit)?;
    edit_session_page_view(repository, project_id, page)
}

/// Creates the M10 native JSON/CSV/HTML coordination bundle in an explicit, separate local
/// folder. It never copies media, writes beside a source/workset, modifies returned files, or
/// changes a production manifest. A Package handoff is deliberately rejected until a future
/// frozen-manifest adapter can delegate the exact M9 verified-copy execution without recomputing
/// selection.
pub fn generate_edit_handoff(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session_id: &str,
    input: &EditHandoffInput,
) -> PersistenceResult<EditHandoffView> {
    let destination = canonical_edit_handoff_destination(&input.destination_path)?;
    let canonical_input = EditHandoffInput {
        mode: input.mode,
        destination_path: destination.to_string_lossy().into_owned(),
    };
    let preparation = repository.prepare_edit_handoff(project_id, session_id, &canonical_input)?;
    if let Err(error) =
        ensure_edit_handoff_destination_separate(&destination, &preparation.source_root_paths)
    {
        // The reservation is intentionally durable so duplicate clicks cannot race a handoff
        // write. If this post-reservation filesystem safety check fails, release it into a
        // recoverable failed state instead of leaving a permanent `writing` record.
        let _ = repository.record_edit_handoff_failure(
            project_id,
            &preparation.handoff.id,
            &error.to_string(),
        );
        return Err(error);
    }
    if preparation.already_prepared {
        return Ok(EditHandoffView {
            id: preparation.handoff.id,
            session_id: preparation.session.id,
            handoff_version: preparation.handoff.handoff_version,
            state: preparation.handoff.state,
            manifest_checksum: preparation.handoff.manifest_checksum,
        });
    }
    let canonical = canonical_edit_manifest(&preparation, input.mode)?;
    let adapter = NativeManifestAdapter;
    let files = match adapter.render(&canonical) {
        Ok(value) => value,
        Err(error) => {
            let _ = repository.record_edit_handoff_failure(
                project_id,
                &preparation.handoff.id,
                &error.to_string(),
            );
            return Err(PersistenceError::InvalidData(error.to_string()));
        }
    };
    let write = write_edit_handoff_files(&destination, &files);
    if let Err(error) = write {
        let _ = repository.record_edit_handoff_failure(
            project_id,
            &preparation.handoff.id,
            &error.to_string(),
        );
        return Err(error);
    }
    let checksum = canonical
        .checksum()
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let handoff = repository.finish_edit_handoff(project_id, &preparation.handoff.id, &checksum)?;
    Ok(EditHandoffView {
        id: handoff.id,
        session_id: handoff.session_id,
        handoff_version: handoff.handoff_version,
        state: handoff.state,
        manifest_checksum: handoff.manifest_checksum,
    })
}

/// Explicitly scans one selected local root for supported returned-media files. Traversal is
/// iterative, does not follow symlinks, stays beneath the canonical root, streams BLAKE3 hashes,
/// and persists each discovery independently so a corrupt/unreadable file does not block other
/// outputs. It never runs during project startup.
pub fn register_edit_outputs(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session_id: &str,
    selected_path: &str,
) -> PersistenceResult<EditOutputRegistrationSummary> {
    let root = canonical_edit_output_root(selected_path)?;
    let root_record = repository.prepare_edit_output_root(
        project_id,
        session_id,
        selected_path,
        &root.to_string_lossy(),
    )?;
    let candidates = repository.edit_output_match_candidates(project_id, session_id)?;
    let matcher = OutputCandidateIndex::build(candidates);
    let mut summary = EditOutputRegistrationSummary::default();
    let mut seen_relative_paths = std::collections::HashSet::new();
    for discovery in discover_edit_output_files(&root)? {
        if discovery.symlink_skipped {
            summary.skipped_symlink_count += 1;
            continue;
        }
        let Some(path) = discovery.path else { continue };
        let relative = discovery.relative_path.ok_or_else(|| {
            PersistenceError::InvalidData(
                "A returned output path escaped its selected local root".into(),
            )
        })?;
        seen_relative_paths.insert(relative.clone());
        let extension = path.extension().and_then(|value| value.to_str());
        let media_type = classify_extension(extension);
        if !is_edit_output_media_type(&media_type) {
            continue;
        }
        let checksum = match hash_file(&path) {
            Ok(value) => value,
            Err(_) => {
                // A disappearing file is not deleted from catalog history; it simply cannot be
                // registered in this scan. The explicit root remains available for a later retry.
                continue;
            }
        };
        let extracted = extract_metadata(&path, &media_type);
        let technical_status = artifact_status_name(extracted.status.clone()).to_owned();
        let technical_issue = extracted.failure_reason.clone();
        let resolution = matcher.resolve(&OutputMatchInput {
            relative_path: relative.clone(),
            filename: path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned(),
            checksum: Some(checksum.clone()),
        });
        // A real automatic match only exists when the deterministic matcher returned a
        // provenance-preserving work-item link. A unique filename/stem candidate (Strong/Possible)
        // is useful evidence but must never be reported as a matched output until a human confirms
        // it, so the persisted state and the registration summary follow `auto_match_work_item_id`.
        let match_state = if resolution.auto_match_work_item_id.is_some() {
            OutputMatchState::Matched
        } else {
            output_match_state_from_resolution(resolution.confidence)
        };
        let created = repository.register_edit_output(
            project_id,
            session_id,
            &EditOutputRegistration {
                output_root: root_record.clone(),
                relative_path: relative,
                display_filename: path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        PersistenceError::InvalidData(
                            "Returned output filename is not valid Unicode".into(),
                        )
                    })?
                    .to_owned(),
                normalized_basename: normalize_edit_output_basename(&path),
                byte_size: production_io(fs::metadata(&path))?.len(),
                checksum,
                media_type: media_type_name(&media_type).to_owned(),
                metadata: edit_output_metadata_json(&extracted),
                technical_status: technical_status.clone(),
                technical_issue,
                match_state,
                match_confidence: Some(resolution.confidence),
                match_evidence: Some(resolution.evidence),
                auto_match_work_item_id: resolution.auto_match_work_item_id,
            },
        )?;
        summary.discovered_count += 1;
        if created.created {
            summary.created_count += 1;
        }
        match created.output.discovery_state {
            OutputMatchState::Matched => summary.matched_count += 1,
            OutputMatchState::Ambiguous => summary.ambiguous_count += 1,
            OutputMatchState::Unmatched | OutputMatchState::Discovered => {
                summary.unmatched_count += 1
            }
        }
        if technical_status != "ready" {
            summary.technically_unreadable_count += 1;
        }
    }
    // The scan completed successfully, so any previously tracked output of this same session root
    // that was not seen on disk during this scan is transitioned to the offline state. A failed,
    // incomplete, or inaccessible scan never reaches this point, so it can never mass-mark outputs
    // offline. This is idempotent: outputs already offline are left unchanged.
    let seen: Vec<String> = seen_relative_paths.into_iter().collect();
    repository.mark_missing_edit_outputs_offline(project_id, session_id, &root_record.id, &seen)?;
    Ok(summary)
}

pub fn manually_match_edit_output(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    output_id: &str,
    work_item_id: &str,
) -> PersistenceResult<EditOutputView> {
    let outcome = repository.manually_match_edit_output(project_id, output_id, work_item_id)?;
    Ok(edit_output_view(&outcome.output, outcome.version.as_ref()))
}

pub fn review_edit_version(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    version_id: &str,
    state: edit_bridge::EditVersionReviewState,
) -> PersistenceResult<EditVersionRecord> {
    repository.set_edit_version_review_state(project_id, version_id, state)
}

fn edit_session_page_view(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    page: EditSessionPage,
) -> PersistenceResult<EditSessionPageView> {
    let latest_handoff_state = page.handoffs.first().map(|handoff| handoff.state.as_str());
    let session = edit_session_view(repository, project_id, &page.session, latest_handoff_state)?;
    let outputs_by_id = page
        .outputs
        .iter()
        .map(|output| (output.id.as_str(), output))
        .collect::<BTreeMap<_, _>>();
    let versions_by_id = page
        .versions
        .iter()
        .map(|version| (version.id.as_str(), version))
        .collect::<BTreeMap<_, _>>();
    let versions_by_output = page
        .versions
        .iter()
        .map(|version| (version.output_id.as_str(), version))
        .collect::<BTreeMap<_, _>>();
    let work_items = page
        .work_items
        .iter()
        .map(|item| {
            let version = item
                .current_edit_version_id
                .as_deref()
                .and_then(|id| versions_by_id.get(id).copied());
            let output =
                version.and_then(|value| outputs_by_id.get(value.output_id.as_str()).copied());
            EditWorkItemView {
                id: item.id.clone(),
                session_id: item.session_id.clone(),
                source_asset_id: item.source_media_asset_id.clone(),
                source_filename: item.original_filename.clone(),
                source_thumbnail_preview_url: preview_url(item.source_preview_artifact_id.clone()),
                source_available: item.source_available,
                moment_label: item.moment_label.clone(),
                rating: Some(item.rating),
                starred: item.starred,
                state: serializable_enum_name(&item.state),
                handoff_relative_path: Some(item.handoff_relative_path.clone()),
                latest_output_id: output.map(|value| value.id.clone()),
                latest_output_filename: output.map(|value| value.display_filename.clone()),
                latest_output_thumbnail_preview_url: None,
                latest_version_number: version.map(|value| value.version_number),
                review_state: version.and_then(edit_review_state_for_view),
            }
        })
        .collect();
    let outputs = page
        .outputs
        .iter()
        .map(|output| edit_output_view(output, versions_by_output.get(output.id.as_str()).copied()))
        .collect();
    let versions = page
        .versions
        .iter()
        .filter_map(|version| {
            let output = outputs_by_id.get(version.output_id.as_str())?;
            Some(edit_version_view(&page.session.id, version, output))
        })
        .collect();
    Ok(EditSessionPageView {
        total_work_items: session.work_item_count,
        session,
        work_items,
        outputs,
        versions,
        has_more: page.has_more,
    })
}

fn edit_session_view(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session: &EditSessionRecord,
    latest_handoff_state: Option<&str>,
) -> PersistenceResult<EditSessionView> {
    let source_plan_name = repository
        .production_plan(project_id, &session.source_production_plan_id)?
        .map(|plan| plan.name);
    Ok(EditSessionView {
        id: session.id.clone(),
        project_id: session.project_id.clone(),
        name: session.name.clone(),
        template: serializable_enum_name(&session.template),
        state: if session.source_stale {
            "stale".into()
        } else {
            serializable_enum_name(&session.workflow_state)
        },
        export_manifest_id: session.source_export_manifest_id.clone(),
        source_plan_name,
        source_manifest_version: Some(session.source_manifest_version),
        source_manifest_checksum: Some(session.source_manifest_checksum.clone()),
        expected_output_policy: match session.expected_output_policy {
            ExpectedOutputPolicy::Required => "one_per_work_item".into(),
            ExpectedOutputPolicy::Optional => "optional".into(),
        },
        work_item_count: session.work_item_count,
        estimated_bytes: session.estimated_bytes,
        handoff_state: latest_handoff_state
            .map(str::to_owned)
            .or_else(|| (session.handoff_count > 0).then(|| "recorded".into())),
        returned_output_count: session.output_count,
        approved_count: session.approved_count,
        needs_revision_count: session.needs_revision_count,
        missing_output_count: session.missing_output_count,
        blocked_count: session.blocked_count,
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
    })
}

fn eligible_edit_manifest_view(source: &EligibleEditSource) -> EligibleEditManifestView {
    EligibleEditManifestView {
        id: source.export_manifest_id.clone(),
        plan_id: source.production_plan_id.clone(),
        plan_name: source.production_plan_name.clone(),
        manifest_version: source.export_manifest_version,
        checksum: source.export_manifest_checksum.clone(),
        selected_file_count: source.selected_file_count,
        estimated_bytes: source.estimated_bytes,
        destination_path: None,
        export_state: "completed".into(),
    }
}

fn edit_output_view(
    output: &persistence::EditOutputRecord,
    version: Option<&EditVersionRecord>,
) -> EditOutputView {
    let state = if output.availability != "available" {
        "offline".into()
    } else if let Some(version) = version {
        match version.review_state {
            edit_bridge::EditVersionReviewState::Approved => "approved".into(),
            edit_bridge::EditVersionReviewState::NeedsRevision => "needs_revision".into(),
            edit_bridge::EditVersionReviewState::Superseded => "superseded".into(),
            edit_bridge::EditVersionReviewState::ReadyForReview => "ready_for_review".into(),
        }
    } else {
        serializable_enum_name(&output.discovery_state)
    };
    EditOutputView {
        id: output.id.clone(),
        session_id: output.session_id.clone(),
        filename: output.display_filename.clone(),
        thumbnail_preview_url: None,
        availability: output.availability.clone(),
        state,
        match_state: output
            .match_confidence
            .as_ref()
            .map(serializable_enum_name)
            .unwrap_or_else(|| serializable_enum_name(&output.discovery_state)),
        match_evidence: output
            .match_evidence
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| vec![value.into()])
            .unwrap_or_default(),
        matched_work_item_id: output.matched_work_item_id.clone(),
        suggested_work_item_id: None,
        latest_version_id: version.map(|value| value.id.clone()),
        latest_version_number: version.map(|value| value.version_number),
        registered_at: output.registered_at.clone(),
    }
}

fn edit_version_view(
    session_id: &str,
    version: &EditVersionRecord,
    output: &persistence::EditOutputRecord,
) -> EditVersionView {
    let width = output
        .metadata
        .get("width")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let height = output
        .metadata
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    EditVersionView {
        id: version.id.clone(),
        session_id: session_id.into(),
        output_id: output.id.clone(),
        work_item_id: Some(version.work_item_id.clone()),
        version_number: version.version_number,
        filename: output.display_filename.clone(),
        thumbnail_preview_url: None,
        availability: output.availability.clone(),
        review_state: edit_review_state_for_view(version),
        is_current: version.is_current,
        byte_size: Some(output.byte_size),
        width,
        height,
        media_type: Some(output.media_type.clone()),
        registered_at: output.registered_at.clone(),
    }
}

fn edit_review_state_for_view(version: &EditVersionRecord) -> Option<String> {
    match version.review_state {
        edit_bridge::EditVersionReviewState::Approved
        | edit_bridge::EditVersionReviewState::NeedsRevision => {
            Some(serializable_enum_name(&version.review_state))
        }
        edit_bridge::EditVersionReviewState::ReadyForReview
        | edit_bridge::EditVersionReviewState::Superseded => None,
    }
}

fn serializable_enum_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unavailable".into())
}

fn canonical_edit_manifest(
    preparation: &persistence::EditHandoffPreparation,
    mode: HandoffMode,
) -> PersistenceResult<CanonicalEditManifest> {
    let work_items = preparation
        .work_items
        .iter()
        .map(|item| EditManifestWorkItem {
            work_item_id: item.id.clone(),
            source_media_asset_id: item.source_media_asset_id.clone(),
            source_manifest_entry_id: item.source_export_manifest_entry_id.clone(),
            handoff_relative_path: item.handoff_relative_path.clone(),
            original_filename: item.original_filename.clone(),
            source_checksum: item.source_checksum.clone(),
            captured_at: item.captured_at.clone(),
            camera: item.camera.clone(),
            moment_label: item.moment_label.clone(),
            human_decision: item.human_decision.clone(),
            rating: item.rating,
            starred: item.starred,
            expected_output_policy: item.expected_output_policy,
        })
        .collect();
    let manifest = CanonicalEditManifest {
        schema_version: EDIT_MANIFEST_SCHEMA_VERSION,
        session_id: preparation.session.id.clone(),
        handoff_id: preparation.handoff.id.clone(),
        created_at: preparation.handoff.created_at.clone(),
        project: EditManifestProject {
            id: preparation.session.project_id.clone(),
            name: preparation.project_name.clone(),
        },
        source: EditManifestSource {
            production_plan_id: preparation.session.source_production_plan_id.clone(),
            export_manifest_id: preparation.session.source_export_manifest_id.clone(),
            export_manifest_checksum: preparation.session.source_manifest_checksum.clone(),
            export_manifest_version: preparation.session.source_manifest_version,
            handoff_mode: mode,
        },
        expected_output_policy: preparation.session.expected_output_policy,
        privacy: EditManifestPrivacy::default(),
        adapter: EditAdapterIdentity::native_manifest(),
        work_items,
    };
    manifest
        .validate()
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    Ok(manifest)
}

fn canonical_edit_handoff_destination(value: &str) -> PersistenceResult<PathBuf> {
    let selected = Path::new(value);
    let metadata = fs::symlink_metadata(selected).map_err(|error| {
        PersistenceError::InvalidData(format!("Edit Handoff destination is unavailable: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PersistenceError::InvalidData(
            "Edit Handoff destination must be an existing, non-symlink local folder".into(),
        ));
    }
    selected.canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("Edit Handoff destination is unavailable: {error}"))
    })
}

fn canonical_edit_output_root(value: &str) -> PersistenceResult<PathBuf> {
    let selected = Path::new(value);
    let metadata = fs::symlink_metadata(selected).map_err(|error| {
        PersistenceError::InvalidData(format!("Returned-output folder is unavailable: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PersistenceError::InvalidData(
            "Returned-output folder must be an existing, non-symlink local folder".into(),
        ));
    }
    selected.canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("Returned-output folder is unavailable: {error}"))
    })
}

fn ensure_edit_handoff_destination_separate(
    destination: &Path,
    source_roots: &[String],
) -> PersistenceResult<()> {
    for raw_root in source_roots {
        let root = match Path::new(raw_root).canonicalize() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if destination.starts_with(&root) || root.starts_with(destination) {
            return Err(PersistenceError::InvalidData(
                "Edit Handoff metadata must be written to a separate local folder, never inside an original source or verified workset".into(),
            ));
        }
    }
    Ok(())
}

fn write_edit_handoff_files(
    destination: &Path,
    files: &edit_bridge::EditHandoffFiles,
) -> PersistenceResult<()> {
    let entries = [
        (
            "captureos-edit-manifest.json",
            files.manifest_json.as_bytes(),
        ),
        ("editor-handoff.csv", files.csv.as_bytes()),
        ("captureos-edit-handoff.html", files.html.as_bytes()),
    ];
    for (name, contents) in entries {
        write_new_or_identical_handoff_file(destination, name, contents)?;
    }
    Ok(())
}

fn write_new_or_identical_handoff_file(
    destination: &Path,
    filename: &str,
    contents: &[u8],
) -> PersistenceResult<()> {
    let target = destination.join(filename);
    if target.exists() {
        let metadata = production_io(fs::symlink_metadata(&target))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(PersistenceError::InvalidData(
                "Edit Handoff target conflicts with an unsafe existing path".into(),
            ));
        }
        let existing = production_io(fs::read(&target))?;
        if existing == contents {
            return Ok(());
        }
        return Err(PersistenceError::InvalidData(
            "Edit Handoff target already exists with different content; CaptureOS will not overwrite it".into(),
        ));
    }
    let temporary = destination.join(format!(".{filename}.captureos-{}.partial", Uuid::new_v4()));
    let mut file = production_io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary),
    )?;
    production_io(file.write_all(contents))?;
    production_io(file.sync_all())?;
    match fs::hard_link(&temporary, &target) {
        Ok(()) => {
            production_io(fs::remove_file(&temporary))?;
            Ok(())
        }
        Err(error) if target.exists() => {
            let _ = fs::remove_file(&temporary);
            let metadata = production_io(fs::symlink_metadata(&target))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(PersistenceError::InvalidData(
                    "Edit Handoff target conflicts with an unsafe existing path".into(),
                ));
            }
            let existing = production_io(fs::read(&target))?;
            if existing == contents {
                Ok(())
            } else {
                Err(PersistenceError::InvalidData(format!(
                    "Edit Handoff target already exists with different content: {error}"
                )))
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(PersistenceError::InvalidData(format!(
                "Edit Handoff could not be finalized without overwrite: {error}"
            )))
        }
    }
}

#[derive(Debug, Clone)]
struct EditOutputDiscovery {
    path: Option<PathBuf>,
    relative_path: Option<String>,
    symlink_skipped: bool,
}

fn discover_edit_output_files(root: &Path) -> PersistenceResult<Vec<EditOutputDiscovery>> {
    const MAX_SAFE_OUTPUT_DEPTH: usize = 64;
    let mut directories = vec![(root.to_path_buf(), 0_usize)];
    let mut discoveries = Vec::new();
    while let Some((directory, depth)) = directories.pop() {
        let entries = production_io(fs::read_dir(&directory))?;
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(_) => continue,
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() {
                discoveries.push(EditOutputDiscovery {
                    path: None,
                    relative_path: None,
                    symlink_skipped: true,
                });
                continue;
            }
            if metadata.is_dir() {
                if depth >= MAX_SAFE_OUTPUT_DEPTH {
                    return Err(PersistenceError::InvalidData(
                        "Returned-output traversal exceeded the safe directory depth; choose a narrower output root".into(),
                    ));
                }
                let canonical = match path.canonicalize() {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if canonical.starts_with(root) {
                    directories.push((canonical, depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(value) => value,
                Err(_) => continue,
            };
            if !canonical.starts_with(root) {
                continue;
            }
            let relative = canonical
                .strip_prefix(root)
                .ok()
                .and_then(|value| value.to_str())
                .map(|value| value.replace('\\', "/"));
            if relative
                .as_deref()
                .is_none_or(|value| !edit_bridge::safe_relative_path(value))
            {
                continue;
            }
            discoveries.push(EditOutputDiscovery {
                path: Some(canonical),
                relative_path: relative,
                symlink_skipped: false,
            });
        }
    }
    discoveries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(discoveries)
}

fn is_edit_output_media_type(media_type: &MediaType) -> bool {
    matches!(
        media_type,
        MediaType::Jpeg | MediaType::Heif | MediaType::Png | MediaType::Tiff | MediaType::Video
    )
}

fn media_type_name(media_type: &MediaType) -> &'static str {
    match media_type {
        MediaType::RawPhoto => "raw_photo",
        MediaType::Jpeg => "jpeg",
        MediaType::Heif => "heif",
        MediaType::Png => "png",
        MediaType::Tiff => "tiff",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Sidecar => "sidecar",
        MediaType::Proxy => "proxy",
        MediaType::Thumbnail => "thumbnail",
        MediaType::ProjectDocument => "project_document",
        MediaType::Unknown => "unknown",
    }
}

fn artifact_status_name(status: ArtifactStatus) -> &'static str {
    match status {
        ArtifactStatus::Pending => "pending",
        ArtifactStatus::Ready => "ready",
        ArtifactStatus::Unsupported => "unsupported",
        ArtifactStatus::Offline => "offline",
        ArtifactStatus::Corrupt => "corrupt",
        ArtifactStatus::Failed => "failed",
        ArtifactStatus::Timeout => "timeout",
        ArtifactStatus::Cancelled => "cancelled",
        ArtifactStatus::Stale => "stale",
    }
}

fn normalize_edit_output_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

fn edit_output_metadata_json(metadata: &media_visual::ExtractedMetadata) -> serde_json::Value {
    serde_json::json!({
        "mimeType": metadata.mime_type,
        "byteSize": metadata.byte_size,
        "capturedAtLocal": metadata.captured_at_local,
        "width": metadata.width,
        "height": metadata.height,
        "orientation": metadata.orientation,
        "cameraModel": metadata.camera_model,
        "colorSpace": metadata.color_space,
        "status": artifact_status_name(metadata.status.clone()),
    })
}

fn output_match_state_from_resolution(confidence: MatchConfidence) -> OutputMatchState {
    match confidence {
        MatchConfidence::Exact => OutputMatchState::Matched,
        MatchConfidence::Strong => OutputMatchState::Unmatched,
        MatchConfidence::Ambiguous => OutputMatchState::Ambiguous,
        MatchConfidence::Possible | MatchConfidence::Unmatched | MatchConfidence::Manual => {
            OutputMatchState::Unmatched
        }
    }
}

#[derive(Debug, Clone)]
struct ProductionLocalPreflight {
    destination_writable: bool,
    available_bytes: Option<u64>,
    required_bytes: u64,
    reserve_bytes: u64,
    headroom_bytes: Option<u64>,
    available_source_count: u64,
    offline_source_count: u64,
    existing_identical_count: u64,
    collision_count: u64,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

fn production_preview_from_draft(
    plan: &ProductionPlanRecord,
    manifest_draft: ManifestDraft,
    source_roots: &BTreeMap<String, String>,
    inspection: ProductionPlanInspection,
) -> PersistenceResult<ProductionPlanPreview> {
    let local = preflight_draft_entries(
        plan.destination_path.as_deref(),
        manifest_draft.estimated_bytes,
        plan.destination_reserve_bytes,
        &manifest_draft.entries,
        source_roots,
    )?;
    let mut blockers = local.blockers;
    blockers.extend(
        manifest_draft
            .issues
            .iter()
            .filter(|issue| issue.blocking)
            .map(|issue| issue.message.clone()),
    );
    let mut warnings = local.warnings;
    warnings.extend(
        manifest_draft
            .issues
            .iter()
            .filter(|issue| !issue.blocking)
            .map(|issue| issue.message.clone()),
    );
    Ok(ProductionPlanPreview {
        plan: plan.clone(),
        destination_path: plan.destination_path.clone(),
        required_bytes: local.required_bytes,
        reserve_bytes: local.reserve_bytes,
        manifest_summary: ProductionManifestPreviewSummary {
            selected_file_count: manifest_draft.selected_file_count,
            estimated_bytes: manifest_draft.estimated_bytes,
            checksum: manifest_draft.checksum.clone(),
            blocking_issue_count: manifest_draft
                .issues
                .iter()
                .filter(|issue| issue.blocking)
                .count() as u64,
            warning_issue_count: manifest_draft
                .issues
                .iter()
                .filter(|issue| !issue.blocking)
                .count() as u64,
        },
        inspection,
        manifest_draft: manifest_draft.clone(),
        destination_writable: local.destination_writable,
        available_bytes: local.available_bytes,
        headroom_bytes: local.headroom_bytes,
        available_source_count: local.available_source_count,
        offline_source_count: local.offline_source_count,
        existing_identical_count: local.existing_identical_count,
        collision_count: local.collision_count,
        blockers,
        warnings,
        naming_examples: manifest_draft
            .entries
            .iter()
            .take(5)
            .map(|entry| ProductionNamingExample {
                original_filename: entry.original_filename.clone(),
                destination_relative_path: entry.destination_relative_path.clone(),
            })
            .collect(),
    })
}

fn production_plan_inspection_seed(
    plan: &ProductionPlanRecord,
    overrides: &[PlanOverride],
    assets: &[delivery_brain::DeliveryAssetCandidate],
    virtual_collection_asset_ids: Option<&[String]>,
) -> ProductionPlanInspection {
    const INSPECTION_LIMIT: usize = 120;
    let overrides = overrides
        .iter()
        .map(|value| (value.media_asset_id.as_str(), value.kind))
        .collect::<BTreeMap<_, _>>();
    let virtual_collection_asset_ids = virtual_collection_asset_ids
        .map(|ids| ids.iter().map(String::as_str).collect::<HashSet<_>>());
    let mut sorted_assets = assets.iter().collect::<Vec<_>>();
    sorted_assets.sort_by(|left, right| {
        left.captured_at
            .cmp(&right.captured_at)
            .then_with(|| {
                left.moment
                    .as_ref()
                    .map(|moment| moment.ordinal)
                    .cmp(&right.moment.as_ref().map(|moment| moment.ordinal))
            })
            .then_with(|| left.asset_id.cmp(&right.asset_id))
    });

    let mut inspection = ProductionPlanInspection::default();
    let mut all_items = Vec::with_capacity(sorted_assets.len().min(INSPECTION_LIMIT));
    for asset in sorted_assets {
        let override_kind = overrides.get(asset.asset_id.as_str()).copied();
        let selected = match override_kind {
            Some(PlanOverrideKind::ForceInclude) => true,
            Some(PlanOverrideKind::ForceExclude) => false,
            None => {
                virtual_collection_asset_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(asset.asset_id.as_str()))
                    && plan.selection_rules.matches(asset)
            }
        };
        let item = if selected {
            inspection.included_count += 1;
            ProductionPlanInspectionItem {
                asset_id: asset.asset_id.clone(),
                original_filename: asset.original_filename.clone(),
                human_decision: asset.human_decision.clone(),
                state: "included".into(),
                destination_relative_path: None,
                reason: None,
                plan_override: override_kind,
            }
        } else {
            inspection.excluded_count += 1;
            let reason = match override_kind {
                Some(PlanOverrideKind::ForceExclude) => {
                    "Excluded by this plan's local override.".into()
                }
                Some(PlanOverrideKind::ForceInclude) => {
                    "The forced inclusion could not be resolved into this preview.".into()
                }
                None => "Does not meet this plan's explicit human selection rules.".into(),
            };
            ProductionPlanInspectionItem {
                asset_id: asset.asset_id.clone(),
                original_filename: asset.original_filename.clone(),
                human_decision: asset.human_decision.clone(),
                state: "excluded".into(),
                destination_relative_path: None,
                reason: Some(reason),
                plan_override: override_kind,
            }
        };
        if all_items.len() < INSPECTION_LIMIT {
            all_items.push(item);
        }
    }
    inspection.remaining_count = inspection
        .included_count
        .saturating_add(inspection.excluded_count)
        .saturating_add(inspection.blocked_count)
        .saturating_sub(all_items.len() as u64);
    inspection.items = all_items;
    inspection
}

fn production_plan_inspection_from_seed(
    mut inspection: ProductionPlanInspection,
    manifest_draft: &ManifestDraft,
) -> ProductionPlanInspection {
    let entries = manifest_draft
        .entries
        .iter()
        .map(|entry| (entry.media_asset_id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let blocked_total = manifest_draft
        .entries
        .iter()
        .filter(|entry| entry.status != delivery_brain::ManifestEntryStatus::Planned)
        .count() as u64;
    inspection.included_count = inspection.included_count.saturating_sub(blocked_total);
    inspection.blocked_count = blocked_total;
    for item in &mut inspection.items {
        let Some(entry) = entries.get(item.asset_id.as_str()) else {
            continue;
        };
        item.destination_relative_path = Some(entry.destination_relative_path.clone());
        item.reason = entry.issue.clone();
        if entry.status != delivery_brain::ManifestEntryStatus::Planned {
            item.state = "blocked".into();
        }
    }
    inspection
}

fn preflight_draft_entries(
    destination_path: Option<&str>,
    estimated_bytes: u64,
    reserve_bytes: u64,
    entries: &[delivery_brain::ManifestEntryDraft],
    source_roots: &BTreeMap<String, String>,
) -> PersistenceResult<ProductionLocalPreflight> {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let required_bytes = estimated_bytes.checked_add(reserve_bytes).ok_or_else(|| {
        PersistenceError::InvalidData("destination space requirement overflow".into())
    })?;
    let Some(destination_path) = destination_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return Ok(ProductionLocalPreflight {
            destination_writable: false,
            available_bytes: None,
            required_bytes,
            reserve_bytes,
            headroom_bytes: None,
            available_source_count: 0,
            offline_source_count: entries.len() as u64,
            existing_identical_count: 0,
            collision_count: 0,
            blockers: vec!["A local destination is required before this plan can export.".into()],
            warnings,
        });
    };
    let destination_root = match canonical_local_destination(destination_path) {
        Ok(path) => path,
        Err(error) => {
            return Ok(ProductionLocalPreflight {
                destination_writable: false,
                available_bytes: None,
                required_bytes,
                reserve_bytes,
                headroom_bytes: None,
                available_source_count: 0,
                offline_source_count: entries.len() as u64,
                existing_identical_count: 0,
                collision_count: 0,
                blockers: vec![format!("Destination unavailable: {error}")],
                warnings,
            });
        }
    };
    let destination_writable = fs::metadata(&destination_root)
        .map(|metadata| !metadata.permissions().readonly())
        .unwrap_or(false);
    if !destination_writable {
        blockers.push("Destination appears read-only or is not writable.".into());
    }
    let available_bytes = storage::available_bytes(&destination_root).ok();
    if let Some(available) = available_bytes {
        if available < required_bytes {
            blockers.push(format!(
                "Destination needs approximately {} including the safety reserve; only {} is available.",
                format_bytes(required_bytes),
                format_bytes(available)
            ));
        }
    } else {
        warnings.push("Available destination capacity could not be measured locally.".into());
    }
    let headroom_bytes =
        available_bytes.and_then(|available| available.checked_sub(estimated_bytes));
    let mut available_source_count = 0_u64;
    let mut offline_source_count = 0_u64;
    let mut identical = 0_u64;
    let mut collisions = 0_u64;
    let mut source_roots_seen = HashSet::<PathBuf>::new();
    for entry in entries {
        if entry.status != delivery_brain::ManifestEntryStatus::Planned {
            continue;
        }
        let source = entry
            .selected_file_instance_id
            .as_deref()
            .and_then(|instance_id| source_roots.get(instance_id))
            .zip(entry.source_relative_path.as_deref())
            .and_then(|(root, relative)| safe_source_path(root, relative).ok());
        let Some(source) = source else {
            offline_source_count += 1;
            continue;
        };
        if fs::metadata(&source).map(|metadata| metadata.len()).ok()
            != Some(entry.expected_byte_size)
        {
            offline_source_count += 1;
            continue;
        }
        available_source_count += 1;
        if let Some(root) = entry
            .selected_file_instance_id
            .as_deref()
            .and_then(|instance_id| source_roots.get(instance_id))
            .and_then(|root| Path::new(root).canonicalize().ok())
        {
            source_roots_seen.insert(root);
        }
        let target = match destination_relative_target(
            &destination_root,
            &entry.destination_relative_path,
        ) {
            Ok(target) => target,
            Err(error) => {
                blockers.push(format!(
                    "Unsafe destination name for {}: {error}",
                    entry.original_filename
                ));
                continue;
            }
        };
        if source == target {
            blockers.push(format!(
                "{} resolves to the same source and destination file path.",
                entry.original_filename
            ));
            continue;
        }
        if target.exists() {
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    collisions += 1;
                    blockers.push(format!(
                        "{} already exists at the destination but is not a safe regular file.",
                        entry.destination_relative_path
                    ));
                }
                Ok(_) => match (hash_file(&source), hash_file(&target)) {
                    (Ok(source_hash), Ok(destination_hash)) if source_hash == destination_hash => {
                        identical += 1;
                    }
                    (Ok(_), Ok(_)) => {
                        collisions += 1;
                        blockers.push(format!(
                            "{} already exists with different content; CaptureOS will not overwrite it.",
                            entry.destination_relative_path
                        ));
                    }
                    _ => {
                        collisions += 1;
                        blockers.push(format!(
                            "{} cannot be safely compared with existing destination content.",
                            entry.destination_relative_path
                        ));
                    }
                },
                Err(error) => {
                    collisions += 1;
                    blockers.push(format!(
                        "Cannot inspect existing destination {}: {error}",
                        entry.destination_relative_path
                    ));
                }
            }
        }
    }
    if offline_source_count > 0 {
        blockers.push(format!(
            "{offline_source_count} selected original{} offline or unavailable.",
            if offline_source_count == 1 {
                " is"
            } else {
                "s are"
            }
        ));
    }
    if !source_roots_seen.is_empty()
        && source_roots_seen.iter().any(|source_root| {
            destination_root == *source_root || destination_root.starts_with(source_root)
        })
    {
        blockers.push("Destination is inside a selected source root, which could change the source inventory during export.".into());
    }
    Ok(ProductionLocalPreflight {
        destination_writable,
        available_bytes,
        required_bytes,
        reserve_bytes,
        headroom_bytes,
        available_source_count,
        offline_source_count,
        existing_identical_count: identical,
        collision_count: collisions,
        blockers,
        warnings,
    })
}

fn production_preflight_from_entries(
    destination_path: &str,
    estimated_bytes: u64,
    reserve_bytes: u64,
    entries: &[persistence::ExportManifestEntryRecord],
    execution_entries: &[persistence::ExportManifestExecutionEntry],
) -> PersistenceResult<ProductionLocalPreflight> {
    let source_roots = execution_entries
        .iter()
        .filter_map(|entry| {
            entry
                .entry
                .selected_file_instance_id
                .as_ref()
                .zip(entry.source_root_path.as_ref())
                .map(|(instance_id, root)| (instance_id.clone(), root.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let drafts = entries
        .iter()
        .zip(execution_entries)
        .map(|(entry, execution)| delivery_brain::ManifestEntryDraft {
            media_asset_id: entry.media_asset_id.clone(),
            selected_file_instance_id: entry.selected_file_instance_id.clone(),
            source_relative_path: execution.source_relative_path.clone(),
            original_filename: entry.original_filename.clone(),
            destination_relative_path: entry.destination_relative_path.clone(),
            destination_filename: entry.destination_filename.clone(),
            expected_byte_size: entry.expected_byte_size,
            source_checksum: entry.source_checksum.clone(),
            human_decision: entry.human_decision.clone(),
            rating: entry.rating,
            starred: entry.starred,
            moment_id: entry.moment_id.clone(),
            moment_label: entry.moment_label.clone(),
            status: match entry.status.as_str() {
                "planned" => delivery_brain::ManifestEntryStatus::Planned,
                "blocked_source_unavailable" => {
                    delivery_brain::ManifestEntryStatus::BlockedSourceUnavailable
                }
                _ => delivery_brain::ManifestEntryStatus::BlockedInternalCollision,
            },
            issue: entry.issue.clone(),
        })
        .collect::<Vec<_>>();
    preflight_draft_entries(
        Some(destination_path),
        estimated_bytes,
        reserve_bytes,
        &drafts,
        &source_roots,
    )
}

fn canonical_local_destination(value: &str) -> PersistenceResult<PathBuf> {
    let path = Path::new(value).canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("destination is unavailable: {error}"))
    })?;
    if !path.is_dir() {
        return Err(PersistenceError::InvalidData(
            "destination must be an existing local folder".into(),
        ));
    }
    Ok(path)
}

fn safe_source_path(root: &str, relative: &str) -> PersistenceResult<PathBuf> {
    let root = Path::new(root).canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("source root is unavailable: {error}"))
    })?;
    if !root.is_dir() {
        return Err(PersistenceError::InvalidData(
            "source root is not a folder".into(),
        ));
    }
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PersistenceError::InvalidData(
            "source relative path is unsafe".into(),
        ));
    }
    let path = root.join(relative_path);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        PersistenceError::InvalidData(format!("source is unavailable: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PersistenceError::InvalidData(
            "source is not a safe regular local file".into(),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("source is unavailable: {error}"))
    })?;
    if !canonical.starts_with(&root) {
        return Err(PersistenceError::InvalidData(
            "source path escaped its approved root".into(),
        ));
    }
    Ok(canonical)
}

fn safe_execution_source(
    execution: &persistence::ExportManifestExecutionEntry,
) -> PersistenceResult<PathBuf> {
    if !execution.source_available {
        return Err(PersistenceError::InvalidData(
            "selected source FileInstance is offline".into(),
        ));
    }
    let root = execution.source_root_path.as_deref().ok_or_else(|| {
        PersistenceError::InvalidData("selected FileInstance has no approved index root".into())
    })?;
    let relative = execution.source_relative_path.as_deref().ok_or_else(|| {
        PersistenceError::InvalidData(
            "selected FileInstance has no safe relative source path".into(),
        )
    })?;
    safe_source_path(root, relative)
}

fn destination_relative_target(root: &Path, relative: &str) -> PersistenceResult<PathBuf> {
    if !delivery_brain::safe_destination_relative_path(relative) {
        return Err(PersistenceError::InvalidData(
            "destination relative path is unsafe".into(),
        ));
    }
    let mut target = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(PersistenceError::InvalidData(
                "destination relative path contains traversal".into(),
            ));
        };
        target.push(component);
    }
    Ok(target)
}

fn record_export_entry_failure(
    repository: &impl CatalogRepository,
    export_job: &mut ExportJobRecord,
    durable_job: &mut BackgroundJob,
    entry_id: &str,
    message: &str,
) -> PersistenceResult<()> {
    repository.update_export_job_entry(
        &export_job.id,
        &ExportJobEntryUpdate {
            manifest_entry_id: entry_id.into(),
            state: "failed".into(),
            copied_bytes: 0,
            source_checksum: None,
            destination_checksum: None,
            error_message: Some(message.into()),
        },
    )?;
    export_job.items_completed += 1;
    export_job.failed_count += 1;
    export_job.updated_at = Utc::now().to_rfc3339();
    durable_job.items_completed = export_job.items_completed;
    durable_job.files_processed = export_job.items_completed;
    durable_job.error_count = export_job.failed_count;
    durable_job.updated_at = Utc::now();
    repository.update_export_job(export_job, durable_job)
}

fn production_export_progress(
    export_job: &ExportJobRecord,
    stage: &str,
    current_filename: Option<String>,
    message: Option<String>,
) -> ProductionExportProgress {
    ProductionExportProgress {
        export_job_id: export_job.id.clone(),
        manifest_id: export_job.manifest_id.clone(),
        state: export_job.state.clone(),
        stage: stage.into(),
        items_completed: export_job.items_completed,
        items_total: export_job.items_total,
        verified_count: export_job.verified_count,
        skipped_identical_count: export_job.skipped_identical_count,
        failed_count: export_job.failed_count,
        verified_bytes: export_job.verified_bytes,
        current_filename,
        message,
    }
}

fn delivery_report_payload(
    manifest: &ExportManifestRecord,
    export_job: &ExportJobRecord,
    plan_name: &str,
    delivery_reference: &str,
) -> PersistenceResult<(serde_json::Value, String)> {
    let report = serde_json::json!({
        "formatVersion": 1,
        "deliveryReference": delivery_reference,
        "planName": plan_name,
        "manifestChecksum": manifest.checksum,
        "completedAt": export_job.finished_at,
        "filesVerified": export_job.verified_count,
        "filesAlreadyPresent": export_job.skipped_identical_count,
        "failedCount": export_job.failed_count,
        "verifiedBytes": export_job.verified_bytes,
        "status": export_job.state,
        "destinationType": "local_folder",
        "verificationPolicy": "streaming blake3 source-to-destination equivalence",
        "privacy": "No notes, AI scores, Studio Brain predictions, embeddings, source paths, or internal asset identifiers are included.",
    });
    let text = format!(
        "CaptureOS Delivery Report\n\nPlan: {plan_name}\n\nFiles: {} verified{}\nTotal: {}\nStatus: {}\nVerification: streaming BLAKE3 source-to-destination equivalence\n",
        export_job.verified_count,
        if export_job.failed_count > 0 { format!(", {} failed", export_job.failed_count) } else { String::new() },
        format_bytes(export_job.verified_bytes),
        export_job.state,
    );
    Ok((report, text))
}

fn write_local_delivery_report(
    destination_root: &Path,
    delivery_reference: &str,
    report: &serde_json::Value,
    text: &str,
) -> PersistenceResult<()> {
    let reports = safe_delivery_reports_directory(destination_root)?;
    let json_path = reports.join(format!(
        "CaptureOS_Delivery_Report_{delivery_reference}.json"
    ));
    let text_path = reports.join(format!(
        "CaptureOS_Delivery_Report_{delivery_reference}.txt"
    ));
    let json_text = serde_json::to_string_pretty(&report)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let mut json_file = production_io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(json_path),
    )?;
    production_io(json_file.write_all(json_text.as_bytes()))?;
    production_io(json_file.sync_all())?;
    let mut text_file = production_io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(text_path),
    )?;
    production_io(text_file.write_all(text.as_bytes()))?;
    production_io(text_file.sync_all())?;
    Ok(())
}

fn editor_handoff_manifest_payload(
    manifest: &ExportManifestRecord,
    export_job: &ExportJobRecord,
    plan_name: &str,
    entries: &[persistence::ExportManifestEntryRecord],
    delivery_reference: &str,
) -> PersistenceResult<serde_json::Value> {
    Ok(serde_json::json!({
        "formatVersion": 1,
        "kind": "captureos_editor_handoff",
        "planName": plan_name,
        "deliveryReference": delivery_reference,
        "manifestChecksum": manifest.checksum,
        "status": export_job.state,
        "files": entries.iter().map(|entry| serde_json::json!({
            "originalFilename": entry.original_filename,
            "destinationRelativePath": entry.destination_relative_path,
            "destinationFilename": entry.destination_filename,
            "humanDecision": entry.human_decision,
            "rating": entry.rating,
            "starred": entry.starred,
            "momentLabel": entry.moment_label,
            "status": entry.status,
        })).collect::<Vec<_>>(),
        "privacy": "No notes, source paths, internal asset identifiers, AI scores, Studio Brain predictions, embeddings, or model data are included.",
    }))
}

fn write_local_editor_handoff_manifest(
    destination_root: &Path,
    delivery_reference: &str,
    handoff: &serde_json::Value,
) -> PersistenceResult<()> {
    let reports = safe_delivery_reports_directory(destination_root)?;
    let path = reports.join(format!(
        "CaptureOS_Editor_Handoff_Manifest_{delivery_reference}.json"
    ));
    let text = serde_json::to_string_pretty(handoff)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let mut file = production_io(OpenOptions::new().write(true).create_new(true).open(path))?;
    production_io(file.write_all(text.as_bytes()))?;
    production_io(file.sync_all())?;
    Ok(())
}

/// A client-visible delivery reference deliberately excludes internal database identifiers. It
/// combines the immutable manifest checksum with an already-visible completion timestamp and is
/// safe to include in destination filenames.
fn delivery_reference(manifest: &ExportManifestRecord, export_job: &ExportJobRecord) -> String {
    let completed = export_job
        .finished_at
        .as_deref()
        .unwrap_or(export_job.updated_at.as_str())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let checksum = manifest.checksum.get(..12).unwrap_or("manifest");
    format!("delivery-{completed}-{checksum}")
}

fn safe_delivery_reports_directory(destination_root: &Path) -> PersistenceResult<PathBuf> {
    let reports = destination_root.join("CaptureOS_Delivery_Reports");
    if reports.exists() {
        let metadata = production_io(fs::symlink_metadata(&reports))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PersistenceError::InvalidData(
                "delivery report folder is not a safe directory".into(),
            ));
        }
    } else {
        production_io(fs::create_dir(&reports))?;
    }
    Ok(reports)
}

fn format_bytes(value: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if value >= GIB {
        format!("{:.1} GB", value as f64 / GIB as f64)
    } else if value >= MIB {
        format!("{:.1} MB", value as f64 / MIB as f64)
    } else {
        format!("{value} B")
    }
}

fn production_io<T>(result: std::io::Result<T>) -> PersistenceResult<T> {
    result.map_err(|error| {
        PersistenceError::InvalidData(format!(
            "local production filesystem operation failed: {error}"
        ))
    })
}

/// Runs one explicit local retrain. The desktop calls this from a separate SQLite connection and
/// background worker; this function never opens original media or calls a network service.
pub fn train_studio_brain(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    mut on_progress: impl FnMut(&StudioBrainProgress),
) -> PersistenceResult<StudioBrainProgress> {
    let initial_status = repository.studio_brain_project_status(project_id)?;
    let profile_id = initial_status.profile_id.clone();
    let started_at = Utc::now();
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::StudioTraining,
        items_completed: 0,
        items_total: None,
        files_discovered: 0,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({
            "pipeline": "studio-training",
            "profile_id": &profile_id,
            "mode": "explicit_local_retrain",
        })),
        created_at: started_at,
        updated_at: started_at,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    on_progress(&studio_progress(
        &profile_id,
        "training",
        true,
        "materialize_history",
        0,
        0,
        0,
        initial_status.active_model_version.clone(),
        Some("Preparing an explicit local snapshot of eligible human decisions.".into()),
        None,
    ));

    // Historical materialization is deliberately tied to this explicit action rather than status
    // loading. Its idempotent source IDs prevent a model retry from duplicating human evidence.
    if let Err(error) = repository.materialize_historical_studio_training_examples(&profile_id) {
        return finish_pre_run_studio_training(
            repository,
            &mut job,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    let (examples, pairwise_records, source_revision) =
        match studio_training_source_snapshot(repository, &profile_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return finish_pre_run_studio_training(
                    repository,
                    &mut job,
                    &profile_id,
                    initial_status.active_model_version,
                    error.to_string(),
                    &mut on_progress,
                );
            }
        };
    let observations = examples
        .iter()
        .filter_map(studio_training_observation)
        .collect::<Vec<_>>();
    // Relative representative evidence is a separate human signal. It is never synthesized from
    // generic technical rankings and never changes Similar Set membership or culling labels.
    let pairwise_preferences = pairwise_records
        .iter()
        .filter_map(studio_pairwise_preference)
        .collect::<Vec<_>>();
    let config = StudioTrainingConfig::default();
    let snapshot_hash = match studio_training_snapshot_hash(&observations, &pairwise_preferences) {
        Ok(hash) => hash,
        Err(error) => {
            return finish_pre_run_studio_training(
                repository,
                &mut job,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    job.items_total = Some(examples.len() as u64);
    job.files_discovered = examples.len() as u64;
    job.updated_at = Utc::now();
    if let Err(error) = repository.update_background_job(&job) {
        return finish_pre_run_studio_training(
            repository,
            &mut job,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    // Keep a verified prior artifact in memory only long enough to compare it on this exact
    // frozen holdout. It is never retrained, mutated, or exposed as source data.
    let previous_active_model = match repository.active_studio_model(&profile_id) {
        Ok(model) => model,
        Err(error) => {
            return finish_pre_run_studio_training(
                repository,
                &mut job,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let parameters_json = match serde_json::to_value(&config) {
        Ok(value) => value,
        Err(error) => {
            return finish_pre_run_studio_training(
                repository,
                &mut job,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let mut run = StudioTrainingRunRecord {
        id: Uuid::new_v4().to_string(),
        profile_id: profile_id.clone(),
        background_job_id: job.id.to_string(),
        algorithm: "regularized_linear_softmax".into(),
        algorithm_version: STUDIO_BRAIN_ALGORITHM_VERSION.into(),
        feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.into(),
        parameters_json,
        snapshot_hash,
        snapshot_count: examples.len() as u64,
        previous_active_model_id: previous_active_model.as_ref().map(|model| model.id.clone()),
        state: "training".into(),
        error_message: None,
        created_at: started_at.to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
        finished_at: None,
    };
    if let Err(error) = repository.create_studio_training_run(&run) {
        return finish_pre_run_studio_training(
            repository,
            &mut job,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    let previous_active_artifact = match previous_active_model.as_ref() {
        Some(model) => match serde_json::to_string(&model.artifact_json)
            .map_err(PersistenceError::from)
            .and_then(|artifact_json| {
                decode_verified_model_artifact(&VerifiedModelArtifact {
                    artifact_json,
                    checksum: model.checksum.clone(),
                })
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))
            }) {
            Ok(artifact) => Some(artifact),
            Err(error) => {
                return finish_failed_studio_training(
                    repository,
                    &mut job,
                    &mut run,
                    &profile_id,
                    initial_status.active_model_version,
                    error.to_string(),
                    &mut on_progress,
                );
            }
        },
        None => None,
    };
    for (index, example) in examples.iter().enumerate() {
        let split = studio_snapshot_split(&examples, index, &config);
        let label = studio_decision_label(example).map(studio_decision_value_name);
        if let Err(error) = repository.store_studio_training_snapshot(
            &run.id,
            &example.id,
            split,
            &example.feature_snapshot_json,
            label,
        ) {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    }
    on_progress(&studio_progress(
        &profile_id,
        "training",
        true,
        "fit_local_model",
        observations.len() as u64,
        examples.len() as u64,
        0,
        initial_status.active_model_version.clone(),
        Some("Training a compact local model from explicit human decisions only.".into()),
        None,
    ));

    let result = match train_studio_model(&observations, &pairwise_preferences, &config) {
        Ok(result) => result,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let readiness_json = match serde_json::to_value(&result.readiness) {
        Ok(value) => value,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let readiness_state = studio_training_status_name(result.readiness.status);
    job.items_completed = examples.len() as u64;
    job.files_processed = examples.len() as u64;
    job.stage = JobStage::StudioEvaluation;
    job.updated_at = Utc::now();
    if let Err(error) = repository.update_background_job(&job) {
        return finish_failed_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    on_progress(&studio_progress(
        &profile_id,
        "evaluating",
        true,
        "leakage_aware_evaluation",
        examples.len() as u64,
        examples.len() as u64,
        0,
        initial_status.active_model_version.clone(),
        Some("Checking grouped held-out evidence and calibration before any candidate can become active.".into()),
        None,
    ));

    let Some(artifact) = result
        .artifact
        .filter(|_| result.readiness.status == StudioTrainingStatus::Ready)
    else {
        run.state = "not_activated".into();
        run.updated_at = Utc::now().to_rfc3339();
        run.finished_at = Some(run.updated_at.clone());
        if let Err(error) = repository.update_studio_training_run(&run) {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
        if let Err(error) = repository.update_studio_profile_training_state(
            &profile_id,
            readiness_state,
            &readiness_json,
            None,
        ) {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
        job.state = WorkflowRunState::Completed;
        job.stage = JobStage::Finalize;
        job.updated_at = Utc::now();
        job.finished_at = Some(job.updated_at);
        job.resume_metadata = Some(serde_json::json!({
            "pipeline": "studio-training", "profile_id": &profile_id,
            "summary": "No candidate was activated because local readiness conditions were not met.",
            "readiness": &readiness_json,
        }));
        if let Err(error) = repository.update_background_job(&job) {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
        let progress = studio_progress(
            &profile_id,
            readiness_state,
            false,
            "complete",
            examples.len() as u64,
            examples.len() as u64,
            0,
            initial_status.active_model_version,
            Some(result.readiness.message),
            None,
        );
        on_progress(&progress);
        return Ok(progress);
    };

    // A ready candidate still cannot displace a retained model merely because it passed an
    // absolute threshold. Compare both artifacts on the exact same grouped current snapshot.
    // If comparison is unavailable, stay conservative: the candidate remains unactivated.
    if let Some(previous_artifact) = previous_active_artifact {
        let candidate_evaluation =
            match evaluate_studio_model_on_observations(&artifact, &observations, &config) {
                Ok(evaluation) => evaluation,
                Err(error) => {
                    return finish_failed_studio_training(
                        repository,
                        &mut job,
                        &mut run,
                        &profile_id,
                        initial_status.active_model_version,
                        error.to_string(),
                        &mut on_progress,
                    );
                }
            };
        let previous_evaluation =
            match evaluate_studio_model_on_observations(&previous_artifact, &observations, &config)
            {
                Ok(evaluation) => evaluation,
                Err(error) => {
                    return finish_failed_studio_training(
                        repository,
                        &mut job,
                        &mut run,
                        &profile_id,
                        initial_status.active_model_version,
                        error.to_string(),
                        &mut on_progress,
                    );
                }
            };
        if let Some(reason) = studio_candidate_rejection_reason(
            &candidate_evaluation,
            &previous_evaluation,
            config.minimum_validation_examples,
        ) {
            return finish_rejected_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                &reason,
                &mut on_progress,
            );
        }
    }

    // Safe structured JSON and checksum validation happen before persistence and again before
    // activation. A malformed/corrupt candidate cannot replace a known-good active model.
    let encoded = match encode_verified_model_artifact(&artifact) {
        Ok(encoded) => encoded,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            )
        }
    };
    if let Err(error) = decode_verified_model_artifact(&encoded) {
        return finish_failed_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    run.state = "persisting".into();
    run.updated_at = Utc::now().to_rfc3339();
    if let Err(error) = repository.update_studio_training_run(&run) {
        return finish_failed_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    let model_id = Uuid::new_v4().to_string();
    let artifact_json = match serde_json::from_str::<serde_json::Value>(&encoded.artifact_json) {
        Ok(value) => value,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            )
        }
    };
    let metrics = serde_json::json!({
        "readiness": &result.readiness,
        "evaluation": &result.evaluation,
        "pairwise": &result.pairwise_summary,
        "diagnostics": &result.diagnostics,
        "calibration": artifact.calibration,
    });
    let model = StudioModelRecord {
        id: model_id.clone(),
        profile_id: profile_id.clone(),
        training_run_id: run.id.clone(),
        algorithm: "regularized_linear_softmax".into(),
        model_version: format!("studio-brain-v1-{}", &run.id[..8]),
        feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.into(),
        artifact_json,
        checksum: encoded.checksum,
        artifact_size_bytes: encoded.artifact_json.len() as u64,
        state: "candidate".into(),
        metrics_json: metrics,
        created_at: Utc::now().to_rfc3339(),
        activated_at: None,
    };
    if let Err(error) = repository.store_studio_model(&model) {
        return finish_failed_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    // Candidate recommendations are computed and persisted while the prior model remains active.
    // Cached candidate rows are invisible until the final atomic activation transaction succeeds.
    let recommendations = match studio_recommendations_for_project(
        repository,
        project_id,
        &profile_id,
        &model,
        &artifact,
        &config,
    ) {
        Ok(recommendations) => recommendations,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            )
        }
    };
    if let Err(error) = repository.replace_studio_recommendations(
        &profile_id,
        &model_id,
        project_id,
        &recommendations,
    ) {
        return finish_failed_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            error.to_string(),
            &mut on_progress,
        );
    }
    // Review continues while a compact candidate trains. If a new eligible explicit decision
    // arrived, the candidate's snapshot is no longer the one the user asked to evaluate, so it
    // stays inactive and the next explicit update starts from a fresh immutable snapshot.
    let current_examples = match repository.studio_training_examples(&profile_id) {
        Ok(examples) => examples,
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let current_observations = current_examples
        .iter()
        .filter_map(studio_training_observation)
        .collect::<Vec<_>>();
    let current_pairwise_preferences = match repository.studio_pairwise_preferences(&profile_id) {
        Ok(preferences) => preferences
            .iter()
            .filter_map(studio_pairwise_preference)
            .collect::<Vec<_>>(),
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    };
    let current_snapshot_hash =
        match studio_training_snapshot_hash(&current_observations, &current_pairwise_preferences) {
            Ok(hash) => hash,
            Err(error) => {
                return finish_failed_studio_training(
                    repository,
                    &mut job,
                    &mut run,
                    &profile_id,
                    initial_status.active_model_version,
                    error.to_string(),
                    &mut on_progress,
                );
            }
        };
    if current_snapshot_hash != run.snapshot_hash {
        return finish_stale_studio_training(
            repository,
            &mut job,
            &mut run,
            &profile_id,
            initial_status.active_model_version,
            &mut on_progress,
        );
    }
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::Finalize;
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "studio-training", "profile_id": &profile_id, "model_id": &model_id,
        "readiness": &readiness_json,
    }));
    // The model, run, profile, recommendation visibility, audit events, and terminal job state
    // change together. Once this succeeds there is no later fallible write that could falsely
    // tell the UI that the retained model is still active.
    match repository.activate_studio_model(&profile_id, &model_id, source_revision, &job) {
        Ok(StudioModelActivationOutcome::Activated) => {}
        Ok(StudioModelActivationOutcome::SourceSnapshotStale) => {
            return finish_stale_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                &mut on_progress,
            );
        }
        Err(error) => {
            return finish_failed_studio_training(
                repository,
                &mut job,
                &mut run,
                &profile_id,
                initial_status.active_model_version,
                error.to_string(),
                &mut on_progress,
            );
        }
    }
    let progress = studio_progress(
        &profile_id,
        "ready",
        false,
        "complete",
        examples.len() as u64,
        examples.len() as u64,
        0,
        Some(model.model_version),
        Some("A checked local candidate was activated. Recommendations remain advisory and never change your decisions.".into()),
        None,
    );
    on_progress(&progress);
    Ok(progress)
}

pub fn recover_interrupted_studio_training(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_studio_training()
}

// This constructs the public progress contract explicitly so every terminal and intermediate
// state carries the same complete, non-inferred evidence to the desktop boundary.
#[allow(clippy::too_many_arguments)]
fn studio_progress(
    profile_id: &str,
    state: &str,
    active: bool,
    stage: &str,
    completed: u64,
    total: u64,
    error_count: u64,
    active_model_version: Option<String>,
    message: Option<String>,
    last_error: Option<String>,
) -> StudioBrainProgress {
    StudioBrainProgress {
        profile_id: profile_id.into(),
        state: state.into(),
        active,
        stage: stage.into(),
        completed,
        total,
        error_count,
        active_model_version,
        message,
        last_error,
    }
}

fn studio_training_snapshot_hash(
    observations: &[TrainingObservation],
    pairwise_preferences: &[PairwisePreference],
) -> PersistenceResult<String> {
    Ok(blake3::hash(&serde_json::to_vec(&serde_json::json!({
        "observations": observations,
        "pairwisePreferences": pairwise_preferences,
    }))?)
    .to_hex()
    .to_string())
}

/// Reads a bounded training snapshot only when the explicit-source generation is stable. The
/// later activation transaction rechecks the same revision, closing both the normal read race
/// and the final check-to-activation race without blocking human review for training.
fn studio_training_source_snapshot(
    repository: &impl CatalogRepository,
    profile_id: &str,
) -> PersistenceResult<(
    Vec<StudioTrainingExampleRecord>,
    Vec<StudioPairwisePreferenceRecord>,
    u64,
)> {
    for _ in 0..2 {
        let before = repository.studio_training_source_state(profile_id)?;
        if before.materialization_pending {
            continue;
        }
        let examples = repository.studio_training_examples(profile_id)?;
        let preferences = repository.studio_pairwise_preferences(profile_id)?;
        let after = repository.studio_training_source_state(profile_id)?;
        if !after.materialization_pending && before.revision == after.revision {
            return Ok((examples, preferences, after.revision));
        }
    }
    Err(PersistenceError::InvalidData(
        "Explicit Studio Brain sources are still being recorded locally; run the update again when review activity settles."
            .into(),
    ))
}

fn finish_stale_studio_training(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    run: &mut StudioTrainingRunRecord,
    profile_id: &str,
    active_model_version: Option<String>,
    on_progress: &mut impl FnMut(&StudioBrainProgress),
) -> PersistenceResult<StudioBrainProgress> {
    let has_previous_model = active_model_version.is_some();
    run.state = "not_activated".into();
    run.error_message = Some(
        "New eligible human decisions were recorded while this Studio Brain candidate trained."
            .into(),
    );
    run.updated_at = Utc::now().to_rfc3339();
    run.finished_at = Some(run.updated_at.clone());
    repository.update_studio_training_run(run)?;
    let status = if has_previous_model {
        "stale"
    } else {
        "learning"
    };
    repository.update_studio_profile_training_state(
        profile_id,
        status,
        &serde_json::json!({
            "state": status,
            "reasons": [
                "New explicit decisions arrived during training, so the candidate was not activated.",
                "Run an explicit Studio Brain update to evaluate a fresh local snapshot."
            ]
        }),
        None,
    )?;
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::Finalize;
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "studio-training",
        "profile_id": profile_id,
        "summary": "Candidate not activated because new explicit local decisions arrived.",
    }));
    repository.update_background_job(job)?;
    let progress = studio_progress(
        profile_id,
        status,
        false,
        "complete",
        job.items_completed,
        job.items_total.unwrap_or(0),
        0,
        active_model_version,
        Some(
            "New explicit decisions arrived while Studio Brain was updating. The candidate was not activated; update again when ready."
                .into(),
        ),
        None,
    );
    on_progress(&progress);
    Ok(progress)
}

fn studio_training_status_name(status: StudioTrainingStatus) -> &'static str {
    match status {
        StudioTrainingStatus::NotReady => "not_ready",
        StudioTrainingStatus::Learning => "learning",
        StudioTrainingStatus::Ready => "ready",
        StudioTrainingStatus::Stale => "stale",
        StudioTrainingStatus::Error => "error",
    }
}

fn studio_decision_label(example: &StudioTrainingExampleRecord) -> Option<StudioHumanDecision> {
    if example.decision_type != "culling_decision" || !example.training_eligible {
        return None;
    }
    match example.decision_value.as_deref() {
        Some("keep") => Some(StudioHumanDecision::Keep),
        Some("review") => Some(StudioHumanDecision::Review),
        Some("reject") => Some(StudioHumanDecision::Reject),
        _ => None,
    }
}

fn studio_decision_value_name(value: StudioHumanDecision) -> &'static str {
    match value {
        StudioHumanDecision::Keep => "keep",
        StudioHumanDecision::Review => "review",
        StudioHumanDecision::Reject => "reject",
    }
}

fn studio_training_observation(
    example: &StudioTrainingExampleRecord,
) -> Option<TrainingObservation> {
    if example.feature_schema_version != STUDIO_BRAIN_FEATURE_SCHEMA_VERSION
        || example
            .feature_snapshot_json
            .get("schemaVersion")
            .and_then(serde_json::Value::as_str)
            != Some(STUDIO_BRAIN_FEATURE_SCHEMA_VERSION)
    {
        // Persisted snapshots are immutable provenance. A future feature change must perform an
        // explicit migration/backfill rather than silently parsing an old shape as new inputs.
        return None;
    }
    let decision = studio_decision_label(example)?;
    let asset_id = example.media_asset_id.clone()?;
    let occurred_at_unix_ms = DateTime::parse_from_rfc3339(&example.occurred_at)
        .map(|value| value.timestamp_millis())
        .unwrap_or(0);
    Some(TrainingObservation {
        observation_id: example.id.clone(),
        project_id: example.project_id.clone(),
        asset_id,
        decision,
        action_kind: if example.provenance == "historical_backfill" {
            ExplicitHumanActionKind::BackfilledExplicitDecision
        } else {
            ExplicitHumanActionKind::CullingDecision
        },
        occurred_at_unix_ms,
        // Capture time is used only for leakage-aware fallback grouping. It is not a preference
        // feature and missing/invalid timestamps remain unavailable rather than invented.
        captured_at_unix_ms: example
            .feature_snapshot_json
            .get("capturedAt")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis()),
        review_session_id: example.review_session_id.clone(),
        similarity_set_id: example.similarity_group_id.clone(),
        moment_id: example.moment_id.clone(),
        // The legacy UI did not record presentation state; unknown is intentionally not promoted
        // to `shown`. The flag is provenance only and never enters feature assembly.
        recommendation_was_shown: example.recommendation_shown == "shown",
        generic_recommendation_at_decision: studio_generic_recommendation(
            &example.generic_recommendation_json,
        ),
        auxiliary_human_signals: AuxiliaryHumanSignals::default(),
        features: studio_feature_input(&example.feature_snapshot_json),
        training_eligible: example.training_eligible,
    })
}

fn studio_pairwise_preference(
    preference: &StudioPairwisePreferenceRecord,
) -> Option<PairwisePreference> {
    let has_current_schema = |snapshot: &serde_json::Value| {
        snapshot
            .get("schemaVersion")
            .and_then(serde_json::Value::as_str)
            == Some(STUDIO_BRAIN_FEATURE_SCHEMA_VERSION)
    };
    if !has_current_schema(&preference.chosen_feature_snapshot_json)
        || !has_current_schema(&preference.alternative_feature_snapshot_json)
    {
        return None;
    }
    let occurred_at_unix_ms = DateTime::parse_from_rfc3339(&preference.occurred_at)
        .map(|value| value.timestamp_millis())
        .unwrap_or(0);
    if preference.chosen_asset_id == preference.alternative_asset_id {
        return None;
    }
    Some(PairwisePreference {
        preference_id: preference.id.clone(),
        project_id: preference.project_id.clone(),
        similarity_set_id: preference.similarity_group_id.clone(),
        chosen_asset_id: preference.chosen_asset_id.clone(),
        alternative_asset_id: preference.alternative_asset_id.clone(),
        occurred_at_unix_ms,
        chosen_features: studio_feature_input(&preference.chosen_feature_snapshot_json),
        alternative_features: studio_feature_input(&preference.alternative_feature_snapshot_json),
        training_eligible: preference.training_eligible,
    })
}

fn studio_generic_recommendation(value: &serde_json::Value) -> Option<GenericRecommendation> {
    match value
        .get("label")
        .or_else(|| value.get("genericRecommendation"))
        .and_then(serde_json::Value::as_str)
    {
        Some("strong_candidate") | Some("strong_alternative") | Some("keep") => {
            Some(GenericRecommendation::Keep)
        }
        Some("review") => Some(GenericRecommendation::Review),
        Some("technical_issue") | Some("probable_duplicate") | Some("reject") => {
            Some(GenericRecommendation::Reject)
        }
        _ => None,
    }
}

fn studio_feature_input(value: &serde_json::Value) -> StudioFeatureInput {
    let technical_score = studio_score(value.get("technicalQualityScore"));
    let sharpness_score = studio_score(value.get("globalSharpness"));
    let blur_score = studio_score(value.get("directionalBlurRatio"));
    let similar_set_size = value
        .get("similarityGroupSize")
        .and_then(serde_json::Value::as_u64)
        .map(|count| count.min(u32::MAX as u64) as u32);
    let higher_peers = value
        .get("higherTechnicalPeers")
        .and_then(serde_json::Value::as_u64);
    let relative_technical_rank = similar_set_size.zip(higher_peers).map(|(size, higher)| {
        if size <= 1 {
            1.0
        } else {
            1.0 - (higher as f32 / (size.saturating_sub(1) as f32)).clamp(0.0, 1.0)
        }
    });
    let moment_size = value
        .get("momentSize")
        .and_then(serde_json::Value::as_u64)
        .map(|count| count.min(u32::MAX as u64) as u32);
    let moment_position = moment_size
        .zip(
            value
                .get("momentOrdinal")
                .and_then(serde_json::Value::as_i64),
        )
        .map(|(size, ordinal)| {
            if size <= 1 {
                0.0
            } else {
                (ordinal.max(0) as f32 / (size.saturating_sub(1) as f32)).clamp(0.0, 1.0)
            }
        });
    StudioFeatureInput {
        technical_score,
        sharpness_score,
        blur_score,
        exposure_score: None,
        anonymous_face_count: value
            .get("anonymousFaceCount")
            .and_then(serde_json::Value::as_u64)
            .map(|count| count.min(u32::MAX as u64) as u32),
        open_eyes_count: value
            .get("openEyesCount")
            .and_then(serde_json::Value::as_u64)
            .map(|count| count.min(u32::MAX as u64) as u32),
        similar_set_size,
        relative_technical_rank,
        relative_sharpness_rank: None,
        moment_size,
        moment_position,
        timeline_boundary_score: None,
        generic_recommendation: studio_generic_recommendation(value),
        is_generic_representative: value
            .get("isGenericRepresentative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        is_human_representative: value
            .get("isHumanRepresentative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        semantic_evidence_available: value
            .get("semanticEvidenceAvailable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

fn studio_score(value: Option<&serde_json::Value>) -> Option<f32> {
    let value = value.and_then(serde_json::Value::as_f64)?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let normalized = if value > 1.0 { value / 100.0 } else { value };
    (normalized <= 1.0).then_some(normalized as f32)
}

fn studio_snapshot_split(
    examples: &[StudioTrainingExampleRecord],
    index: usize,
    config: &StudioTrainingConfig,
) -> &'static str {
    let example = &examples[index];
    if studio_decision_label(example).is_none() {
        return "excluded";
    }
    let eligible = examples
        .iter()
        .filter(|value| studio_decision_label(value).is_some())
        .collect::<Vec<_>>();
    let distinct_projects = eligible
        .iter()
        .map(|value| value.project_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let key = studio_snapshot_group_key(example, distinct_projects);
    let mut keys = eligible
        .iter()
        .map(|value| studio_snapshot_group_key(value, distinct_projects))
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    if keys.len() < 2 {
        return "train";
    }
    keys.sort_by_key(|value| {
        let digest = blake3::hash(value.as_bytes());
        u64::from_le_bytes(
            digest.as_bytes()[0..8]
                .try_into()
                .expect("blake3 has eight bytes"),
        )
    });
    let validation_groups =
        ((keys.len() as f32 * config.holdout_fraction).round() as usize).clamp(1, keys.len() - 1);
    if keys
        .iter()
        .take(validation_groups)
        .any(|candidate| candidate == &key)
    {
        "validation"
    } else {
        "train"
    }
}

/// Keep the persisted run snapshot split identical to the pure model's leakage-aware splitter.
/// In a single-project history a whole Similar Set, Moment, or capture-day stays together; a
/// timestamp-less asset remains isolated rather than being grouped optimistically.
fn studio_snapshot_group_key(
    example: &StudioTrainingExampleRecord,
    distinct_projects: usize,
) -> String {
    if distinct_projects >= 2 {
        return format!("project:{}", example.project_id);
    }
    if let Some(group) = &example.similarity_group_id {
        return format!("similar:{group}");
    }
    if let Some(moment) = &example.moment_id {
        return format!("moment:{moment}");
    }
    if let Some(day) = example
        .feature_snapshot_json
        .get("capturedAt")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis().div_euclid(86_400_000))
    {
        return format!("day:{day}");
    }
    format!(
        "asset:{}",
        example.media_asset_id.as_deref().unwrap_or(&example.id)
    )
}

fn studio_recommendations_for_project(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    profile_id: &str,
    model: &StudioModelRecord,
    artifact: &studio_brain::StudioModelArtifact,
    config: &StudioTrainingConfig,
) -> PersistenceResult<Vec<StudioRecommendationRecord>> {
    let candidates = repository.studio_feature_candidates(project_id)?;
    // The pairwise ranker is an entirely separate advisory channel. Its output is stored only as
    // bounded explanation metadata beside an already-separate Studio recommendation; it never
    // rewrites a Similar Set's technical or human representative.
    let mut pairwise_by_asset = HashMap::<String, (usize, f32, Vec<String>)>::new();
    if artifact.pairwise_ranker.is_some() {
        let mut by_group = BTreeMap::<String, Vec<&persistence::StudioFeatureCandidate>>::new();
        for candidate in &candidates {
            if let Some(group_id) = &candidate.similarity_group_id {
                by_group
                    .entry(group_id.clone())
                    .or_default()
                    .push(candidate);
            }
        }
        for group in by_group.into_values().filter(|group| group.len() > 1) {
            let ranked = rank_similar_set(
                artifact,
                &group
                    .iter()
                    .map(|candidate| SimilarSetCandidate {
                        asset_id: candidate.media_asset_id.clone(),
                        features: studio_feature_input(&candidate.feature_snapshot_json),
                    })
                    .collect::<Vec<_>>(),
                config.maximum_explanations,
            )
            .map_err(|error| {
                PersistenceError::InvalidData(format!(
                    "local Studio Similar Set ranking could not be evaluated: {error}"
                ))
            })?;
            for (index, candidate) in ranked.into_iter().enumerate() {
                pairwise_by_asset.insert(
                    candidate.asset_id,
                    (
                        index + 1,
                        candidate.relative_rank_score,
                        candidate
                            .explanations
                            .iter()
                            .map(studio_pairwise_explanation_factor)
                            .collect(),
                    ),
                );
            }
        }
    }
    let mut records = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let input = studio_feature_input(&candidate.feature_snapshot_json);
        let prediction = predict(artifact, &input, config).map_err(|error| {
            PersistenceError::InvalidData(format!(
                "local Studio prediction could not be evaluated: {error}"
            ))
        })?;
        let (recommendation, confidence_band) = match prediction.recommendation {
            Some(StudioHumanDecision::Keep) => (
                "likely_keep",
                studio_confidence_band_name(prediction.confidence_band),
            ),
            Some(StudioHumanDecision::Review) => (
                "likely_review",
                studio_confidence_band_name(prediction.confidence_band),
            ),
            Some(StudioHumanDecision::Reject) => (
                "likely_reject",
                studio_confidence_band_name(prediction.confidence_band),
            ),
            None => ("not_enough_evidence", "unavailable"),
        };
        let factors = prediction
            .explanations
            .iter()
            .map(studio_explanation_factor)
            .collect::<Vec<_>>();
        let pairwise = pairwise_by_asset.get(&candidate.media_asset_id);
        records.push(StudioRecommendationRecord {
            id: Uuid::new_v4().to_string(),
            profile_id: profile_id.into(),
            model_id: model.id.clone(),
            project_id: candidate.project_id,
            media_asset_id: candidate.media_asset_id,
            feature_schema_version: STUDIO_BRAIN_FEATURE_SCHEMA_VERSION.into(),
            feature_fingerprint: candidate.feature_fingerprint,
            recommendation: recommendation.into(),
            confidence_band: confidence_band.into(),
            explanation_json: serde_json::json!({
                "factors": factors,
                "similarSetRank": pairwise.map(|value| value.0),
                "relativeRankScore": pairwise.map(|value| value.1),
                "pairwiseFactors": pairwise.map(|value| value.2.clone()).unwrap_or_default(),
            }),
            generic_recommendation_json: candidate.generic_recommendation_json,
            agreement: match prediction.generic_agreement {
                GenericAgreement::Agrees => "agrees",
                GenericAgreement::Differs => "differs",
                GenericAgreement::NoGenericRecommendation | GenericAgreement::Abstained => {
                    "unavailable"
                }
            }
            .into(),
            generated_at: Utc::now().to_rfc3339(),
        });
    }
    Ok(records)
}

fn studio_confidence_band_name(value: studio_brain::ConfidenceBand) -> &'static str {
    match value {
        studio_brain::ConfidenceBand::High => "high",
        studio_brain::ConfidenceBand::Moderate => "moderate",
        studio_brain::ConfidenceBand::Low => "low",
        studio_brain::ConfidenceBand::Abstain => "unavailable",
    }
}

fn studio_explanation_factor(value: &studio_brain::FeatureContribution) -> String {
    let direction = value.direction.as_str();
    let subject = match value.feature.as_str() {
        "technical_score" => "current technical quality evidence",
        "sharpness_score" => "current sharpness evidence",
        "blur_score" => "current blur evidence",
        "anonymous_face_count_capped" => "available anonymous face-count evidence",
        "open_eyes_fraction" => "available eye-state evidence",
        "similar_set_size_capped" => "the size of this Similar Set",
        "relative_technical_rank" => "this frame's technical position within its Similar Set",
        "relative_sharpness_rank" => "this frame's sharpness position within its Similar Set",
        "moment_size_capped" | "moment_position" => "its local Moment context",
        "generic_keep" | "generic_review" | "generic_reject" => {
            "the separate generic technical recommendation"
        }
        "generic_representative" => "the generic Similar Set starting point",
        "semantic_evidence_available" => "availability of local semantic evidence",
        _ => "a compact local evidence feature",
    };
    format!("{subject} {direction} this advisory result")
}

fn studio_pairwise_explanation_factor(value: &studio_brain::FeatureContribution) -> String {
    let subject = match value.feature.as_str() {
        "technical_score" => "technical quality evidence",
        "sharpness_score" => "sharpness evidence",
        "blur_score" => "blur evidence",
        "anonymous_face_count_capped" => "anonymous face-count evidence",
        "open_eyes_fraction" => "available eye-state evidence",
        "relative_technical_rank" => "technical position within this Similar Set",
        "relative_sharpness_rank" => "sharpness position within this Similar Set",
        "generic_representative" => "the generic technical starting point",
        _ => "a compact local comparison feature",
    };
    format!(
        "{subject} {} the non-binding Similar Set order",
        value.direction
    )
}

// These are deliberately small non-inferiority tolerances, not a claim that a decimal change
// reflects photographer-world quality. Both models are scored on the same frozen, grouped local
// holdout; a candidate that loses more than either tolerance stays inactive.
const STUDIO_MAX_VALIDATION_MACRO_F1_DROP: f32 = 0.03;
const STUDIO_MAX_VALIDATION_BRIER_INCREASE: f32 = 0.03;

fn studio_candidate_rejection_reason(
    candidate: &studio_brain::EvaluationReport,
    previous: &studio_brain::EvaluationReport,
    minimum_validation_examples: usize,
) -> Option<String> {
    let (Some(candidate), Some(previous)) = (
        candidate.personal_model.as_ref(),
        previous.personal_model.as_ref(),
    ) else {
        return Some(
            "New Studio Brain could not be compared safely with the active model. Previous model remains active."
                .into(),
        );
    };
    if candidate.sample_count < minimum_validation_examples
        || previous.sample_count < minimum_validation_examples
        || candidate.sample_count != previous.sample_count
    {
        return Some(
            "New Studio Brain could not be compared safely with the active model. Previous model remains active."
                .into(),
        );
    }
    if candidate.macro_f1 + STUDIO_MAX_VALIDATION_MACRO_F1_DROP < previous.macro_f1
        || candidate.brier_score > previous.brier_score + STUDIO_MAX_VALIDATION_BRIER_INCREASE
    {
        return Some(
            "New Studio Brain did not improve validation performance. Previous model remains active."
                .into(),
        );
    }
    None
}

fn finish_rejected_studio_training(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    run: &mut StudioTrainingRunRecord,
    profile_id: &str,
    active_model_version: Option<String>,
    reason: &str,
    on_progress: &mut impl FnMut(&StudioBrainProgress),
) -> PersistenceResult<StudioBrainProgress> {
    run.state = "not_activated".into();
    run.error_message = Some(reason.into());
    run.updated_at = Utc::now().to_rfc3339();
    run.finished_at = Some(run.updated_at.clone());
    repository.update_studio_training_run(run)?;
    repository.update_studio_profile_training_state(
        profile_id,
        "stale",
        &serde_json::json!({
            "state": "stale",
            "reasons": [
                "The new local candidate was not activated after comparison with the retained active model.",
                reason,
                "Run another explicit update after more human review if you want to reassess personalization."
            ]
        }),
        None,
    )?;
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::Finalize;
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "studio-training",
        "profile_id": profile_id,
        "summary": reason,
        "candidateActivated": false,
        "previousModelRetained": true,
    }));
    repository.update_background_job(job)?;
    let progress = studio_progress(
        profile_id,
        "stale",
        false,
        "complete",
        job.items_completed,
        job.items_total.unwrap_or(0),
        0,
        active_model_version,
        Some(reason.into()),
        None,
    );
    on_progress(&progress);
    Ok(progress)
}

/// Covers failures after a durable Studio background job exists but before a training-run row can
/// reliably represent the failure. It deliberately returns a safe terminal progress projection
/// rather than leaking a raw repository error into the desktop's last-resort worker path.
fn finish_pre_run_studio_training(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    profile_id: &str,
    active_model_version: Option<String>,
    diagnostic: String,
    on_progress: &mut impl FnMut(&StudioBrainProgress),
) -> PersistenceResult<StudioBrainProgress> {
    let recovery_message = if active_model_version.is_some() {
        "Studio Brain update could not be completed. Your previous personalized model is still active."
    } else {
        "Studio Brain update could not be completed. No personalized model was activated; generic technical evidence remains available."
    };
    let now = Utc::now();
    job.state = WorkflowRunState::Failed;
    job.stage = JobStage::Finalize;
    job.error_count = 1;
    job.error_message = Some(diagnostic.clone());
    job.updated_at = now;
    job.finished_at = Some(now);
    // Both writes are best effort: this path exists precisely because a prior repository action
    // failed. The truthful UI result is still preferable to leaving the worker to claim a model
    // that may not exist; startup recovery handles any unreachable durable job later.
    let _ = repository.update_studio_profile_training_state(
        profile_id,
        "error",
        &serde_json::json!({
            "state": "error",
            "reasons": ["A local Studio Brain update stopped before a training candidate was created."]
        }),
        Some(&diagnostic),
    );
    let _ = repository.update_background_job(job);
    let progress = studio_progress(
        profile_id,
        "error",
        false,
        "complete",
        job.items_completed,
        job.items_total.unwrap_or(0),
        1,
        active_model_version,
        Some(recovery_message.into()),
        Some(diagnostic),
    );
    on_progress(&progress);
    Ok(progress)
}

fn finish_failed_studio_training(
    repository: &impl CatalogRepository,
    job: &mut BackgroundJob,
    run: &mut StudioTrainingRunRecord,
    profile_id: &str,
    active_model_version: Option<String>,
    diagnostic: String,
    on_progress: &mut impl FnMut(&StudioBrainProgress),
) -> PersistenceResult<StudioBrainProgress> {
    let recovery_message = if active_model_version.is_some() {
        "Studio Brain update could not be completed. Your previous personalized model is still active."
    } else {
        "Studio Brain update could not be completed. No personalized model was activated; generic technical evidence remains available."
    };
    run.state = "failed".into();
    run.error_message = Some(diagnostic.clone());
    run.updated_at = Utc::now().to_rfc3339();
    run.finished_at = Some(run.updated_at.clone());
    repository.update_studio_training_run(run)?;
    repository.update_studio_profile_training_state(
        profile_id,
        "error",
        &serde_json::json!({"state":"error","reasons":["A local candidate could not be validated or activated. The prior model remains unchanged."]}),
        Some(&diagnostic),
    )?;
    job.state = WorkflowRunState::Failed;
    job.stage = JobStage::Finalize;
    job.error_count = 1;
    job.error_message = Some(diagnostic.clone());
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    repository.update_background_job(job)?;
    let progress = studio_progress(
        profile_id,
        "error",
        false,
        "complete",
        job.items_completed,
        job.items_total.unwrap_or(0),
        1,
        active_model_version,
        Some(recovery_message.into()),
        Some(diagnostic),
    );
    on_progress(&progress);
    Ok(progress)
}

pub fn apply_culling_decision(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    asset_id: &MediaAssetId,
    update: &CullingDecisionUpdate,
) -> PersistenceResult<CullingDecisionView> {
    repository.update_culling_decision(project_id, asset_id, update)
}

pub fn update_culling_position(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session_id: &str,
    asset_id: Option<&MediaAssetId>,
    group_id: Option<&str>,
    mode: &str,
    filter_context: Option<&str>,
) -> PersistenceResult<()> {
    repository.update_review_position(
        project_id,
        session_id,
        asset_id,
        group_id,
        mode,
        filter_context,
    )
}

pub fn set_culling_group_representative(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    group_id: &str,
    asset_id: &MediaAssetId,
    session_id: Option<&str>,
) -> PersistenceResult<()> {
    repository.set_group_human_representative(project_id, group_id, asset_id, session_id)
}

pub fn complete_culling_group(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    group_id: &str,
    session_id: Option<&str>,
) -> PersistenceResult<()> {
    repository.mark_group_review_complete(project_id, group_id, session_id)
}

pub fn finish_culling_review(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    session_id: &str,
) -> PersistenceResult<ReviewSessionView> {
    repository.finish_review_session(project_id, session_id)
}

/// Writes an explicit report only to the caller-selected *new* file. No source directory is
/// inferred, no source file is opened for writing, and an existing destination is never replaced.
pub fn export_culling_report(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    destination: &Path,
    format: &str,
) -> PersistenceResult<usize> {
    if !matches!(format, "csv" | "json") {
        return Err(PersistenceError::InvalidData(
            "culling report format must be csv or json".into(),
        ));
    }
    let parent = destination.parent().ok_or_else(|| {
        PersistenceError::InvalidData("culling report destination must have a parent folder".into())
    })?;
    if !parent.is_dir() || destination.file_name().is_none() {
        return Err(PersistenceError::InvalidData(
            "culling report destination must be a new file inside an existing folder".into(),
        ));
    }
    let rows = repository.culling_report(project_id)?;
    let body = if format == "json" {
        serde_json::to_vec_pretty(&rows).map_err(PersistenceError::Serialization)?
    } else {
        culling_report_csv(&rows).into_bytes()
    };
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            PersistenceError::InvalidData(format!(
                "could not create culling report destination without overwriting an existing file: {error}"
            ))
        })?;
    output.write_all(&body).map_err(|error| {
        PersistenceError::InvalidData(format!("could not write culling report: {error}"))
    })?;
    output.flush().map_err(|error| {
        PersistenceError::InvalidData(format!("could not finalize culling report: {error}"))
    })?;
    Ok(rows.len())
}

/// Developer/research-only local export of anonymized Studio Brain foundation data. It is
/// intentionally not surfaced in the photographer workflow and has no image/path fields.
pub fn export_studio_brain_preference_examples(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    destination: &Path,
) -> PersistenceResult<usize> {
    let examples = repository.preference_examples(project_id)?;
    let body = serde_json::to_vec_pretty(&examples).map_err(PersistenceError::Serialization)?;
    write_new_local_export(destination, &body, "Studio Brain preference export")?;
    Ok(examples.len())
}

fn write_new_local_export(destination: &Path, body: &[u8], label: &str) -> PersistenceResult<()> {
    let parent = destination.parent().ok_or_else(|| {
        PersistenceError::InvalidData(format!("{label} destination must have a parent folder"))
    })?;
    if !parent.is_dir() || destination.file_name().is_none() {
        return Err(PersistenceError::InvalidData(format!(
            "{label} destination must be a new file inside an existing folder"
        )));
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            PersistenceError::InvalidData(format!(
                "could not create {label} without overwriting an existing file: {error}"
            ))
        })?;
    output.write_all(body).map_err(|error| {
        PersistenceError::InvalidData(format!("could not write {label}: {error}"))
    })?;
    output.flush().map_err(|error| {
        PersistenceError::InvalidData(format!("could not finalize {label}: {error}"))
    })?;
    Ok(())
}

fn culling_report_csv(rows: &[CullingReportRow]) -> String {
    fn cell(value: impl AsRef<str>) -> String {
        format!("\"{}\"", value.as_ref().replace('"', "\"\""))
    }
    let mut csv = String::from("filename,media_asset_id,decision,rating,starred,ai_recommendation,technical_score,similarity_group_id\n");
    for row in rows {
        let score = row
            .technical_score
            .map(|value| format!("{value:.2}"))
            .unwrap_or_default();
        csv.push_str(
            &[
                cell(&row.filename),
                cell(&row.media_asset_id),
                cell(row.decision.as_deref().unwrap_or("unreviewed")),
                row.rating.to_string(),
                row.starred.to_string(),
                cell(row.ai_recommendation.as_deref().unwrap_or("")),
                cell(score),
                cell(row.similarity_group_id.as_deref().unwrap_or("")),
            ]
            .join(","),
        );
        csv.push('\n');
    }
    csv
}

fn ensure_asset_project(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    asset_id: &MediaAssetId,
) -> PersistenceResult<()> {
    if repository.media_asset_belongs_to_project(asset_id, project_id)? {
        Ok(())
    } else {
        Err(PersistenceError::InvalidData(
            "media asset does not belong to the selected project".into(),
        ))
    }
}

#[derive(Debug, Clone)]
struct CandidateResult {
    status: AnalysisStatus,
    error_message: Option<String>,
}

#[derive(Debug)]
struct ComputedCandidate {
    candidate: AnalysisInputCandidate,
    analysis: capture_intelligence::ImageAnalysisResult,
}

/// Aggregate input contract for the current local analyzers. The resolver can reuse a single
/// image that meets the minimum, while creating a 2048px analysis-only rendition only when the
/// available cache is too small or absent. This leaves room for later analyzers to declare a
/// different requirement without multiplying cache artifacts today.
#[derive(Debug, Clone, Copy)]
struct AnalysisInputRequirement {
    minimum_long_edge: u32,
    preferred_long_edge: u32,
}

impl Default for AnalysisInputRequirement {
    fn default() -> Self {
        Self {
            minimum_long_edge: 1600,
            preferred_long_edge: ANALYSIS_PREVIEW_LONG_EDGE,
        }
    }
}

/// Resolves a safe local image for Capture Intelligence. It never exposes a source path to an
/// analyzer: sources are read only to create a contained cache artifact, then analysis consumes
/// that artifact. `NEEDS_ORIGINAL` is produced only after every usable FileInstance is absent.
struct AnalysisInputResolver<'a, R: CatalogRepository, A: ThumbnailProvider> {
    repository: &'a R,
    cache_root: &'a Path,
    adapters: &'a A,
    requirement: AnalysisInputRequirement,
}

impl<'a, R: CatalogRepository, A: ThumbnailProvider> AnalysisInputResolver<'a, R, A> {
    fn new(
        repository: &'a R,
        cache_root: &'a Path,
        adapters: &'a A,
        requirement: AnalysisInputRequirement,
    ) -> Self {
        Self {
            repository,
            cache_root,
            adapters,
            requirement,
        }
    }

    fn resolve(
        &self,
        candidate: AnalysisInputCandidate,
    ) -> PersistenceResult<AnalysisInputCandidate> {
        debug_assert_eq!(
            self.requirement.preferred_long_edge,
            ANALYSIS_PREVIEW_LONG_EDGE
        );
        if candidate.preview_status == "ready"
            && preview_meets_requirement(&candidate, self.requirement)
            && candidate
                .preview_relative_path
                .as_deref()
                .and_then(|relative| resolve_analysis_preview_path(self.cache_root, relative))
                .is_some()
        {
            return Ok(candidate);
        }
        if matches!(candidate.preview_status.as_str(), "corrupt" | "unsupported")
            && candidate.preview_artifact_type.as_deref() == Some("analysis_preview")
        {
            return Ok(candidate);
        }

        let media_type = media_type_from_label(&candidate.media_type)?;
        let copies = self
            .repository
            .preparation_file_instance_candidates(&candidate.asset_id)?;
        let mut terminal_failure: Option<(ArtifactStatus, String, AnalysisInputCandidate)> = None;
        for copy in copies {
            // Index availability is the authority for a physical copy. Do not reach around an
            // explicitly offline FileInstance merely because its old path happens to exist on
            // this machine; a cached input can still be reused above.
            if !copy.is_available {
                continue;
            }
            let source = source_path(copy.selected_root.as_deref(), &copy.relative_path);
            let generated = prepare_analysis_preview(
                self.adapters,
                self.cache_root,
                &copy.asset_id,
                &copy.file_instance_id,
                source.as_deref().unwrap_or_else(|| Path::new("")),
                &media_type,
                &copy.source_fingerprint,
            )
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
            self.repository
                .upsert_preview_artifact(&PreviewArtifactRecord {
                    id: Uuid::new_v4().to_string(),
                    media_asset_id: copy.asset_id.clone(),
                    source_file_instance_id: copy.file_instance_id.clone(),
                    artifact_type: generated.artifact_type.clone(),
                    size_class: generated.size.as_str().into(),
                    cache_relative_path: generated.cache_relative_path.clone(),
                    provider: generated.provider.clone(),
                    generator_version: ANALYSIS_PREVIEW_GENERATOR_VERSION.into(),
                    source_fingerprint: generated.source_fingerprint.clone(),
                    created_at: Utc::now().to_rfc3339(),
                    status: generated.status.as_str().into(),
                    failure_reason: generated.failure_reason.clone(),
                })?;
            let resolved = resolved_analysis_candidate(&candidate, &copy, &generated);
            if generated.status == ArtifactStatus::Ready
                && resolved
                    .preview_relative_path
                    .as_deref()
                    .and_then(|relative| resolve_analysis_preview_path(self.cache_root, relative))
                    .is_some()
            {
                return Ok(resolved);
            }
            if generated.status != ArtifactStatus::Offline {
                let message = generated
                    .failure_reason
                    .unwrap_or_else(|| "CaptureOS could not prepare a local analysis image".into());
                if terminal_failure.as_ref().is_none_or(|(status, _, _)| {
                    analysis_input_failure_rank(&generated.status)
                        > analysis_input_failure_rank(status)
                }) {
                    terminal_failure = Some((generated.status, message, resolved));
                }
            }
        }
        if let Some((status, message, resolved)) = terminal_failure {
            return Ok(resolved_terminal_candidate(resolved, status, message));
        }
        Ok(resolved_terminal_candidate(
            candidate,
            ArtifactStatus::Offline,
            "No sufficient cached analysis image exists and no usable local FileInstance is currently available".into(),
        ))
    }
}

fn preview_meets_requirement(
    candidate: &AnalysisInputCandidate,
    requirement: AnalysisInputRequirement,
) -> bool {
    let long_edge = match (
        candidate.preview_artifact_type.as_deref(),
        candidate.preview_size_class.as_deref(),
    ) {
        (Some("analysis_preview"), Some("analysis")) => ANALYSIS_PREVIEW_LONG_EDGE,
        (Some("thumbnail"), Some("preview")) => 1600,
        (Some("thumbnail"), Some("medium")) => 768,
        (Some("thumbnail"), Some("small")) => 256,
        _ => 0,
    };
    long_edge >= requirement.minimum_long_edge
}

fn resolved_analysis_candidate(
    base: &AnalysisInputCandidate,
    copy: &persistence::MediaPreparationCandidate,
    generated: &media_visual::GeneratedPreview,
) -> AnalysisInputCandidate {
    let mut resolved = base.clone();
    resolved.file_instance_id = copy.file_instance_id.clone();
    resolved.filename = copy.filename.clone();
    resolved.media_type = copy.media_type.clone();
    resolved.input_fingerprint = analysis_input_fingerprint(
        &generated.source_fingerprint,
        &generated.artifact_type,
        ANALYSIS_PREVIEW_GENERATOR_VERSION,
        generated.size.as_str(),
    );
    resolved.preview_relative_path =
        (generated.status == ArtifactStatus::Ready).then(|| generated.cache_relative_path.clone());
    resolved.preview_artifact_type = Some(generated.artifact_type.clone());
    resolved.preview_size_class = Some(generated.size.as_str().into());
    resolved.preview_status = generated.status.as_str().into();
    resolved.preview_failure_reason = generated.failure_reason.clone();
    resolved.is_available = copy.is_available;
    resolved
}

fn resolved_terminal_candidate(
    mut candidate: AnalysisInputCandidate,
    status: ArtifactStatus,
    message: String,
) -> AnalysisInputCandidate {
    candidate.preview_relative_path = None;
    candidate.preview_artifact_type = None;
    candidate.preview_size_class = None;
    candidate.preview_status = match status {
        ArtifactStatus::Corrupt => "corrupt",
        ArtifactStatus::Unsupported => "unsupported",
        ArtifactStatus::Offline => "needs_original",
        _ => "failed",
    }
    .into();
    candidate.preview_failure_reason = Some(message);
    candidate
}

fn failed_analysis_input_candidate(
    candidate: AnalysisInputCandidate,
    message: String,
) -> AnalysisInputCandidate {
    resolved_terminal_candidate(candidate, ArtifactStatus::Failed, message)
}

fn analysis_input_failure_rank(status: &ArtifactStatus) -> u8 {
    match status {
        ArtifactStatus::Corrupt => 3,
        ArtifactStatus::Unsupported => 2,
        ArtifactStatus::Failed | ArtifactStatus::Timeout | ArtifactStatus::Cancelled => 1,
        _ => 0,
    }
}

fn analysis_input_fingerprint(
    source_fingerprint: &str,
    artifact_type: &str,
    generator_version: &str,
    size_class: &str,
) -> String {
    let label = if artifact_type == "thumbnail" {
        "preview"
    } else {
        "analysis-preview"
    };
    format!("{source_fingerprint}|{label}:{generator_version}:{size_class}")
}

/// Returns the maximum number of concurrently decoded local previews for the selected resource
/// mode. The caller supplies the observed CPU capacity so the policy remains unit-testable and
/// never assumes an unavailable processor count means zero workers.
fn analysis_worker_limit(
    resource_mode: AnalysisResourceMode,
    available_parallelism: usize,
) -> usize {
    let available_parallelism = available_parallelism.max(1);
    match resource_mode {
        AnalysisResourceMode::Eco => 1,
        AnalysisResourceMode::Balanced => available_parallelism.min(2),
        AnalysisResourceMode::Fast => available_parallelism.min(4),
    }
}

fn analysis_worker_count(resource_mode: AnalysisResourceMode) -> usize {
    let available_parallelism = thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    analysis_worker_limit(resource_mode, available_parallelism)
}

/// Computes one bounded batch in parallel and returns candidates in catalog order. The workers
/// hold no repository handle: all persistent state is written by `persist_candidate_analysis` on
/// the calling thread after every local computation has finished. A provider panic is converted
/// into a per-asset FAILED result so a bad preview cannot abort the rest of the batch.
fn compute_analysis_batch(
    batch: &[AnalysisInputCandidate],
    cache_root: &Path,
) -> Vec<ComputedCandidate> {
    let cache_root = cache_root.to_path_buf();
    thread::scope(|scope| {
        let workers = batch
            .iter()
            .cloned()
            .map(|candidate| {
                let join_fallback_candidate = candidate.clone();
                let cache_root = cache_root.clone();
                let worker = scope.spawn(move || {
                    let computation_fallback_candidate = candidate.clone();
                    std::panic::catch_unwind(AssertUnwindSafe(|| ComputedCandidate {
                        analysis: compute_candidate_analysis(&candidate, &cache_root),
                        candidate,
                    }))
                    .unwrap_or_else(|_| ComputedCandidate {
                        candidate: computation_fallback_candidate,
                        analysis: failed_candidate_analysis(
                            "A local analysis worker stopped unexpectedly; no evidence was stored",
                        ),
                    })
                });
                (join_fallback_candidate, worker)
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|(fallback_candidate, worker)| {
                worker.join().unwrap_or_else(|_| ComputedCandidate {
                    candidate: fallback_candidate,
                    analysis: failed_candidate_analysis(
                        "A local analysis worker could not be joined; no evidence was stored",
                    ),
                })
            })
            .collect()
    })
}

fn failed_candidate_analysis(
    message: impl Into<String>,
) -> capture_intelligence::ImageAnalysisResult {
    let message = message.into();
    capture_intelligence::ImageAnalysisResult {
        status: AnalysisStatus::Failed,
        fingerprint: None,
        technical: None,
        faces: unavailable_face_result(message.clone()),
        error_message: Some(message),
    }
}

fn unavailable_face_result(message: impl Into<String>) -> FaceProviderResult {
    let identity = unavailable_face_provider_identity();
    FaceProviderResult {
        provider: identity.provider,
        provider_version: identity.provider_version,
        resolved_provider: "none".into(),
        resolved_provider_version: "no-approved-local-provider.v1".into(),
        status: AnalysisStatus::NotApplicable,
        faces: Vec::new(),
        provider_attempt_error: None,
        landmark_status: AnalysisStatus::NotApplicable,
        landmark_error_message: Some(
            "No local landmark provider is enabled; eye state is not analyzable".into(),
        ),
        error_message: Some(message.into()),
    }
}

fn compute_candidate_analysis(
    candidate: &AnalysisInputCandidate,
    cache_root: &Path,
) -> capture_intelligence::ImageAnalysisResult {
    let analyzer = DeterministicImageAnalyzer;
    let face_detector = PlatformFaceDetector;
    let input_path = candidate
        .preview_relative_path
        .as_deref()
        .and_then(|relative| resolve_analysis_preview_path(cache_root, relative));
    if candidate.preview_status == "ready" {
        return match input_path {
            Some(path) => analyze_preview(&path, &LocalPreviewDecoder, &analyzer, &face_detector),
            None => capture_intelligence::ImageAnalysisResult {
                status: AnalysisStatus::NeedsOriginal,
                fingerprint: None,
                technical: None,
                faces: unavailable_face_result(
                    "Cached analysis preview is no longer safely available",
                ),
                error_message: Some("Cached analysis preview is no longer safely available".into()),
            },
        };
    }
    let (status, message) = match candidate.preview_status.as_str() {
        "corrupt" => (
            AnalysisStatus::Corrupt,
            candidate
                .preview_failure_reason
                .as_deref()
                .unwrap_or("The available analysis input is corrupt"),
        ),
        "unsupported" => (
            AnalysisStatus::Unsupported,
            candidate
                .preview_failure_reason
                .as_deref()
                .unwrap_or("The available source cannot be decoded by the current local provider"),
        ),
        "needs_original" => (
            AnalysisStatus::NeedsOriginal,
            candidate
                .preview_failure_reason
                .as_deref()
                .unwrap_or("No usable local FileInstance is currently available"),
        ),
        _ => (
            AnalysisStatus::Failed,
            candidate
                .preview_failure_reason
                .as_deref()
                .unwrap_or("The analysis input resolver did not produce a usable local image"),
        ),
    };
    capture_intelligence::ImageAnalysisResult {
        status,
        fingerprint: None,
        technical: None,
        faces: unavailable_face_result(message),
        error_message: Some(message.into()),
    }
}

/// Computes only the independently versioned face component from an existing safe cache image.
/// This must remain narrower than `analyze_preview`: a face-adapter repair cannot silently redo
/// deterministic technical evidence, embeddings, similarity groups, or cache generation.
fn compute_face_candidate_analysis(
    candidate: &AnalysisInputCandidate,
    cache_root: &Path,
) -> FaceProviderResult {
    let Some(relative_path) = candidate.preview_relative_path.as_deref() else {
        return failed_face_component(
            AnalysisStatus::NeedsOriginal,
            "A current face refresh requires a CaptureOS-managed analysis preview",
        );
    };
    let Some(preview_path) = resolve_analysis_preview_path(cache_root, relative_path) else {
        return failed_face_component(
            AnalysisStatus::NeedsOriginal,
            "Cached face-analysis preview is no longer safely available",
        );
    };
    let image = match LocalPreviewDecoder.decode(&preview_path) {
        Ok(image) => image,
        Err(error) => return failed_face_component(error.status(), error.to_string()),
    };
    let mut faces = PlatformFaceDetector.detect(&preview_path, &image);
    for face in &mut faces.faces {
        face.face_sharpness = face_sharpness(&image, face);
    }
    faces
}

fn failed_face_component(status: AnalysisStatus, message: impl Into<String>) -> FaceProviderResult {
    let message = message.into();
    let identity = platform_face_provider_identity();
    FaceProviderResult {
        provider: identity.provider,
        provider_version: identity.provider_version,
        resolved_provider: "none".into(),
        resolved_provider_version: "not-run".into(),
        status,
        faces: Vec::new(),
        provider_attempt_error: None,
        landmark_status: AnalysisStatus::NotApplicable,
        landmark_error_message: Some(
            "No decoded image was available for landmarks or eye state".into(),
        ),
        error_message: Some(message),
    }
}

fn persist_face_candidate_analysis(
    repository: &impl CatalogRepository,
    candidate: &AnalysisInputCandidate,
    face_result: FaceProviderResult,
) -> PersistenceResult<CandidateResult> {
    let asset_id = MediaAssetId::try_from(candidate.asset_id.as_str())
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let now = Utc::now();
    let face_identity = face_result.identity();
    let face_input_fingerprint =
        face_result.analysis_input_fingerprint(&candidate.input_fingerprint);
    let status = face_result.status.clone();
    let error_message = face_result.error_message.clone();
    let faces = face_result
        .faces
        .into_iter()
        .map(|face| FaceAnalysis {
            id: FaceAnalysisId::new(),
            media_asset_id: asset_id.clone(),
            input_fingerprint: face_input_fingerprint.clone(),
            provider: face_identity.provider.clone(),
            provider_version: face_identity.provider_version.clone(),
            x: face.x,
            y: face.y,
            width: face.width,
            height: face.height,
            detection_confidence: face.detection_confidence,
            relative_size: (face.width * face.height).clamp(0.0, 1.0),
            visibility: Some(if face.width >= 0.10 { "good" } else { "small" }.into()),
            pose: face.pose,
            face_sharpness: face.face_sharpness,
            eye_state: face.eye_state,
            eye_confidence: face.eye_confidence,
            generated_at: now,
        })
        .collect::<Vec<_>>();
    let artifact = AnalysisArtifact {
        id: AnalysisArtifactId::new(),
        media_asset_id: asset_id.clone(),
        artifact_type: "face_detection".into(),
        payload: serde_json::json!({
            "face_count": faces.len(),
            "input_preview_fingerprint": candidate.input_fingerprint,
            "face_provider_cache_identity": face_identity.cache_identity.clone(),
            "face_analysis_input_fingerprint": face_input_fingerprint.clone(),
            "face_settings_version": FACE_ANALYSIS_SETTINGS_VERSION,
            "resolved_face_provider": face_result.resolved_provider,
            "resolved_face_provider_version": face_result.resolved_provider_version,
            "provider_attempt_error": face_result.provider_attempt_error,
            "landmark_status": face_result.landmark_status.as_str(),
            "landmark_error": face_result.landmark_error_message,
            "privacy": "local_only_no_identity_recognition",
        }),
        provenance: Provenance {
            source: "local_capture_intelligence".into(),
            algorithm_id: Some(face_identity.provider.clone()),
            algorithm_version: Some(face_identity.provider_version.clone()),
            produced_at: now,
            human_confirmed: false,
        },
        provider: Some(face_identity.provider.clone()),
        provider_version: Some(face_identity.provider_version.clone()),
        model_version: None,
        settings_version: Some(FACE_ANALYSIS_SETTINGS_VERSION.into()),
        input_fingerprint: Some(face_input_fingerprint.clone()),
        generated_at: Some(now),
        confidence: None,
        status: Some(status.clone()),
        error_message: error_message.clone(),
    };
    repository.upsert_analysis_artifact(&artifact)?;
    repository.replace_face_analyses(&asset_id, &face_input_fingerprint, &faces)?;
    Ok(CandidateResult {
        status,
        error_message,
    })
}

fn persist_candidate_analysis(
    repository: &impl CatalogRepository,
    candidate: &AnalysisInputCandidate,
    analyzer: &DeterministicImageAnalyzer,
    analysis: capture_intelligence::ImageAnalysisResult,
) -> PersistenceResult<CandidateResult> {
    let asset_id = MediaAssetId::try_from(candidate.asset_id.as_str())
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    let now = Utc::now();
    let face_identity = analysis.faces.identity();
    let face_input_fingerprint = analysis
        .faces
        .analysis_input_fingerprint(&candidate.input_fingerprint);
    let face_status = analysis.faces.status.clone();
    let face_error_message = analysis.faces.error_message.clone();
    let resolved_face_provider = analysis.faces.resolved_provider.clone();
    let resolved_face_provider_version = analysis.faces.resolved_provider_version.clone();
    let face_provider_attempt_error = analysis.faces.provider_attempt_error.clone();
    let landmark_status = analysis.faces.landmark_status.clone();
    let landmark_error = analysis.faces.landmark_error_message.clone();
    let face_count = analysis.faces.faces.len();
    let primary = AnalysisArtifact {
        id: AnalysisArtifactId::new(),
        media_asset_id: asset_id.clone(),
        artifact_type: "capture_intelligence".into(),
        payload: serde_json::json!({
            "stages": {
                "fingerprint": analysis.fingerprint.as_ref().map(|_| "ready").unwrap_or("not_run"),
                "technical": analysis.technical.as_ref().map(|_| "ready").unwrap_or("not_run"),
                "faces": face_status.as_str(),
            },
            "face_provider": face_identity.provider.clone(),
            "face_provider_version": face_identity.provider_version.clone(),
            "face_provider_cache_identity": face_identity.cache_identity.clone(),
            "privacy": "local_only_no_identity_recognition",
            "input_preview_fingerprint": candidate.input_fingerprint,
            "analysis_cache_key": capture_intelligence::analysis_cache_key(
                &candidate.input_fingerprint,
                analyzer.provider(),
                analyzer.version(),
                capture_intelligence::ANALYSIS_SETTINGS_VERSION,
            ),
        }),
        provenance: Provenance {
            source: "local_capture_intelligence".into(),
            algorithm_id: Some(analyzer.provider().into()),
            algorithm_version: Some(analyzer.version().into()),
            produced_at: now,
            human_confirmed: false,
        },
        provider: Some(analyzer.provider().into()),
        provider_version: Some(analyzer.version().into()),
        model_version: None,
        settings_version: Some(capture_intelligence::ANALYSIS_SETTINGS_VERSION.into()),
        input_fingerprint: Some(candidate.input_fingerprint.clone()),
        generated_at: Some(now),
        confidence: analysis
            .technical
            .as_ref()
            .map(|technical| technical.confidence),
        status: Some(analysis.status.clone()),
        error_message: analysis.error_message.clone(),
    };
    repository.upsert_analysis_artifact(&primary)?;
    let face_artifact = AnalysisArtifact {
        id: AnalysisArtifactId::new(),
        media_asset_id: asset_id.clone(),
        artifact_type: "face_detection".into(),
        payload: serde_json::json!({
            "face_count": face_count,
            "input_preview_fingerprint": candidate.input_fingerprint,
            "face_provider_cache_identity": face_identity.cache_identity.clone(),
            "face_analysis_input_fingerprint": face_input_fingerprint.clone(),
            "face_settings_version": FACE_ANALYSIS_SETTINGS_VERSION,
            "resolved_face_provider": resolved_face_provider,
            "resolved_face_provider_version": resolved_face_provider_version,
            "provider_attempt_error": face_provider_attempt_error,
            "landmark_status": landmark_status.as_str(),
            "landmark_error": landmark_error,
            "privacy": "local_only_no_identity_recognition",
        }),
        provenance: Provenance {
            source: "local_capture_intelligence".into(),
            algorithm_id: Some(face_identity.provider.clone()),
            algorithm_version: Some(face_identity.provider_version.clone()),
            produced_at: now,
            human_confirmed: false,
        },
        provider: Some(face_identity.provider.clone()),
        provider_version: Some(face_identity.provider_version.clone()),
        model_version: None,
        settings_version: Some(FACE_ANALYSIS_SETTINGS_VERSION.into()),
        input_fingerprint: Some(face_input_fingerprint.clone()),
        generated_at: Some(now),
        confidence: None,
        status: Some(face_status),
        error_message: face_error_message,
    };
    repository.upsert_analysis_artifact(&face_artifact)?;

    if let Some(fingerprint) = analysis.fingerprint {
        repository.upsert_visual_fingerprint(&VisualFingerprint {
            media_asset_id: asset_id.clone(),
            input_fingerprint: candidate.input_fingerprint.clone(),
            provider: analyzer.provider().into(),
            provider_version: analyzer.version().into(),
            perceptual_hash: fingerprint.perceptual_hash,
            difference_hash: fingerprint.difference_hash,
            color_signature: fingerprint.color_signature,
            embedding: fingerprint.embedding,
            embedding_dimensions: 64,
            bucket_keys: fingerprint.bucket_keys,
            generated_at: now,
            status: AnalysisStatus::Ready,
            error_message: None,
        })?;
    }
    if let Some(technical) = analysis.technical {
        repository.upsert_technical_quality(&TechnicalQualityEvidence {
            media_asset_id: asset_id.clone(),
            input_fingerprint: candidate.input_fingerprint.clone(),
            provider: analyzer.provider().into(),
            provider_version: analyzer.version().into(),
            global_sharpness: Some(technical.global_sharpness),
            sharpness_band: technical.sharpness_band,
            directional_blur_ratio: Some(technical.directional_blur_ratio),
            blur_level: technical.blur_level,
            mean_luminance: Some(technical.mean_luminance),
            median_luminance: Some(technical.median_luminance),
            highlight_clipping_percent: Some(technical.highlight_clipping_percent),
            shadow_clipping_percent: Some(technical.shadow_clipping_percent),
            channel_clipping_percent: Some(technical.channel_clipping_percent),
            technical_quality_score: Some(technical.technical_quality_score),
            technical_quality_band: technical.technical_quality_band,
            confidence: technical.confidence,
            generated_at: now,
            status: AnalysisStatus::Ready,
            error_message: None,
        })?;
    }
    let faces = analysis
        .faces
        .faces
        .into_iter()
        .map(|face| FaceAnalysis {
            id: FaceAnalysisId::new(),
            media_asset_id: asset_id.clone(),
            input_fingerprint: face_input_fingerprint.clone(),
            provider: face_identity.provider.clone(),
            provider_version: face_identity.provider_version.clone(),
            x: face.x,
            y: face.y,
            width: face.width,
            height: face.height,
            detection_confidence: face.detection_confidence,
            relative_size: (face.width * face.height).clamp(0.0, 1.0),
            visibility: Some(if face.width >= 0.10 { "good" } else { "small" }.into()),
            pose: face.pose,
            face_sharpness: face.face_sharpness,
            eye_state: face.eye_state,
            eye_confidence: face.eye_confidence,
            generated_at: now,
        })
        .collect::<Vec<_>>();
    repository.replace_face_analyses(&asset_id, &face_input_fingerprint, &faces)?;
    Ok(CandidateResult {
        status: primary.status.unwrap_or(AnalysisStatus::Failed),
        error_message: primary.error_message,
    })
}

fn rebuild_similarity_groups_and_recommendations(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<()> {
    let inputs = repository.grouping_inputs(project_id)?;
    let grouping_inputs = inputs
        .iter()
        .filter_map(|input| {
            Some(capture_intelligence::GroupingInput {
                project_id: input.project_id.clone(),
                asset_id: input.asset_id.clone(),
                // `content_hash` is populated from BLAKE3 full-hash evidence by Index Mode or
                // verified ingest. The bounded fast fingerprint is intentionally excluded.
                verified_content_hash: input.content_hash.clone(),
                captured_at_unix_seconds: input
                    .captured_at
                    .as_deref()
                    .and_then(parse_rfc3339_seconds),
                camera_model: input.camera_model.clone(),
                fingerprint: FingerprintEvidence {
                    perceptual_hash: input.perceptual_hash.clone()?,
                    difference_hash: input.difference_hash.clone()?,
                    color_signature: input.color_signature.clone()?,
                    embedding: input.embedding.clone()?,
                    bucket_keys: input.bucket_keys.clone()?,
                },
                technical_quality_score: input.technical_quality_score,
            })
        })
        .collect::<Vec<_>>();
    let (built, diagnostics) = build_similarity_groups_with_diagnostics(&grouping_inputs);
    // Detailed candidate-recall measurements are intentionally local developer diagnostics, not
    // photographer-facing claims. They contain only persisted asset IDs and deterministic
    // descriptor evidence, never image pixels, paths, face data, or cloud-derived results.
    eprintln!(
        "[captureos][intelligence][similarity-diagnostics] {}",
        serde_json::json!({
            "projectId": project_id.to_string(),
            "embeddingProvider": DETERMINISTIC_PROVIDER,
            "embeddingProviderVersion": DETERMINISTIC_VERSION,
            "successfulEmbeddings": grouping_inputs.len(),
            "diagnostics": diagnostics,
        })
    );
    let now = Utc::now();
    let mut groups = Vec::with_capacity(built.len());
    let mut members = Vec::new();
    for group in &built {
        groups.push(SimilarityGroup {
            id: group.id.clone(),
            project_id: project_id.clone(),
            kind: group.kind,
            representative_asset_id: MediaAssetId::try_from(group.representative_asset_id.as_str())
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
            grouping_method: group.grouping_method.clone(),
            grouping_version: group.grouping_version.clone(),
            similarity_confidence: group.similarity_confidence,
            time_proximity_seconds: group.time_proximity_seconds,
            visual_similarity: group.visual_similarity,
            created_at: now,
        });
        for (ordinal, member) in group.members.iter().enumerate() {
            members.push(SimilarityGroupMember {
                group_id: group.id.clone(),
                media_asset_id: MediaAssetId::try_from(member.asset_id.as_str())
                    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
                ordinal: ordinal as u32,
                similarity_confidence: member.similarity_confidence,
                time_proximity_seconds: member.time_proximity_seconds,
                is_representative: member.is_representative,
            });
        }
    }
    repository.replace_similarity_groups(project_id, &groups, &members)?;

    // Recompute advisory labels from stored, inspectable technical evidence after grouping.
    // This changes recommendation rows only; it never overwrites an append-only human decision.
    for input in inputs {
        let asset_id = MediaAssetId::try_from(input.asset_id.as_str())
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
        let Some(detail) = repository.capture_intelligence_detail(&asset_id)? else {
            continue;
        };
        let Some(technical) = detail.technical else {
            continue;
        };
        if technical.status != "ready" {
            continue;
        }
        // An asset can belong to exact, near, burst, and similar sets at once. The intelligence
        // crate owns the explicit primary order, which is also used for persisted group output.
        let group = primary_group_for_asset(&built, &input.asset_id);
        let open_eyes = detail
            .faces
            .iter()
            .filter(|face| face.eye_state == "open")
            .count();
        let possible_closed_eyes = detail
            .faces
            .iter()
            .filter(|face| face.eye_state == "closed")
            .count();
        let recommendation = recommend(&RecommendationInput {
            technical: TechnicalEvidence {
                global_sharpness: technical.global_sharpness.unwrap_or(0.0),
                sharpness_band: technical.sharpness_band.clone(),
                laplacian_variance: 0.0,
                edge_strength: 0.0,
                local_high_frequency: 0.0,
                directional_blur_ratio: technical.directional_blur_ratio.unwrap_or(1.0),
                blur_level: blur_level_from_label(&technical.blur_level),
                mean_luminance: technical.mean_luminance.unwrap_or(0.0),
                median_luminance: technical.median_luminance.unwrap_or(0.0),
                highlight_clipping_percent: technical.highlight_clipping_percent.unwrap_or(0.0),
                shadow_clipping_percent: technical.shadow_clipping_percent.unwrap_or(0.0),
                channel_clipping_percent: technical.channel_clipping_percent.unwrap_or(0.0),
                technical_quality_score: technical.technical_quality_score.unwrap_or(0.0),
                technical_quality_band: quality_band_from_label(&technical.technical_quality_band),
                confidence: technical.confidence,
            },
            face_count: detail.faces.len(),
            open_eyes,
            possible_closed_eyes,
            group_kind: group.map(|(group, _)| group.kind),
            is_group_representative: group.is_none_or(|(_, member)| member.is_representative),
            group_rank: group.and_then(|(group, member)| {
                group
                    .members
                    .iter()
                    .position(|candidate| candidate.asset_id == member.asset_id)
                    .map(|value| value + 1)
            }),
        });
        repository.upsert_ai_recommendation(&AiRecommendation {
            id: RecommendationId::new(),
            media_asset_id: asset_id,
            similarity_group_id: group.map(|(group, _)| group.id.clone()),
            input_fingerprint: detail.input_fingerprint,
            provider: capture_intelligence::RECOMMENDATION_PROVIDER.into(),
            provider_version: capture_intelligence::RECOMMENDATION_VERSION.into(),
            label: recommendation.label,
            confidence: recommendation.confidence,
            reasons: recommendation.reasons,
            generated_at: now,
            status: AnalysisStatus::Ready,
        })?;
    }
    Ok(())
}

fn intelligence_progress(
    job: &BackgroundJob,
    counts: CaptureIntelligenceTerminalCounts,
    resource_mode: AnalysisResourceMode,
    current_asset_id: Option<String>,
    message: Option<String>,
) -> CaptureIntelligenceProgress {
    CaptureIntelligenceProgress {
        state: workflow_state_label(&job.state),
        stage: job_stage_label(&job.stage),
        resource_mode: resource_mode.as_str().into(),
        items_completed: job.items_completed,
        items_total: job.items_total.unwrap_or(0),
        error_count: job.error_count,
        ready_count: counts.ready,
        unsupported_count: counts.unsupported,
        corrupt_count: counts.corrupt,
        needs_original_count: counts.needs_original,
        failed_count: counts.failed,
        not_applicable_count: counts.not_applicable,
        stale_count: counts.stale,
        current_asset_id,
        current_stage_detail: Some(match job.stage {
            JobStage::Fingerprint => "fingerprint".into(),
            JobStage::AiAnalysis => "technical / faces".into(),
            JobStage::Finalize => "grouping / recommendations".into(),
            _ => "analysis".into(),
        }),
        started_at: job.created_at.to_rfc3339(),
        finished_at: job.finished_at.map(|value| value.to_rfc3339()),
        message,
    }
}

fn record_intelligence_status(
    counts: &mut CaptureIntelligenceTerminalCounts,
    job: &mut BackgroundJob,
    status: &AnalysisStatus,
) {
    counts.total += 1;
    match status {
        AnalysisStatus::Ready => counts.ready += 1,
        AnalysisStatus::Unsupported => counts.unsupported += 1,
        AnalysisStatus::Corrupt => counts.corrupt += 1,
        AnalysisStatus::NeedsOriginal => counts.needs_original += 1,
        AnalysisStatus::Failed => {
            counts.failed += 1;
            job.error_count += 1;
        }
        AnalysisStatus::NotApplicable => counts.not_applicable += 1,
        AnalysisStatus::Stale => counts.stale += 1,
        AnalysisStatus::Pending => {}
    }
}

fn resolve_analysis_preview_path(cache_root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let root = cache_root.canonicalize().ok()?;
    let candidate = root.join(relative).canonicalize().ok()?;
    (candidate.is_file() && candidate.starts_with(root)).then_some(candidate)
}

fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp())
}

fn blur_level_from_label(value: &str) -> BlurEvidenceLevel {
    match value {
        "low" => BlurEvidenceLevel::Low,
        "moderate" => BlurEvidenceLevel::Moderate,
        "high" => BlurEvidenceLevel::High,
        "not_applicable" => BlurEvidenceLevel::NotApplicable,
        _ => BlurEvidenceLevel::Uncertain,
    }
}

fn quality_band_from_label(value: &str) -> TechnicalQualityBand {
    match value {
        "strong" => TechnicalQualityBand::Strong,
        "good" => TechnicalQualityBand::Good,
        "technical_issue" => TechnicalQualityBand::TechnicalIssue,
        "not_applicable" => TechnicalQualityBand::NotApplicable,
        _ => TechnicalQualityBand::Review,
    }
}

fn workflow_state_label(state: &WorkflowRunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "failed".into())
}

fn job_stage_label(stage: &JobStage) -> String {
    serde_json::to_value(stage)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "ai_analysis".into())
}

#[derive(Default)]
struct PreparationCounts {
    ready: u64,
    unsupported: u64,
    corrupt: u64,
    offline: u64,
    failed: u64,
    timeout: u64,
    cancelled: u64,
    current_asset_id: Option<String>,
    current_file_instance_id: Option<String>,
    current_provider: Option<String>,
}

fn prepare_visual_candidates(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    candidates: Vec<persistence::MediaPreparationCandidate>,
    cache_root: &Path,
    adapters: &impl ThumbnailProvider,
    mut on_progress: impl FnMut(&MediaPreparationProgress),
) -> PersistenceResult<MediaPreparationProgress> {
    if candidates.is_empty() {
        return Ok(empty_preparation_progress());
    }
    let now = Utc::now();
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::Metadata,
        items_completed: 0,
        items_total: Some(candidates.len() as u64),
        files_discovered: candidates.len() as u64,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project_id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: Some(serde_json::json!({ "pipeline": "visual-preparation" })),
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    let mut counts = PreparationCounts::default();
    let mut progress = preparation_progress(&job, &counts, None);
    on_progress(&progress);

    let preparation = (|| -> PersistenceResult<()> {
        for (index, initial_candidate) in candidates.into_iter().enumerate() {
            let asset_started = Instant::now();
            let media_type = media_type_from_label(&initial_candidate.media_type)?;
            let resolved = resolve_asset_metadata(select_preparation_source(
                repository,
                initial_candidate,
                &media_type,
            )?)?;
            let candidate = resolved.selected.candidate.clone();
            let source = resolved.selected.source.clone();
            let metadata = resolved.metadata.clone();
            counts.current_asset_id = Some(candidate.asset_id.clone());
            counts.current_file_instance_id = Some(candidate.file_instance_id.clone());
            counts.current_provider = Some("local-metadata".into());
            eprintln!(
                "[captureos][preview] start timestamp={} asset={} file_instance={} queue={}/{} stage=metadata",
                Utc::now().to_rfc3339(),
                candidate.asset_id,
                candidate.file_instance_id,
                index + 1,
                job.items_total.unwrap_or(0)
            );
            for observation in &resolved.observations {
                repository.upsert_capture_time_observation(observation)?;
            }
            repository.upsert_media_metadata(&metadata_record(&candidate, metadata.clone()))?;

            job.stage = JobStage::Thumbnail;
            job.updated_at = Utc::now();
            repository.update_background_job(&job)?;
            progress = preparation_progress(&job, &counts, Some(candidate.filename.clone()));
            on_progress(&progress);

            let generated = match source.as_deref() {
                Some(source) => prepare_previews(
                    adapters,
                    cache_root,
                    &candidate.asset_id,
                    &candidate.file_instance_id,
                    source,
                    &media_type,
                    &candidate.source_fingerprint,
                ),
                None => prepare_previews(
                    adapters,
                    cache_root,
                    &candidate.asset_id,
                    &candidate.file_instance_id,
                    Path::new(""),
                    &media_type,
                    &candidate.source_fingerprint,
                ),
            }
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
            job.stage = JobStage::Preview;
            let provider = generated
                .first()
                .map(|artifact| artifact.provider.clone())
                .unwrap_or_else(|| "metadata-only".into());
            counts.current_provider = Some(provider.clone());
            let terminal = terminal_status(&metadata.status, &generated);
            for artifact in generated {
                repository.upsert_preview_artifact(&PreviewArtifactRecord {
                    id: Uuid::new_v4().to_string(),
                    media_asset_id: candidate.asset_id.clone(),
                    source_file_instance_id: candidate.file_instance_id.clone(),
                    artifact_type: artifact.artifact_type,
                    size_class: artifact.size.as_str().into(),
                    cache_relative_path: artifact.cache_relative_path,
                    provider: artifact.provider,
                    generator_version: GENERATOR_VERSION.into(),
                    source_fingerprint: artifact.source_fingerprint,
                    created_at: Utc::now().to_rfc3339(),
                    status: artifact.status.as_str().into(),
                    failure_reason: artifact.failure_reason,
                })?;
            }
            record_terminal_status(&mut counts, &mut job, &terminal);
            eprintln!(
                "[captureos][preview] terminal timestamp={} asset={} file_instance={} provider={} status={} queue={}/{} elapsed_ms={}",
                Utc::now().to_rfc3339(),
                candidate.asset_id,
                candidate.file_instance_id,
                provider,
                terminal.as_str(),
                index + 1,
                job.items_total.unwrap_or(0),
                asset_started.elapsed().as_millis(),
            );
            job.files_processed += 1;
            job.items_completed = job.files_processed;
            job.updated_at = Utc::now();
            repository.update_background_job(&job)?;
            progress = preparation_progress(&job, &counts, Some(candidate.filename));
            on_progress(&progress);
        }
        Ok(())
    })();

    if let Err(error) = preparation {
        job.state = WorkflowRunState::Failed;
        job.stage = JobStage::Finalize;
        job.error_count += 1;
        job.error_message = Some(error.to_string());
        job.finished_at = Some(Utc::now());
        job.updated_at = job.finished_at.expect("finished time is set");
        progress = preparation_progress(&job, &counts, job.error_message.clone());
        job.resume_metadata = Some(serde_json::json!({
            "pipeline": "visual-preparation",
            "summary": &progress,
        }));
        let _ = repository.update_background_job(&job);
        on_progress(&progress);
        return Err(error);
    }
    job.state = WorkflowRunState::Completed;
    job.stage = JobStage::Finalize;
    job.finished_at = Some(Utc::now());
    job.updated_at = job.finished_at.expect("finished time is set");
    progress = preparation_progress(&job, &counts, None);
    job.resume_metadata = Some(serde_json::json!({
        "pipeline": "visual-preparation",
        "summary": &progress,
    }));
    repository.update_background_job(&job)?;
    on_progress(&progress);
    Ok(progress)
}

pub fn clear_visual_cache(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    cache_root: &Path,
) -> PersistenceResult<()> {
    clear_cache(cache_root).map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    repository.clear_preview_artifacts(project_id)
}

/// Startup recovery for a desktop process that stopped while a local preview worker was active.
/// Artifact records are only written with terminal states, so only the durable job itself needs
/// transitioning here.
pub fn recover_interrupted_visual_preparations(
    repository: &impl CatalogRepository,
) -> PersistenceResult<u64> {
    repository.recover_interrupted_visual_preparations()
}

fn preview_url(artifact_id: Option<String>) -> Option<String> {
    let artifact_id = artifact_id?;
    Uuid::parse_str(&artifact_id).ok()?;
    Some(format!("captureos-preview://localhost/{artifact_id}"))
}

fn empty_preparation_progress() -> MediaPreparationProgress {
    MediaPreparationProgress {
        state: "completed".into(),
        stage: "finalize".into(),
        items_completed: 0,
        items_total: 0,
        error_count: 0,
        ready_count: 0,
        unsupported_count: 0,
        corrupt_count: 0,
        offline_count: 0,
        failed_count: 0,
        timeout_count: 0,
        cancelled_count: 0,
        current_asset_id: None,
        current_file_instance_id: None,
        current_provider: None,
        started_at: Utc::now().to_rfc3339(),
        finished_at: Some(Utc::now().to_rfc3339()),
        message: None,
    }
}

fn cache_size(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let path = entry.path();
            match entry.file_type() {
                Ok(file_type) if file_type.is_file() => {
                    entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
                }
                Ok(file_type) if file_type.is_dir() => cache_size(&path),
                _ => 0, // Do not follow symlinks from a generated-artifact cache.
            }
        })
        .sum()
}

fn source_path(selected_root: Option<&str>, relative_path: &str) -> Option<PathBuf> {
    let root = Path::new(selected_root?).canonicalize().ok()?;
    if !root.is_dir() {
        return None;
    }
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let source = root.join(relative).canonicalize().ok()?;
    source
        .is_file()
        .then_some(source)
        .filter(|path| path.starts_with(&root))
}

#[derive(Debug, Clone)]
struct MetadataSourceInspection {
    candidate: persistence::MediaPreparationCandidate,
    source: Option<PathBuf>,
    metadata: media_visual::ExtractedMetadata,
}

fn inspect_metadata_candidates(
    candidates: Vec<persistence::MediaPreparationCandidate>,
    media_type: &MediaType,
) -> Vec<MetadataSourceInspection> {
    candidates
        .into_iter()
        .map(|candidate| {
            let source = source_path(candidate.selected_root.as_deref(), &candidate.relative_path);
            let metadata = source
                .as_deref()
                .map(|path| extract_metadata(path, media_type))
                .unwrap_or_else(|| media_visual::ExtractedMetadata {
                    status: ArtifactStatus::Offline,
                    failure_reason: Some(
                        "The file instance has no safely resolvable index root or is offline"
                            .into(),
                    ),
                    ..Default::default()
                });
            MetadataSourceInspection {
                candidate,
                source,
                metadata,
            }
        })
        .collect()
}

fn select_preparation_source(
    repository: &impl CatalogRepository,
    initial: persistence::MediaPreparationCandidate,
    media_type: &MediaType,
) -> PersistenceResult<Vec<MetadataSourceInspection>> {
    let copies = repository.preparation_file_instance_candidates(&initial.asset_id)?;
    let candidates = if copies.is_empty() {
        vec![initial]
    } else {
        copies
    };
    let inspections = inspect_metadata_candidates(candidates, media_type);
    (!inspections.is_empty())
        .then_some(inspections)
        .ok_or_else(|| PersistenceError::InvalidData("media asset has no file instances".into()))
}

fn preferred_metadata_source(
    inspections: &[MetadataSourceInspection],
) -> PersistenceResult<&MetadataSourceInspection> {
    inspections
        .iter()
        .find(|inspection| {
            matches!(
                inspection.metadata.status,
                ArtifactStatus::Ready | ArtifactStatus::Unsupported
            )
        })
        .or_else(|| inspections.last())
        .ok_or_else(|| PersistenceError::InvalidData("media asset has no metadata sources".into()))
}

#[derive(Debug, Clone)]
struct CaptureTimeObservationCandidate {
    source_file_instance_id: String,
    candidate: CaptureTimeCandidate,
}

#[derive(Debug, Clone)]
struct ResolvedAssetMetadata {
    /// The physical copy selected for normal visual metadata and preview work. Capture-time
    /// evidence can intentionally come from another equivalent copy; its file-instance ID is
    /// retained only in the local developer diagnostic below.
    selected: MetadataSourceInspection,
    metadata: media_visual::ExtractedMetadata,
    observations: Vec<CaptureTimeObservationRecord>,
    copy_conflict: bool,
}

fn capture_time_candidate(inspection: &MetadataSourceInspection) -> Option<CaptureTimeCandidate> {
    Some(CaptureTimeCandidate {
        raw: inspection.metadata.captured_at_raw.clone()?,
        local: inspection.metadata.captured_at_local.clone()?,
        timezone: inspection.metadata.capture_timezone.clone()?,
        source: inspection.metadata.capture_time_source.clone()?,
        confidence: inspection.metadata.capture_time_confidence.clone()?,
    })
}

fn capture_time_observation_record(
    inspection: &MetadataSourceInspection,
    extracted_at: &str,
) -> CaptureTimeObservationRecord {
    CaptureTimeObservationRecord {
        media_asset_id: inspection.candidate.asset_id.clone(),
        source_file_instance_id: inspection.candidate.file_instance_id.clone(),
        source_fingerprint: inspection.candidate.source_fingerprint.clone(),
        extractor: "local-platform-metadata".into(),
        extractor_version: METADATA_EXTRACTOR_VERSION.into(),
        status: inspection.metadata.status.as_str().into(),
        failure_reason: inspection.metadata.failure_reason.clone(),
        extracted_at: extracted_at.into(),
        captured_at_raw: inspection.metadata.captured_at_raw.clone(),
        captured_at_local: inspection.metadata.captured_at_local.clone(),
        capture_timezone: inspection.metadata.capture_timezone.clone(),
        capture_time_source: inspection.metadata.capture_time_source.clone(),
        capture_time_confidence: inspection.metadata.capture_time_confidence.clone(),
    }
}

/// Resolves one logical asset from the local observations of all currently available physical
/// copies. The selection is provenance first, then consensus, then a stable local ID tie-breaker;
/// neither a filesystem timestamp nor a filename can win over embedded camera evidence.
fn resolve_asset_metadata(
    inspections: Vec<MetadataSourceInspection>,
) -> PersistenceResult<ResolvedAssetMetadata> {
    let selected = preferred_metadata_source(&inspections)?.clone();
    let extracted_at = Utc::now().to_rfc3339();
    let observations = inspections
        .iter()
        .map(|inspection| capture_time_observation_record(inspection, &extracted_at))
        .collect::<Vec<_>>();
    let candidates = inspections
        .iter()
        .filter_map(|inspection| {
            capture_time_candidate(inspection).map(|candidate| CaptureTimeObservationCandidate {
                source_file_instance_id: inspection.candidate.file_instance_id.clone(),
                candidate,
            })
        })
        .collect::<Vec<_>>();

    let highest_priority = candidates
        .iter()
        .map(|candidate| capture_time_priority(Some(&candidate.candidate.source)))
        .max();
    let mut selected_capture_time = None;
    let mut selected_capture_time_file_instance_id = None;
    if let Some(highest_priority) = highest_priority {
        let mut groups =
            BTreeMap::<(String, String, String), Vec<&CaptureTimeObservationCandidate>>::new();
        for candidate in candidates.iter().filter(|candidate| {
            capture_time_priority(Some(&candidate.candidate.source)) == highest_priority
        }) {
            groups
                .entry((
                    candidate.candidate.local.clone(),
                    candidate.candidate.timezone.clone(),
                    candidate.candidate.source.clone(),
                ))
                .or_default()
                .push(candidate);
        }
        // `BTreeMap` provides the stable lexical fallback; reverse count chooses the local
        // consensus before that fallback. A copied file's observation order never changes this.
        let mut groups = groups.into_iter().collect::<Vec<_>>();
        groups.sort_by(|(left_key, left_values), (right_key, right_values)| {
            right_values
                .len()
                .cmp(&left_values.len())
                .then_with(|| left_key.cmp(right_key))
        });
        if let Some((_, mut values)) = groups.into_iter().next() {
            values.sort_by(|left, right| {
                left.source_file_instance_id
                    .cmp(&right.source_file_instance_id)
            });
            if let Some(winner) = values.first() {
                selected_capture_time = Some(winner.candidate.clone());
                selected_capture_time_file_instance_id =
                    Some(winner.source_file_instance_id.clone());
            }
        }
    }

    // Distinct embedded values on physical copies are a diagnostic even when the deterministic
    // resolver can select one. Low-confidence filesystem observations deliberately do not
    // create this warning: copies commonly have different filesystem dates.
    let embedded_values = candidates
        .iter()
        .filter(|candidate| capture_time_priority(Some(&candidate.candidate.source)) >= 80)
        .map(|candidate| {
            (
                candidate.candidate.local.clone(),
                candidate.candidate.timezone.clone(),
            )
        })
        .collect::<HashSet<_>>();
    let copy_conflict = embedded_values.len() > 1;

    let mut metadata = selected.metadata.clone();
    if let Some(candidate) = selected_capture_time.as_ref() {
        metadata.captured_at_raw = Some(candidate.raw.clone());
        metadata.captured_at_local = Some(candidate.local.clone());
        metadata.capture_timezone = Some(candidate.timezone.clone());
        metadata.capture_time_source = Some(candidate.source.clone());
        metadata.capture_time_confidence = Some(candidate.confidence.clone());
    }
    let diagnostics = copy_conflict.then(|| {
        serde_json::json!({
            "kind": "embedded_capture_time_conflict",
            "evidence": "Available physical copies reported different embedded camera capture times. CaptureOS selected the highest-provenance local consensus without changing customer media.",
        })
    });
    let copy_observations = inspections
        .iter()
        .map(|inspection| {
            serde_json::json!({
                "sourceFileInstanceId": inspection.candidate.file_instance_id,
                "status": inspection.metadata.status.as_str(),
                "capturedAtLocal": inspection.metadata.captured_at_local,
                "captureTimezone": inspection.metadata.capture_timezone,
                "captureTimeSource": inspection.metadata.capture_time_source,
                "captureTimeConfidence": inspection.metadata.capture_time_confidence,
            })
        })
        .collect::<Vec<_>>();
    let resolution = serde_json::json!({
        "version": METADATA_EXTRACTOR_VERSION,
        "state": if selected_capture_time.is_some() { "resolved" } else { "unavailable" },
        "selectedSourceFileInstanceId": selected_capture_time_file_instance_id,
        "selected": selected_capture_time,
        "observationCount": inspections.len(),
        "diagnostics": diagnostics.into_iter().collect::<Vec<_>>(),
        "observations": copy_observations,
    });
    if let Some(object) = metadata.raw.as_object_mut() {
        object.insert("captureTimeResolution".into(), resolution);
    } else {
        metadata.raw = serde_json::json!({ "captureTimeResolution": resolution });
    }

    Ok(ResolvedAssetMetadata {
        selected,
        metadata,
        observations,
        copy_conflict,
    })
}

fn terminal_status(
    metadata_status: &ArtifactStatus,
    generated: &[media_visual::GeneratedPreview],
) -> ArtifactStatus {
    if generated.is_empty() {
        return metadata_status.clone();
    }
    let statuses = generated.iter().map(|artifact| &artifact.status);
    if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Ready))
    {
        ArtifactStatus::Ready
    } else if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Timeout))
    {
        ArtifactStatus::Timeout
    } else if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Failed))
    {
        ArtifactStatus::Failed
    } else if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Corrupt))
    {
        ArtifactStatus::Corrupt
    } else if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Offline))
    {
        ArtifactStatus::Offline
    } else if statuses
        .clone()
        .any(|status| matches!(status, ArtifactStatus::Cancelled))
    {
        ArtifactStatus::Cancelled
    } else {
        ArtifactStatus::Unsupported
    }
}

fn record_terminal_status(
    counts: &mut PreparationCounts,
    job: &mut BackgroundJob,
    status: &ArtifactStatus,
) {
    match status {
        ArtifactStatus::Ready => counts.ready += 1,
        ArtifactStatus::Unsupported => counts.unsupported += 1,
        ArtifactStatus::Corrupt => counts.corrupt += 1,
        ArtifactStatus::Offline => counts.offline += 1,
        ArtifactStatus::Failed => {
            counts.failed += 1;
            job.error_count += 1;
        }
        ArtifactStatus::Timeout => {
            counts.timeout += 1;
            job.error_count += 1;
        }
        ArtifactStatus::Cancelled => counts.cancelled += 1,
        ArtifactStatus::Pending | ArtifactStatus::Stale => {
            counts.failed += 1;
            job.error_count += 1;
        }
    }
}

fn metadata_record(
    candidate: &persistence::MediaPreparationCandidate,
    extracted: media_visual::ExtractedMetadata,
) -> MediaMetadataRecord {
    MediaMetadataRecord {
        media_asset_id: candidate.asset_id.clone(),
        source_file_instance_id: candidate.file_instance_id.clone(),
        source_fingerprint: candidate.source_fingerprint.clone(),
        extractor: "local-platform-metadata".into(),
        extractor_version: METADATA_EXTRACTOR_VERSION.into(),
        status: extracted.status.as_str().into(),
        failure_reason: extracted.failure_reason,
        extracted_at: Utc::now().to_rfc3339(),
        mime_type: extracted.mime_type,
        byte_size: extracted.byte_size,
        captured_at_raw: extracted.captured_at_raw,
        captured_at_local: extracted.captured_at_local,
        capture_timezone: extracted.capture_timezone,
        capture_time_source: extracted.capture_time_source,
        capture_time_confidence: extracted.capture_time_confidence,
        width: extracted.width,
        height: extracted.height,
        orientation: extracted.orientation,
        camera_make: extracted.camera_make,
        camera_model: extracted.camera_model,
        lens_make: extracted.lens_make,
        lens_model: extracted.lens_model,
        focal_length_mm: extracted.focal_length_mm,
        focal_length_equivalent_mm: extracted.focal_length_equivalent_mm,
        aperture: extracted.aperture,
        shutter_speed: extracted.shutter_speed,
        iso: extracted.iso,
        exposure_compensation: extracted.exposure_compensation,
        flash: extracted.flash,
        white_balance: extracted.white_balance,
        color_space: extracted.color_space,
        gps_present: extracted.gps_present,
        duration_ms: extracted.duration_ms,
        frame_rate: extracted.frame_rate,
        codec: extracted.codec,
        pixel_format: extracted.pixel_format,
        bitrate: extracted.bitrate,
        audio_streams: extracted.audio_streams,
        video_streams: extracted.video_streams,
        sample_rate: extracted.sample_rate,
        bit_depth: extracted.bit_depth,
        channels: extracted.channels,
        raw_metadata: extracted.raw,
    }
}

fn media_type_from_label(value: &str) -> PersistenceResult<MediaType> {
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
            "unknown persisted media type: {value}"
        ))),
    }
}

fn preparation_progress(
    job: &BackgroundJob,
    counts: &PreparationCounts,
    message: Option<String>,
) -> MediaPreparationProgress {
    MediaPreparationProgress {
        state: format!("{:?}", job.state).to_ascii_lowercase(),
        stage: format!("{:?}", job.stage).to_ascii_lowercase(),
        items_completed: job.items_completed,
        items_total: job.items_total.unwrap_or(0),
        error_count: job.error_count,
        ready_count: counts.ready,
        unsupported_count: counts.unsupported,
        corrupt_count: counts.corrupt,
        offline_count: counts.offline,
        failed_count: counts.failed,
        timeout_count: counts.timeout,
        cancelled_count: counts.cancelled,
        current_asset_id: counts.current_asset_id.clone(),
        current_file_instance_id: counts.current_file_instance_id.clone(),
        current_provider: counts.current_provider.clone(),
        started_at: job.created_at.to_rfc3339(),
        finished_at: job.finished_at.map(|value| value.to_rfc3339()),
        message,
    }
}

/// Performs local read-only ingest pre-flight. It does not copy or create source files.
pub fn preflight_ingest(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: IngestRequest,
    policy: IngestProtectionPolicy,
) -> PersistenceResult<IngestPreflightView> {
    preflight_ingest_with_inspector(
        repository,
        project_id,
        request,
        policy,
        &LocalVolumeInspector,
    )
}

fn preflight_ingest_with_inspector(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    mut request: IngestRequest,
    policy: IngestProtectionPolicy,
    volume_inspector: &impl VolumeInspector,
) -> PersistenceResult<IngestPreflightView> {
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project not found".into()))?;
    request.project_name = project.name;
    let mut report = preflight(&request, &LocalAvailableSpace)
        .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
    append_volume_preflight_warnings(&mut report, volume_inspector)?;
    Ok(IngestPreflightView {
        can_start: report.can_start(),
        report,
        policy: enum_label(&policy)?,
    })
}

/// Creates a durable ingest job and processes each destination independently.
/// Source files are read only; destination copies become final only after BLAKE3 verification.
pub fn start_ingest(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: IngestRequest,
    policy: IngestProtectionPolicy,
    start_request_id: &str,
    on_progress: impl FnMut(&IngestReport),
) -> PersistenceResult<IngestReport> {
    start_ingest_with_inspector(
        repository,
        project_id,
        request,
        policy,
        start_request_id,
        &LocalVolumeInspector,
        on_progress,
    )
}

fn start_ingest_with_inspector(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    request: IngestRequest,
    policy: IngestProtectionPolicy,
    start_request_id: &str,
    volume_inspector: &impl VolumeInspector,
    mut on_progress: impl FnMut(&IngestReport),
) -> PersistenceResult<IngestReport> {
    let start_request_id = uuid::Uuid::parse_str(start_request_id)
        .map_err(|_| {
            PersistenceError::InvalidData("ingest start request ID must be a UUID".into())
        })?
        .to_string();
    if let Some(existing) = repository.ingest_job_by_start_request_id(&start_request_id)? {
        if existing.project_id != *project_id {
            return Err(PersistenceError::InvalidData(
                "ingest start request ID belongs to another project".into(),
            ));
        }
        return repository.ingest_report(&existing.id)?.ok_or_else(|| {
            PersistenceError::InvalidData("existing ingest report is unavailable".into())
        });
    }
    let preflight = preflight_ingest_with_inspector(
        repository,
        project_id,
        request.clone(),
        policy.clone(),
        volume_inspector,
    )?;
    if !preflight.can_start {
        return Err(PersistenceError::InvalidData(
            "ingest pre-flight has blocking errors".into(),
        ));
    }
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project not found".into()))?;
    let now = Utc::now();
    let mut job = IngestJob {
        id: IngestJobId::new(),
        project_id: project.id.clone(),
        start_request_id: Some(start_request_id.clone()),
        state: IngestJobState::Running,
        policy,
        guardian_state: CaptureGuardianState::Unprotected,
        safe_to_eject: false,
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    if let Err(error) = repository.insert_ingest_job(&job) {
        if let Some(existing) = repository.ingest_job_by_start_request_id(&start_request_id)? {
            return repository.ingest_report(&existing.id)?.ok_or_else(|| {
                PersistenceError::InvalidData("existing ingest report is unavailable".into())
            });
        }
        return Err(error);
    }
    audit(repository, &job.id, "INGEST_CREATED", None)?;
    audit(
        repository,
        &job.id,
        "PREFLIGHT_COMPLETED",
        Some(format!(
            "{} source files / {} bytes",
            preflight
                .report
                .sources
                .iter()
                .map(|source| source.file_count)
                .sum::<u64>(),
            preflight.report.total_source_bytes
        )),
    )?;

    let mut sources = Vec::new();
    for inventory in &preflight.report.sources {
        let selected_root = canonical_directory(&inventory.selected_path)?;
        let volume = resolve_storage_volume(
            repository,
            volume_inspector
                .inspect(&selected_root)
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
        )?;
        let root = register_ingest_root(repository, &project, &selected_root, &volume)?;
        let source = IngestSource {
            id: IngestSourceId::new(),
            ingest_job_id: job.id.clone(),
            label: inventory.label.clone(),
            selected_path: inventory.selected_path.clone(),
            storage_volume_id: volume.id,
            file_count: inventory.file_count,
            total_bytes: inventory.total_bytes,
            status: IngestItemState::Queued,
        };
        repository.insert_ingest_source(&source, &[])?;
        sources.push((inventory, source, root));
    }

    let mut destinations = Vec::new();
    for destination in &preflight.report.destinations {
        let selected_root = canonical_directory(&destination.selected_path)?;
        let volume = resolve_storage_volume(
            repository,
            volume_inspector
                .inspect(&selected_root)
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
        )?;
        let root = register_ingest_root(repository, &project, &selected_root, &volume)?;
        let persisted = IngestDestination {
            id: IngestDestinationId::new(),
            ingest_job_id: job.id.clone(),
            role: destination.role.clone(),
            selected_path: destination.selected_path.clone(),
            storage_volume_id: volume.id,
            available_bytes: destination.available_bytes,
            required_bytes: destination.required_bytes,
            writable: destination.writable,
            status: IngestItemState::Queued,
        };
        repository.insert_ingest_destination(&persisted)?;
        destinations.push((destination, persisted, root));
    }

    let layout = DefaultDestinationLayout;
    for (inventory, source, source_root) in &sources {
        for file in &inventory.files {
            for (destination, persisted_destination, destination_root) in &destinations {
                let final_path = layout
                    .destination_file(
                        Path::new(&destination.selected_path),
                        &project.name,
                        &source.label,
                        &file.relative_path,
                    )
                    .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
                let destination_relative_path = final_path
                    .strip_prefix(Path::new(&destination.selected_path))
                    .map_err(|_| PersistenceError::InvalidData("destination escaped root".into()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut item = IngestItemRecord {
                    id: IngestItemId::new(),
                    ingest_job_id: job.id.clone(),
                    ingest_source_id: source.id.clone(),
                    ingest_destination_id: persisted_destination.id.clone(),
                    media_asset_id: None,
                    source_relative_path: file.relative_path.clone(),
                    destination_relative_path,
                    byte_size: file.byte_size,
                    source_hash: None,
                    destination_hash: None,
                    state: IngestItemState::Copying,
                    verification_state: VerificationState::NotVerified,
                    source_file_instance_id: None,
                    destination_file_instance_id: None,
                    error_message: None,
                    updated_at: Utc::now(),
                };
                item = repository.upsert_ingest_item(&item)?;
                audit(
                    repository,
                    &job.id,
                    "COPY_STARTED",
                    Some(format!(
                        "{} → {}",
                        file.relative_path, destination.selected_path
                    )),
                )?;
                let outcome = copy_and_verify(
                    &file.source_path,
                    Path::new(&destination.selected_path),
                    &final_path,
                    || false,
                    |_| {},
                );
                match outcome {
                    Ok(CopyVerificationOutcome::Verified(result)) => {
                        audit(
                            repository,
                            &job.id,
                            "COPY_COMPLETED",
                            Some(file.relative_path.clone()),
                        )?;
                        audit(
                            repository,
                            &job.id,
                            "VERIFY_STARTED",
                            Some(file.relative_path.clone()),
                        )?;
                        let asset =
                            verified_asset(repository, &project, file, &result.source_hash)?;
                        let source_instance = repository.upsert_file_instance(&FileInstance {
                            id: FileInstanceId::new(),
                            media_asset_id: asset.id.clone(),
                            storage_volume_id: source.storage_volume_id.clone(),
                            index_root_id: Some(source_root.id.clone()),
                            relative_path: file.relative_path.clone(),
                            observed_at: Utc::now(),
                            is_available: true,
                        })?;
                        let destination_instance =
                            repository.upsert_file_instance(&FileInstance {
                                id: FileInstanceId::new(),
                                media_asset_id: asset.id.clone(),
                                storage_volume_id: persisted_destination.storage_volume_id.clone(),
                                index_root_id: Some(destination_root.id.clone()),
                                relative_path: item.destination_relative_path.clone(),
                                observed_at: Utc::now(),
                                is_available: true,
                            })?;
                        insert_copy_relationships(
                            repository,
                            &destination_instance,
                            &source_instance,
                        )?;
                        item.media_asset_id = Some(asset.id);
                        item.source_hash = Some(result.source_hash);
                        item.destination_hash = Some(result.destination_hash);
                        item.source_file_instance_id = Some(source_instance.id);
                        item.destination_file_instance_id = Some(destination_instance.id);
                        item.state = IngestItemState::Verified;
                        item.verification_state = VerificationState::Verified;
                        item.updated_at = Utc::now();
                        item = repository.upsert_ingest_item(&item)?;
                        audit(
                            repository,
                            &job.id,
                            "VERIFY_SUCCEEDED",
                            Some(item.source_relative_path.clone()),
                        )?;
                    }
                    Ok(CopyVerificationOutcome::Conflict { message }) => {
                        item.state = IngestItemState::Conflict;
                        item.error_message = Some(message.clone());
                        item.updated_at = Utc::now();
                        repository.upsert_ingest_item(&item)?;
                        audit(repository, &job.id, "VERIFY_FAILED", Some(message))?;
                    }
                    Ok(CopyVerificationOutcome::VerificationFailed { message }) => {
                        item.state = IngestItemState::Failed;
                        item.verification_state = VerificationState::Failed;
                        item.error_message = Some(message.clone());
                        item.updated_at = Utc::now();
                        repository.upsert_ingest_item(&item)?;
                        audit(repository, &job.id, "VERIFY_FAILED", Some(message))?;
                    }
                    Ok(CopyVerificationOutcome::SourceChanged { message }) => {
                        item.state = IngestItemState::SourceChanged;
                        item.verification_state = VerificationState::SourceChanged;
                        item.error_message = Some(message.clone());
                        item.updated_at = Utc::now();
                        repository.upsert_ingest_item(&item)?;
                        audit(repository, &job.id, "VERIFY_FAILED", Some(message))?;
                    }
                    Ok(CopyVerificationOutcome::Cancelled) => {
                        item.state = IngestItemState::Cancelled;
                        item.error_message =
                            Some("Copy cancelled; partial file is retained safely.".into());
                        item.updated_at = Utc::now();
                        repository.upsert_ingest_item(&item)?;
                    }
                    Err(error) => {
                        item.state = IngestItemState::Unavailable;
                        item.verification_state = VerificationState::Unavailable;
                        let message = error.to_string();
                        item.error_message = Some(message.clone());
                        item.updated_at = Utc::now();
                        repository.upsert_ingest_item(&item)?;
                        let event_type = if !file.source_path.exists()
                            || !Path::new(&destination.selected_path).exists()
                        {
                            "VOLUME_DISCONNECTED"
                        } else {
                            "VERIFY_FAILED"
                        };
                        audit(
                            repository,
                            &job.id,
                            event_type,
                            Some(format!("{}: {message}", file.relative_path)),
                        )?;
                    }
                }
                job.updated_at = Utc::now();
                repository.update_ingest_job(&job)?;
                if let Some(report) = repository.ingest_report(&job.id)? {
                    on_progress(&report);
                }
            }
        }
    }
    update_ingest_endpoint_statuses(repository, &job.id)?;
    finalize_ingest_guardian(repository, &mut job)?;
    let report = repository
        .ingest_report(&job.id)?
        .ok_or_else(|| PersistenceError::InvalidData("ingest report disappeared".into()))?;
    on_progress(&report);
    Ok(report)
}

pub fn ingest_history(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
) -> PersistenceResult<Vec<persistence::IngestJobSummary>> {
    repository.ingest_jobs_for_project(project_id)
}

pub fn ingest_report(
    repository: &impl CatalogRepository,
    job_id: &IngestJobId,
) -> PersistenceResult<Option<IngestReport>> {
    repository.ingest_report(job_id)
}

/// Re-runs an interrupted or attention-needed ingest as a new audited pass.
/// Previously verified destination files are matched by BLAKE3 and registered as
/// existing evidence instead of being copied again.
pub fn restart_ingest(
    repository: &impl CatalogRepository,
    job_id: &IngestJobId,
    on_progress: impl FnMut(&IngestReport),
) -> PersistenceResult<IngestReport> {
    let job = repository
        .ingest_job(job_id)?
        .ok_or_else(|| PersistenceError::InvalidData("ingest job not found".into()))?;
    let project = repository
        .get_project(&job.project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project not found".into()))?;
    let sources = repository
        .ingest_sources(job_id)?
        .into_iter()
        .map(|source| ingest::IngestSourceInput {
            label: source.label,
            selected_path: source.selected_path,
        })
        .collect::<Vec<_>>();
    let destinations = repository.ingest_destinations(job_id)?;
    let master = destinations
        .iter()
        .find(|destination| destination.role == "master")
        .ok_or_else(|| PersistenceError::InvalidData("ingest has no master destination".into()))?;
    audit(
        repository,
        job_id,
        "JOB_RESUMED",
        Some("Started a new retry pass; verified files will be reused as evidence.".into()),
    )?;
    start_ingest(
        repository,
        &project.id,
        IngestRequest {
            project_name: project.name,
            sources,
            master: ingest::IngestDestinationInput {
                role: IngestDestinationRole::Master,
                selected_path: master.selected_path.clone(),
            },
            backups: destinations
                .into_iter()
                .filter(|destination| destination.role == "backup")
                .map(|destination| ingest::IngestDestinationInput {
                    role: IngestDestinationRole::Backup,
                    selected_path: destination.selected_path,
                })
                .collect(),
        },
        job.policy,
        &Uuid::new_v4().to_string(),
        on_progress,
    )
}

pub fn recover_interrupted_ingests(repository: &impl CatalogRepository) -> PersistenceResult<u64> {
    repository.recover_interrupted_ingests()
}

/// Runs the Milestone 1 index pipeline synchronously on the caller's worker thread.
/// It opens source files only for metadata and hashing; all writes target the local catalog.
pub fn index_local_folder(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    selected_path: &str,
    on_progress: impl FnMut(&JobView),
) -> PersistenceResult<JobView> {
    index_local_folder_with_inspector(
        repository,
        project_id,
        selected_path,
        &LocalVolumeInspector,
        on_progress,
    )
}

fn index_local_folder_with_inspector(
    repository: &impl CatalogRepository,
    project_id: &ProjectId,
    selected_path: &str,
    volume_inspector: &impl VolumeInspector,
    mut on_progress: impl FnMut(&JobView),
) -> PersistenceResult<JobView> {
    let project = repository
        .get_project(project_id)?
        .ok_or_else(|| PersistenceError::InvalidData("project not found".into()))?;
    let selected_root = canonical_directory(selected_path)?;
    let volume = resolve_storage_volume(
        repository,
        volume_inspector
            .inspect(&selected_root)
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
    )?;
    let mut root = IndexRoot {
        id: IndexRootId::new(),
        project_id: project.id.clone(),
        selected_path: selected_root.to_string_lossy().into_owned(),
        storage_volume_id: volume.id.clone(),
        added_at: Utc::now(),
        last_indexed_at: None,
        status: IndexRootStatus::Indexing,
    };
    root = repository.register_index_root(&root)?;
    let now = Utc::now();
    let mut job = BackgroundJob {
        id: JobId::new(),
        state: WorkflowRunState::Running,
        stage: JobStage::Discover,
        items_completed: 0,
        items_total: None,
        files_discovered: 0,
        files_processed: 0,
        error_count: 0,
        project_id: Some(project.id.clone()),
        index_root_id: Some(root.id.clone()),
        error_message: None,
        resume_metadata: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
    };
    repository.insert_background_job(&job)?;
    // A completed reindex reactivates the files it sees; entries left unavailable
    // are an honest record of files no longer present under this selected root.
    repository.mark_root_instances_unavailable(&root.id)?;
    on_progress(&job_view(job.clone()));

    let result = scan_read_only(&selected_root, |event| {
        apply_index_event(repository, &project, &root, &mut job, event)?;
        on_progress(&job_view(job.clone()));
        Ok(())
    });
    match result {
        Ok(_) => {
            if let Err(error) = repository.associate_sidecars_for_root(&root.id) {
                job.state = WorkflowRunState::Failed;
                job.error_message = Some(error.to_string());
                job.updated_at = Utc::now();
                job.finished_at = Some(job.updated_at);
                root.status = IndexRootStatus::Failed;
                repository.update_background_job(&job)?;
                repository.update_index_root(&root)?;
                let view = job_view(job);
                on_progress(&view);
                return Ok(view);
            }
            job.state = WorkflowRunState::Completed;
            job.stage = JobStage::Finalize;
            job.items_total = Some(job.files_discovered);
            job.items_completed = job.files_processed;
            job.updated_at = Utc::now();
            job.finished_at = Some(job.updated_at);
            root.status = IndexRootStatus::Ready;
            root.last_indexed_at = Some(job.updated_at);
            repository.update_background_job(&job)?;
            repository.update_index_root(&root)?;
            let view = job_view(job);
            on_progress(&view);
            Ok(view)
        }
        Err(error) => {
            job.state = WorkflowRunState::Failed;
            job.error_message = Some(error.to_string());
            job.updated_at = Utc::now();
            job.finished_at = Some(job.updated_at);
            root.status = IndexRootStatus::Failed;
            repository.update_background_job(&job)?;
            repository.update_index_root(&root)?;
            let view = job_view(job);
            on_progress(&view);
            Ok(view)
        }
    }
}

fn apply_index_event(
    repository: &impl CatalogRepository,
    project: &Project,
    root: &IndexRoot,
    job: &mut BackgroundJob,
    event: IndexEvent,
) -> Result<(), String> {
    match event {
        IndexEvent::Stage(stage) => job.stage = stage,
        IndexEvent::FileDiscovered { count } => job.files_discovered = count,
        IndexEvent::Warning(warning) => {
            job.error_count += 1;
            repository
                .insert_index_issue(&IndexIssue {
                    id: IndexIssueId::new(),
                    job_id: job.id.clone(),
                    relative_path: warning.relative_path.map(|path| path.as_str().to_owned()),
                    severity: warning.severity,
                    message: warning.message,
                    created_at: Utc::now(),
                })
                .map_err(|error| error.to_string())?;
        }
        IndexEvent::Candidate(candidate) => {
            job.stage = JobStage::Persist;
            persist_candidate(repository, project, root, candidate)
                .map_err(|error| error.to_string())?;
            job.files_processed += 1;
            job.items_completed = job.files_processed;
        }
    }
    job.updated_at = Utc::now();
    repository
        .update_background_job(job)
        .map_err(|error| error.to_string())
}

fn persist_candidate(
    repository: &impl CatalogRepository,
    project: &Project,
    root: &IndexRoot,
    candidate: IndexCandidate,
) -> PersistenceResult<()> {
    let asset = match repository.media_asset_by_fingerprint(
        &project.id,
        &candidate.media_type,
        candidate.extension.as_deref(),
        &candidate.fingerprint,
    )? {
        Some(asset) => asset,
        None => {
            let asset = MediaAsset {
                id: MediaAssetId::new(),
                project_id: project.id.clone(),
                media_type: candidate.media_type.clone(),
                display_name: candidate.display_name.clone(),
                extension: candidate.extension.clone(),
                captured_at: None,
                fingerprint: candidate.fingerprint.clone(),
                created_at: Utc::now(),
            };
            repository.insert_media_asset(&asset)?;
            asset
        }
    };
    repository.upsert_file_instance(&FileInstance {
        id: FileInstanceId::new(),
        media_asset_id: asset.id,
        storage_volume_id: root.storage_volume_id.clone(),
        index_root_id: Some(root.id.clone()),
        relative_path: candidate.relative_path.as_str().to_owned(),
        observed_at: Utc::now(),
        is_available: true,
    })?;
    Ok(())
}

fn canonical_directory(selected_path: &str) -> PersistenceResult<PathBuf> {
    let path = Path::new(selected_path).canonicalize().map_err(|error| {
        PersistenceError::InvalidData(format!("selected folder is unavailable: {error}"))
    })?;
    if !path.is_dir() {
        return Err(PersistenceError::InvalidData(
            "selected path is not a folder".into(),
        ));
    }
    Ok(path)
}

fn resolve_storage_volume(
    repository: &impl CatalogRepository,
    observation: VolumeObservation,
) -> PersistenceResult<StorageVolume> {
    if let Some(mut existing) =
        repository.storage_volume_by_filesystem_identity(&observation.filesystem_identity)?
    {
        apply_volume_observation(&mut existing, observation);
        repository.update_storage_volume(&existing)?;
        return Ok(existing);
    }
    let volume = StorageVolume {
        id: StorageVolumeId::new(),
        display_name: observation.display_name,
        filesystem_identity: Some(observation.filesystem_identity),
        capacity_bytes: observation.capacity_bytes,
        mount_state: MountState::Online,
        mount_location: observation.mount_location,
        last_seen_at: Some(Utc::now()),
        filesystem_type: observation.filesystem_type,
        classification: observation.classification,
    };
    repository.insert_storage_volume(&volume)?;
    Ok(volume)
}

fn refresh_known_volume_metadata(
    repository: &impl CatalogRepository,
    roots: &[IndexRoot],
) -> PersistenceResult<()> {
    let inspector = LocalVolumeInspector;
    for root in roots {
        let Ok(observation) = inspector.inspect(Path::new(&root.selected_path)) else {
            continue;
        };
        let Some(mut volume) = repository.get_storage_volume(&root.storage_volume_id)? else {
            continue;
        };
        if volume.filesystem_identity.as_deref() == Some(observation.filesystem_identity.as_str()) {
            apply_volume_observation(&mut volume, observation);
            repository.update_storage_volume(&volume)?;
        }
    }
    Ok(())
}

fn apply_volume_observation(volume: &mut StorageVolume, observation: VolumeObservation) {
    volume.display_name = observation.display_name;
    volume.mount_state = MountState::Online;
    volume.mount_location = observation.mount_location;
    volume.capacity_bytes = observation.capacity_bytes;
    volume.last_seen_at = Some(Utc::now());
    volume.filesystem_type = observation.filesystem_type;
    volume.classification = observation.classification;
}

fn append_volume_preflight_warnings(
    report: &mut PreflightReport,
    volume_inspector: &impl VolumeInspector,
) -> PersistenceResult<()> {
    let mut destination_volumes = Vec::new();
    for destination in &report.destinations {
        let observation = volume_inspector
            .inspect(Path::new(&destination.selected_path))
            .map_err(|error| PersistenceError::InvalidData(error.to_string()))?;
        destination_volumes.push(observation.filesystem_identity);
    }
    let distinct_volumes = destination_volumes
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if destination_volumes.len() > 1 && distinct_volumes < destination_volumes.len() {
        report.issues.push(PreflightIssue {
            severity: PreflightSeverity::Warning,
            code: "same_volume_redundancy".into(),
            message: "Master and backup are on the same storage device and do not provide independent device protection.".into(),
        });
    }
    Ok(())
}

fn register_ingest_root(
    repository: &impl CatalogRepository,
    project: &Project,
    selected_root: &Path,
    volume: &StorageVolume,
) -> PersistenceResult<IndexRoot> {
    repository.register_index_root(&IndexRoot {
        id: IndexRootId::new(),
        project_id: project.id.clone(),
        selected_path: selected_root.to_string_lossy().into_owned(),
        storage_volume_id: volume.id.clone(),
        added_at: Utc::now(),
        last_indexed_at: Some(Utc::now()),
        status: IndexRootStatus::Ready,
    })
}

fn verified_asset(
    repository: &impl CatalogRepository,
    project: &Project,
    file: &ingest::DiscoveredSourceFile,
    content_hash: &str,
) -> PersistenceResult<MediaAsset> {
    if let Some(asset) = repository.media_asset_by_content_hash(
        &project.id,
        &file.media_type,
        file.extension.as_deref(),
        file.byte_size,
        content_hash,
    )? {
        return Ok(asset);
    }
    let display_name = Path::new(&file.relative_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&file.relative_path)
        .to_owned();
    let asset = MediaAsset {
        id: MediaAssetId::new(),
        project_id: project.id.clone(),
        media_type: file.media_type.clone(),
        display_name,
        extension: file.extension.clone(),
        captured_at: None,
        fingerprint: MediaFingerprint {
            cryptographic_hash: Some(content_hash.into()),
            byte_size: Some(file.byte_size),
            ..Default::default()
        },
        created_at: Utc::now(),
    };
    repository.insert_media_asset(&asset)?;
    Ok(asset)
}

fn insert_copy_relationships(
    repository: &impl CatalogRepository,
    destination: &FileInstance,
    source: &FileInstance,
) -> PersistenceResult<()> {
    for kind in [
        RelationshipKind::CopiedFrom,
        RelationshipKind::VerifiedCopyOf,
    ] {
        repository.insert_relationship(&Relationship {
            id: RelationshipId::new(),
            source: EntityRef {
                kind: EntityKind::FileInstance,
                id: destination.id.to_string(),
            },
            target: EntityRef {
                kind: EntityKind::FileInstance,
                id: source.id.to_string(),
            },
            kind,
            confidence: None,
            provenance: Provenance {
                source: "ingest".into(),
                algorithm_id: Some("blake3-copy-verification".into()),
                algorithm_version: Some("1".into()),
                produced_at: Utc::now(),
                human_confirmed: false,
            },
            created_at: Utc::now(),
        })?;
    }
    Ok(())
}

fn finalize_ingest_guardian(
    repository: &impl CatalogRepository,
    job: &mut IngestJob,
) -> PersistenceResult<()> {
    let items = repository.ingest_items(&job.id)?;
    let destinations = repository.ingest_destinations(&job.id)?;
    let has_failures = items.iter().any(|item| {
        matches!(
            item.state,
            IngestItemState::Failed
                | IngestItemState::Conflict
                | IngestItemState::SourceChanged
                | IngestItemState::Unavailable
                | IngestItemState::Cancelled
        )
    });
    let all_verified_for = |role: &str| {
        let destination_ids = destinations
            .iter()
            .filter(|destination| destination.role == role)
            .map(|destination| destination.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        !destination_ids.is_empty()
            && items
                .iter()
                .filter(|item| {
                    destination_ids.contains(item.ingest_destination_id.to_string().as_str())
                })
                .all(|item| item.state == IngestItemState::Verified)
    };
    let master_verified = all_verified_for("master");
    let backup_verified = all_verified_for("backup");
    let distinct_volumes = destinations
        .iter()
        .map(|destination| destination.storage_volume_id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let independent = destinations.len() > 1 && distinct_volumes == destinations.len();

    job.guardian_state = if has_failures {
        CaptureGuardianState::NeedsAttention
    } else if !master_verified {
        CaptureGuardianState::Unprotected
    } else if job.policy == IngestProtectionPolicy::Basic {
        CaptureGuardianState::MasterCopy
    } else if backup_verified && independent {
        CaptureGuardianState::Protected
    } else {
        CaptureGuardianState::PartiallyProtected
    };
    job.safe_to_eject = match job.policy {
        IngestProtectionPolicy::Basic => master_verified && !has_failures,
        IngestProtectionPolicy::Standard => {
            master_verified && backup_verified && independent && !has_failures
        }
    };
    job.state = if has_failures {
        IngestJobState::NeedsAttention
    } else {
        IngestJobState::Completed
    };
    job.updated_at = Utc::now();
    job.finished_at = Some(job.updated_at);
    repository.update_ingest_job(job)?;
    audit(
        repository,
        &job.id,
        "GUARDIAN_STATUS_CHANGED",
        Some(enum_label(&job.guardian_state)?),
    )?;
    audit(repository, &job.id, "INGEST_COMPLETED", None)?;
    Ok(())
}

fn update_ingest_endpoint_statuses(
    repository: &impl CatalogRepository,
    job_id: &IngestJobId,
) -> PersistenceResult<()> {
    let items = repository.ingest_items(job_id)?;
    for source in repository.ingest_sources(job_id)? {
        let state = endpoint_state(
            items
                .iter()
                .filter(|item| item.ingest_source_id.to_string() == source.id),
        );
        repository.update_ingest_source_status(
            &IngestSourceId::try_from(source.id.as_str())
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
            state,
        )?;
    }
    for destination in repository.ingest_destinations(job_id)? {
        let state = endpoint_state(
            items
                .iter()
                .filter(|item| item.ingest_destination_id.to_string() == destination.id),
        );
        repository.update_ingest_destination_status(
            &IngestDestinationId::try_from(destination.id.as_str())
                .map_err(|error| PersistenceError::InvalidData(error.to_string()))?,
            state,
        )?;
    }
    Ok(())
}

fn endpoint_state<'a>(items: impl Iterator<Item = &'a IngestItemRecord>) -> IngestItemState {
    let items = items.collect::<Vec<_>>();
    if !items.is_empty()
        && items
            .iter()
            .all(|item| item.state == IngestItemState::Verified)
    {
        return IngestItemState::Verified;
    }
    for state in [
        IngestItemState::SourceChanged,
        IngestItemState::Unavailable,
        IngestItemState::Conflict,
        IngestItemState::Cancelled,
        IngestItemState::Failed,
    ] {
        if items.iter().any(|item| item.state == state) {
            return state;
        }
    }
    IngestItemState::Queued
}

fn audit(
    repository: &impl CatalogRepository,
    job_id: &IngestJobId,
    event_type: &str,
    detail: Option<String>,
) -> PersistenceResult<()> {
    repository.insert_ingest_audit_event(&IngestAuditEvent {
        id: IngestAuditEventId::new(),
        ingest_job_id: job_id.clone(),
        event_type: event_type.into(),
        detail,
        created_at: Utc::now(),
    })
}

fn enum_label<T: Serialize>(value: &T) -> PersistenceResult<String> {
    serde_json::to_string(value)
        .map(|value| value.trim_matches('"').to_owned())
        .map_err(Into::into)
}

fn project_view(project: Project) -> ProjectView {
    ProjectView {
        id: project.id.to_string(),
        name: project.name,
    }
}

fn project_library_item(item: PersistedProjectLibraryItem) -> ProjectLibraryItem {
    ProjectLibraryItem {
        id: item.id,
        name: item.name,
        created_at: item.created_at,
        last_activity_at: item.last_activity_at,
        media_asset_count: item.media_asset_count,
        storage_volume_count: item.storage_volume_count,
        protection_state: item.protection_state,
    }
}

fn index_root_view(root: IndexRoot) -> IndexRootView {
    IndexRootView {
        id: root.id.to_string(),
        selected_path: root.selected_path,
        status: format!("{:?}", root.status).to_ascii_lowercase(),
        last_indexed_at: root.last_indexed_at.map(|value| value.to_rfc3339()),
        storage_volume_id: root.storage_volume_id.to_string(),
    }
}

fn job_view(job: BackgroundJob) -> JobView {
    JobView {
        id: job.id.to_string(),
        state: format!("{:?}", job.state).to_ascii_lowercase(),
        stage: format!("{:?}", job.stage).to_ascii_lowercase(),
        files_discovered: job.files_discovered,
        files_processed: job.files_processed,
        error_count: job.error_count,
        started_at: job.created_at.to_rfc3339(),
        finished_at: job.finished_at.map(|value| value.to_rfc3339()),
        error_message: job.error_message,
    }
}

pub fn load_golden_shoot(repository: &impl CatalogRepository) -> PersistenceResult<CoreStatus> {
    let manifest: GoldenShootManifest = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/golden-shoot/manifest.json"
    )))?;
    if manifest.fixture_version != 1 {
        return Err(PersistenceError::InvalidData(
            "unsupported Golden Shoot fixture version".into(),
        ));
    }
    let created_at = Utc
        .timestamp_opt(1_704_067_200, 0)
        .single()
        .expect("fixed timestamp is valid");
    let project = Project {
        id: pid(1),
        name: manifest.project_name.clone(),
        created_at,
    };
    let shoot = Shoot {
        id: sid(2),
        project_id: project.id.clone(),
        name: "Golden Wedding — 2024-01-01".into(),
        captured_at: Some(created_at),
    };
    let ceremony = Moment {
        id: mid(3),
        shoot_id: shoot.id.clone(),
        name: "Ceremony".into(),
        started_at: Some(created_at),
        ended_at: Some(created_at),
    };
    let partner_a = PersonCluster {
        id: person_id(4),
        project_id: project.id.clone(),
        display_name: Some("Partner A".into()),
        created_at,
    };
    let partner_b = PersonCluster {
        id: person_id(5),
        project_id: project.id.clone(),
        display_name: Some("Partner B".into()),
        created_at,
    };
    let master = volume(
        6,
        "Master SSD",
        MountState::Online,
        Some("/fixture/master"),
        Some("fixture-master"),
    );
    let backup = volume(
        7,
        "Backup HDD",
        MountState::Offline,
        None,
        Some("fixture-backup"),
    );
    let camera_a = CaptureDevice {
        id: device_id(8),
        project_id: project.id.clone(),
        display_name: "Camera A".into(),
        device_type: "camera".into(),
    };
    let camera_b = CaptureDevice {
        id: device_id(9),
        project_id: project.id.clone(),
        display_name: "Camera B".into(),
        device_type: "camera".into(),
    };
    let recorder = CaptureDevice {
        id: device_id(10),
        project_id: project.id.clone(),
        display_name: "Audio Recorder".into(),
        device_type: "audio_recorder".into(),
    };
    let raw = asset(
        11,
        &project,
        MediaType::RawPhoto,
        "IMG_8421.ARW",
        created_at,
        "golden-raw-1",
    );
    let jpeg = asset(
        12,
        &project,
        MediaType::Jpeg,
        "Final_0294.jpg",
        created_at,
        "golden-jpeg-1",
    );
    let video = asset(
        13,
        &project,
        MediaType::Video,
        "C0001.MP4",
        created_at,
        "golden-video-1",
    );
    let audio = asset(
        14,
        &project,
        MediaType::Audio,
        "REC0001.WAV",
        created_at,
        "golden-audio-1",
    );
    let unprotected_raw = asset(
        15,
        &project,
        MediaType::RawPhoto,
        "IMG_8422.ARW",
        created_at,
        "golden-raw-2",
    );

    repository.insert_project(&project)?;
    repository.insert_shoot(&shoot)?;
    repository.insert_moment(&ceremony)?;
    repository.insert_person_cluster(&partner_a)?;
    repository.insert_person_cluster(&partner_b)?;
    repository.insert_storage_volume(&master)?;
    repository.insert_storage_volume(&backup)?;
    repository.insert_capture_device(&camera_a)?;
    repository.insert_capture_device(&camera_b)?;
    repository.insert_capture_device(&recorder)?;
    for item in [&raw, &jpeg, &video, &audio, &unprotected_raw] {
        repository.insert_media_asset(item)?;
    }

    let raw_master = file(
        21,
        &raw,
        &master,
        "GoldenWedding/RAW/IMG_8421.ARW",
        created_at,
        true,
    );
    let raw_backup = file(
        22,
        &raw,
        &backup,
        "GoldenWedding/RAW/IMG_8421.ARW",
        created_at,
        false,
    );
    let jpeg_master = file(
        23,
        &jpeg,
        &master,
        "GoldenWedding/Exports/Final_0294.jpg",
        created_at,
        true,
    );
    let video_master = file(
        24,
        &video,
        &master,
        "GoldenWedding/Video/C0001.MP4",
        created_at,
        true,
    );
    let audio_master = file(
        25,
        &audio,
        &master,
        "GoldenWedding/Audio/REC0001.WAV",
        created_at,
        true,
    );
    let unprotected_master = file(
        26,
        &unprotected_raw,
        &master,
        "GoldenWedding/RAW/IMG_8422.ARW",
        created_at,
        true,
    );
    for item in [
        &raw_master,
        &raw_backup,
        &jpeg_master,
        &video_master,
        &audio_master,
        &unprotected_master,
    ] {
        repository.insert_file_instance(item)?;
    }

    repository.insert_derivative(&Derivative {
        id: derivative_id(31),
        source_asset_id: raw.id.clone(),
        derived_asset_id: jpeg.id.clone(),
        recipe_hint: Some("synthetic export lineage".into()),
        created_at,
    })?;
    repository.insert_backup_copy(&BackupCopy {
        id: backup_id(32),
        source_file_instance_id: raw_master.id.clone(),
        backup_file_instance_id: raw_backup.id.clone(),
        verified_at: Some(created_at),
        created_at,
    })?;
    for relationship in [
        relation(
            41,
            &raw,
            EntityKind::Moment,
            ceremony.id.to_string(),
            RelationshipKind::BelongsToMoment,
            Some(0.87),
            created_at,
        ),
        relation(
            42,
            &raw,
            EntityKind::PersonCluster,
            partner_a.id.to_string(),
            RelationshipKind::ContainsPerson,
            Some(0.94),
            created_at,
        ),
        relation(
            43,
            &raw,
            EntityKind::PersonCluster,
            partner_b.id.to_string(),
            RelationshipKind::ContainsPerson,
            Some(0.93),
            created_at,
        ),
        relation(
            44,
            &audio,
            EntityKind::Moment,
            ceremony.id.to_string(),
            RelationshipKind::BelongsToMoment,
            Some(1.0),
            created_at,
        ),
        relation(
            45,
            &audio,
            EntityKind::MediaAsset,
            raw.id.to_string(),
            RelationshipKind::HasAudio,
            Some(1.0),
            created_at,
        ),
        relation(
            46,
            &raw,
            EntityKind::CaptureDevice,
            camera_a.id.to_string(),
            RelationshipKind::CapturedBy,
            Some(1.0),
            created_at,
        ),
        relation(
            47,
            &video,
            EntityKind::CaptureDevice,
            camera_b.id.to_string(),
            RelationshipKind::CapturedBy,
            Some(1.0),
            created_at,
        ),
        relation(
            48,
            &audio,
            EntityKind::CaptureDevice,
            recorder.id.to_string(),
            RelationshipKind::CapturedBy,
            Some(1.0),
            created_at,
        ),
        relation(
            49,
            &raw,
            EntityKind::StorageVolume,
            master.id.to_string(),
            RelationshipKind::StoredOn,
            Some(1.0),
            created_at,
        ),
        relation(
            50,
            &raw,
            EntityKind::StorageVolume,
            backup.id.to_string(),
            RelationshipKind::BackedUpAs,
            Some(1.0),
            created_at,
        ),
    ] {
        repository.insert_relationship(&relationship)?;
    }
    repository.insert_correction(&Correction {
        id: correction_id(51),
        subject_kind: "relationship".into(),
        subject_id: relationship_id(41).to_string(),
        field_name: "moment_name".into(),
        previous_value: Some(serde_json::json!("Ceremony")),
        corrected_value: serde_json::json!("Ceremony"),
        corrected_at: created_at,
        author_hint: Some("Golden fixture user".into()),
    })?;
    repository.insert_background_job(&BackgroundJob {
        id: job_id(61),
        state: WorkflowRunState::Completed,
        stage: JobStage::Metadata,
        items_completed: 5,
        items_total: Some(5),
        files_discovered: 5,
        files_processed: 5,
        error_count: 0,
        project_id: Some(project.id.clone()),
        index_root_id: None,
        error_message: None,
        resume_metadata: None,
        created_at,
        updated_at: created_at,
        finished_at: Some(created_at),
    })?;

    assert_fixture_counts(
        repository.counts()?,
        &manifest.expected,
        &manifest.description,
    )
}

fn assert_fixture_counts(
    counts: CatalogCounts,
    expected: &ExpectedCounts,
    project_name: &str,
) -> PersistenceResult<CoreStatus> {
    if counts.media_assets != expected.media_assets
        || counts.file_instances != expected.file_instances
        || counts.storage_volumes != expected.storage_volumes
    {
        return Err(PersistenceError::InvalidData(format!(
            "fixture count mismatch for {project_name}"
        )));
    }
    Ok(CoreStatus {
        foundation_ready: true,
        sample_project: "Golden Wedding".into(),
        media_assets: counts.media_assets,
        file_instances: counts.file_instances,
        storage_volumes: counts.storage_volumes,
        local_only: true,
    })
}

fn uuid(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
fn pid(value: u128) -> ProjectId {
    ProjectId::from_uuid(uuid(value))
}
fn sid(value: u128) -> ShootId {
    ShootId::from_uuid(uuid(value))
}
fn mid(value: u128) -> MomentId {
    MomentId::from_uuid(uuid(value))
}
fn person_id(value: u128) -> PersonClusterId {
    PersonClusterId::from_uuid(uuid(value))
}
fn volume_id(value: u128) -> StorageVolumeId {
    StorageVolumeId::from_uuid(uuid(value))
}
fn device_id(value: u128) -> CaptureDeviceId {
    CaptureDeviceId::from_uuid(uuid(value))
}
fn asset_id(value: u128) -> MediaAssetId {
    MediaAssetId::from_uuid(uuid(value))
}
fn file_id(value: u128) -> FileInstanceId {
    FileInstanceId::from_uuid(uuid(value))
}
fn derivative_id(value: u128) -> DerivativeId {
    DerivativeId::from_uuid(uuid(value))
}
fn backup_id(value: u128) -> BackupCopyId {
    BackupCopyId::from_uuid(uuid(value))
}
fn relationship_id(value: u128) -> RelationshipId {
    RelationshipId::from_uuid(uuid(value))
}
fn correction_id(value: u128) -> CorrectionId {
    CorrectionId::from_uuid(uuid(value))
}
fn job_id(value: u128) -> JobId {
    JobId::from_uuid(uuid(value))
}

fn volume(
    value: u128,
    name: &str,
    state: MountState,
    mount: Option<&str>,
    filesystem_identity: Option<&str>,
) -> StorageVolume {
    StorageVolume {
        id: volume_id(value),
        display_name: name.into(),
        filesystem_identity: filesystem_identity.map(str::to_owned),
        capacity_bytes: Some(2_000_000_000),
        mount_state: state,
        mount_location: mount.map(str::to_owned),
        last_seen_at: Some(
            Utc.timestamp_opt(1_704_067_200, 0)
                .single()
                .expect("fixed timestamp is valid"),
        ),
        filesystem_type: Some("synthetic".into()),
        classification: StorageClassification::Removable,
    }
}
fn asset(
    value: u128,
    project: &Project,
    media_type: MediaType,
    name: &str,
    created_at: Timestamp,
    fingerprint: &str,
) -> MediaAsset {
    MediaAsset {
        id: asset_id(value),
        project_id: project.id.clone(),
        media_type,
        display_name: name.into(),
        extension: name
            .rsplit_once('.')
            .map(|(_, value)| value.to_ascii_lowercase()),
        captured_at: Some(created_at),
        fingerprint: MediaFingerprint {
            fast_fingerprint: Some(fingerprint.into()),
            byte_size: Some(1024),
            ..Default::default()
        },
        created_at,
    }
}
fn file(
    value: u128,
    asset: &MediaAsset,
    volume: &StorageVolume,
    path: &str,
    observed_at: Timestamp,
    is_available: bool,
) -> FileInstance {
    FileInstance {
        id: file_id(value),
        media_asset_id: asset.id.clone(),
        storage_volume_id: volume.id.clone(),
        index_root_id: None,
        relative_path: path.into(),
        observed_at,
        is_available,
    }
}
fn relation(
    value: u128,
    source: &MediaAsset,
    target_kind: EntityKind,
    target_id: String,
    kind: RelationshipKind,
    confidence: Option<f64>,
    created_at: Timestamp,
) -> Relationship {
    Relationship {
        id: relationship_id(value),
        source: EntityRef {
            kind: EntityKind::MediaAsset,
            id: source.id.to_string(),
        },
        target: EntityRef {
            kind: target_kind,
            id: target_id,
        },
        kind,
        confidence,
        provenance: Provenance {
            source: "synthetic_fixture".into(),
            algorithm_id: Some("fixture-generator".into()),
            algorithm_version: Some("1".into()),
            produced_at: created_at,
            human_confirmed: false,
        },
        created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magic_search::FixtureSemanticProvider;
    use persistence::SqliteRepository;
    use std::{collections::HashMap, fs, io, io::Write, process::Command};
    use storage::VolumeObservation;
    use tempfile::tempdir;

    #[derive(Debug)]
    struct SimulatedVolumeInspector {
        first_root: PathBuf,
    }

    impl VolumeInspector for SimulatedVolumeInspector {
        fn inspect(&self, path: &Path) -> io::Result<VolumeObservation> {
            let first_volume = path.starts_with(&self.first_root);
            Ok(VolumeObservation {
                filesystem_identity: if first_volume {
                    "simulated-device:first".into()
                } else {
                    "simulated-device:second".into()
                },
                display_name: if first_volume {
                    "Simulated First Volume".into()
                } else {
                    "Simulated Second Volume".into()
                },
                mount_location: Some(if first_volume {
                    "/Volumes/SimulatedFirst".into()
                } else {
                    "/Volumes/SimulatedSecond".into()
                }),
                capacity_bytes: Some(1_000_000),
                filesystem_type: Some("simfs".into()),
                classification: StorageClassification::Removable,
            })
        }
    }

    #[derive(Debug)]
    struct MappedVolumeInspector {
        roots: Vec<(PathBuf, &'static str, &'static str)>,
    }

    impl VolumeInspector for MappedVolumeInspector {
        fn inspect(&self, path: &Path) -> io::Result<VolumeObservation> {
            let (_, identity, display_name) = self
                .roots
                .iter()
                .find(|(root, _, _)| path.starts_with(root))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "simulated volume missing")
                })?;
            Ok(VolumeObservation {
                filesystem_identity: (*identity).into(),
                display_name: (*display_name).into(),
                mount_location: Some(format!("/Volumes/{display_name}")),
                capacity_bytes: Some(1_000_000),
                filesystem_type: Some("simfs".into()),
                classification: StorageClassification::Removable,
            })
        }
    }

    #[cfg(target_os = "macos")]
    fn write_real_test_jpeg(destination: &Path, width: u32, height: u32, variant: u8) {
        let ppm = destination.with_extension("ppm");
        let mut pixels = format!("P6\n{width} {height}\n255\n").into_bytes();
        for y in 0..height {
            for x in 0..width {
                let value = ((x + y + u32::from(variant)) % 256) as u8;
                pixels.extend_from_slice(&[value, value.saturating_add(8), 255 - value]);
            }
        }
        fs::write(&ppm, pixels).unwrap();
        assert!(Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "-s", "formatOptions", "80", "--out"])
            .arg(destination)
            .arg(&ppm)
            .status()
            .unwrap()
            .success());
        fs::remove_file(ppm).unwrap();
    }

    #[test]
    fn golden_shoot_load_is_deterministic() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let status = load_golden_shoot(&repository).unwrap();
        assert_eq!(status.sample_project, "Golden Wedding");
        assert_eq!(status.media_assets, 5);
        assert_eq!(status.file_instances, 6);
        assert_eq!(repository.relationships().unwrap().len(), 10);
        assert_eq!(
            repository
                .file_instances_for_asset(&asset_id(15))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn local_index_creates_assets_merges_copies_and_reindexes_without_duplicates() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("first.jpg"), b"identical bytes").unwrap();
        fs::write(directory.path().join("copy.jpg"), b"identical bytes").unwrap();
        fs::write(directory.path().join("nested/clip.mp4"), b"identical bytes").unwrap();
        fs::write(directory.path().join("unknown.xyz"), b"unknown").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Index test").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let mut observed = Vec::new();
        let first_job = index_local_folder(
            &repository,
            &project_id,
            directory.path().to_str().unwrap(),
            |job| observed.push(job.clone()),
        )
        .unwrap();
        assert_eq!(first_job.state, "completed");
        assert_eq!(first_job.files_discovered, 4);
        assert_eq!(first_job.files_processed, 4);
        assert!(!observed.is_empty());
        let home =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 50).unwrap();
        assert_eq!(home.media.len(), 4);
        let copies = home
            .media
            .iter()
            .filter(|item| item.filename == "first.jpg" || item.filename == "copy.jpg")
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].asset_id, copies[1].asset_id);
        let video = home
            .media
            .iter()
            .find(|item| item.filename == "clip.mp4")
            .unwrap();
        assert_ne!(copies[0].asset_id, video.asset_id);
        assert_eq!(home.summary.duplicate_fast_fingerprint_count, 1);
        index_local_folder(
            &repository,
            &project_id,
            directory.path().to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        let after_reindex =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 50).unwrap();
        assert_eq!(after_reindex.media.len(), 4);
        assert_eq!(repository.counts().unwrap().file_instances, 4);
    }

    #[test]
    fn production_plan_exports_only_explicit_human_keeps_with_verified_resume_and_private_reports()
    {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("delivery");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(
            source.join("KEEP.JPG"),
            b"first photographer-owned source bytes",
        )
        .unwrap();
        fs::write(
            source.join("REVIEW.JPG"),
            b"second photographer-owned source bytes",
        )
        .unwrap();
        let original_keep = fs::read(source.join("KEEP.JPG")).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Delivery fixture").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let indexed =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 20).unwrap();
        let keep_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "KEEP.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        let review_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "REVIEW.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        repository
            .update_culling_decision(
                &project_id,
                &keep_id,
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
        repository
            .update_culling_decision(
                &project_id,
                &review_id,
                &CullingDecisionUpdate {
                    decision: Some(CullingDecisionValue::Review),
                    clear_decision: false,
                    rating: None,
                    starred: None,
                    note: Some("Private client detail must not leave the catalog".into()),
                    flags: None,
                    session_id: None,
                },
            )
            .unwrap();

        let plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Client delivery".into(),
                plan_type: delivery_brain::ProductionPlanType::ClientDelivery,
                selection_rules: delivery_brain::SelectionRules::client_delivery(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        update_production_plan_destination(
            &repository,
            &project_id,
            &plan.id,
            Some(destination.to_str().unwrap()),
        )
        .unwrap();
        let preview = preview_production_plan(&repository, &project_id, &plan.id).unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        assert_eq!(preview.manifest_draft.selected_file_count, 1);
        assert_eq!(
            preview.manifest_draft.entries[0].original_filename,
            "KEEP.JPG"
        );
        assert_eq!(preview.inspection.included_count, 1);
        assert_eq!(preview.inspection.excluded_count, 1);
        assert!(preview
            .inspection
            .items
            .iter()
            .any(|item| item.original_filename == "KEEP.JPG" && item.state == "included"));
        assert!(preview
            .inspection
            .items
            .iter()
            .any(|item| item.original_filename == "REVIEW.JPG" && item.state == "excluded"));
        assert!(
            !destination.join("KEEP.JPG").exists(),
            "dry run must not write media"
        );

        let manifest =
            create_production_export_manifest(&repository, &project_id, &plan.id).unwrap();
        let first =
            export_production_manifest(&repository, &project_id, &manifest.id, || false, |_| {})
                .unwrap();
        assert_eq!(first.state, "completed");
        assert_eq!(first.verified_count, 1);
        assert_eq!(
            fs::read(destination.join("KEEP.JPG")).unwrap(),
            original_keep
        );
        assert!(!destination.join("REVIEW.JPG").exists());
        assert_eq!(
            fs::read(source.join("KEEP.JPG")).unwrap(),
            original_keep,
            "export must not alter source media"
        );
        let report_path = fs::read_dir(destination.join("CaptureOS_Delivery_Reports"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("CaptureOS_Delivery_Report_") && name.ends_with(".json")
                    })
            })
            .unwrap();
        let report = fs::read_to_string(report_path).unwrap();
        assert!(report.contains("Client delivery"));
        assert!(!report.contains("Private client detail"));
        assert!(!report.contains(source.to_str().unwrap()));
        assert!(!report.contains(&keep_id.to_string()));
        assert!(!report.contains(&first.export_job_id));
        assert!(!report.contains("\"jobId\""));

        let resumed =
            export_production_manifest(&repository, &project_id, &manifest.id, || false, |_| {})
                .unwrap();
        assert_eq!(resumed.state, "completed");
        assert_eq!(resumed.verified_count, 1);
        assert_eq!(
            resumed.skipped_identical_count, 1,
            "same frozen manifest must safely reuse verified content"
        );
        let cancelled =
            export_production_manifest(&repository, &project_id, &manifest.id, || true, |_| {})
                .unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert_eq!(
            fs::read(destination.join("KEEP.JPG")).unwrap(),
            original_keep,
            "cancellation must retain an already verified destination file"
        );
        let history = load_production_workspace(&repository, &project_id).unwrap();
        assert_eq!(history.recent_exports.len(), 3);

        let editor_destination = directory.path().join("editor-workset");
        fs::create_dir_all(&editor_destination).unwrap();
        let editor_plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Editor handoff".into(),
                plan_type: delivery_brain::ProductionPlanType::EditorWorkset,
                selection_rules: delivery_brain::SelectionRules::editor_workset(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        update_production_plan_destination(
            &repository,
            &project_id,
            &editor_plan.id,
            Some(editor_destination.to_str().unwrap()),
        )
        .unwrap();
        let editor_manifest =
            create_production_export_manifest(&repository, &project_id, &editor_plan.id).unwrap();
        let editor_export = export_production_manifest(
            &repository,
            &project_id,
            &editor_manifest.id,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(editor_export.state, "completed");
        assert!(editor_destination.join("KEEP.JPG").is_file());
        assert!(editor_destination.join("REVIEW.JPG").is_file());
        let handoff_path = fs::read_dir(editor_destination.join("CaptureOS_Delivery_Reports"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("CaptureOS_Editor_Handoff_Manifest_")
                            && name.ends_with(".json")
                    })
            })
            .unwrap();
        let handoff = fs::read_to_string(handoff_path).unwrap();
        assert!(handoff.contains("REVIEW.JPG"));
        assert!(handoff.contains("\"humanDecision\": \"review\""));
        assert!(!handoff.contains("Private client detail"));
        assert!(!handoff.contains(source.to_str().unwrap()));
        assert!(!handoff.contains(&keep_id.to_string()));
        assert!(!handoff.contains(&review_id.to_string()));
        assert!(!handoff.contains(&editor_export.export_job_id));
        assert!(!handoff.contains("\"jobId\""));
    }

    #[test]
    fn production_preflight_uses_the_frozen_manifest_reserve_and_refuses_missing_evidence() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("delivery");
        fs::create_dir_all(&destination).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Frozen reserve fixture").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();

        let missing_reserve_plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Missing frozen reserve".into(),
                plan_type: delivery_brain::ProductionPlanType::ClientDelivery,
                selection_rules: delivery_brain::SelectionRules::client_delivery(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        let (_, _, _, _, missing_revision, _) = repository
            .production_manifest_build_input(&project_id, &missing_reserve_plan.id)
            .unwrap();
        let missing_reserve_manifest = repository
            .create_export_manifest(
                &missing_reserve_plan,
                missing_revision,
                destination.to_str().unwrap(),
                &serde_json::json!({}),
                &[],
                "missing-frozen-reserve",
            )
            .unwrap();
        assert!(preflight_production_export(
            &repository,
            &project_id,
            &missing_reserve_manifest.id,
        )
        .is_err());

        let configured_reserve = delivery_brain::MIN_DESTINATION_RESERVE_BYTES + 4_096;
        let explicit_reserve_plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Explicit frozen reserve".into(),
                plan_type: delivery_brain::ProductionPlanType::ClientDelivery,
                selection_rules: delivery_brain::SelectionRules::client_delivery(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        let (_, _, _, _, explicit_revision, _) = repository
            .production_manifest_build_input(&project_id, &explicit_reserve_plan.id)
            .unwrap();
        let explicit_reserve_manifest = repository
            .create_export_manifest(
                &explicit_reserve_plan,
                explicit_revision,
                destination.to_str().unwrap(),
                &serde_json::json!({ "reserveBytes": configured_reserve }),
                &[],
                "explicit-frozen-reserve",
            )
            .unwrap();
        let preflight =
            preflight_production_export(&repository, &project_id, &explicit_reserve_manifest.id)
                .unwrap();
        assert_eq!(preflight.reserve_bytes, configured_reserve);
        assert_eq!(preflight.required_bytes, configured_reserve);
    }

    #[test]
    fn production_preview_blocks_a_different_destination_collision_without_writing() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("delivery");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"trusted source bytes").unwrap();
        fs::write(
            destination.join("IMG_0001.JPG"),
            b"different existing destination bytes",
        )
        .unwrap();
        let original_collision = fs::read(destination.join("IMG_0001.JPG")).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "No overwrite").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let asset_id = MediaAssetId::try_from(
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 10)
                .unwrap()
                .media[0]
                .asset_id
                .as_str(),
        )
        .unwrap();
        repository
            .update_culling_decision(
                &project_id,
                &asset_id,
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
        let plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Collision check".into(),
                plan_type: delivery_brain::ProductionPlanType::ClientDelivery,
                selection_rules: delivery_brain::SelectionRules::client_delivery(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        update_production_plan_destination(
            &repository,
            &project_id,
            &plan.id,
            Some(destination.to_str().unwrap()),
        )
        .unwrap();
        let preview = preview_production_plan(&repository, &project_id, &plan.id).unwrap();
        assert!(preview
            .blockers
            .iter()
            .any(|blocker| blocker.contains("different content")));
        assert!(create_production_export_manifest(&repository, &project_id, &plan.id).is_err());
        assert_eq!(
            fs::read(destination.join("IMG_0001.JPG")).unwrap(),
            original_collision
        );
    }

    fn metadata_inspection_for_capture_time(
        file_instance_id: &str,
        local: &str,
        source: &str,
        file_modified_at: &str,
    ) -> MetadataSourceInspection {
        MetadataSourceInspection {
            candidate: persistence::MediaPreparationCandidate {
                asset_id: "asset-1".into(),
                file_instance_id: file_instance_id.into(),
                filename: "IMG_0001.JPG".into(),
                media_type: "jpeg".into(),
                selected_root: None,
                relative_path: "IMG_0001.JPG".into(),
                source_fingerprint: "fixture-fingerprint".into(),
                is_available: true,
            },
            source: None,
            metadata: media_visual::ExtractedMetadata {
                status: ArtifactStatus::Ready,
                captured_at_raw: Some(local.replace('T', " ")),
                captured_at_local: Some(local.into()),
                capture_timezone: Some("unknown".into()),
                capture_time_source: Some(source.into()),
                capture_time_confidence: Some("high".into()),
                file_modified_at: Some(file_modified_at.into()),
                raw: serde_json::json!({}),
                ..Default::default()
            },
        }
    }

    #[test]
    fn capture_time_resolution_uses_matching_embedded_time_not_copy_filesystem_dates() {
        let resolved = resolve_asset_metadata(vec![
            metadata_inspection_for_capture_time(
                "copy-b",
                "2025-10-14T15:42:18.120",
                "exif_datetime_original",
                "2026-01-02T03:04:05Z",
            ),
            metadata_inspection_for_capture_time(
                "copy-a",
                "2025-10-14T15:42:18.120",
                "exif_datetime_original",
                "2024-01-02T03:04:05Z",
            ),
        ])
        .unwrap();

        assert_eq!(
            resolved.metadata.captured_at_local.as_deref(),
            Some("2025-10-14T15:42:18.120")
        );
        assert_eq!(
            resolved.metadata.capture_time_source.as_deref(),
            Some("exif_datetime_original")
        );
        assert_eq!(
            resolved.metadata.capture_timezone.as_deref(),
            Some("unknown")
        );
        assert!(!resolved.copy_conflict);
        assert_eq!(resolved.observations.len(), 2);
        assert_eq!(
            resolved
                .metadata
                .raw
                .pointer("/captureTimeResolution/selected/source")
                .and_then(serde_json::Value::as_str),
            Some("exif_datetime_original")
        );
    }

    #[test]
    fn capture_time_resolution_keeps_a_developer_conflict_diagnostic_for_copies() {
        let resolved = resolve_asset_metadata(vec![
            metadata_inspection_for_capture_time(
                "copy-b",
                "2025-10-14T15:42:19",
                "exif_datetime_original",
                "2026-01-02T03:04:05Z",
            ),
            metadata_inspection_for_capture_time(
                "copy-a",
                "2025-10-14T15:42:18",
                "exif_datetime_original",
                "2024-01-02T03:04:05Z",
            ),
        ])
        .unwrap();

        assert!(resolved.copy_conflict);
        assert_eq!(
            resolved.metadata.captured_at_local.as_deref(),
            Some("2025-10-14T15:42:18")
        );
        assert_eq!(
            resolved
                .metadata
                .raw
                .pointer("/captureTimeResolution/diagnostics/0/kind")
                .and_then(serde_json::Value::as_str),
            Some("embedded_capture_time_conflict")
        );
    }

    #[test]
    fn unknown_timezone_moment_time_stays_a_local_wall_clock_value() {
        let local = "2025-10-14T15:42:18.120";
        assert!(moment_unix_millis(local).is_some());
        let capture_time_by_asset = HashMap::from([("asset-1".into(), local.into())]);
        assert_eq!(
            materialized_moment_capture_time(
                "asset-1",
                moment_unix_millis(local),
                &capture_time_by_asset
            )
            .as_deref(),
            Some(local)
        );
    }

    /// Deliberately pixel-free, synthetic EXIF fixture for the existing-catalog refresh test.
    /// It must stay independent of the private AI Test photographs.
    fn synthetic_exif_jpeg(datetime_original: &str) -> Vec<u8> {
        let mut datetime = datetime_original.as_bytes().to_vec();
        datetime.push(0);
        let exif_ifd_offset = 26_u32;
        let datetime_offset = 44_u32;
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42_u16.to_le_bytes());
        tiff.extend_from_slice(&8_u32.to_le_bytes());
        tiff.extend_from_slice(&1_u16.to_le_bytes());
        tiff.extend_from_slice(&0x8769_u16.to_le_bytes()); // ExifIFDPointer
        tiff.extend_from_slice(&4_u16.to_le_bytes()); // LONG
        tiff.extend_from_slice(&1_u32.to_le_bytes());
        tiff.extend_from_slice(&exif_ifd_offset.to_le_bytes());
        tiff.extend_from_slice(&0_u32.to_le_bytes()); // no next IFD
        assert_eq!(tiff.len(), exif_ifd_offset as usize);
        tiff.extend_from_slice(&1_u16.to_le_bytes());
        tiff.extend_from_slice(&0x9003_u16.to_le_bytes()); // DateTimeOriginal
        tiff.extend_from_slice(&2_u16.to_le_bytes()); // ASCII
        tiff.extend_from_slice(&(datetime.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&datetime_offset.to_le_bytes());
        tiff.extend_from_slice(&0_u32.to_le_bytes()); // no next Exif IFD
        assert_eq!(tiff.len(), datetime_offset as usize);
        tiff.extend_from_slice(&datetime);

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend_from_slice(
            &u16::try_from(app1.len() + 2)
                .expect("synthetic EXIF app segment fits JPEG")
                .to_be_bytes(),
        );
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn metadata_refresh_repairs_an_existing_catalog_without_mutating_human_or_preview_data() {
        let directory = tempdir().unwrap();
        let source_root = directory.path().join("already-indexed");
        fs::create_dir_all(&source_root).unwrap();
        let jpeg = synthetic_exif_jpeg("2025:10:14 15:42:18");
        fs::write(source_root.join("IMG_0001.JPG"), &jpeg).unwrap();
        fs::write(source_root.join("IMG_0001 copy.JPG"), &jpeg).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Metadata refresh fixture").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(
            &repository,
            &project_id,
            source_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        let item = repository
            .visual_media_page(&project_id, &VisualMediaQuery::default())
            .unwrap()
            .items
            .into_iter()
            .next()
            .unwrap();
        let asset_id = MediaAssetId::try_from(item.asset_id.as_str()).unwrap();
        save_human_intelligence_decision(
            &repository,
            &project_id,
            &asset_id,
            HumanDecisionValue::Keep,
            Some("keep this local fixture"),
        )
        .unwrap();
        let preview_id = Uuid::new_v4().to_string();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: preview_id.clone(),
                media_asset_id: item.asset_id.clone(),
                source_file_instance_id: item.file_instance_id.clone(),
                artifact_type: "thumbnail".into(),
                size_class: "small".into(),
                cache_relative_path: "synthetic/thumbnail.jpg".into(),
                provider: "fixture".into(),
                generator_version: "fixture".into(),
                source_fingerprint: "fixture".into(),
                created_at: Utc::now().to_rfc3339(),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        let culling_before = load_culling_progress(&repository, &project_id).unwrap();
        assert!(repository
            .media_asset_detail(&asset_id)
            .unwrap()
            .unwrap()
            .metadata
            .is_none());

        let mut updates = Vec::new();
        let progress = refresh_capture_metadata(&repository, &project_id, |update| {
            updates.push(update.clone());
        })
        .unwrap();

        assert_eq!(progress.state, "completed");
        assert_eq!(progress.items_total, 1);
        assert_eq!(progress.resolved_capture_time_count, 1);
        assert_eq!(progress.high_confidence_capture_time_count, 1);
        assert!(updates.iter().any(|update| update.state == "running"));
        let detail = repository.media_asset_detail(&asset_id).unwrap().unwrap();
        let metadata = detail.metadata.unwrap();
        assert_eq!(
            metadata.captured_at_local.as_deref(),
            Some("2025-10-14T15:42:18")
        );
        assert_eq!(
            metadata.capture_time_source.as_deref(),
            Some("exif_datetime_original")
        );
        assert_eq!(metadata.capture_timezone.as_deref(), Some("unknown"));
        assert_eq!(metadata.capture_time_confidence.as_deref(), Some("high"));
        assert_eq!(
            metadata
                .raw_metadata
                .pointer("/captureTimeResolution/observationCount")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            load_culling_progress(&repository, &project_id).unwrap(),
            culling_before
        );
        assert!(repository
            .preview_render_artifact(&preview_id)
            .unwrap()
            .is_some());

        let inputs = repository
            .moment_analysis_inputs(&project_id, None)
            .unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs[0].captured_at.as_deref(),
            Some("2025-10-14T15:42:18")
        );
        let model_root = directory.path().join("no-semantic-model-installed");
        let moment_progress = start_moment_analysis(
            &repository,
            &project_id,
            &SiglipProviderCache::new(&model_root),
            AnalysisResourceMode::Balanced,
            true,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(moment_progress.timeline_ready);
        assert!(
            repository
                .moment_timeline_status(&project_id)
                .unwrap()
                .unwrap()
                .moment_count
                > 0
        );
    }

    #[test]
    fn media_detail_similarity_and_human_decisions_reject_the_wrong_project_context() {
        let directory = tempdir().unwrap();
        let first_root = directory.path().join("first-project");
        let second_root = directory.path().join("second-project");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        fs::write(first_root.join("first.jpg"), b"first project media").unwrap();
        fs::write(second_root.join("second.jpg"), b"second project media").unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let first = create_local_project(&repository, "Test Wedding").unwrap();
        let second = create_local_project(&repository, "AI Test").unwrap();
        let first_id = ProjectId::try_from(first.id.as_str()).unwrap();
        let second_id = ProjectId::try_from(second.id.as_str()).unwrap();
        index_local_folder(&repository, &first_id, first_root.to_str().unwrap(), |_| {}).unwrap();
        index_local_folder(
            &repository,
            &second_id,
            second_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        let first_asset = repository
            .visual_media_page(&first_id, &VisualMediaQuery::default())
            .unwrap()
            .items
            .pop()
            .unwrap()
            .asset_id;
        let first_asset = MediaAssetId::try_from(first_asset.as_str()).unwrap();
        let cache = directory.path().join("preview-cache");

        assert!(
            load_media_asset_detail(&repository, &first_id, &first_asset, &cache)
                .unwrap()
                .is_some()
        );
        for result in [
            load_media_asset_detail(&repository, &second_id, &first_asset, &cache).map(|_| ()),
            load_similarity_group(&repository, &second_id, &first_asset, 24, 0).map(|_| ()),
            save_human_intelligence_decision(
                &repository,
                &second_id,
                &first_asset,
                HumanDecisionValue::Keep,
                None,
            ),
        ] {
            assert!(
                matches!(result, Err(PersistenceError::InvalidData(message)) if message.contains("does not belong"))
            );
        }
    }

    #[test]
    fn different_roots_on_one_volume_share_storage_but_keep_root_associations() {
        let directory = tempdir().unwrap();
        let first_root = directory.path().join("first-root");
        let second_root = directory.path().join("second-root");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        fs::write(first_root.join("first.jpg"), b"first root media").unwrap();
        fs::write(second_root.join("second.jpg"), b"second root media").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Two roots").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();

        index_local_folder(
            &repository,
            &project_id,
            first_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        index_local_folder(
            &repository,
            &project_id,
            second_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();

        let roots = repository.index_roots_for_project(&project_id).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].storage_volume_id, roots[1].storage_volume_id);
        assert_eq!(repository.counts().unwrap().storage_volumes, 1);
        let volume = repository
            .get_storage_volume(&roots[0].storage_volume_id)
            .unwrap()
            .unwrap();
        assert_ne!(volume.display_name, "first-root");
        assert_ne!(volume.display_name, "second-root");
        assert_ne!(volume.mount_location.as_deref(), first_root.to_str());
        assert_ne!(volume.mount_location.as_deref(), second_root.to_str());

        let mut stale_volume = volume;
        stale_volume.display_name = "first-root".into();
        stale_volume.mount_location = Some(first_root.to_string_lossy().into_owned());
        repository.update_storage_volume(&stale_volume).unwrap();

        let home =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 50).unwrap();
        let refreshed_volume = repository
            .get_storage_volume(&roots[0].storage_volume_id)
            .unwrap()
            .unwrap();
        assert_ne!(refreshed_volume.display_name, "first-root");
        assert_ne!(
            refreshed_volume.mount_location.as_deref(),
            first_root.to_str()
        );
        let first = home
            .media
            .iter()
            .find(|row| row.filename == "first.jpg")
            .unwrap();
        let second = home
            .media
            .iter()
            .find(|row| row.filename == "second.jpg")
            .unwrap();
        assert_eq!(
            first.selected_root,
            first_root.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(
            second.selected_root,
            second_root.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(first.storage_volume_id, second.storage_volume_id);

        index_local_folder(
            &repository,
            &project_id,
            first_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        assert_eq!(repository.counts().unwrap().storage_volumes, 1);
        assert_eq!(
            repository
                .index_roots_for_project(&project_id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn different_simulated_volumes_create_distinct_storage_records() {
        let directory = tempdir().unwrap();
        let first_root = directory.path().join("first-root");
        let second_root = directory.path().join("second-root");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        fs::write(first_root.join("first.jpg"), b"first volume media").unwrap();
        fs::write(second_root.join("second.jpg"), b"second volume media").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Simulated volumes").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let inspector = SimulatedVolumeInspector {
            first_root: first_root.canonicalize().unwrap(),
        };

        index_local_folder_with_inspector(
            &repository,
            &project_id,
            first_root.to_str().unwrap(),
            &inspector,
            |_| {},
        )
        .unwrap();
        index_local_folder_with_inspector(
            &repository,
            &project_id,
            second_root.to_str().unwrap(),
            &inspector,
            |_| {},
        )
        .unwrap();

        let roots = repository.index_roots_for_project(&project_id).unwrap();
        assert_eq!(roots.len(), 2);
        assert_ne!(roots[0].storage_volume_id, roots[1].storage_volume_id);
        assert_eq!(repository.counts().unwrap().storage_volumes, 2);
    }

    fn ingest_request(
        sources: Vec<(&str, &Path)>,
        master: &Path,
        backups: Vec<&Path>,
    ) -> IngestRequest {
        IngestRequest {
            project_name: "ignored by core".into(),
            sources: sources
                .into_iter()
                .map(|(label, path)| ingest::IngestSourceInput {
                    label: label.into(),
                    selected_path: path.to_string_lossy().into_owned(),
                })
                .collect(),
            master: ingest::IngestDestinationInput {
                role: IngestDestinationRole::Master,
                selected_path: master.to_string_lossy().into_owned(),
            },
            backups: backups
                .into_iter()
                .map(|path| ingest::IngestDestinationInput {
                    role: IngestDestinationRole::Backup,
                    selected_path: path.to_string_lossy().into_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn ingest_copies_multiple_sources_verifies_each_destination_and_preserves_source_bytes() {
        let directory = tempdir().unwrap();
        let camera_a = directory.path().join("camera-a");
        let camera_b = directory.path().join("camera-b");
        let master = directory.path().join("master");
        let backup = directory.path().join("backup");
        for path in [&camera_a, &camera_b, &master, &backup] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(camera_a.join("DSC_0001.JPG"), b"camera a bytes").unwrap();
        fs::write(camera_b.join("DSC_0001.JPG"), b"camera b different bytes").unwrap();
        let source_a_before = fs::read(camera_a.join("DSC_0001.JPG")).unwrap();
        let source_b_before = fs::read(camera_b.join("DSC_0001.JPG")).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Priya + Rahul").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();

        let mut progress = Vec::new();
        let report = start_ingest(
            &repository,
            &project_id,
            ingest_request(
                vec![("Camera A", &camera_a), ("Camera B", &camera_b)],
                &master,
                vec![&backup],
            ),
            IngestProtectionPolicy::Standard,
            "00000000-0000-0000-0000-000000000101",
            |report| progress.push(report.job.files_verified),
        )
        .unwrap();

        assert_eq!(
            fs::read(camera_a.join("DSC_0001.JPG")).unwrap(),
            source_a_before
        );
        assert_eq!(
            fs::read(camera_b.join("DSC_0001.JPG")).unwrap(),
            source_b_before
        );
        assert_eq!(report.job.files_total, 4);
        assert_eq!(report.job.files_verified, 4);
        assert_eq!(report.job.guardian_state, "partially_protected");
        assert!(!report.job.safe_to_eject);
        assert!(report.same_volume_warning);
        assert!(!progress.is_empty());
        assert!(master
            .join("Priya + Rahul/01_SOURCES/Camera A/DSC_0001.JPG")
            .exists());
        assert!(master
            .join("Priya + Rahul/01_SOURCES/Camera B/DSC_0001.JPG")
            .exists());
        assert_eq!(repository.counts().unwrap().media_assets, 2);
        assert_eq!(repository.counts().unwrap().file_instances, 6);
        assert_eq!(
            repository
                .ingest_items(&IngestJobId::try_from(report.job.id.as_str()).unwrap())
                .unwrap()
                .len(),
            4
        );
        let history = ingest_history(&repository, &project_id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, report.job.id);
    }

    #[test]
    fn duplicate_start_request_is_idempotent_with_multiple_sources_destinations_and_items() {
        let directory = tempdir().unwrap();
        let camera_a = directory.path().join("camera-a");
        let camera_b = directory.path().join("camera-b");
        let master = directory.path().join("master");
        let backup = directory.path().join("backup");
        for path in [&camera_a, &camera_b, &master, &backup] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(camera_a.join("A001.JPG"), b"camera a one").unwrap();
        fs::write(camera_a.join("A002.JPG"), b"camera a two").unwrap();
        fs::write(camera_b.join("B001.JPG"), b"camera b one").unwrap();
        fs::write(camera_b.join("B002.JPG"), b"camera b two").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Idempotent start").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let request = ingest_request(
            vec![("Camera A", &camera_a), ("Camera B", &camera_b)],
            &master,
            vec![&backup],
        );
        let start_request_id = "00000000-0000-0000-0000-000000000201";

        let first = start_ingest(
            &repository,
            &project_id,
            request.clone(),
            IngestProtectionPolicy::Standard,
            start_request_id,
            |_| {},
        )
        .unwrap();
        let duplicate = start_ingest(
            &repository,
            &project_id,
            request,
            IngestProtectionPolicy::Standard,
            start_request_id,
            |_| {},
        )
        .unwrap();

        assert_eq!(duplicate.job.id, first.job.id);
        assert_eq!(first.job.files_total, 8);
        assert_eq!(first.job.files_verified, 8);
        assert_eq!(ingest_history(&repository, &project_id).unwrap().len(), 1);
        let job_id = IngestJobId::try_from(first.job.id.as_str()).unwrap();
        assert_eq!(repository.ingest_sources(&job_id).unwrap().len(), 2);
        assert_eq!(repository.ingest_destinations(&job_id).unwrap().len(), 2);
        assert_eq!(repository.ingest_items(&job_id).unwrap().len(), 8);
        assert_eq!(
            repository
                .ingest_audit_events(&job_id)
                .unwrap()
                .iter()
                .filter(|event| event.event_type == "INGEST_CREATED")
                .count(),
            1
        );
    }

    #[test]
    fn separate_start_request_ids_remain_separate_history_entries() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&master).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"deliberate separate runs").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Separate runs").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let request = ingest_request(vec![("Camera A", &source)], &master, vec![]);

        let first = start_ingest(
            &repository,
            &project_id,
            request.clone(),
            IngestProtectionPolicy::Basic,
            "00000000-0000-0000-0000-000000000202",
            |_| {},
        )
        .unwrap();
        let second = start_ingest(
            &repository,
            &project_id,
            request,
            IngestProtectionPolicy::Basic,
            "00000000-0000-0000-0000-000000000203",
            |_| {},
        )
        .unwrap();

        assert_ne!(first.job.id, second.job.id);
        let history = ingest_history(&repository, &project_id).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|job| job.id == first.job.id));
        assert!(history.iter().any(|job| job.id == second.job.id));
    }

    #[test]
    fn idempotent_start_request_persists_across_repository_restart() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        let catalog = directory.path().join("captureos.sqlite3");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&master).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"persistent request ID").unwrap();
        let start_request_id = "00000000-0000-0000-0000-000000000204";
        let (project_id, first_job_id) = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let project = create_local_project(&repository, "Persisted request ID").unwrap();
            let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
            let report = start_ingest(
                &repository,
                &project_id,
                ingest_request(vec![("Camera A", &source)], &master, vec![]),
                IngestProtectionPolicy::Basic,
                start_request_id,
                |_| {},
            )
            .unwrap();
            (project_id, report.job.id)
        };
        let repository = SqliteRepository::open(&catalog).unwrap();
        let duplicate = start_ingest(
            &repository,
            &project_id,
            ingest_request(vec![("Camera A", &source)], &master, vec![]),
            IngestProtectionPolicy::Basic,
            start_request_id,
            |_| {},
        )
        .unwrap();

        assert_eq!(duplicate.job.id, first_job_id);
        assert_eq!(ingest_history(&repository, &project_id).unwrap().len(), 1);
    }

    #[test]
    fn ingest_conflict_never_overwrites_existing_destination_content() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&master).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"first version").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Conflict test").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let request = ingest_request(vec![("Camera A", &source)], &master, vec![]);
        let first = start_ingest(
            &repository,
            &project_id,
            request.clone(),
            IngestProtectionPolicy::Basic,
            "00000000-0000-0000-0000-000000000102",
            |_| {},
        )
        .unwrap();
        assert_eq!(first.job.guardian_state, "master_copy");
        let target = master.join("Conflict test/01_SOURCES/Camera A/IMG_0001.JPG");
        fs::write(source.join("IMG_0001.JPG"), b"different source content").unwrap();
        let second = start_ingest(
            &repository,
            &project_id,
            request,
            IngestProtectionPolicy::Basic,
            "00000000-0000-0000-0000-000000000103",
            |_| {},
        )
        .unwrap();
        assert_eq!(second.job.guardian_state, "needs_attention");
        assert_eq!(fs::read(target).unwrap(), b"first version");
    }

    #[test]
    fn independent_verified_destinations_are_protected_and_audited() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("camera");
        let master = directory.path().join("master");
        let backup = directory.path().join("backup");
        for path in [&source, &master, &backup] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(source.join("C0001.MOV"), b"camera media").unwrap();
        let inspector = MappedVolumeInspector {
            roots: vec![
                (source.canonicalize().unwrap(), "source-card", "Camera Card"),
                (master.canonicalize().unwrap(), "master-ssd", "Master SSD"),
                (backup.canonicalize().unwrap(), "backup-hdd", "Backup HDD"),
            ],
        };
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Independent protection").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();

        let report = start_ingest_with_inspector(
            &repository,
            &project_id,
            ingest_request(vec![("Camera A", &source)], &master, vec![&backup]),
            IngestProtectionPolicy::Standard,
            "00000000-0000-0000-0000-000000000104",
            &inspector,
            |_| {},
        )
        .unwrap();

        assert_eq!(report.job.guardian_state, "protected");
        assert!(report.job.safe_to_eject);
        assert!(!report.same_volume_warning);
        assert_eq!(report.job.files_verified, 2);
        assert_eq!(repository.counts().unwrap().storage_volumes, 3);
        assert_eq!(repository.counts().unwrap().media_assets, 1);
        assert_eq!(repository.counts().unwrap().file_instances, 3);
        let job_id = IngestJobId::try_from(report.job.id.as_str()).unwrap();
        let items = repository.ingest_items(&job_id).unwrap();
        assert!(items.iter().all(|item| {
            item.state == IngestItemState::Verified
                && item.verification_state == VerificationState::Verified
                && item.source_hash == item.destination_hash
                && item.destination_file_instance_id.is_some()
        }));
        let events = repository.ingest_audit_events(&job_id).unwrap();
        for expected in [
            "INGEST_CREATED",
            "PREFLIGHT_COMPLETED",
            "COPY_STARTED",
            "COPY_COMPLETED",
            "VERIFY_STARTED",
            "VERIFY_SUCCEEDED",
            "GUARDIAN_STATUS_CHANGED",
            "INGEST_COMPLETED",
        ] {
            assert!(events.iter().any(|event| event.event_type == expected));
        }
        let relationships = repository.relationships().unwrap();
        assert_eq!(
            relationships
                .iter()
                .filter(|relationship| relationship.kind == RelationshipKind::CopiedFrom)
                .count(),
            2
        );
        assert_eq!(
            relationships
                .iter()
                .filter(|relationship| relationship.kind == RelationshipKind::VerifiedCopyOf)
                .count(),
            2
        );
    }

    #[test]
    fn retry_reuses_verified_files_without_duplicate_assets_or_file_instances() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&master).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"retry-safe content").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Retry test").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        let first = start_ingest(
            &repository,
            &project_id,
            ingest_request(vec![("Camera A", &source)], &master, vec![]),
            IngestProtectionPolicy::Basic,
            "00000000-0000-0000-0000-000000000105",
            |_| {},
        )
        .unwrap();
        let first_job_id = IngestJobId::try_from(first.job.id.as_str()).unwrap();
        let retried = restart_ingest(&repository, &first_job_id, |_| {}).unwrap();

        assert_eq!(retried.job.files_verified, 1);
        assert_eq!(retried.job.guardian_state, "master_copy");
        assert_eq!(repository.counts().unwrap().media_assets, 1);
        assert_eq!(repository.counts().unwrap().file_instances, 2);
        assert!(repository
            .ingest_audit_events(&first_job_id)
            .unwrap()
            .iter()
            .any(|event| event.event_type == "JOB_RESUMED"));
    }

    #[test]
    fn fixture_demo_ingest_verifies_two_sources_to_independent_master_and_backup() {
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ingest");
        let camera_a = fixture_root.join("camera-a").canonicalize().unwrap();
        let camera_b = fixture_root.join("camera-b").canonicalize().unwrap();
        let camera_a_before = fs::read(camera_a.join("DCIM/IMG_0001.JPG")).unwrap();
        let camera_b_before = fs::read(camera_b.join("DCIM/IMG_0001.JPG")).unwrap();
        let directory = tempdir().unwrap();
        let master = directory.path().join("demo-master");
        let backup = directory.path().join("demo-backup");
        fs::create_dir_all(&master).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let inspector = MappedVolumeInspector {
            roots: vec![
                (camera_a.clone(), "fixture-camera-a", "Camera A Card"),
                (camera_b.clone(), "fixture-camera-b", "Camera B Card"),
                (
                    master.canonicalize().unwrap(),
                    "fixture-master",
                    "Demo Master",
                ),
                (
                    backup.canonicalize().unwrap(),
                    "fixture-backup",
                    "Demo Backup",
                ),
            ],
        };
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Fixture demo").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();

        let report = start_ingest_with_inspector(
            &repository,
            &project_id,
            ingest_request(
                vec![("Camera A", &camera_a), ("Camera B", &camera_b)],
                &master,
                vec![&backup],
            ),
            IngestProtectionPolicy::Standard,
            "00000000-0000-0000-0000-000000000106",
            &inspector,
            |_| {},
        )
        .unwrap();

        assert_eq!(report.job.files_total, 8);
        assert_eq!(report.job.files_verified, 8);
        assert_eq!(report.job.guardian_state, "protected");
        assert!(report.job.safe_to_eject);
        assert_eq!(repository.counts().unwrap().media_assets, 4);
        assert_eq!(repository.counts().unwrap().file_instances, 12);
        assert!(master
            .join("Fixture demo/01_SOURCES/Camera A/DCIM/IMG_0001.JPG")
            .exists());
        assert!(backup
            .join("Fixture demo/01_SOURCES/Camera B/DCIM/C0002.MOV")
            .exists());
        assert_eq!(
            fs::read(camera_a.join("DCIM/IMG_0001.JPG")).unwrap(),
            camera_a_before
        );
        assert_eq!(
            fs::read(camera_b.join("DCIM/IMG_0001.JPG")).unwrap(),
            camera_b_before
        );
        let items = repository
            .ingest_items(&IngestJobId::try_from(report.job.id.as_str()).unwrap())
            .unwrap();
        assert_eq!(items.len(), 8);
        assert!(items.iter().all(|item| {
            item.state == IngestItemState::Verified
                && item.verification_state == VerificationState::Verified
                && item.source_hash == item.destination_hash
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn visual_preparation_generates_cached_thumbnails_and_survives_offline_originals() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("camera");
        let cache = directory.path().join("captureos-preview-cache");
        fs::create_dir_all(&root).unwrap();
        let ppm = root.join("fixture.ppm");
        let jpeg = root.join("IMG_0001.JPG");
        let mut ppm_file = fs::File::create(&ppm).unwrap();
        ppm_file
            .write_all(b"P3\n4 2\n255\n255 0 0  0 255 0  0 0 255  255 255 255\n255 255 0  0 255 255  255 0 255  0 0 0\n")
            .unwrap();
        assert!(Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "--out"])
            .arg(&jpeg)
            .arg(&ppm)
            .status()
            .unwrap()
            .success());
        fs::remove_file(ppm).unwrap();
        let before = fs::read(&jpeg).unwrap();
        let catalog = directory.path().join("captureos.sqlite3");
        let repository = SqliteRepository::open(&catalog).unwrap();
        let project = create_local_project(&repository, "Visual cache").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, root.to_str().unwrap(), |_| {}).unwrap();

        let mut observed = Vec::new();
        let progress = prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |event| observed.push(event.clone()),
        )
        .unwrap();
        assert_eq!(progress.state, "completed");
        assert!(observed.iter().any(|event| event.stage == "metadata"));
        assert!(observed.iter().any(|event| event.stage == "thumbnail"));
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].thumbnail_preview_url.is_some());
        assert!(page.items[0]
            .thumbnail_preview_url
            .as_deref()
            .is_some_and(|url| url.starts_with("captureos-preview://localhost/")));
        assert_eq!(page.items[0].width, Some(4));
        assert_eq!(page.items[0].height, Some(2));
        assert_eq!(fs::read(&jpeg).unwrap(), before);
        let original_preview_url = page.items[0].thumbnail_preview_url.clone();
        let artifact_id = original_preview_url
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .unwrap();
        let artifact = repository
            .preview_render_artifact(artifact_id)
            .unwrap()
            .unwrap();
        let artifact_path = cache.join(artifact.cache_relative_path);
        assert!(artifact_path.is_file());
        assert!(fs::metadata(&artifact_path).unwrap().len() > 0);
        let decoded = Command::new("/usr/bin/sips")
            .args(["-g", "format", "-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(&artifact_path)
            .output()
            .unwrap();
        assert!(decoded.status.success());
        let decoded = String::from_utf8(decoded.stdout)
            .unwrap()
            .to_ascii_lowercase();
        assert!(decoded.contains("jpeg"));
        assert!(decoded.contains("pixelwidth"));
        assert!(decoded.contains("pixelheight"));

        let summary = load_visual_preparation_summary(&repository, &project_id)
            .unwrap()
            .unwrap();
        assert_eq!(summary.items_completed, 1);
        assert_eq!(summary.items_total, 1);
        assert_eq!(summary.ready_count, 1);

        let reused = prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |_| {},
        )
        .unwrap();
        assert_eq!(reused.items_total, 0);
        assert_eq!(
            load_visual_preparation_summary(&repository, &project_id)
                .unwrap()
                .unwrap()
                .items_total,
            1
        );

        let root_id = repository.index_roots_for_project(&project_id).unwrap()[0]
            .id
            .clone();
        repository
            .mark_root_instances_unavailable(&root_id)
            .unwrap();
        let offline = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery {
                filter: persistence::VisualMediaFilter::Offline,
                ..Default::default()
            },
            &cache,
        )
        .unwrap();
        assert_eq!(offline.items.len(), 1);
        assert!(offline.items[0].thumbnail_preview_url.is_some());

        clear_visual_cache(&repository, &project_id, &cache).unwrap();
        assert_eq!(fs::read(&jpeg).unwrap(), before);
        let detail = load_media_asset_detail(
            &repository,
            &project_id,
            &MediaAssetId::try_from(offline.items[0].asset_id.as_str()).unwrap(),
            &cache,
        )
        .unwrap()
        .unwrap();
        assert!(detail.metadata.is_some());
        assert!(detail.item.thumbnail_preview_url.is_none());

        // Clearing is limited to generated cache content. A subsequent index observation makes
        // the original available again, and preparation creates new artifact identities/URLs.
        index_local_folder(&repository, &project_id, root.to_str().unwrap(), |_| {}).unwrap();
        let regenerated = prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |_| {},
        )
        .unwrap();
        assert_eq!(regenerated.items_total, 1);
        assert_eq!(regenerated.ready_count, 1);
        let regenerated_page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(regenerated_page.items.len(), 1);
        assert!(regenerated_page.items[0].thumbnail_preview_url.is_some());
        assert_ne!(
            regenerated_page.items[0].thumbnail_preview_url,
            original_preview_url
        );

        // This follows the exact persisted job summary after a fresh repository open, mirroring
        // an application restart rather than reconstructing a count from catalog rows.
        drop(repository);
        let reopened = SqliteRepository::open(&catalog).unwrap();
        let restored_summary = load_visual_preparation_summary(&reopened, &project_id)
            .unwrap()
            .unwrap();
        assert_eq!(restored_summary.items_completed, 1);
        assert_eq!(restored_summary.items_total, 1);
        assert_eq!(restored_summary.ready_count, 1);
        let restored_page =
            load_visual_media_page(&reopened, &project_id, &VisualMediaQuery::default(), &cache)
                .unwrap();
        assert_eq!(restored_page.items[0].preview_status, "ready");
        assert!(restored_page.items[0].thumbnail_preview_url.is_some());
    }

    #[test]
    fn legacy_terminal_artifacts_are_reconciled_into_a_durable_logical_summary() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("legacy-preview-source");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("IMG_0001.JPG"), b"legacy indexed source").unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Legacy preview summary").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, root.to_str().unwrap(), |_| {}).unwrap();
        let item = repository
            .visual_media_page(&project_id, &VisualMediaQuery::default())
            .unwrap()
            .items
            .pop()
            .unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: Uuid::new_v4().to_string(),
                media_asset_id: item.asset_id.clone(),
                source_file_instance_id: item.file_instance_id.clone(),
                artifact_type: "thumbnail".into(),
                size_class: "small".into(),
                cache_relative_path: "legacy/thumbnail.jpg".into(),
                provider: "legacy-test".into(),
                generator_version: "m3".into(),
                source_fingerprint: "legacy-source".into(),
                created_at: Utc::now().to_rfc3339(),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: Uuid::new_v4().to_string(),
                media_asset_id: item.asset_id,
                source_file_instance_id: item.file_instance_id,
                artifact_type: "thumbnail".into(),
                size_class: "medium".into(),
                cache_relative_path: "legacy/medium.jpg".into(),
                provider: "legacy-test".into(),
                generator_version: "m3".into(),
                source_fingerprint: "legacy-source".into(),
                created_at: Utc::now().to_rfc3339(),
                status: "failed".into(),
                failure_reason: Some(
                    "A larger rendition failed after the thumbnail was ready".into(),
                ),
            })
            .unwrap();

        let summary = load_visual_preparation_summary(&repository, &project_id)
            .unwrap()
            .unwrap();
        assert_eq!(summary.items_completed, 1);
        assert_eq!(summary.items_total, 1);
        assert_eq!(summary.ready_count, 1);
        assert_eq!(summary.failed_count, 0);
        assert_eq!(
            summary.message.as_deref(),
            Some("Recovered from terminal preview artifacts")
        );
        let persisted = repository
            .latest_visual_preparation_job(&project_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted
                .resume_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("summary_origin"))
                .and_then(serde_json::Value::as_str),
            Some("reconciled-terminal-artifacts-v2")
        );
    }

    #[test]
    fn usable_preview_wins_over_a_failed_provider_attempt_for_logical_status() {
        let generated = |status: ArtifactStatus, provider: &str| media_visual::GeneratedPreview {
            artifact_type: "poster".into(),
            size: media_visual::PreviewSize::Medium,
            cache_relative_path: "test/poster.png".into(),
            provider: provider.into(),
            source_fingerprint: "test".into(),
            status,
            failure_reason: Some("provider attempt failed".into()),
        };

        assert_eq!(
            terminal_status(
                &ArtifactStatus::Ready,
                &[
                    generated(ArtifactStatus::Failed, "provider-a"),
                    generated(ArtifactStatus::Ready, "provider-b"),
                ],
            ),
            ArtifactStatus::Ready
        );
        assert_eq!(
            terminal_status(
                &ArtifactStatus::Ready,
                &[
                    generated(ArtifactStatus::Failed, "provider-a"),
                    generated(ArtifactStatus::Failed, "provider-b"),
                ],
            ),
            ArtifactStatus::Failed
        );
        assert_eq!(
            terminal_status(
                &ArtifactStatus::Ready,
                &[generated(ArtifactStatus::Corrupt, "validation")],
            ),
            ArtifactStatus::Corrupt
        );
        assert_eq!(
            terminal_status(
                &ArtifactStatus::Unsupported,
                &[generated(ArtifactStatus::Unsupported, "boundary")],
            ),
            ArtifactStatus::Unsupported
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn corrupt_fixture_media_reaches_terminal_progress_and_does_not_block_real_jpeg() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("mixed");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("IMG_0001.JPG"), b"fixture text, not a JPEG").unwrap();
        fs::write(root.join("C0001.MOV"), b"fixture text, not a MOV").unwrap();
        fs::write(root.join("IMG_0003.heic"), b"fixture text, not a HEIC").unwrap();
        fs::write(root.join("REC0001.WAV"), b"fixture text, not a WAV").unwrap();
        let ppm = root.join("real.ppm");
        let jpeg = root.join("IMG_9484.JPG");
        fs::write(&ppm, b"P3\n2 1\n255\n255 0 0  0 255 0\n").unwrap();
        assert!(Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "--out"])
            .arg(&jpeg)
            .arg(&ppm)
            .status()
            .unwrap()
            .success());
        fs::remove_file(ppm).unwrap();
        let original = fs::read(&jpeg).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Mixed preview queue").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, root.to_str().unwrap(), |_| {}).unwrap();
        let progress = prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |_| {},
        )
        .unwrap();

        assert_eq!(progress.state, "completed");
        assert_eq!(progress.items_completed, 5);
        assert_eq!(progress.items_total, 5);
        assert_eq!(progress.ready_count, 1);
        assert_eq!(progress.corrupt_count, 4);
        assert!(cache_size(&cache) > 0);
        assert_eq!(fs::read(&jpeg).unwrap(), original);
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert!(page.items.iter().any(|item| {
            item.filename == "IMG_9484.JPG"
                && item.thumbnail_preview_url.is_some()
                && item.preview_status == "ready"
        }));
        assert!(page
            .items
            .iter()
            .filter(|item| item.filename != "IMG_9484.JPG")
            .all(|item| item.preview_status == "corrupt"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn provider_failures_and_timeouts_are_terminal_and_do_not_block_later_assets() {
        struct ControlledProvider;
        impl ThumbnailProvider for ControlledProvider {
            fn generate(
                &self,
                source: &Path,
                _media_type: &MediaType,
                destination: &Path,
                _size: media_visual::PreviewSize,
            ) -> Result<media_visual::ProviderResult, media_visual::VisualError> {
                let name = source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                let (status, failure_reason) = if name.starts_with("FAILED") {
                    (ArtifactStatus::Failed, Some("Test provider failed".into()))
                } else if name.starts_with("TIMEOUT") {
                    (
                        ArtifactStatus::Timeout,
                        Some("Test provider timed out".into()),
                    )
                } else {
                    fs::create_dir_all(destination.parent().expect("cache destination parent"))?;
                    fs::write(destination, b"generated-preview")?;
                    (ArtifactStatus::Ready, None)
                };
                Ok(media_visual::ProviderResult {
                    provider: "controlled-test-provider".into(),
                    status,
                    failure_reason,
                })
            }
        }

        let directory = tempdir().unwrap();
        let root = directory.path().join("provider-outcomes");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&root).unwrap();
        for (name, pixels) in [
            ("READY.JPG", "255 0 0  0 255 0"),
            ("FAILED.JPG", "0 255 0  0 0 255"),
            ("TIMEOUT.JPG", "0 0 255  255 255 0"),
        ] {
            let ppm = root.join(format!("{name}.ppm"));
            fs::write(&ppm, format!("P3\n2 1\n255\n{pixels}\n")).unwrap();
            assert!(Command::new("/usr/bin/sips")
                .args(["-s", "format", "jpeg", "--out"])
                .arg(root.join(name))
                .arg(&ppm)
                .status()
                .unwrap()
                .success());
            fs::remove_file(ppm).unwrap();
        }
        let ready_before = fs::read(root.join("READY.JPG")).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Provider outcomes").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, root.to_str().unwrap(), |_| {}).unwrap();
        let candidates = repository
            .visual_preparation_candidates(&project_id, &VisualMediaQuery::default())
            .unwrap();
        let progress = prepare_visual_candidates(
            &repository,
            &project_id,
            candidates,
            &cache,
            &ControlledProvider,
            |_| {},
        )
        .unwrap();

        assert_eq!(progress.items_completed, 3);
        assert_eq!(progress.ready_count, 1);
        assert_eq!(progress.failed_count, 1);
        assert_eq!(progress.timeout_count, 1);
        assert_eq!(progress.error_count, 2);
        assert!(cache_size(&cache) > 0);
        assert_eq!(fs::read(root.join("READY.JPG")).unwrap(), ready_before);

        let retry = retry_failed_visual_media(&repository, &project_id, &cache, |_| {}).unwrap();
        assert_eq!(retry.items_total, 2);
        assert_eq!(retry.items_completed, 2);
        assert_eq!(fs::read(root.join("READY.JPG")).unwrap(), ready_before);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn preparation_uses_available_copy_when_primary_path_is_unreadable() {
        let directory = tempdir().unwrap();
        let first_root = directory.path().join("first");
        let second_root = directory.path().join("second");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let ppm = second_root.join("fixture.ppm");
        let second_jpeg = second_root.join("IMG_9484.JPG");
        fs::write(&ppm, b"P3\n2 1\n255\n255 0 0  0 255 0\n").unwrap();
        assert!(Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "--out"])
            .arg(&second_jpeg)
            .arg(&ppm)
            .status()
            .unwrap()
            .success());
        fs::remove_file(ppm).unwrap();
        fs::copy(&second_jpeg, first_root.join("IMG_9484.JPG")).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Copy fallback").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        // Index the usable copy first so the later, now-missing root is the nominal primary.
        index_local_folder(
            &repository,
            &project_id,
            second_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        index_local_folder(
            &repository,
            &project_id,
            first_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        fs::remove_file(first_root.join("IMG_9484.JPG")).unwrap();

        prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |_| {},
        )
        .unwrap();
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].selected_root.as_deref(),
            second_root.canonicalize().unwrap().to_str()
        );
        assert!(page.items[0].thumbnail_preview_url.is_some());
    }

    #[test]
    fn capture_intelligence_resource_modes_bound_local_worker_count() {
        // Explicit capacities make the policy stable on developer laptops and constrained CI.
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Eco, 0), 1);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Eco, 8), 1);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Balanced, 1), 1);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Balanced, 8), 2);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Fast, 1), 1);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Fast, 3), 3);
        assert_eq!(analysis_worker_limit(AnalysisResourceMode::Fast, 8), 4);
    }

    #[test]
    fn moment_resource_modes_only_change_materialization_yield_cadence() {
        assert_eq!(moment_transform_batch_size(AnalysisResourceMode::Eco), 16);
        assert_eq!(
            moment_transform_batch_size(AnalysisResourceMode::Balanced),
            64
        );
        assert_eq!(moment_transform_batch_size(AnalysisResourceMode::Fast), 256);

        // More than one Fast batch proves that all three modes traverse the exact same durable
        // evidence, even though ECO yields more frequently while materializing it.
        let captured_at = Utc.timestamp_opt(1_704_067_200, 0).single().unwrap();
        let inputs = (0..257)
            .map(|ordinal| MomentAnalysisInput {
                asset_id: format!("moment-resource-fixture-{ordinal:03}"),
                captured_at: Some(
                    (captured_at + chrono::Duration::seconds(i64::from(ordinal))).to_rfc3339(),
                ),
                capture_time_source: Some("catalog".into()),
                camera_model: Some("Fixture camera".into()),
                lens_model: Some("Fixture lens".into()),
                orientation: Some(
                    match ordinal % 3 {
                        0 => "landscape",
                        1 => "portrait",
                        _ => "square",
                    }
                    .into(),
                ),
                face_count: Some((ordinal % 4) as u64),
                technical_quality_band: Some("good".into()),
                technical_quality_score: Some(70.0 + f64::from(ordinal % 25)),
                rating: (ordinal % 6) as u8,
                starred: ordinal % 2 == 0,
                decision: match ordinal % 3 {
                    0 => Some("keep".into()),
                    1 => Some("review".into()),
                    _ => None,
                },
                similar_set_ids: vec![format!("similar-set-{}", ordinal % 5)],
                embedding: Some(vec![1.0, ordinal as f32 + 1.0, 0.5]),
            })
            .collect::<Vec<_>>();

        let eco = materialize_moment_timeline_assets(
            &inputs,
            Some("fixture-semantic-model"),
            AnalysisResourceMode::Eco,
        );
        let balanced = materialize_moment_timeline_assets(
            &inputs,
            Some("fixture-semantic-model"),
            AnalysisResourceMode::Balanced,
        );
        let fast = materialize_moment_timeline_assets(
            &inputs,
            Some("fixture-semantic-model"),
            AnalysisResourceMode::Fast,
        );

        assert_eq!(eco.len(), inputs.len());
        assert_eq!(eco, balanced);
        assert_eq!(balanced, fast);
    }

    #[test]
    fn analysis_input_resolver_needs_original_only_when_every_file_instance_is_offline() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("OFFLINE.JPG"),
            b"bytes never decoded while offline",
        )
        .unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Offline analysis input").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let root_id = repository.index_roots_for_project(&project_id).unwrap()[0]
            .id
            .clone();
        repository
            .mark_root_instances_unavailable(&root_id)
            .unwrap();

        // The file happens to remain on disk, but the FileInstance is explicitly OFFLINE. The
        // resolver must respect catalog availability rather than reading a path behind the
        // user's back; no cached input exists, so NEEDS_ORIGINAL is now accurate.
        let result = analyze_capture_intelligence(
            &repository,
            &project_id,
            &cache,
            AnalysisResourceMode::Eco,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(result.needs_original_count, 1);
        assert_eq!(result.ready_count, 0);
        assert_eq!(result.failed_count, 0);
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(
            page.items[0].intelligence.status.as_deref(),
            Some("needs_original")
        );
        assert!(
            !cache.exists(),
            "offline sources must not produce analysis cache files"
        );
    }

    #[test]
    fn analysis_input_resolver_reports_an_available_unsupported_decoder_as_unsupported() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("DSC03364.ARW"),
            b"raw fixture; no decoder is bundled",
        )
        .unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Unsupported analysis input").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();

        let result = analyze_capture_intelligence(
            &repository,
            &project_id,
            &cache,
            AnalysisResourceMode::Eco,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(result.unsupported_count, 1);
        assert_eq!(result.needs_original_count, 0);
        assert_eq!(result.failed_count, 0);
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(
            page.items[0].intelligence.status.as_deref(),
            Some("unsupported")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn capture_intelligence_end_to_end_uses_real_cached_jpeg_previews() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&cache).unwrap();

        // Generated test raster only: two JPEGs have distinct source bytes but remain visually
        // near-identical. The test then uses the actual M3 preview producer and M4 BMP bridge.
        let mut first = b"P6\n128 128\n255\n".to_vec();
        let mut second = first.clone();
        for y in 0..128_u32 {
            for x in 0..128_u32 {
                let value = if (((x / 16) + (y / 16)) & 1) == 0 {
                    20
                } else {
                    240
                };
                first.extend_from_slice(&[value, value, value]);
                let adjusted = if x == 0 && y == 0 {
                    value.saturating_add(1)
                } else {
                    value
                };
                second.extend_from_slice(&[adjusted, adjusted, adjusted]);
            }
        }
        let first_ppm = source.join("first.ppm");
        let second_ppm = source.join("second.ppm");
        let first_jpeg = source.join("FRAME_0001.JPG");
        let second_jpeg = source.join("FRAME_0002.JPG");
        fs::write(&first_ppm, first).unwrap();
        fs::write(&second_ppm, second).unwrap();
        for (ppm, jpeg, quality) in [
            (&first_ppm, &first_jpeg, "72"),
            (&second_ppm, &second_jpeg, "48"),
        ] {
            assert!(Command::new("/usr/bin/sips")
                .args([
                    "-s",
                    "format",
                    "jpeg",
                    "-s",
                    "formatOptions",
                    quality,
                    "--out",
                ])
                .arg(jpeg)
                .arg(ppm)
                .status()
                .unwrap()
                .success());
        }
        fs::remove_file(&first_ppm).unwrap();
        fs::remove_file(&second_ppm).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Real preview intelligence").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let preparation = prepare_visual_media(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
            |_| {},
        )
        .unwrap();
        assert_eq!(preparation.ready_count, 2);
        let face_provider = platform_face_provider_identity();
        let reusable_inputs = repository
            .analysis_input_candidates(
                &project_id,
                "captureos-deterministic-image",
                "m4.det.v1",
                capture_intelligence::ANALYSIS_SETTINGS_VERSION,
                &FaceAnalysisProviderConfig::new(
                    face_provider.provider,
                    face_provider.provider_version,
                    FACE_ANALYSIS_SETTINGS_VERSION,
                ),
            )
            .unwrap();
        assert_eq!(reusable_inputs.len(), 2);
        assert!(reusable_inputs.iter().all(|candidate| {
            candidate.preview_artifact_type.as_deref() == Some("thumbnail")
                && candidate.preview_size_class.as_deref() == Some("preview")
                && candidate.preview_status == "ready"
        }));
        let analysis = analyze_capture_intelligence(
            &repository,
            &project_id,
            &cache,
            AnalysisResourceMode::Eco,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(analysis.ready_count, 2);
        assert_eq!(analysis.failed_count, 0);

        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page
            .items
            .iter()
            .all(|item| item.intelligence.status.as_deref() == Some("ready")));
        assert!(page
            .items
            .iter()
            .any(|item| item.intelligence.similar_count >= 2));
        assert!(
            !cache.join(ANALYSIS_PREVIEW_GENERATOR_VERSION).exists(),
            "a sufficient existing PREVIEW is reused without generating another artifact"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn analysis_input_resolver_generates_and_reuses_local_analysis_previews() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("available-originals");
        let cache = directory.path().join("preview-cache");
        let catalog = directory.path().join("captureos.sqlite3");
        fs::create_dir_all(&source).unwrap();
        for index in 0..12_u8 {
            write_real_test_jpeg(
                &source.join(format!("DSC{:05}.JPG", u32::from(index) + 3300)),
                192,
                128,
                index,
            );
        }
        let originals_before = fs::read_dir(&source)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();

        let project_id = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let project = create_local_project(&repository, "Analysis preview generation").unwrap();
            let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
            index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();

            // There is deliberately no M3 browsing preparation here. An available JPEG must
            // enter the resolver as NEEDS_INPUT, not NEEDS_ORIGINAL, and generate its own
            // CaptureOS-owned analysis image automatically.
            let queued = repository
                .analysis_input_candidates(
                    &project_id,
                    "captureos-deterministic-image",
                    "m4.det.v1",
                    capture_intelligence::ANALYSIS_SETTINGS_VERSION,
                    &FaceAnalysisProviderConfig::new(
                        "none",
                        "no-approved-local-provider.v1",
                        FACE_ANALYSIS_SETTINGS_VERSION,
                    ),
                )
                .unwrap();
            assert_eq!(queued.len(), 12);
            assert!(queued
                .iter()
                .all(|candidate| candidate.preview_status == "needs_input"));

            // A ready 768px browsing artifact is deliberately not sufficient for the current
            // technical/face input contract. It must cause one high-resolution dedicated cache
            // artifact to be generated, rather than being silently analyzed as though it met
            // the 1600px minimum.
            let first = queued.first().expect("twelve indexed candidates");
            let first_copy = repository
                .preparation_file_instance_candidates(&first.asset_id)
                .unwrap()
                .into_iter()
                .find(|copy| copy.is_available)
                .expect("available first source");
            let medium_relative = "test/insufficient-medium.ppm";
            let medium_path = cache.join(medium_relative);
            fs::create_dir_all(medium_path.parent().unwrap()).unwrap();
            fs::write(&medium_path, b"P3\n2 1\n255\n255 0 0  0 255 0\n").unwrap();
            repository
                .upsert_preview_artifact(&PreviewArtifactRecord {
                    id: Uuid::new_v4().to_string(),
                    media_asset_id: first_copy.asset_id.clone(),
                    source_file_instance_id: first_copy.file_instance_id.clone(),
                    artifact_type: "thumbnail".into(),
                    size_class: "medium".into(),
                    cache_relative_path: medium_relative.into(),
                    provider: "test".into(),
                    generator_version: "test".into(),
                    source_fingerprint: first_copy.source_fingerprint.clone(),
                    created_at: Utc::now().to_rfc3339(),
                    status: "ready".into(),
                    failure_reason: None,
                })
                .unwrap();
            let with_insufficient_preview = repository
                .analysis_input_candidates(
                    &project_id,
                    "captureos-deterministic-image",
                    "m4.det.v1",
                    capture_intelligence::ANALYSIS_SETTINGS_VERSION,
                    &FaceAnalysisProviderConfig::new(
                        "none",
                        "no-approved-local-provider.v1",
                        FACE_ANALYSIS_SETTINGS_VERSION,
                    ),
                )
                .unwrap();
            assert!(with_insufficient_preview.iter().any(|candidate| {
                candidate.asset_id == first_copy.asset_id
                    && candidate.preview_status == "ready"
                    && candidate.preview_size_class.as_deref() == Some("medium")
            }));

            let completed = analyze_capture_intelligence(
                &repository,
                &project_id,
                &cache,
                AnalysisResourceMode::Balanced,
                || false,
                |_| {},
            )
            .unwrap();
            assert_eq!(completed.ready_count, 12);
            assert_eq!(completed.needs_original_count, 0);
            assert_eq!(completed.failed_count, 0);

            let page = load_visual_media_page(
                &repository,
                &project_id,
                &VisualMediaQuery::default(),
                &cache,
            )
            .unwrap();
            assert_eq!(page.items.len(), 12);
            for item in page.items {
                assert_eq!(item.intelligence.status.as_deref(), Some("ready"));
                let copy = repository
                    .preparation_file_instance_candidates(&item.asset_id)
                    .unwrap()
                    .into_iter()
                    .find(|copy| copy.is_available)
                    .expect("available original copy");
                let relative = media_visual::analysis_cache_relative_path(
                    &copy.asset_id,
                    &copy.file_instance_id,
                    &copy.source_fingerprint,
                    &MediaType::Jpeg,
                );
                assert!(cache.join(relative).is_file());
            }
            assert_eq!(
                analyze_capture_intelligence(
                    &repository,
                    &project_id,
                    &cache,
                    AnalysisResourceMode::Fast,
                    || false,
                    |_| {},
                )
                .unwrap()
                .items_total,
                0,
                "a ready artifact and input cache must not regenerate on reopen"
            );
            project_id
        };

        // Closing and reopening the catalog preserves READY results and the contained cache;
        // source bytes remain byte-for-byte untouched throughout generation and analysis.
        for (name, before) in originals_before {
            assert_eq!(fs::read(source.join(name)).unwrap(), before);
        }
        let reopened = SqliteRepository::open(&catalog).unwrap();
        assert_eq!(
            analyze_capture_intelligence(
                &reopened,
                &project_id,
                &cache,
                AnalysisResourceMode::Eco,
                || false,
                |_| {},
            )
            .unwrap()
            .items_total,
            0
        );
        assert_eq!(
            load_capture_intelligence_summary(&reopened, &project_id)
                .unwrap()
                .unwrap()
                .ready_count,
            12
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn analysis_input_resolver_uses_another_available_copy_when_preferred_copy_disappears() {
        let directory = tempdir().unwrap();
        let first_root = directory.path().join("first-root");
        let second_root = directory.path().join("second-root");
        let cache = directory.path().join("preview-cache");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let surviving = second_root.join("DSC03364.JPG");
        write_real_test_jpeg(&surviving, 192, 128, 4);
        fs::copy(&surviving, first_root.join("DSC03364.JPG")).unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Analysis copy fallback").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        // Index the surviving copy first, then make the later (nominally preferred) instance
        // disappear without an availability refresh. The resolver must continue to the next
        // deterministic available copy rather than report NEEDS_ORIGINAL.
        index_local_folder(
            &repository,
            &project_id,
            second_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        index_local_folder(
            &repository,
            &project_id,
            first_root.to_str().unwrap(),
            |_| {},
        )
        .unwrap();
        fs::remove_file(first_root.join("DSC03364.JPG")).unwrap();

        let completed = analyze_capture_intelligence(
            &repository,
            &project_id,
            &cache,
            AnalysisResourceMode::Eco,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(completed.ready_count, 1);
        assert_eq!(completed.needs_original_count, 0);
        let page = load_visual_media_page(
            &repository,
            &project_id,
            &VisualMediaQuery::default(),
            &cache,
        )
        .unwrap();
        assert_eq!(page.items[0].intelligence.status.as_deref(), Some("ready"));
        let surviving_copy = repository
            .preparation_file_instance_candidates(&page.items[0].asset_id)
            .unwrap()
            .into_iter()
            .find(|copy| {
                copy.selected_root.as_deref() == second_root.canonicalize().unwrap().to_str()
            })
            .expect("surviving FileInstance");
        assert!(cache
            .join(media_visual::analysis_cache_relative_path(
                &surviving_copy.asset_id,
                &surviving_copy.file_instance_id,
                &surviving_copy.source_fingerprint,
                &MediaType::Jpeg,
            ))
            .is_file());
    }

    #[test]
    fn capture_intelligence_uses_cached_previews_groups_related_frames_and_reuses_results() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let cache = directory.path().join("preview-cache");
        let catalog = directory.path().join("captureos.sqlite3");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&cache).unwrap();
        // Source bytes are deliberately distinct so Index Mode keeps three logical assets. Their
        // test-safe cached PPM previews below control visual evidence without touching sources.
        for (name, bytes) in [
            ("A.JPG", b"source-a".as_slice()),
            ("B.JPG", b"source-b".as_slice()),
            ("CORRUPT.JPG", b"source-corrupt".as_slice()),
        ] {
            fs::write(source.join(name), bytes).unwrap();
        }
        let project_id = {
            let repository = SqliteRepository::open(&catalog).unwrap();
            let project = create_local_project(&repository, "Intelligence cache").unwrap();
            let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
            index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
            let page = repository
                .visual_media_page(&project_id, &VisualMediaQuery::default())
                .unwrap();
            for item in page.items {
                let source_fingerprint = repository
                    .preparation_file_instance_candidates(&item.asset_id)
                    .unwrap()
                    .into_iter()
                    .next()
                    .expect("indexed asset has a FileInstance")
                    .source_fingerprint;
                let status = if item.filename == "CORRUPT.JPG" {
                    "corrupt"
                } else {
                    "ready"
                };
                let relative = format!("m4/{}/analysis.ppm", item.asset_id);
                let artifact = cache.join(&relative);
                fs::create_dir_all(artifact.parent().unwrap()).unwrap();
                if status == "ready" {
                    // Same composition at two resolutions exercises resize-stable pHash grouping.
                    let dimensions = if item.filename == "A.JPG" {
                        "64 64"
                    } else {
                        "128 128"
                    };
                    let mut pixels = String::new();
                    let (width, height) = if item.filename == "A.JPG" {
                        (64, 64)
                    } else {
                        (128, 128)
                    };
                    for y in 0..height {
                        for x in 0..width {
                            let value = if (((x * 8 / width) + (y * 8 / height)) & 1) == 0 {
                                20
                            } else {
                                240
                            };
                            pixels.push_str(&format!("{value} {value} {value} "));
                        }
                    }
                    fs::write(&artifact, format!("P3\n{dimensions}\n255\n{pixels}\n")).unwrap();
                }
                repository
                    .upsert_preview_artifact(&PreviewArtifactRecord {
                        id: Uuid::new_v4().to_string(),
                        media_asset_id: item.asset_id,
                        source_file_instance_id: item.file_instance_id,
                        artifact_type: "thumbnail".into(),
                        size_class: "preview".into(),
                        cache_relative_path: relative,
                        provider: "test-cache".into(),
                        generator_version: "m4-test".into(),
                        // Artifacts are reusable only when they belong to the current source
                        // evidence. This mirrors the resolver's production lookup rather than
                        // letting a synthetic cache artifact mask changed media bytes.
                        source_fingerprint,
                        created_at: Utc::now().to_rfc3339(),
                        status: status.into(),
                        failure_reason: if status == "corrupt" {
                            Some("synthetic corrupt fixture".into())
                        } else {
                            None
                        },
                    })
                    .unwrap();
            }

            let paused = analyze_capture_intelligence(
                &repository,
                &project_id,
                &cache,
                AnalysisResourceMode::Eco,
                || true,
                |_| {},
            )
            .unwrap();
            assert_eq!(paused.state, "paused");
            let completed = analyze_capture_intelligence(
                &repository,
                &project_id,
                &cache,
                AnalysisResourceMode::Balanced,
                || false,
                |_| {},
            )
            .unwrap();
            assert_eq!(completed.state, "completed");
            assert_eq!(completed.ready_count, 2);
            assert_eq!(completed.corrupt_count, 1);
            assert_eq!(completed.failed_count, 0);

            let visual = load_visual_media_page(
                &repository,
                &project_id,
                &VisualMediaQuery::default(),
                &cache,
            )
            .unwrap();
            let ready = visual
                .items
                .iter()
                .find(|item| item.filename == "A.JPG")
                .unwrap();
            assert_eq!(ready.intelligence.status.as_deref(), Some("ready"));
            assert_eq!(ready.intelligence.similar_count, 2);
            let corrupt = visual
                .items
                .iter()
                .find(|item| item.filename == "CORRUPT.JPG")
                .unwrap();
            assert_eq!(corrupt.intelligence.status.as_deref(), Some("corrupt"));
            let group = load_similarity_group(
                &repository,
                &project_id,
                &MediaAssetId::try_from(ready.asset_id.as_str()).unwrap(),
                24,
                0,
            )
            .unwrap()
            .unwrap();
            assert_eq!(group.members.len(), 2);
            assert!(group
                .members
                .iter()
                .all(|member| member.medium_preview_url.is_some()));

            let similar_groups = load_visual_media_page(
                &repository,
                &project_id,
                &VisualMediaQuery {
                    filter: persistence::VisualMediaFilter::SimilarGroups,
                    ..VisualMediaQuery::default()
                },
                &cache,
            )
            .unwrap();
            assert_eq!(similar_groups.items.len(), 2);

            // A second pass sees matching source/provider/settings artifacts and does not decode
            // or replace successful results merely because the UI was reopened.
            let cached = analyze_capture_intelligence(
                &repository,
                &project_id,
                &cache,
                AnalysisResourceMode::Fast,
                || false,
                |_| {},
            )
            .unwrap();
            assert_eq!(cached.items_total, 0);
            project_id
        };
        let reopened = SqliteRepository::open(&catalog).unwrap();
        let summary = load_capture_intelligence_summary(&reopened, &project_id)
            .unwrap()
            .unwrap();
        assert_eq!(summary.ready_count, 2);
        assert_eq!(summary.corrupt_count, 1);
        let page =
            load_visual_media_page(&reopened, &project_id, &VisualMediaQuery::default(), &cache)
                .unwrap();
        assert_eq!(
            page.items
                .iter()
                .filter(|item| item.intelligence.status.as_deref() == Some("ready"))
                .count(),
            2
        );
    }

    #[test]
    fn m6_indexing_uses_managed_cached_previews_and_remains_searchable_when_sources_are_offline() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("preview-cache");
        let index_root = directory.path().join("semantic-index");
        fs::create_dir_all(&cache).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let created_at = Utc.timestamp_opt(1_704_067_200, 0).single().unwrap();
        let project = Project {
            id: pid(960),
            name: "M6 managed cache".into(),
            created_at,
        };
        let storage = volume(
            961,
            "M6 source offline",
            MountState::Offline,
            None,
            Some("m6-cache-test"),
        );
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&storage).unwrap();
        let first = asset(
            962,
            &project,
            MediaType::Jpeg,
            "FIRST.JPG",
            created_at,
            "m6-first",
        );
        let second = asset(
            963,
            &project,
            MediaType::Jpeg,
            "SECOND.JPG",
            created_at,
            "m6-second",
        );
        for (offset, asset) in [&first, &second].into_iter().enumerate() {
            repository.insert_media_asset(asset).unwrap();
            let instance = file(
                964 + offset as u128,
                asset,
                &storage,
                &format!("offline/{}", asset.display_name),
                created_at,
                false,
            );
            repository.insert_file_instance(&instance).unwrap();
            let relative = format!("m6/{}/preview.ppm", asset.id);
            let preview = cache.join(&relative);
            fs::create_dir_all(preview.parent().unwrap()).unwrap();
            fs::write(&preview, b"P3\n1 1\n255\n255 0 0\n").unwrap();
            repository
                .upsert_preview_artifact(&PreviewArtifactRecord {
                    id: Uuid::new_v4().to_string(),
                    media_asset_id: asset.id.to_string(),
                    source_file_instance_id: instance.id.to_string(),
                    artifact_type: "thumbnail".into(),
                    size_class: "preview".into(),
                    cache_relative_path: relative,
                    provider: "test-managed-cache".into(),
                    generator_version: "m6-test".into(),
                    source_fingerprint: asset.fingerprint.fast_fingerprint.clone().unwrap(),
                    created_at: Utc::now().to_rfc3339(),
                    status: "ready".into(),
                    failure_reason: None,
                })
                .unwrap();
        }
        let provider = FixtureSemanticProvider::new(
            3,
            vec![1.0, 0.0, 0.0],
            HashMap::from([("red".into(), vec![1.0, 0.0, 0.0])]),
        );
        let should_not_pause = || false;
        let mut ignore_progress: fn(&SemanticIndexProgress) = |_| {};
        let progress = index_semantic_embeddings_with_provider(
            &repository,
            &project.id,
            SemanticStorageRoots {
                preview_cache_root: &cache,
                index_root: &index_root,
            },
            &mut SemanticIndexExecution {
                resource_mode: AnalysisResourceMode::Eco,
                provider: &provider,
                model_status: SemanticModelStatus {
                    installed: true,
                    message: Some("fixture only".into()),
                    identity: None,
                },
                should_pause: &should_not_pause,
                on_progress: &mut ignore_progress,
            },
        )
        .unwrap();
        assert_eq!(progress.state, "completed");
        assert!(progress.index_ready);
        assert_eq!(progress.counts.ready, 2);
        let model = semantic_model_config(provider.identity());
        assert!(load_active_semantic_index(
            &repository,
            &project.id,
            &model,
            &provider.identity().cache_key(),
            &index_root,
        )
        .unwrap()
        .is_some());

        // The original FileInstances are already offline; remove the temporary cache previews
        // too. Find Similar still operates from durable local embeddings and the derived index.
        fs::remove_dir_all(cache.join("m6")).unwrap();
        let result = find_similar_with_provider(
            &repository,
            &project.id,
            FindSimilarRequest {
                asset_id: &first.id,
                limit: 10,
                offset: 0,
            },
            FindSimilarExecution {
                provider: &provider,
                index_root: &index_root,
                preview_cache_root: &cache,
            },
        )
        .unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].item.asset_id, second.id.to_string());
    }

    #[test]
    fn m6_cached_embeddings_are_project_scoped_and_find_similar_never_touches_culling_or_sets() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("preview-cache");
        let index_root = directory.path().join("semantic-index");
        fs::create_dir_all(&cache).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let created_at = Utc.timestamp_opt(1_704_067_200, 0).single().unwrap();
        let first_project = Project {
            id: pid(950),
            name: "M6 first".into(),
            created_at,
        };
        let second_project = Project {
            id: pid(951),
            name: "M6 second".into(),
            created_at,
        };
        let storage = volume(
            952,
            "M6 offline cache fixture",
            MountState::Offline,
            None,
            Some("m6-offline-cache"),
        );
        repository.insert_project(&first_project).unwrap();
        repository.insert_project(&second_project).unwrap();
        repository.insert_storage_volume(&storage).unwrap();
        let source = asset(
            953,
            &first_project,
            MediaType::Jpeg,
            "SOURCE.JPG",
            created_at,
            "m6-source",
        );
        let related = asset(
            954,
            &first_project,
            MediaType::Jpeg,
            "RELATED.JPG",
            created_at,
            "m6-related",
        );
        let other = asset(
            955,
            &first_project,
            MediaType::Jpeg,
            "OTHER.JPG",
            created_at,
            "m6-other",
        );
        let isolated = asset(
            956,
            &second_project,
            MediaType::Jpeg,
            "ISOLATED.JPG",
            created_at,
            "m6-isolated",
        );
        for (offset, asset) in [&source, &related, &other, &isolated]
            .into_iter()
            .enumerate()
        {
            repository.insert_media_asset(asset).unwrap();
            // Simulate originals being offline after a prior local indexing pass. Search must
            // use persisted local embeddings rather than reaching into a source path.
            repository
                .insert_file_instance(&file(
                    1000 + offset as u128,
                    asset,
                    &storage,
                    &format!("offline/{}", asset.display_name),
                    created_at,
                    false,
                ))
                .unwrap();
        }
        let provider = FixtureSemanticProvider::new(
            3,
            vec![1.0, 0.0, 0.0],
            HashMap::from([("yellow boat".into(), vec![1.0, 0.0, 0.0])]),
        );
        let model = semantic_model_config(provider.identity());
        for (asset, vector) in [
            (&source, vec![1.0, 0.0, 0.0]),
            (&related, vec![1.0, 0.0, 0.0]),
            (&other, vec![0.0, 1.0, 0.0]),
            (&isolated, vec![1.0, 0.0, 0.0]),
        ] {
            repository
                .upsert_semantic_embedding(&SemanticEmbeddingRecord {
                    media_asset_id: asset.id.clone(),
                    project_id: asset.project_id.clone(),
                    input_fingerprint: format!("{}-input", asset.id),
                    model: model.clone(),
                    embedding: Some(vector),
                    generated_at: Utc::now(),
                    status: AnalysisStatus::Ready,
                    error_message: None,
                })
                .unwrap();
        }
        let build = PersistentVectorIndex::build_and_store(
            &index_root,
            &first_project.id.to_string(),
            &provider.identity().cache_key(),
            model.dimensions,
            repository
                .semantic_embeddings_for_index(&first_project.id, &model)
                .unwrap()
                .into_iter()
                .map(|vector| (vector.asset_id, vector.vector)),
        )
        .unwrap();
        let indexed_at = Utc::now();
        repository
            .upsert_semantic_index_version(&SemanticIndexVersion {
                id: SemanticIndexVersionId::new(),
                project_id: first_project.id.clone(),
                model: model.clone(),
                index_format: SEMANTIC_INDEX_FORMAT.into(),
                index_relative_path: build.relative_path,
                index_checksum: build.checksum,
                embedding_count: build.vector_count as u64,
                status: "ready".into(),
                stale: false,
                created_at: indexed_at,
                rebuilt_at: indexed_at,
            })
            .unwrap();

        let response = find_similar_with_provider(
            &repository,
            &first_project.id,
            FindSimilarRequest {
                asset_id: &source.id,
                limit: 10,
                offset: 0,
            },
            FindSimilarExecution {
                provider: &provider,
                index_root: &index_root,
                preview_cache_root: &cache,
            },
        )
        .unwrap();
        assert!(response.semantic_available && response.semantic_applied);
        assert_eq!(response.results[0].item.asset_id, related.id.to_string());
        assert!(response
            .results
            .iter()
            .all(|result| result.item.asset_id != source.id.to_string()));
        assert!(response
            .results
            .iter()
            .all(|result| result.item.asset_id != isolated.id.to_string()));
        assert!(response.results[0]
            .explanation
            .contains("does not create or alter Similar Sets"));
        assert!(repository
            .similarity_group_for_asset(&source.id, 10, 0)
            .unwrap()
            .is_none());

        // A missing static model pack must not turn language into a broad or fabricated match.
        let unavailable_provider_cache =
            SiglipProviderCache::new(directory.path().join("no-model-pack"));
        let unavailable = search_magic(
            &repository,
            &first_project.id,
            &MagicSearchRequest {
                query: "yellow boat".into(),
                ..MagicSearchRequest::default()
            },
            &unavailable_provider_cache,
            &index_root,
            &cache,
        )
        .unwrap();
        assert!(!unavailable.semantic_available && !unavailable.semantic_applied);
        assert!(unavailable.results.is_empty());
        assert_eq!(
            load_magic_search_history(&repository, &first_project.id, 10)
                .unwrap()
                .len(),
            1
        );

        let hybrid_plan = plan_query("yellow boat with 2 faces").unwrap();
        let hybrid = score_semantic_candidates(
            vec![
                SemanticSearchCandidate {
                    asset_id: source.id.to_string(),
                    vector: Some(vec![1.0, 0.0, 0.0]),
                    face_count: Some(2),
                    rating: 0,
                    decision: None,
                    sharpness_band: None,
                    blur_level: None,
                    technical_quality_band: None,
                    technical_quality_score: None,
                    camera_model: None,
                    captured_at: None,
                },
                SemanticSearchCandidate {
                    asset_id: related.id.to_string(),
                    vector: Some(vec![1.0, 0.0, 0.0]),
                    face_count: Some(1),
                    rating: 0,
                    decision: None,
                    sharpness_band: None,
                    blur_level: None,
                    technical_quality_band: None,
                    technical_quality_score: None,
                    camera_model: None,
                    captured_at: None,
                },
            ],
            &hybrid_plan,
            &[1.0, 0.0, 0.0],
            "relevance",
            true,
        )
        .unwrap();
        assert_eq!(hybrid.len(), 1);
        assert_eq!(hybrid[0].candidate.asset_id, source.id.to_string());
    }

    #[test]
    fn culling_report_is_explicit_non_destructive_and_never_overwrites() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let original = source.join("DSC00001.JPG");
        let original_bytes = b"source media must remain byte-for-byte unchanged";
        fs::write(&original, original_bytes).unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Culling Report").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let workspace = load_culling_workspace(
            &repository,
            &project_id,
            &CullingQuery::default(),
            directory.path(),
        )
        .unwrap();
        let asset_id = MediaAssetId::try_from(workspace.items[0].media.asset_id.as_str()).unwrap();
        apply_culling_decision(
            &repository,
            &project_id,
            &asset_id,
            &CullingDecisionUpdate {
                decision: Some(CullingDecisionValue::Keep),
                clear_decision: false,
                rating: Some(5),
                starred: Some(true),
                note: Some("Album".into()),
                flags: None,
                session_id: Some(workspace.session.id),
            },
        )
        .unwrap();
        let report = directory.path().join("Culling Report.csv");
        assert_eq!(
            export_culling_report(&repository, &project_id, &report, "csv").unwrap(),
            1
        );
        let body = fs::read_to_string(&report).unwrap();
        assert!(body.contains("DSC00001.JPG") && body.contains("keep"));
        assert_eq!(fs::read(&original).unwrap(), original_bytes);
        assert!(export_culling_report(&repository, &project_id, &report, "csv").is_err());
        assert_eq!(fs::read(&original).unwrap(), original_bytes);
    }

    #[test]
    fn m7_local_timeline_persists_human_structure_and_uses_a_bounded_append_update() {
        let directory = tempdir().unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let captured_at = Utc.timestamp_opt(1_704_067_200, 0).single().unwrap();
        let project = Project {
            id: pid(1_100),
            name: "M7 local timeline".into(),
            created_at: captured_at,
        };
        let storage = volume(
            1_101,
            "M7 fixture storage",
            MountState::Offline,
            None,
            Some("m7-local-timeline"),
        );
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&storage).unwrap();
        let mut assets = Vec::new();
        for ordinal in 0..8u128 {
            let seconds = if ordinal < 4 {
                ordinal as i64
            } else {
                600 + (ordinal as i64 - 4)
            };
            let asset = asset(
                1_110 + ordinal,
                &project,
                MediaType::Jpeg,
                &format!("M7_{ordinal:04}.JPG"),
                captured_at + chrono::Duration::seconds(seconds),
                &format!("m7-{ordinal}"),
            );
            repository.insert_media_asset(&asset).unwrap();
            repository
                .insert_file_instance(&file(
                    1_130 + ordinal,
                    &asset,
                    &storage,
                    &format!("offline/{}", asset.display_name),
                    captured_at,
                    false,
                ))
                .unwrap();
            assets.push(asset);
        }
        let unavailable_model = SiglipProviderCache::new(directory.path().join("no-model-pack"));
        let first = start_moment_analysis(
            &repository,
            &project.id,
            &unavailable_model,
            AnalysisResourceMode::Balanced,
            false,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(first.timeline_ready);
        let initial = repository.moment_timeline_page(&project.id, 20, 0).unwrap();
        assert!(initial.moments.len() >= 2);
        assert!(initial
            .moments
            .iter()
            .all(|moment| moment.display_label == "Untitled Moment"));
        let initial_status = repository
            .moment_timeline_status(&project.id)
            .unwrap()
            .unwrap();
        // A normal Update with no catalog changes is a no-op. Repeating it must neither append
        // Moments/memberships nor allocate a replacement run.
        for _ in 0..2 {
            let unchanged = start_moment_analysis(
                &repository,
                &project.id,
                &unavailable_model,
                AnalysisResourceMode::Balanced,
                false,
                || false,
                |_| {},
            )
            .unwrap();
            assert!(unchanged.timeline_ready);
            let status = repository
                .moment_timeline_status(&project.id)
                .unwrap()
                .unwrap();
            assert_eq!(status.active_run_id, initial_status.active_run_id);
            assert_eq!(status.moment_count, initial_status.moment_count);
        }
        let first_moment = initial.moments[0].clone();
        let member_page = repository
            .visual_media_page(
                &project.id,
                &VisualMediaQuery {
                    moment_id: Some(first_moment.id.clone()),
                    ..VisualMediaQuery::default()
                },
            )
            .unwrap();
        assert!(member_page.items.len() >= 2);

        rename_moment(
            &repository,
            &project.id,
            &first_moment.id,
            "Photographer's timeline name",
        )
        .unwrap();
        let representative_id =
            MediaAssetId::try_from(member_page.items[0].asset_id.as_str()).unwrap();
        let representative_id_text = representative_id.to_string();
        set_moment_human_representative(
            &repository,
            &project.id,
            &first_moment.id,
            &representative_id,
        )
        .unwrap();
        split_moment(
            &repository,
            &project.id,
            &first_moment.id,
            &representative_id,
        )
        .unwrap();
        let after_split = repository.moment_timeline_page(&project.id, 20, 0).unwrap();
        assert!(after_split.moments.len() > initial.moments.len());
        let adjacent = after_split.moments[1].clone();
        merge_adjacent_moments(
            &repository,
            &project.id,
            &after_split.moments[0].id,
            &adjacent.id,
        )
        .unwrap();
        create_coverage_checklist_item(
            &repository,
            &project.id,
            &CreateCoverageChecklistItemInput {
                checklist_id: None,
                phrase: "Photographer-confirmed coverage".into(),
            },
        )
        .unwrap();
        let checklist_item_id = load_moment_checklists(&repository, &project.id).unwrap()[0].items
            [0]
        .id
        .clone();
        update_coverage_confirmation(
            &repository,
            &project.id,
            &UpdateCoverageConfirmationInput {
                checklist_item_id: checklist_item_id.clone(),
                state: "confirmed_covered".into(),
                moment_id: Some(first_moment.id.clone()),
                asset_id: Some(representative_id_text.clone()),
            },
        )
        .unwrap();

        // An explicit rebuild reuses only durable local evidence, and must not erase the
        // photographer's separate label/representative, coverage confirmation, or protected
        // split/merge intent.
        let rebuilt_progress = start_moment_analysis(
            &repository,
            &project.id,
            &unavailable_model,
            AnalysisResourceMode::Eco,
            true,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(rebuilt_progress.resource_mode, "eco");
        let rebuilt = repository.moment_timeline_page(&project.id, 20, 0).unwrap();
        assert!(rebuilt
            .moments
            .iter()
            .any(|moment| moment.human_label.as_deref() == Some("Photographer's timeline name")));
        assert!(rebuilt
            .moments
            .iter()
            .any(|moment| moment.human_override_present));
        let rebuilt_checklists = load_moment_checklists(&repository, &project.id).unwrap();
        let rebuilt_confirmation = rebuilt_checklists[0]
            .items
            .iter()
            .find(|item| item.id == checklist_item_id)
            .unwrap();
        assert_eq!(
            rebuilt_confirmation.state, "confirmed_covered",
            "a rebuild preserves the photographer's coverage decision"
        );
        assert_eq!(
            rebuilt_confirmation.confirmed_asset_id.as_deref(),
            Some(representative_id_text.as_str())
        );
        let rebuilt_confirmation_moment_id =
            rebuilt_confirmation.confirmed_moment_id.as_ref().unwrap();
        assert_ne!(rebuilt_confirmation_moment_id, &first_moment.id);
        assert!(rebuilt
            .moments
            .iter()
            .any(|moment| &moment.id == rebuilt_confirmation_moment_id));
        let first_rebuild_run = repository
            .moment_timeline_status(&project.id)
            .unwrap()
            .unwrap()
            .active_run_id;
        // Rebuild is deliberately a new immutable analysis run even if source evidence is
        // unchanged. It replaces only the generated projection; all human authority remains.
        let rebuilt_twice = start_moment_analysis(
            &repository,
            &project.id,
            &unavailable_model,
            AnalysisResourceMode::Eco,
            true,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(rebuilt_twice.timeline_ready);
        let second_rebuild_status = repository
            .moment_timeline_status(&project.id)
            .unwrap()
            .unwrap();
        assert_ne!(second_rebuild_status.active_run_id, first_rebuild_run);
        assert_eq!(
            second_rebuild_status.eligible_count,
            initial_status.eligible_count
        );
        let rebuilt_twice_page = repository.moment_timeline_page(&project.id, 20, 0).unwrap();
        assert_eq!(
            rebuilt_twice_page
                .moments
                .iter()
                .map(|moment| moment.ordinal)
                .collect::<Vec<_>>(),
            (0..rebuilt_twice_page.moments.len() as u64).collect::<Vec<_>>()
        );
        assert!(rebuilt_twice_page
            .moments
            .iter()
            .any(|moment| moment.human_label.as_deref() == Some("Photographer's timeline name")));
        assert!(rebuilt_twice_page
            .moments
            .iter()
            .any(|moment| moment.human_override_present));

        let appended = asset(
            1_150,
            &project,
            MediaType::Jpeg,
            "M7_0008.JPG",
            captured_at + chrono::Duration::seconds(1_300),
            "m7-appended",
        );
        repository.insert_media_asset(&appended).unwrap();
        repository
            .insert_file_instance(&file(
                1_151,
                &appended,
                &storage,
                "offline/M7_0008.JPG",
                captured_at,
                false,
            ))
            .unwrap();
        let incremental = start_moment_analysis(
            &repository,
            &project.id,
            &unavailable_model,
            AnalysisResourceMode::Balanced,
            false,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(incremental.timeline_ready);
        let latest_job = repository
            .latest_moment_analysis_job(&project.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            latest_job
                .resume_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("incremental"))
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "latest moment job: {latest_job:#?}"
        );
        let final_status = repository
            .moment_timeline_status(&project.id)
            .unwrap()
            .unwrap();
        assert_eq!(final_status.eligible_count, 9);
        assert_eq!(final_status.ungrouped_count, 0);
        assert_eq!(
            repository
                .visual_media_page(&project.id, &VisualMediaQuery::default())
                .unwrap()
                .items
                .len(),
            9
        );
    }

    #[test]
    fn m7_persistence_failure_keeps_the_prior_timeline_message_user_safe() {
        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = Project {
            id: pid(1_180),
            name: "M7 persistence recovery".into(),
            created_at: Utc::now(),
        };
        repository.insert_project(&project).unwrap();
        let now = Utc::now();
        let mut job = BackgroundJob {
            id: job_id(1_181),
            state: WorkflowRunState::Running,
            stage: JobStage::MomentAnalysis,
            items_completed: 3,
            items_total: Some(3),
            files_discovered: 3,
            files_processed: 3,
            error_count: 0,
            project_id: Some(project.id.clone()),
            index_root_id: None,
            error_message: None,
            resume_metadata: Some(serde_json::json!({ "pipeline": "moment-analysis" })),
            created_at: now,
            updated_at: now,
            finished_at: None,
        };
        repository.insert_background_job(&job).unwrap();
        let previous_timeline = MomentTimelineStatusRecord {
            timeline_id: "m7-recovery-timeline".into(),
            project_id: project.id.to_string(),
            state: "ready".into(),
            analyzer_id: MOMENT_ANALYZER_ID.into(),
            analyzer_version: "test".into(),
            boundary_algorithm_version: MOMENT_BOUNDARY_ALGORITHM_VERSION.into(),
            semantic_model_key: None,
            input_catalog_version: "test-inputs".into(),
            active_run_id: Some("m7-previous-run".into()),
            moment_count: 2,
            eligible_count: 3,
            ungrouped_count: 0,
            updated_at: now.to_rfc3339(),
        };
        let raw_detail = "Local Moment projection could not be saved: database error: UNIQUE constraint failed: moment_records.run_id, moment_records.ordinal";
        let progress = fail_moment_analysis(
            &repository,
            &mut job,
            Some(&previous_timeline),
            &mut |_| {},
            raw_detail.into(),
        )
        .unwrap();
        assert_eq!(
            progress.message.as_deref(),
            Some("Timeline update could not be saved. Your previous timeline is still available.")
        );
        assert_eq!(progress.last_error.as_deref(), Some(raw_detail));
        let saved = repository.get_background_job(&job.id).unwrap().unwrap();
        assert_eq!(saved.error_message.as_deref(), Some(raw_detail));
        assert_eq!(
            saved
                .resume_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("user_message"))
                .and_then(serde_json::Value::as_str),
            Some("Timeline update could not be saved. Your previous timeline is still available.")
        );
        let reloaded = moment_progress(Some(&saved), Some(&previous_timeline), None);
        assert_eq!(reloaded.message, progress.message);
        assert_eq!(reloaded.last_error.as_deref(), Some(raw_detail));
    }

    #[test]
    fn m7_moment_card_search_requires_a_compatible_local_model_and_never_exposes_scores() {
        let directory = tempdir().unwrap();
        let repository = SqliteRepository::open_in_memory().unwrap();
        let captured_at = Utc.timestamp_opt(1_704_067_200, 0).single().unwrap();
        let project = Project {
            id: pid(1_200),
            name: "M7 Moment card search".into(),
            created_at: captured_at,
        };
        let storage = volume(
            1_201,
            "M7 local-only search fixture",
            MountState::Offline,
            None,
            Some("m7-moment-card-search"),
        );
        let asset = asset(
            1_202,
            &project,
            MediaType::Jpeg,
            "M7_WATER.JPG",
            captured_at,
            "m7-moment-card-water",
        );
        repository.insert_project(&project).unwrap();
        repository.insert_storage_volume(&storage).unwrap();
        repository.insert_media_asset(&asset).unwrap();
        repository
            .insert_file_instance(&file(
                1_203,
                &asset,
                &storage,
                "offline/M7_WATER.JPG",
                captured_at,
                false,
            ))
            .unwrap();

        let provider = FixtureSemanticProvider::new(
            3,
            vec![1.0, 0.0, 0.0],
            HashMap::from([("water".into(), vec![1.0, 0.0, 0.0])]),
        );
        let model_key = provider.identity().cache_key();
        let created_at = captured_at.to_rfc3339();
        let timeline_id = "m7-search-timeline";
        let run_id = "m7-search-run";
        let segment_id = "m7-search-segment";
        let moment_id = "m7-search-moment";
        let timeline = MomentTimelineStatusRecord {
            timeline_id: timeline_id.into(),
            project_id: project.id.to_string(),
            state: "ready".into(),
            analyzer_id: MOMENT_ANALYZER_ID.into(),
            analyzer_version: "test".into(),
            boundary_algorithm_version: MOMENT_BOUNDARY_ALGORITHM_VERSION.into(),
            semantic_model_key: Some(model_key.clone()),
            input_catalog_version: "m7-search-test-inputs".into(),
            active_run_id: Some(run_id.into()),
            moment_count: 1,
            eligible_count: 1,
            ungrouped_count: 0,
            updated_at: created_at.clone(),
        };
        let run = MomentAnalysisRunRecord {
            id: run_id.into(),
            timeline_id: timeline_id.into(),
            project_id: project.id.to_string(),
            state: "completed".into(),
            analyzer_id: MOMENT_ANALYZER_ID.into(),
            analyzer_version: "test".into(),
            boundary_algorithm_version: MOMENT_BOUNDARY_ALGORITHM_VERSION.into(),
            semantic_model_key: Some(model_key),
            input_catalog_version: "m7-search-test-inputs".into(),
            items_total: 1,
            items_completed: 1,
            error_count: 0,
            started_at: created_at.clone(),
            finished_at: Some(created_at.clone()),
        };
        repository
            .replace_active_moment_analysis(
                &timeline,
                &run,
                &[TimelineSegmentRecord {
                    id: segment_id.into(),
                    project_id: project.id.to_string(),
                    run_id: run_id.into(),
                    ordinal: 0,
                    started_at: Some(created_at.clone()),
                    ended_at: Some(created_at.clone()),
                    asset_count: 1,
                    boundary_category: None,
                    boundary_evidence: serde_json::json!({"test": true}),
                    created_at: created_at.clone(),
                    stale: false,
                }],
                &[MomentRecord {
                    id: moment_id.into(),
                    project_id: project.id.to_string(),
                    timeline_id: timeline_id.into(),
                    run_id: run_id.into(),
                    segment_id: segment_id.into(),
                    anchor_asset_id: asset.id.to_string(),
                    ordinal: 0,
                    started_at: Some(created_at.clone()),
                    ended_at: Some(created_at.clone()),
                    asset_count: 1,
                    ai_representative_asset_id: Some(asset.id.to_string()),
                    centroid: Some(vec![1.0, 0.0, 0.0]),
                    centroid_dimensions: Some(3),
                    suggested_label: None,
                    label_confidence: None,
                    label_evidence: serde_json::json!({"state": "abstained"}),
                    label_state: "abstained".into(),
                    created_at: created_at.clone(),
                    stale: false,
                }],
                &[MomentMembershipRecord {
                    id: "m7-search-membership".into(),
                    project_id: project.id.to_string(),
                    run_id: run_id.into(),
                    moment_id: Some(moment_id.into()),
                    media_asset_id: asset.id.to_string(),
                    ordinal: 0,
                    membership_state: "member".into(),
                    created_at: created_at.clone(),
                    active: true,
                }],
                &[],
            )
            .unwrap();

        let response = search_moments_with_provider(
            &repository,
            &project.id,
            &MomentSearchRequest {
                query: "water".into(),
                limit: 24,
            },
            "water",
            "water",
            &provider,
            directory.path(),
        )
        .unwrap();
        assert!(response.semantic_available && response.semantic_applied);
        assert_eq!(response.total_results, 1);
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].id, moment_id);
        assert_eq!(response.results[0].label.display_label, "Untitled Moment");
        assert_eq!(response.results[0].asset_count, 1);

        // A same-dimension fixture with a different embedding-space identity must not compare
        // against the stored centroid. This also guards a future provider/model revision.
        let incompatible_provider = FixtureSemanticProvider::new(
            2,
            vec![1.0, 0.0],
            HashMap::from([("water".into(), vec![1.0, 0.0])]),
        );
        let incompatible = search_moments_with_provider(
            &repository,
            &project.id,
            &MomentSearchRequest {
                query: "water".into(),
                limit: 24,
            },
            "water",
            "water",
            &incompatible_provider,
            directory.path(),
        )
        .unwrap();
        assert!(incompatible.semantic_available && !incompatible.semantic_applied);
        assert!(incompatible.results.is_empty());
        assert!(incompatible
            .semantic_unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("compatible")));

        // No approved local provider yields no broad catalog fallback or fabricated card.
        let unavailable_cache = SiglipProviderCache::new(directory.path().join("no-model-pack"));
        let unavailable = search_moments(
            &repository,
            &project.id,
            &MomentSearchRequest {
                query: "water".into(),
                limit: 24,
            },
            &unavailable_cache,
            directory.path(),
        )
        .unwrap();
        assert!(!unavailable.semantic_available && !unavailable.semantic_applied);
        assert!(unavailable.results.is_empty());
    }

    #[test]
    fn m7_clock_advisory_is_local_direction_free_and_never_mutates_timeline_inputs() {
        let project_id = pid(1_300);
        let mut inputs = Vec::new();
        for index in 0..3_i64 {
            let mut camera_a = TimelineAssetInput::minimal(
                format!("clock-a-{index}"),
                Some(1_000_000 + index * 120_000),
            );
            camera_a.camera_model = Some("Camera A".into());
            camera_a.similar_set_ids = Some(vec![format!("related-{index}")]);
            let mut camera_b = TimelineAssetInput::minimal(
                format!("clock-b-{index}"),
                Some(1_060_000 + index * 120_000),
            );
            camera_b.camera_model = Some("Camera B".into());
            camera_b.similar_set_ids = Some(vec![format!("related-{index}")]);
            inputs.extend([camera_a, camera_b]);
        }
        let before = inputs.clone();
        let records = moment_clock_diagnostic_records(
            &project_id,
            "m7-clock-run",
            &inputs,
            Utc.timestamp_opt(1_704_067_200, 0).single().unwrap(),
        );
        assert_eq!(inputs, before, "clock observation must be read-only");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].possible_offset_seconds, Some(60));
        assert_eq!(
            records[0]
                .evidence_json
                .get("independentSimilarSetComparisons")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
        let view = moment_clock_diagnostic_view(records.into_iter().next().unwrap()).unwrap();
        assert_eq!(view.camera_label, "Camera A ↔ Camera B");
        assert_eq!(view.state, "possible_offset");
        assert!(view.summary.contains("Possible camera time offset"));
        assert!(view.summary.contains("about 1 minute"));
        assert!(view.summary.contains("did not change any timestamps"));
        assert!(!view.summary.contains("ahead"));
        assert!(!view.summary.contains("behind"));
    }

    fn m8_evaluation_report(
        macro_f1: f32,
        brier_score: f32,
        sample_count: usize,
    ) -> studio_brain::EvaluationReport {
        studio_brain::EvaluationReport {
            split: studio_brain::HoldoutSplitSummary {
                strategy: studio_brain::HoldoutStrategy::ProjectHoldout,
                training_count: 24,
                validation_count: sample_count,
                held_out_group_count: 1,
                notes: vec!["test-only grouped holdout".into()],
            },
            personal_model: Some(studio_brain::ClassificationMetrics {
                sample_count,
                accuracy: macro_f1,
                macro_f1,
                log_loss: 0.4,
                brier_score,
                expected_calibration_error: 0.1,
                confusion: Vec::new(),
            }),
            generic_baseline: None,
            majority_baseline: None,
            calibration: None,
            caveats: Vec::new(),
        }
    }

    #[test]
    fn m8_candidate_noninferiority_rejects_regression_and_incomparable_holdouts() {
        let previous = m8_evaluation_report(0.71, 0.30, 12);
        let small_non_regression = m8_evaluation_report(0.69, 0.32, 12);
        assert!(studio_candidate_rejection_reason(&small_non_regression, &previous, 8).is_none());

        let regressed = m8_evaluation_report(0.65, 0.30, 12);
        assert_eq!(
            studio_candidate_rejection_reason(&regressed, &previous, 8).as_deref(),
            Some("New Studio Brain did not improve validation performance. Previous model remains active.")
        );

        let incomparable = m8_evaluation_report(0.73, 0.28, 7);
        assert_eq!(
            studio_candidate_rejection_reason(&incomparable, &previous, 8).as_deref(),
            Some("New Studio Brain could not be compared safely with the active model. Previous model remains active.")
        );
    }

    #[test]
    fn m10_only_exact_provenance_is_persisted_as_a_matched_output_before_human_review() {
        assert_eq!(
            output_match_state_from_resolution(MatchConfidence::Exact),
            OutputMatchState::Matched
        );
        // A unique filename-derived candidate is useful evidence, but AGENTS' M10 contract
        // forbids it from becoming a final source association until a human confirms it.
        assert_eq!(
            output_match_state_from_resolution(MatchConfidence::Strong),
            OutputMatchState::Unmatched
        );
        assert_eq!(
            output_match_state_from_resolution(MatchConfidence::Possible),
            OutputMatchState::Unmatched
        );
        assert_eq!(
            output_match_state_from_resolution(MatchConfidence::Ambiguous),
            OutputMatchState::Ambiguous
        );
    }

    #[test]
    fn m10_register_edit_outputs_counts_only_automatic_matches_as_matched() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("KEEP.JPG"),
            b"first photographer-owned source bytes",
        )
        .unwrap();
        fs::write(
            source.join("REVIEW.JPG"),
            b"second photographer-owned source bytes",
        )
        .unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project =
            create_local_project(&repository, "Edit output registration fixture").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let indexed =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 20).unwrap();
        let keep_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "KEEP.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        let review_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "REVIEW.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        repository
            .update_culling_decision(
                &project_id,
                &keep_id,
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
        repository
            .update_culling_decision(
                &project_id,
                &review_id,
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

        let plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Editor workset".into(),
                plan_type: delivery_brain::ProductionPlanType::EditorWorkset,
                selection_rules: delivery_brain::SelectionRules::editor_workset(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        let workset = directory.path().join("editor-workset");
        fs::create_dir_all(&workset).unwrap();
        update_production_plan_destination(
            &repository,
            &project_id,
            &plan.id,
            Some(workset.to_str().unwrap()),
        )
        .unwrap();
        let manifest =
            create_production_export_manifest(&repository, &project_id, &plan.id).unwrap();
        let export =
            export_production_manifest(&repository, &project_id, &manifest.id, || false, |_| {})
                .unwrap();
        assert_eq!(export.state, "completed");

        let session = create_edit_session(
            &repository,
            &project_id,
            &CreateEditSessionInput {
                name: "M10 output registration".into(),
                template: edit_bridge::EditSessionTemplate::Custom,
                export_manifest_id: manifest.id.clone(),
                expected_output_policy: edit_bridge::ExpectedOutputPolicy::Required,
            },
        )
        .unwrap();

        // Two returned outputs: one at the frozen handoff path (exact auto-match) and one whose
        // filename uniquely matches a work item but at a different path/checksum (strong candidate).
        let page = edit_session_page(&repository, &project_id, &session.id, 0, 50).unwrap();
        let keep_handoff = page
            .work_items
            .iter()
            .find(|item| item.source_filename == "KEEP.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let review_handoff = page
            .work_items
            .iter()
            .find(|item| item.source_filename == "REVIEW.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let review_filename = std::path::Path::new(&review_handoff)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let returned = directory.path().join("returned");
        fs::create_dir_all(returned.join("sub")).unwrap();
        fs::write(
            returned.join(&keep_handoff),
            b"first photographer-owned source bytes",
        )
        .unwrap();
        fs::write(
            returned.join("sub").join(&review_filename),
            b"edited reviewer-owned output bytes differ from source",
        )
        .unwrap();

        let summary = register_edit_outputs(
            &repository,
            &project_id,
            &session.id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            summary.discovered_count, 2,
            "both returned files are discovered"
        );
        assert_eq!(
            summary.created_count, 2,
            "both returned files are newly registered"
        );
        assert_eq!(
            summary.matched_count, 1,
            "only the exact provenance match is reported as matched"
        );
        assert_eq!(
            summary.unmatched_count, 1,
            "the strong filename candidate is not reported as matched"
        );
        assert_eq!(summary.ambiguous_count, 0);

        let after = edit_session_page(&repository, &project_id, &session.id, 0, 50).unwrap();
        let exact_output = after
            .outputs
            .iter()
            .find(|output| {
                output.filename
                    == std::path::Path::new(&keep_handoff)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .as_ref()
            })
            .unwrap();
        assert_eq!(exact_output.match_state, "exact");
        assert!(
            exact_output.matched_work_item_id.is_some(),
            "exact provenance links to a work item"
        );

        let strong_output = after
            .outputs
            .iter()
            .find(|output| output.filename == review_filename)
            .unwrap();
        assert_eq!(strong_output.match_state, "strong");
        assert!(
            strong_output.matched_work_item_id.is_none(),
            "a strong filename candidate is not silently linked"
        );

        let matched_detail = after
            .outputs
            .iter()
            .filter(|output| output.match_state == "exact")
            .count();
        assert_eq!(
            matched_detail, summary.matched_count as usize,
            "summary and detail agree on the matched count"
        );
    }

    fn m10_edit_session_fixture(
        name: &str,
        policy: edit_bridge::ExpectedOutputPolicy,
    ) -> (
        SqliteRepository,
        ProjectId,
        String,
        String,
        tempfile::TempDir,
    ) {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        write_real_test_jpeg(&source.join("KEEP.JPG"), 4, 4, 1);
        write_real_test_jpeg(&source.join("REVIEW.JPG"), 4, 4, 2);

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Edit session fixture").unwrap();
        let project_id = ProjectId::try_from(project.id.as_str()).unwrap();
        index_local_folder(&repository, &project_id, source.to_str().unwrap(), |_| {}).unwrap();
        let indexed =
            load_project_home(&repository, &project_id, MediaBrowserFilter::All, 0, 20).unwrap();
        let keep_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "KEEP.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        let review_id = MediaAssetId::try_from(
            indexed
                .media
                .iter()
                .find(|item| item.filename == "REVIEW.JPG")
                .unwrap()
                .asset_id
                .as_str(),
        )
        .unwrap();
        repository
            .update_culling_decision(
                &project_id,
                &keep_id,
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
        repository
            .update_culling_decision(
                &project_id,
                &review_id,
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

        let plan = create_production_plan(
            &repository,
            &project_id,
            &ProductionPlanInput {
                name: "Editor workset".into(),
                plan_type: delivery_brain::ProductionPlanType::EditorWorkset,
                selection_rules: delivery_brain::SelectionRules::editor_workset(),
                organization: delivery_brain::OrganizationStrategy::SingleFolder,
                filename_strategy: delivery_brain::FilenameStrategy::PreserveOriginal,
            },
        )
        .unwrap();
        let workset = directory.path().join("editor-workset");
        fs::create_dir_all(&workset).unwrap();
        update_production_plan_destination(
            &repository,
            &project_id,
            &plan.id,
            Some(workset.to_str().unwrap()),
        )
        .unwrap();
        let manifest =
            create_production_export_manifest(&repository, &project_id, &plan.id).unwrap();
        let export =
            export_production_manifest(&repository, &project_id, &manifest.id, || false, |_| {})
                .unwrap();
        assert_eq!(export.state, "completed");

        let session = create_edit_session(
            &repository,
            &project_id,
            &CreateEditSessionInput {
                name: name.to_string(),
                template: edit_bridge::EditSessionTemplate::Custom,
                export_manifest_id: manifest.id.clone(),
                expected_output_policy: policy,
            },
        )
        .unwrap();
        (repository, project_id, session.id, manifest.id, directory)
    }

    fn m10_register_and_approve(
        repository: &SqliteRepository,
        project_id: &ProjectId,
        session_id: &str,
        source_filename: &str,
        directory: &tempfile::TempDir,
    ) {
        let page = edit_session_page(repository, project_id, session_id, 0, 50).unwrap();
        let handoff = page
            .work_items
            .iter()
            .find(|item| item.source_filename == source_filename)
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let returned = directory.path().join("returned");
        fs::create_dir_all(&returned).unwrap();
        let content = fs::read(directory.path().join("source").join(source_filename)).unwrap();
        fs::write(returned.join(&handoff), content).unwrap();
        register_edit_outputs(
            repository,
            project_id,
            session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        let after = edit_session_page(repository, project_id, session_id, 0, 50).unwrap();
        let output = after
            .outputs
            .iter()
            .find(|output| {
                output.filename
                    == std::path::Path::new(&handoff)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .as_ref()
            })
            .unwrap();
        let version_id = output.latest_version_id.clone().unwrap();
        review_edit_version(
            repository,
            project_id,
            &version_id,
            edit_bridge::EditVersionReviewState::Approved,
        )
        .unwrap();
    }

    #[test]
    fn m10_optional_policy_completes_when_one_optional_output_is_approved_and_another_optional_item_has_no_output(
    ) {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "optional completion",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_and_approve(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        assert_eq!(
            page.session.state, "completed",
            "an approved optional output plus an optional item with no output still completes"
        );
    }

    #[test]
    fn m10_optional_policy_does_not_complete_while_an_output_awaits_review() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "optional completion",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let handoff = page
            .work_items
            .iter()
            .find(|item| item.source_filename == "KEEP.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let returned = directory.path().join("returned");
        fs::create_dir_all(&returned).unwrap();
        let content = fs::read(directory.path().join("source").join("KEEP.JPG")).unwrap();
        fs::write(returned.join(&handoff), content).unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        let after = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        assert_ne!(
            after.session.state, "completed",
            "an output awaiting review must not complete the session"
        );
        assert_eq!(after.session.state, "review");
    }

    #[test]
    fn m10_required_policy_does_not_complete_when_a_required_output_is_missing() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "required completion",
            edit_bridge::ExpectedOutputPolicy::Required,
        );
        m10_register_and_approve(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let after = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        assert_ne!(
            after.session.state, "completed",
            "a missing required output must block completion"
        );
        assert_eq!(after.session.state, "partially_completed");
    }

    fn m10_register_output(
        repository: &SqliteRepository,
        project_id: &ProjectId,
        session_id: &str,
        source_filename: &str,
        directory: &tempfile::TempDir,
    ) {
        let page = edit_session_page(repository, project_id, session_id, 0, 50).unwrap();
        let handoff = page
            .work_items
            .iter()
            .find(|item| item.source_filename == source_filename)
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let returned = directory.path().join("returned");
        fs::create_dir_all(&returned).unwrap();
        let content = fs::read(directory.path().join("source").join(source_filename)).unwrap();
        fs::write(returned.join(&handoff), content).unwrap();
        register_edit_outputs(
            repository,
            project_id,
            session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn m10_registered_output_is_available_while_present() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "availability present",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let output = page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(output.availability, "available");
    }

    #[test]
    fn m10_output_becomes_offline_after_file_removed_and_rescanned() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "availability removed",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let handoff = edit_session_page(&repository, &project_id, &session_id, 0, 50)
            .unwrap()
            .work_items
            .iter()
            .find(|i| i.source_filename == "KEEP.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        fs::remove_file(directory.path().join("returned").join(&handoff)).unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            directory.path().join("returned").to_str().unwrap(),
        )
        .unwrap();
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let output = page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(output.availability, "offline");
    }

    #[test]
    fn m10_output_returns_available_after_file_reappears_and_rescanned() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "availability reappear",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let handoff = edit_session_page(&repository, &project_id, &session_id, 0, 50)
            .unwrap()
            .work_items
            .iter()
            .find(|i| i.source_filename == "KEEP.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        let returned = directory.path().join("returned");
        fs::remove_file(returned.join(&handoff)).unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            edit_session_page(&repository, &project_id, &session_id, 0, 50)
                .unwrap()
                .outputs
                .iter()
                .find(|o| o.filename == "KEEP.JPG")
                .unwrap()
                .availability,
            "offline"
        );
        let content = fs::read(directory.path().join("source").join("KEEP.JPG")).unwrap();
        fs::write(returned.join(&handoff), content).unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let output = page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(output.availability, "available");
    }

    #[test]
    fn m10_inaccessible_scan_does_not_mark_existing_outputs_offline() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "availability inaccessible",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let bad = directory.path().join("does-not-exist-root");
        let result =
            register_edit_outputs(&repository, &project_id, &session_id, bad.to_str().unwrap());
        assert!(result.is_err(), "an inaccessible root must fail the scan");
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let output = page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(
            output.availability, "available",
            "a failed scan must not transition outputs offline"
        );
    }

    #[test]
    fn m10_rescanning_one_session_does_not_affect_another() {
        let (repository, project_id, session_a, manifest_id, directory) = m10_edit_session_fixture(
            "availability session A",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(&repository, &project_id, &session_a, "KEEP.JPG", &directory);
        let session_b = create_edit_session(
            &repository,
            &project_id,
            &CreateEditSessionInput {
                name: "availability session B".into(),
                template: edit_bridge::EditSessionTemplate::Custom,
                export_manifest_id: manifest_id,
                expected_output_policy: edit_bridge::ExpectedOutputPolicy::Optional,
            },
        )
        .unwrap()
        .id;
        m10_register_output(&repository, &project_id, &session_b, "KEEP.JPG", &directory);

        let handoff = edit_session_page(&repository, &project_id, &session_a, 0, 50)
            .unwrap()
            .work_items
            .iter()
            .find(|i| i.source_filename == "KEEP.JPG")
            .unwrap()
            .handoff_relative_path
            .clone()
            .unwrap();
        fs::remove_file(directory.path().join("returned").join(&handoff)).unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_a,
            directory.path().join("returned").to_str().unwrap(),
        )
        .unwrap();

        let a_page = edit_session_page(&repository, &project_id, &session_a, 0, 50).unwrap();
        let a_output = a_page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(a_output.availability, "offline");
        let b_page = edit_session_page(&repository, &project_id, &session_b, 0, 50).unwrap();
        let b_output = b_page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(
            b_output.availability, "available",
            "session B outputs must be unaffected by session A's rescan"
        );
    }

    #[test]
    fn m10_repeated_unchanged_scan_is_idempotent() {
        let (repository, project_id, session_id, _, directory) = m10_edit_session_fixture(
            "availability idempotent",
            edit_bridge::ExpectedOutputPolicy::Optional,
        );
        m10_register_output(
            &repository,
            &project_id,
            &session_id,
            "KEEP.JPG",
            &directory,
        );
        let returned = directory.path().join("returned");
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        register_edit_outputs(
            &repository,
            &project_id,
            &session_id,
            returned.to_str().unwrap(),
        )
        .unwrap();
        let page = edit_session_page(&repository, &project_id, &session_id, 0, 50).unwrap();
        let output = page
            .outputs
            .iter()
            .find(|o| o.filename == "KEEP.JPG")
            .unwrap();
        assert_eq!(output.availability, "available");
        assert_eq!(
            page.outputs.len(),
            1,
            "repeated scans must not duplicate outputs"
        );
    }
}
