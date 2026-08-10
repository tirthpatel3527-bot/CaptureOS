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
use chrono::{DateTime, TimeZone, Utc};
use ingest::{
    copy_and_verify, preflight, CopyVerificationOutcome, DefaultDestinationLayout,
    DestinationLayout, IngestRequest, LocalAvailableSpace, PreflightIssue, PreflightReport,
    PreflightSeverity,
};
use magic_search::{
    discover_siglip_provider, local_siglip_capability, plan_query, rank_normalized_vectors,
    IndexCandidates, LocalModelCapability, PersistentVectorIndex, SemanticEmbeddingProvider,
    SemanticProviderIdentity, ValidatedModelPackMetadata, SUPPORTED_SIGLIP_PACK_DIRECTORY,
};
use media_index::{scan_read_only, IndexCandidate, IndexEvent};
use media_model::*;
use media_visual::{
    clear_cache, extract_metadata, prepare_analysis_preview, prepare_previews, ArtifactStatus,
    LocalVisualAdapters, ThumbnailProvider, ANALYSIS_PREVIEW_GENERATOR_VERSION,
    ANALYSIS_PREVIEW_LONG_EDGE, GENERATOR_VERSION,
};
use persistence::{
    AnalysisInputCandidate, CaptureIntelligenceTerminalCounts, CatalogCounts, CatalogRepository,
    CullingDecisionUpdate, CullingDecisionView, CullingProgress, CullingQuery, CullingReportRow,
    CullingWorkspaceView, FaceAnalysisProviderConfig, IndexedMediaRow, IngestAuditEvent,
    IngestItemRecord, IngestReport, MediaAssetDetail, MediaBrowserFilter, MediaMetadataRecord,
    MagicSearchHistoryEntry, PersistenceError, PreviewArtifactRecord, ProjectIndexSummary,
    ProjectLibraryItem as PersistedProjectLibraryItem, Result as PersistenceResult,
    ReviewSessionView, SemanticEmbeddingRecord, SemanticIndexTerminalCounts,
    SemanticIndexVersion, SemanticInputCandidate, SemanticModelConfig, SemanticSearchCandidate,
    SimilarityGroupView, StoredSemanticVector, VisualMediaPage, VisualMediaQuery,
    VisualMediaRow, VisualPreparationTerminalCounts,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    panic::AssertUnwindSafe,
    path::{Component, Path, PathBuf},
    thread,
    time::Instant,
};
use storage::{LocalVolumeInspector, VolumeInspector, VolumeObservation};
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

/// Project-scoped, local Magic Search indexing state. The model capability is explicit so the
/// UI can offer honest structured-filter fallback rather than fabricated semantic matches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticIndexProgress {
    pub state: String,
    pub stage: String,
    pub resource_mode: String,
    pub items_completed: u64,
    pub items_total: u64,
    pub error_count: u64,
    pub counts: SemanticIndexCounts,
    pub model: LocalModelCapability,
    pub active: bool,
    pub paused: bool,
    pub completed: bool,
    pub index_ready: bool,
    pub index_embedding_count: u64,
    pub last_error: Option<String>,
    pub current_asset_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
}

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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchFilterView {
    pub chips: Vec<String>,
}

/// A result card stays a logical MediaAsset card. The score and explanation are M6 search
/// projections only; no raw embedding is sent to the frontend or written into a human decision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagicSearchResult {
    pub item: VisualMediaRow,
    pub semantic_score: Option<f64>,
    pub score_label: Option<String>,
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
            let (candidate, source, metadata) =
                select_preparation_source(repository, initial_candidate, &media_type)?;
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

fn select_preparation_source(
    repository: &impl CatalogRepository,
    initial: persistence::MediaPreparationCandidate,
    media_type: &MediaType,
) -> PersistenceResult<(
    persistence::MediaPreparationCandidate,
    Option<PathBuf>,
    media_visual::ExtractedMetadata,
)> {
    let copies = repository.preparation_file_instance_candidates(&initial.asset_id)?;
    let candidates = if copies.is_empty() {
        vec![initial]
    } else {
        copies
    };
    let mut last = None;
    for candidate in candidates {
        let source = source_path(candidate.selected_root.as_deref(), &candidate.relative_path);
        let metadata = source
            .as_deref()
            .map(|path| extract_metadata(path, media_type))
            .unwrap_or_else(|| media_visual::ExtractedMetadata {
                status: ArtifactStatus::Offline,
                failure_reason: Some(
                    "The file instance has no safely resolvable index root or is offline".into(),
                ),
                ..Default::default()
            });
        if matches!(
            metadata.status,
            ArtifactStatus::Ready | ArtifactStatus::Unsupported
        ) {
            return Ok((candidate, source, metadata));
        }
        last = Some((candidate, source, metadata));
    }
    last.ok_or_else(|| PersistenceError::InvalidData("media asset has no file instances".into()))
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
        extractor_version: GENERATOR_VERSION.into(),
        status: extracted.status.as_str().into(),
        failure_reason: extracted.failure_reason,
        extracted_at: Utc::now().to_rfc3339(),
        mime_type: extracted.mime_type,
        byte_size: extracted.byte_size,
        captured_at_raw: extracted.captured_at_raw,
        captured_at_local: extracted.captured_at_local,
        capture_timezone: extracted.capture_timezone,
        capture_time_source: extracted.capture_time_source,
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
    use persistence::SqliteRepository;
    use std::{fs, io, io::Write, process::Command};
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
}
