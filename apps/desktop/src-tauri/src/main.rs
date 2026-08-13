#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use capture_core::{
    analyze_capture_intelligence, apply_culling_decision, clear_magic_search_history,
    clear_visual_cache, complete_culling_group,
    create_coverage_checklist_item as create_coverage_checklist_item_core, create_edit_session,
    create_local_project, create_production_export_manifest, create_production_plan,
    create_virtual_collection, edit_session_page, edit_workspace, export_culling_report,
    export_studio_brain_preference_examples, find_similar, finish_culling_review,
    generate_edit_handoff, index_local_folder, index_semantic_embeddings, ingest_history,
    ingest_report, list_local_projects, load_capture_intelligence_summary, load_culling_progress,
    load_culling_workspace, load_magic_search_history, load_media_asset_detail,
    load_moment_checklists, load_moment_detail, load_moment_timeline, load_moment_timeline_status,
    load_production_workspace, load_project_home, load_project_library, load_semantic_index_status,
    load_similarity_group, load_studio_brain_status, load_visual_media_page,
    load_visual_preparation_summary, manually_match_edit_output,
    merge_adjacent_moments as merge_adjacent_moments_core, preflight_ingest,
    preflight_production_export, prepare_visual_media, recover_interrupted_capture_intelligence,
    recover_interrupted_ingests, recover_interrupted_moment_analysis,
    recover_interrupted_production_exports, recover_interrupted_semantic_indexing,
    recover_interrupted_studio_training, recover_interrupted_visual_preparations,
    refresh_capture_metadata, register_edit_outputs, rename_moment as rename_moment_core,
    reset_studio_brain_personalization, restart_ingest, retry_failed_visual_media,
    review_edit_version, save_human_intelligence_decision, search_magic, search_moments,
    set_culling_group_representative,
    set_moment_human_representative as set_moment_human_representative_core,
    set_production_plan_override, set_static_virtual_collection_member,
    set_static_virtual_collection_members, set_studio_brain_enabled,
    set_studio_brain_project_included, split_moment as split_moment_core, start_ingest,
    start_moment_analysis as run_moment_analysis, train_studio_brain,
    update_coverage_confirmation as update_coverage_confirmation_core, update_culling_position,
    update_production_plan_configuration, update_production_plan_destination,
    update_production_plan_destination_reserve, CaptureIntelligenceProgress,
    CreateCoverageChecklistItemInput, EditHandoffView, EditOutputRegistrationSummary,
    EditOutputView, EditSessionPageView, EditSessionView, EditWorkspace, FindSimilarRequest,
    IngestPreflightView, JobView, MagicSearchHistoryEntry, MagicSearchRequest, MagicSearchResponse,
    MediaPreparationProgress, MetadataRefreshProgress, MomentAnalysisProgress, MomentChecklistView,
    MomentDetailView, MomentSearchRequest, MomentSearchResponse, MomentTimelineView,
    ProductionExportProgress, ProductionPlanPreview, ProjectHome, ProjectLibraryItem, ProjectView,
    SemanticIndexProgress, SemanticStorageRoots, SiglipProviderCache, StudioBrainProgress,
    UpdateCoverageConfirmationInput,
};
use delivery_brain::PlanOverrideKind;
use edit_bridge::{EditSessionTemplate, EditVersionReviewState, ExpectedOutputPolicy, HandoffMode};
use ingest::IngestRequest;
use media_model::{
    AnalysisResourceMode, CullingDecisionValue, HumanDecisionValue, IngestJobId,
    IngestProtectionPolicy, MediaAssetId, ProjectId,
};
use persistence::{
    CatalogRepository, CreateEditSessionInput, CullingDecisionUpdate, CullingDecisionView,
    CullingProgress, CullingQuery, CullingWorkspaceView, EditHandoffInput, ExportManifestRecord,
    IngestJobSummary, IngestReport, MediaAssetDetail, MediaBrowserFilter, PersistenceError,
    ProductionPlanInput, ProductionPlanRecord, ProductionPreflight, ProductionWorkspaceView,
    ReviewSessionView, SimilarityGroupView, SqliteRepository, VirtualCollectionInput,
    VirtualCollectionRecord, VisualMediaFilter, VisualMediaPage, VisualMediaQuery, VisualMediaSort,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{http, AppHandle, Emitter, Manager, State};
use uuid::Uuid;

const PREVIEW_PROTOCOL: &str = "captureos-preview";

struct AppState {
    repository: Arc<Mutex<SqliteRepository>>,
    catalog_path: PathBuf,
    preview_cache_root: PathBuf,
    semantic_provider_cache: Arc<SiglipProviderCache>,
    semantic_index_root: PathBuf,
    active_visual_projects: Arc<Mutex<HashSet<String>>>,
    active_metadata_refresh_projects: Arc<Mutex<HashSet<String>>>,
    active_intelligence_projects: Arc<Mutex<HashSet<String>>>,
    intelligence_pause_controls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    active_semantic_projects: Arc<Mutex<HashSet<String>>>,
    semantic_pause_controls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    active_moment_projects: Arc<Mutex<HashSet<String>>>,
    moment_pause_controls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    active_studio_profiles: Arc<Mutex<HashSet<String>>>,
    active_production_manifests: Arc<Mutex<HashSet<String>>>,
    production_cancel_controls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    active_edit_sessions: Arc<Mutex<HashSet<String>>>,
}

/// Studio Brain training and its settings are profile-scoped, rather than project-scoped. This
/// prevents two project pages from racing the one local default profile into competing models.
struct StudioProfileMutationGuard {
    active_profiles: Arc<Mutex<HashSet<String>>>,
    profile_key: String,
}

impl Drop for StudioProfileMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_profiles.lock() {
            active.remove(&self.profile_key);
        }
    }
}

fn begin_studio_profile_mutation(
    active_profiles: &Arc<Mutex<HashSet<String>>>,
    profile_key: String,
) -> Result<StudioProfileMutationGuard, String> {
    let mut active = active_profiles
        .lock()
        .map_err(|_| "Studio Brain profile lock was poisoned".to_owned())?;
    if !active.insert(profile_key.clone()) {
        return Err(
            "Studio Brain is already updating this local profile. Wait for it to finish.".into(),
        );
    }
    Ok(StudioProfileMutationGuard {
        active_profiles: Arc::clone(active_profiles),
        profile_key,
    })
}

/// One project can have either a Moment analysis or a human structural mutation in flight. The
/// frontend provides immediate feedback, but this backend guard is authoritative when duplicate
/// clicks, delayed webview messages, or separate Moment controls race each other.
struct MomentProjectMutationGuard {
    active_projects: Arc<Mutex<HashSet<String>>>,
    project_key: String,
}

impl Drop for MomentProjectMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_projects.lock() {
            active.remove(&self.project_key);
        }
    }
}

fn begin_moment_project_mutation(
    active_projects: &Arc<Mutex<HashSet<String>>>,
    project_key: String,
) -> Result<MomentProjectMutationGuard, String> {
    let mut active = active_projects
        .lock()
        .map_err(|_| "Moment analysis lock was poisoned".to_owned())?;
    if !active.insert(project_key.clone()) {
        return Err(
            "A Moment timeline change is already running for this project. Wait for it to finish."
                .into(),
        );
    }
    Ok(MomentProjectMutationGuard {
        active_projects: Arc::clone(active_projects),
        project_key,
    })
}

/// A frozen manifest represents one exact local copy plan. This process-level guard closes the
/// short gap before the durable active-job constraint is created, while SQLite remains the
/// authoritative guard across windows or processes.
struct ProductionManifestMutationGuard {
    active_manifests: Arc<Mutex<HashSet<String>>>,
    manifest_key: String,
}

impl Drop for ProductionManifestMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_manifests.lock() {
            active.remove(&self.manifest_key);
        }
    }
}

fn begin_production_manifest_mutation(
    active_manifests: &Arc<Mutex<HashSet<String>>>,
    manifest_key: String,
) -> Result<ProductionManifestMutationGuard, String> {
    let mut active = active_manifests
        .lock()
        .map_err(|_| "Production export lock was poisoned".to_owned())?;
    if !active.insert(manifest_key.clone()) {
        return Err(
            "This frozen Delivery Manifest is already exporting locally. Wait for it to finish or cancel it."
                .into(),
        );
    }
    Ok(ProductionManifestMutationGuard {
        active_manifests: Arc::clone(active_manifests),
        manifest_key,
    })
}

/// Handoff generation and explicit returned-output discovery both perform local filesystem I/O.
/// This guard provides immediate same-process duplicate-click feedback while the durable M10
/// transaction remains authoritative across restarts and separate app processes.
struct EditSessionMutationGuard {
    active_sessions: Arc<Mutex<HashSet<String>>>,
    session_key: String,
}

impl Drop for EditSessionMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_sessions.lock() {
            active.remove(&self.session_key);
        }
    }
}

fn begin_edit_session_mutation(
    active_sessions: &Arc<Mutex<HashSet<String>>>,
    project_id: &ProjectId,
    session_id: &str,
) -> Result<EditSessionMutationGuard, String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err("Choose an Edit Session before changing its local handoff or outputs".into());
    }
    let session_key = format!("{}:{session_id}", project_id);
    let mut active = active_sessions
        .lock()
        .map_err(|_| "Edit Session lock was poisoned".to_owned())?;
    if !active.insert(session_key.clone()) {
        return Err(
            "This Edit Session is already preparing a local handoff or scanning returned outputs. Wait for it to finish."
                .into(),
        );
    }
    Ok(EditSessionMutationGuard {
        active_sessions: Arc::clone(active_sessions),
        session_key,
    })
}

fn moment_persistence_error(error: PersistenceError) -> String {
    if matches!(error, PersistenceError::Database(_)) {
        eprintln!("Moment persistence transaction failed: {error}");
        "Timeline update could not be saved. Your previous timeline is still available.".into()
    } else {
        error.to_string()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisualQueryInput {
    filter: Option<String>,
    sort: Option<String>,
    descending: Option<bool>,
    search: Option<String>,
    camera_model: Option<String>,
    lens_model: Option<String>,
    captured_from: Option<String>,
    captured_to: Option<String>,
    moment_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntelligenceDecisionInput {
    asset_id: String,
    decision: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CullingQueryInput {
    mode: Option<String>,
    filter: Option<String>,
    group_id: Option<String>,
    moment_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CullingDecisionInput {
    asset_id: String,
    decision: Option<String>,
    clear_decision: Option<bool>,
    rating: Option<u8>,
    starred: Option<bool>,
    note: Option<String>,
    flags: Option<Vec<String>>,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CullingPositionInput {
    session_id: String,
    asset_id: Option<String>,
    group_id: Option<String>,
    mode: String,
    filter_context: Option<String>,
}

/// The desktop contract intentionally uses product-level template labels rather than exposing
/// persistence enum spellings. Map them explicitly at this untrusted command boundary.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEditSessionCommandInput {
    name: String,
    template: String,
    export_manifest_id: String,
    expected_output_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateEditHandoffCommandInput {
    mode: String,
    destination_path: String,
}

fn normalized_edit_option(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn edit_session_template(value: &str) -> Result<EditSessionTemplate, String> {
    match normalized_edit_option(value).as_str() {
        "main_edit" | "wedding_main_edit" => Ok(EditSessionTemplate::WeddingMainEdit),
        "album_retouch" | "album_select_retouch" => Ok(EditSessionTemplate::AlbumSelectRetouch),
        "portfolio_retouch" => Ok(EditSessionTemplate::PortfolioRetouch),
        "client_revision" | "client_revision_round" => Ok(EditSessionTemplate::ClientRevisionRound),
        "video_rough_cut" => Ok(EditSessionTemplate::VideoRoughCut),
        "custom" => Ok(EditSessionTemplate::Custom),
        _ => Err("Choose a supported Edit Session template".into()),
    }
}

fn expected_output_policy(value: &str) -> Result<ExpectedOutputPolicy, String> {
    match normalized_edit_option(value).as_str() {
        "one_per_work_item" | "required" => Ok(ExpectedOutputPolicy::Required),
        "optional" => Ok(ExpectedOutputPolicy::Optional),
        _ => Err("Choose a supported expected-output policy".into()),
    }
}

fn edit_handoff_mode(value: &str) -> Result<HandoffMode, String> {
    match normalized_edit_option(value).as_str() {
        "reference" => Ok(HandoffMode::Reference),
        "package" => Ok(HandoffMode::Package),
        _ => Err("Choose a supported local handoff mode".into()),
    }
}

fn edit_version_review_state(value: &str) -> Result<EditVersionReviewState, String> {
    match normalized_edit_option(value).as_str() {
        "approved" => Ok(EditVersionReviewState::Approved),
        "needs_revision" => Ok(EditVersionReviewState::NeedsRevision),
        _ => Err("Choose Approved or Needs Revision for this edit version".into()),
    }
}

/// Tauri broadcasts are global to the webview. Keep the project identity beside each job update
/// so a background task from another shoot can never paint the current project workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectScopedEvent<T> {
    project_id: String,
    progress: T,
}

/// A Studio Profile can span multiple projects, so both identities travel with a local training
/// update. The frontend ignores an event unless both match its current Studio workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StudioProfileScopedEvent<T> {
    project_id: String,
    profile_id: String,
    progress: T,
}

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectView>, String> {
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    list_local_projects(&*repository).map_err(|error| error.to_string())
}

#[tauri::command]
fn project_library(state: State<'_, AppState>) -> Result<Vec<ProjectLibraryItem>, String> {
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_project_library(&*repository).map_err(|error| error.to_string())
}

#[tauri::command]
fn create_project(state: State<'_, AppState>, name: String) -> Result<ProjectView, String> {
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_local_project(&*repository, &name).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn project_home(
    state: State<'_, AppState>,
    project_id: String,
    filter: String,
    offset: u32,
    limit: u32,
) -> Result<ProjectHome, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_project_home(
        &*repository,
        &project_id,
        browser_filter(&filter),
        offset,
        limit,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn visual_media_page(
    state: State<'_, AppState>,
    project_id: String,
    query: VisualQueryInput,
) -> Result<VisualMediaPage, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_visual_media_page(
        &*repository,
        &project_id,
        &visual_query(query),
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn media_asset_detail_command(
    state: State<'_, AppState>,
    project_id: String,
    asset_id: String,
) -> Result<Option<MediaAssetDetail>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_media_asset_detail(
        &*repository,
        &project_id,
        &asset_id,
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn visual_preparation_summary_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Option<MediaPreparationProgress>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_visual_preparation_summary(&*repository, &project_id).map_err(|error| error.to_string())
}

/// Explicitly refreshes only local source metadata on a worker connection. This is never called
/// while opening a project: previews, semantic embeddings, Similar Sets, Capture Intelligence,
/// and photographer decisions remain outside this operation's scope.
#[tauri::command(rename_all = "camelCase")]
async fn refresh_metadata_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<MetadataRefreshProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let project_key = project_id.to_string();
    let active_projects = Arc::clone(&state.active_metadata_refresh_projects);
    {
        let mut active = active_projects
            .lock()
            .map_err(|_| "metadata refresh lock was poisoned".to_owned())?;
        if !active.insert(project_key.clone()) {
            return Err("Metadata refresh is already running for this project".into());
        }
    }
    let catalog_path = state.catalog_path.clone();
    let event_project_key = project_key.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            refresh_capture_metadata(&repository, &project_id, |progress| {
                let _ = app.emit(
                    "metadata-refresh-progress",
                    ProjectScopedEvent {
                        project_id: event_project_key.clone(),
                        progress,
                    },
                );
            })
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut active) = active_projects.lock() {
            active.remove(&project_key);
        }
        result
    })
    .await
    .map_err(|error| format!("metadata refresh task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn capture_intelligence_summary_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Option<CaptureIntelligenceProgress>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_capture_intelligence_summary(&*repository, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
async fn start_capture_intelligence_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    resource_mode: String,
) -> Result<CaptureIntelligenceProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let mode = analysis_resource_mode(&resource_mode)?;
    let project_key = project_id.to_string();
    let active_projects = Arc::clone(&state.active_intelligence_projects);
    {
        let mut active = active_projects
            .lock()
            .map_err(|_| "Capture Intelligence lock was poisoned".to_owned())?;
        if !active.insert(project_key.clone()) {
            return Err("Capture Intelligence is already running for this project".into());
        }
    }
    let pause_control = Arc::new(AtomicBool::new(false));
    state
        .intelligence_pause_controls
        .lock()
        .map_err(|_| "Capture Intelligence controls were unavailable".to_owned())?
        .insert(project_key.clone(), Arc::clone(&pause_control));
    let controls = Arc::clone(&state.intelligence_pause_controls);
    let catalog_path = state.catalog_path.clone();
    let preview_cache_root = state.preview_cache_root.clone();
    let event_project_key = project_key.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            analyze_capture_intelligence(
                &repository,
                &project_id,
                &preview_cache_root,
                mode,
                || pause_control.load(Ordering::SeqCst),
                |progress| {
                    let _ = app.emit(
                        "capture-intelligence-progress",
                        ProjectScopedEvent {
                            project_id: event_project_key.clone(),
                            progress,
                        },
                    );
                },
            )
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut active) = active_projects.lock() {
            active.remove(&project_key);
        }
        if let Ok(mut controls) = controls.lock() {
            controls.remove(&project_key);
        }
        result
    })
    .await
    .map_err(|error| format!("Capture Intelligence task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn pause_capture_intelligence_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let controls = state
        .intelligence_pause_controls
        .lock()
        .map_err(|_| "Capture Intelligence controls were unavailable".to_owned())?;
    let Some(control) = controls.get(&project_id.to_string()) else {
        return Err("Capture Intelligence is not running for this project".into());
    };
    control.store(true, Ordering::SeqCst);
    Ok(())
}

/// Returns the local-only semantic model and index status for one project. This command never
/// downloads a model, scans original media, or exposes a model/embedding filesystem path.
#[tauri::command(rename_all = "camelCase")]
fn semantic_index_status_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<SemanticIndexProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_semantic_index_status(
        &*repository,
        &project_id,
        &state.semantic_provider_cache,
        &state.semantic_index_root,
    )
    .map_err(|error| error.to_string())
}

/// Indexes CaptureOS-managed analysis previews with an explicitly installed local model pack.
/// A separate SQLite connection keeps browsing responsive while the index job is running.
#[tauri::command(rename_all = "camelCase")]
async fn start_semantic_index_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    resource_mode: String,
) -> Result<SemanticIndexProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let mode = analysis_resource_mode(&resource_mode)?;
    let project_key = project_id.to_string();
    let active_projects = Arc::clone(&state.active_semantic_projects);
    {
        let mut active = active_projects
            .lock()
            .map_err(|_| "semantic index lock was poisoned".to_owned())?;
        if !active.insert(project_key.clone()) {
            return Err("Magic Search indexing is already running for this project".into());
        }
    }

    let pause_control = Arc::new(AtomicBool::new(false));
    if let Err(error) = state
        .semantic_pause_controls
        .lock()
        .map_err(|_| "semantic index controls were unavailable".to_owned())
        .map(|mut controls| controls.insert(project_key.clone(), Arc::clone(&pause_control)))
    {
        if let Ok(mut active) = active_projects.lock() {
            active.remove(&project_key);
        }
        return Err(error);
    }

    let controls = Arc::clone(&state.semantic_pause_controls);
    let catalog_path = state.catalog_path.clone();
    let preview_cache_root = state.preview_cache_root.clone();
    let semantic_provider_cache = Arc::clone(&state.semantic_provider_cache);
    let semantic_index_root = state.semantic_index_root.clone();
    let event_project_key = project_key.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            index_semantic_embeddings(
                &repository,
                &project_id,
                SemanticStorageRoots {
                    preview_cache_root: &preview_cache_root,
                    index_root: &semantic_index_root,
                },
                &semantic_provider_cache,
                mode,
                || pause_control.load(Ordering::SeqCst),
                |progress| {
                    let _ = app.emit(
                        "semantic-index-progress",
                        ProjectScopedEvent {
                            project_id: event_project_key.clone(),
                            progress,
                        },
                    );
                },
            )
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut active) = active_projects.lock() {
            active.remove(&project_key);
        }
        if let Ok(mut controls) = controls.lock() {
            controls.remove(&project_key);
        }
        result
    })
    .await
    .map_err(|error| format!("Magic Search indexing task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn pause_semantic_index_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let controls = state
        .semantic_pause_controls
        .lock()
        .map_err(|_| "semantic index controls were unavailable".to_owned())?;
    let Some(control) = controls.get(&project_id.to_string()) else {
        return Err("Magic Search indexing is not running for this project".into());
    };
    control.store(true, Ordering::SeqCst);
    Ok(())
}

/// Returns a compact persisted status only. It intentionally never queues, awaits, or loads a
/// local model, so opening a project cannot be blocked by Moment Brain.
#[tauri::command(rename_all = "camelCase")]
fn moment_timeline_status(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Option<MomentAnalysisProgress>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_moment_timeline_status(&*repository, &project_id).map_err(|error| error.to_string())
}

/// Queues a local structural analysis and returns immediately. The worker owns a separate SQLite
/// connection and emits project-scoped progress; it never runs as part of project startup.
#[tauri::command(rename_all = "camelCase")]
fn start_moment_analysis(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    rebuild: bool,
    resource_mode: Option<String>,
) -> Result<MomentAnalysisProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let mode = analysis_resource_mode(resource_mode.as_deref().unwrap_or(""))?;
    let mode_label = mode.as_str().to_owned();
    let project_key = project_id.to_string();
    let mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_key.clone())?;
    let pause_control = Arc::new(AtomicBool::new(false));
    state
        .moment_pause_controls
        .lock()
        .map_err(|_| "Moment analysis controls were unavailable".to_owned())
        .map(|mut controls| controls.insert(project_key.clone(), Arc::clone(&pause_control)))?;
    let controls = Arc::clone(&state.moment_pause_controls);
    let catalog_path = state.catalog_path.clone();
    let semantic_provider_cache = Arc::clone(&state.semantic_provider_cache);
    let event_project_key = project_key.clone();
    let worker_mode_label = mode_label.clone();
    let _worker = tauri::async_runtime::spawn_blocking(move || {
        let _moment_mutation = mutation_guard;
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            run_moment_analysis(
                &repository,
                &project_id,
                &semantic_provider_cache,
                mode,
                rebuild,
                || pause_control.load(Ordering::SeqCst),
                |progress| {
                    let _ = app.emit(
                        "moment-analysis-progress",
                        ProjectScopedEvent {
                            project_id: event_project_key.clone(),
                            progress,
                        },
                    );
                },
            )
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut controls) = controls.lock() {
            controls.remove(&project_key);
        }
        if let Err(error) = &result {
            // A normal analysis failure persists and emits its own detailed progress from core.
            // This is the last-resort path for failures before that durable update can happen;
            // keep the raw diagnostic in developer details only and do not leave the UI queued.
            eprintln!("Moment analysis worker failed before durable recovery: {error}");
            let _ = app.emit(
                "moment-analysis-progress",
                ProjectScopedEvent {
                    project_id: event_project_key,
                    progress: MomentAnalysisProgress {
                        state: "failed".into(),
                        active: false,
                        paused: false,
                        stage: "moment_analysis".into(),
                        resource_mode: worker_mode_label,
                        completed: 0,
                        total: 0,
                        error_count: 1,
                        timeline_ready: false,
                        moment_count: 0,
                        ungrouped_asset_count: 0,
                        last_error: Some(error.clone()),
                        message: Some(
                            "Timeline update could not be saved. Reload the local timeline to check the last stored result.".into(),
                        ),
                    },
                },
            );
        }
        result
    });
    Ok(MomentAnalysisProgress {
        state: "queued".into(),
        active: true,
        paused: false,
        stage: "moment_analysis".into(),
        resource_mode: mode_label,
        completed: 0,
        total: 0,
        error_count: 0,
        timeline_ready: false,
        moment_count: 0,
        ungrouped_asset_count: 0,
        last_error: None,
        message: Some(
            "Moment analysis is queued locally and will not block project browsing.".into(),
        ),
    })
}

#[tauri::command(rename_all = "camelCase")]
fn moment_timeline(
    state: State<'_, AppState>,
    project_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<MomentTimelineView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_moment_timeline(
        &*repository,
        &project_id,
        limit.unwrap_or(60).clamp(1, 120),
        offset.unwrap_or(0),
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn moment_detail(
    state: State<'_, AppState>,
    project_id: String,
    moment_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Option<MomentDetailView>, String> {
    // Member media use the existing separately paginated `visual_media_page` command. Accept
    // the shared detail request shape without loading a member list into this summary call.
    let _ = (limit, offset);
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_moment_detail(
        &*repository,
        &project_id,
        &moment_id,
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn rename_moment(
    state: State<'_, AppState>,
    project_id: String,
    moment_id: String,
    label: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    rename_moment_core(&*repository, &project_id, &moment_id, &label)
        .map_err(moment_persistence_error)
}

#[tauri::command(rename_all = "camelCase")]
fn set_moment_human_representative(
    state: State<'_, AppState>,
    project_id: String,
    moment_id: String,
    asset_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_moment_human_representative_core(&*repository, &project_id, &moment_id, &asset_id)
        .map_err(moment_persistence_error)
}

#[tauri::command(rename_all = "camelCase")]
fn merge_adjacent_moments(
    state: State<'_, AppState>,
    project_id: String,
    left_moment_id: String,
    right_moment_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    merge_adjacent_moments_core(&*repository, &project_id, &left_moment_id, &right_moment_id)
        .map_err(moment_persistence_error)
}

#[tauri::command(rename_all = "camelCase")]
fn split_moment(
    state: State<'_, AppState>,
    project_id: String,
    moment_id: String,
    after_asset_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id =
        MediaAssetId::try_from(after_asset_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    split_moment_core(&*repository, &project_id, &moment_id, &asset_id)
        .map_err(moment_persistence_error)
}

#[tauri::command(rename_all = "camelCase")]
fn moment_checklists(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<MomentChecklistView>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_moment_checklists(&*repository, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn create_coverage_checklist_item(
    state: State<'_, AppState>,
    project_id: String,
    input: CreateCoverageChecklistItemInput,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_coverage_checklist_item_core(&*repository, &project_id, &input)
        .map_err(moment_persistence_error)
}

#[tauri::command(rename_all = "camelCase")]
fn update_coverage_confirmation(
    state: State<'_, AppState>,
    project_id: String,
    input: UpdateCoverageConfirmationInput,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let _mutation_guard =
        begin_moment_project_mutation(&state.active_moment_projects, project_id.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    update_coverage_confirmation_core(&*repository, &project_id, &input)
        .map_err(moment_persistence_error)
}

/// Searches only durable current-project Moment centroids using the explicitly installed local
/// text provider. The response intentionally omits vectors and numeric scores; an unavailable
/// or incompatible local model returns no fabricated Moment-card match.
#[tauri::command(rename_all = "camelCase")]
fn moment_search(
    state: State<'_, AppState>,
    project_id: String,
    request: MomentSearchRequest,
) -> Result<MomentSearchResponse, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    search_moments(
        &*repository,
        &project_id,
        &request,
        &state.semantic_provider_cache,
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

/// Runs a project-scoped local query. The application service enforces that semantic text is
/// unavailable rather than fabricated if an approved local model or derived index is absent.
#[tauri::command(rename_all = "camelCase")]
fn magic_search_command(
    state: State<'_, AppState>,
    project_id: String,
    request: MagicSearchRequest,
) -> Result<MagicSearchResponse, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    search_magic(
        &*repository,
        &project_id,
        &request,
        &state.semantic_provider_cache,
        &state.semantic_index_root,
        &state.preview_cache_root,
    )
    .map_err(|error| error.to_string())
}

/// Finds related frames by local embedding proximity only. It does not create or mutate Similar
/// Sets, culling decisions, ratings, notes, or source media.
#[tauri::command(rename_all = "camelCase")]
fn find_similar_command(
    state: State<'_, AppState>,
    project_id: String,
    asset_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<MagicSearchResponse, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    find_similar(
        &*repository,
        &project_id,
        FindSimilarRequest {
            asset_id: &asset_id,
            limit: limit.unwrap_or(60).clamp(1, 120),
            offset: offset.unwrap_or(0),
        },
        SemanticStorageRoots {
            preview_cache_root: &state.preview_cache_root,
            index_root: &state.semantic_index_root,
        },
        &state.semantic_provider_cache,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn magic_search_history_command(
    state: State<'_, AppState>,
    project_id: String,
    limit: Option<u32>,
) -> Result<Vec<MagicSearchHistoryEntry>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_magic_search_history(&*repository, &project_id, limit.unwrap_or(8).clamp(1, 50))
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn clear_magic_search_history_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    clear_magic_search_history(&*repository, &project_id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn similarity_group_command(
    state: State<'_, AppState>,
    project_id: String,
    asset_id: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<Option<SimilarityGroupView>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_similarity_group(
        &*repository,
        &project_id,
        &asset_id,
        limit.unwrap_or(24).min(120),
        offset.unwrap_or(0),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn save_human_intelligence_decision_command(
    state: State<'_, AppState>,
    project_id: String,
    input: IntelligenceDecisionInput,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id =
        MediaAssetId::try_from(input.asset_id.as_str()).map_err(|error| error.to_string())?;
    let decision = match input.decision.as_str() {
        "keep" => HumanDecisionValue::Keep,
        "review" => HumanDecisionValue::Review,
        "reject" => HumanDecisionValue::Reject,
        _ => return Err("Decision must be keep, review, or reject".into()),
    };
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    save_human_intelligence_decision(
        &*repository,
        &project_id,
        &asset_id,
        decision,
        input.note.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn culling_workspace_command(
    state: State<'_, AppState>,
    project_id: String,
    query: CullingQueryInput,
) -> Result<CullingWorkspaceView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let query = culling_query(query);
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_culling_workspace(&*repository, &project_id, &query, &state.preview_cache_root)
        .map_err(|error| error.to_string())
}

fn culling_query(input: CullingQueryInput) -> CullingQuery {
    CullingQuery {
        mode: input.mode.unwrap_or_else(|| "all_photos".into()),
        filter: input.filter.unwrap_or_else(|| "all".into()),
        group_id: input.group_id,
        moment_id: clean_optional(input.moment_id),
        limit: input.limit.unwrap_or(60).clamp(1, 120),
        offset: input.offset.unwrap_or(0),
    }
}

#[tauri::command(rename_all = "camelCase")]
fn culling_progress_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<CullingProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_culling_progress(&*repository, &project_id).map_err(|error| error.to_string())
}

/// Cheap, read-only Studio Brain status. It deliberately does not backfill history, parse a
/// model artifact, or queue training when a project is opened.
#[tauri::command(rename_all = "camelCase")]
fn studio_brain_status_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<persistence::StudioBrainProjectStatus, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_studio_brain_status(&*repository, &project_id).map_err(|error| error.to_string())
}

/// Queue an explicit local Studio Brain update. The worker gets a separate SQLite connection;
/// it never blocks project browsing and never opens original media.
#[tauri::command(rename_all = "camelCase")]
fn start_studio_brain_training_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<StudioBrainProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let (profile_id, active_model_version) = {
        let repository = state
            .repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        let status = load_studio_brain_status(&*repository, &project_id)
            .map_err(|error| error.to_string())?;
        (status.profile_id, status.active_model_version)
    };
    let mutation_guard =
        begin_studio_profile_mutation(&state.active_studio_profiles, profile_id.clone())?;
    let catalog_path = state.catalog_path.clone();
    let event_project_id = project_id.to_string();
    let event_profile_id = profile_id.clone();
    let worker_profile_id = profile_id.clone();
    let worker_active_model_version = active_model_version.clone();
    let _worker = tauri::async_runtime::spawn_blocking(move || {
        let _studio_mutation = mutation_guard;
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            train_studio_brain(&repository, &project_id, |progress| {
                let _ = app.emit(
                    "studio-brain-progress",
                    StudioProfileScopedEvent {
                        project_id: event_project_id.clone(),
                        profile_id: event_profile_id.clone(),
                        progress,
                    },
                );
            })
            .map_err(|error| error.to_string())
        })();
        if let Err(error) = &result {
            // This is only a last-resort worker failure. Normal core failures persist an M8 run
            // and emit a friendly recovery message; raw diagnostics stay in Developer Details.
            let _ = app.emit(
                "studio-brain-progress",
                StudioProfileScopedEvent {
                    project_id: event_project_id,
                    profile_id: event_profile_id,
                    progress: StudioBrainProgress {
                        profile_id: worker_profile_id.clone(),
                        state: "error".into(),
                        active: false,
                        stage: "complete".into(),
                        completed: 0,
                        total: 0,
                        error_count: 1,
                        active_model_version: worker_active_model_version.clone(),
                        message: Some(if worker_active_model_version.is_some() {
                            "Studio Brain update could not be completed. Your previous personalized model is still active."
                        } else {
                            "Studio Brain update could not be completed. No personalized model was activated; generic technical evidence remains available."
                        }.into()),
                        last_error: Some(error.clone()),
                    },
                },
            );
        }
    });
    Ok(StudioBrainProgress {
        profile_id,
        state: "queued".into(),
        active: true,
        stage: "queued".into(),
        completed: 0,
        total: 0,
        error_count: 0,
        active_model_version,
        message: Some(
            "Studio Brain training is queued locally and will not block project browsing.".into(),
        ),
        last_error: None,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn set_studio_brain_project_included_command(
    state: State<'_, AppState>,
    project_id: String,
    included: bool,
) -> Result<persistence::StudioBrainProjectStatus, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let status = {
        let repository = state
            .repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        load_studio_brain_status(&*repository, &project_id).map_err(|error| error.to_string())?
    };
    let _guard =
        begin_studio_profile_mutation(&state.active_studio_profiles, status.profile_id.clone())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_studio_brain_project_included(&*repository, &project_id, included)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn set_studio_brain_enabled_command(
    state: State<'_, AppState>,
    project_id: String,
    enabled: bool,
) -> Result<persistence::StudioBrainProjectStatus, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let status = {
        let repository = state
            .repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        load_studio_brain_status(&*repository, &project_id).map_err(|error| error.to_string())?
    };
    let _guard =
        begin_studio_profile_mutation(&state.active_studio_profiles, status.profile_id.clone())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_studio_brain_enabled(&*repository, &project_id, enabled).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn reset_studio_brain_personalization_command(
    state: State<'_, AppState>,
    project_id: String,
    confirmed: bool,
) -> Result<persistence::StudioBrainProjectStatus, String> {
    if !confirmed {
        return Err(
            "Reset personalization requires confirmation. Your human decisions will be preserved."
                .into(),
        );
    }
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let status = {
        let repository = state
            .repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        load_studio_brain_status(&*repository, &project_id).map_err(|error| error.to_string())?
    };
    let _guard =
        begin_studio_profile_mutation(&state.active_studio_profiles, status.profile_id.clone())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    reset_studio_brain_personalization(&*repository, &project_id).map_err(|error| error.to_string())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionScopedEvent<T> {
    project_id: String,
    manifest_id: String,
    progress: T,
}

/// Cheap project-scoped production projection. It does not inspect destinations, build a
/// manifest, or start a file operation simply because the photographer opened the workspace.
#[tauri::command(rename_all = "camelCase")]
fn production_workspace_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProductionWorkspaceView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_production_workspace(&*repository, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn create_production_plan_command(
    state: State<'_, AppState>,
    project_id: String,
    input: ProductionPlanInput,
) -> Result<ProductionPlanRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_production_plan(&*repository, &project_id, &input).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn update_production_plan_configuration_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
    input: ProductionPlanInput,
) -> Result<ProductionPlanRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    update_production_plan_configuration(&*repository, &project_id, &plan_id, &input)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn set_production_plan_destination_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
    destination_path: Option<String>,
) -> Result<ProductionPlanRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    update_production_plan_destination(
        &*repository,
        &project_id,
        &plan_id,
        destination_path.as_deref(),
    )
    .map_err(|error| error.to_string())
}

/// Changes only the per-plan free-space headroom required before a local export. This cannot
/// alter source media or create a storage/file-count limit; it simply requires a fresh manifest.
#[tauri::command(rename_all = "camelCase")]
fn set_production_plan_destination_reserve_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
    reserve_bytes: u64,
) -> Result<ProductionPlanRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    update_production_plan_destination_reserve(&*repository, &project_id, &plan_id, reserve_bytes)
        .map_err(|error| error.to_string())
}

/// This is a plan-local organizational exception only. It cannot mutate Keep/Reject/Review,
/// ratings, notes, Studio advice, or source media.
#[tauri::command(rename_all = "camelCase")]
fn set_production_plan_override_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
    asset_id: String,
    kind: Option<PlanOverrideKind>,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_production_plan_override(&*repository, &project_id, &plan_id, &asset_id, kind)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn create_virtual_collection_command(
    state: State<'_, AppState>,
    project_id: String,
    input: VirtualCollectionInput,
) -> Result<VirtualCollectionRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_virtual_collection(&*repository, &project_id, &input).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn set_static_virtual_collection_members_command(
    state: State<'_, AppState>,
    project_id: String,
    collection_id: String,
    asset_ids: Vec<String>,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_ids = asset_ids
        .iter()
        .map(|id| MediaAssetId::try_from(id.as_str()).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_static_virtual_collection_members(&*repository, &project_id, &collection_id, &asset_ids)
        .map_err(|error| error.to_string())
}

/// Bounded one-asset collection mutation for the Production inspection view. It is a local
/// reference update only and cannot alter a Smart Cull decision or source media.
#[tauri::command(rename_all = "camelCase")]
fn set_static_virtual_collection_member_command(
    state: State<'_, AppState>,
    project_id: String,
    collection_id: String,
    asset_id: String,
    included: bool,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_static_virtual_collection_member(
        &*repository,
        &project_id,
        &collection_id,
        &asset_id,
        included,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn production_plan_preview_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
) -> Result<ProductionPlanPreview, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    capture_core::preview_production_plan(&*repository, &project_id, &plan_id)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn create_production_manifest_command(
    state: State<'_, AppState>,
    project_id: String,
    plan_id: String,
) -> Result<ExportManifestRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_production_export_manifest(&*repository, &project_id, &plan_id)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn production_manifest_preflight_command(
    state: State<'_, AppState>,
    project_id: String,
    manifest_id: String,
) -> Result<ProductionPreflight, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    preflight_production_export(&*repository, &project_id, &manifest_id)
        .map_err(|error| error.to_string())
}

/// Starts an explicit local export from an immutable manifest. A fresh repository connection
/// permits browsing and culling while the verified copier runs. Both this guard and the durable
/// partial unique index reject duplicate starts.
#[tauri::command(rename_all = "camelCase")]
fn start_production_export_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    manifest_id: String,
) -> Result<ProductionExportProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let manifest_key = manifest_id.trim().to_owned();
    if manifest_key.is_empty() {
        return Err("Choose a frozen Delivery Manifest before exporting".into());
    }
    let mutation_guard = begin_production_manifest_mutation(
        &state.active_production_manifests,
        manifest_key.clone(),
    )?;
    let cancellation = Arc::new(AtomicBool::new(false));
    state
        .production_cancel_controls
        .lock()
        .map_err(|_| "Production export cancellation lock was poisoned".to_owned())?
        .insert(manifest_key.clone(), Arc::clone(&cancellation));
    let catalog_path = state.catalog_path.clone();
    let event_project_id = project_id.to_string();
    let event_manifest_id = manifest_key.clone();
    let cancellation_controls = Arc::clone(&state.production_cancel_controls);
    let _worker = tauri::async_runtime::spawn_blocking(move || {
        let _manifest_mutation = mutation_guard;
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            capture_core::export_production_manifest(
                &repository,
                &project_id,
                &manifest_key,
                || cancellation.load(Ordering::Relaxed),
                |progress| {
                    let _ = app.emit(
                        "production-export-progress",
                        ProductionScopedEvent {
                            project_id: event_project_id.clone(),
                            manifest_id: event_manifest_id.clone(),
                            progress: progress.clone(),
                        },
                    );
                },
            )
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut controls) = cancellation_controls.lock() {
            controls.remove(&manifest_key);
        }
        if let Err(error) = result {
            let _ = app.emit(
                "production-export-progress",
                ProductionScopedEvent {
                    project_id: event_project_id,
                    manifest_id: event_manifest_id,
                    progress: ProductionExportProgress {
                        export_job_id: String::new(),
                        manifest_id: manifest_key,
                        state: "failed".into(),
                        stage: "finalize".into(),
                        items_completed: 0,
                        items_total: 0,
                        verified_count: 0,
                        skipped_identical_count: 0,
                        failed_count: 1,
                        verified_bytes: 0,
                        current_filename: None,
                        message: Some("The local export could not be started or completed. No source media was changed.".into()),
                    },
                },
            );
            eprintln!("Production export worker diagnostic: {error}");
        }
    });
    Ok(ProductionExportProgress {
        export_job_id: String::new(),
        manifest_id,
        state: "queued".into(),
        stage: "queued".into(),
        items_completed: 0,
        items_total: 0,
        verified_count: 0,
        skipped_identical_count: 0,
        failed_count: 0,
        verified_bytes: 0,
        current_filename: None,
        message: Some(
            "Verified local export is queued and will not block project browsing.".into(),
        ),
    })
}

#[tauri::command(rename_all = "camelCase")]
fn cancel_production_export_command(
    state: State<'_, AppState>,
    manifest_id: String,
) -> Result<(), String> {
    let controls = state
        .production_cancel_controls
        .lock()
        .map_err(|_| "Production export cancellation lock was poisoned".to_owned())?;
    let cancellation = controls
        .get(manifest_id.trim())
        .ok_or_else(|| "No active local export was found for this Delivery Manifest".to_owned())?;
    cancellation.store(true, Ordering::Relaxed);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Milestone 10 — Edit Bridge command boundary
// ─────────────────────────────────────────────────────────────────────────────

/// A compact read-only projection. Opening Edit never scans an external output root, generates
/// a handoff, or re-evaluates a frozen Production manifest.
#[tauri::command(rename_all = "camelCase")]
fn edit_workspace_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<EditWorkspace, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    edit_workspace(&*repository, &project_id).map_err(|error| error.to_string())
}

/// Creates an Edit Session from one already verified, immutable M9 Export Manifest. The core
/// transaction rejects stale or ineligible sources; this command never recreates a selection.
#[tauri::command(rename_all = "camelCase")]
fn create_edit_session_command(
    state: State<'_, AppState>,
    project_id: String,
    input: CreateEditSessionCommandInput,
) -> Result<EditSessionView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let input = CreateEditSessionInput {
        name: input.name,
        template: edit_session_template(&input.template)?,
        export_manifest_id: input.export_manifest_id,
        expected_output_policy: expected_output_policy(&input.expected_output_policy)?,
    };
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    create_edit_session(&*repository, &project_id, &input).map_err(|error| error.to_string())
}

/// Loads at most one bounded session page. Source/returned previews remain cache-backed URLs;
/// this command never returns a source or external-output filesystem path to the webview.
#[tauri::command(rename_all = "camelCase")]
fn edit_session_page_command(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
    offset: u64,
    limit: u32,
) -> Result<EditSessionPageView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err("Choose an Edit Session before loading its queue".into());
    }
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    edit_session_page(&*repository, &project_id, session_id, offset, limit)
        .map_err(|error| error.to_string())
}

/// Generates only a local coordination bundle. A worker-owned SQLite connection leaves project
/// browsing responsive while the core applies its path-separation and no-overwrite checks.
#[tauri::command(rename_all = "camelCase")]
async fn generate_edit_handoff_command(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
    input: GenerateEditHandoffCommandInput,
) -> Result<EditHandoffView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let session_id = session_id.trim().to_owned();
    let input = EditHandoffInput {
        mode: edit_handoff_mode(&input.mode)?,
        destination_path: input.destination_path,
    };
    let mutation_guard =
        begin_edit_session_mutation(&state.active_edit_sessions, &project_id, &session_id)?;
    let catalog_path = state.catalog_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _edit_session_mutation = mutation_guard;
        let repository = SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
        generate_edit_handoff(&repository, &project_id, &session_id, &input)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("Edit handoff task did not complete: {error}"))?
}

/// Explicitly scans only the folder selected by the photographer, on a worker connection. The
/// local session guard rejects duplicate scans/handoff writes while durable transaction checks
/// retain authority across restarts and another process.
#[tauri::command(rename_all = "camelCase")]
async fn register_edit_outputs_command(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
    selected_path: String,
) -> Result<EditOutputRegistrationSummary, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let session_id = session_id.trim().to_owned();
    let mutation_guard =
        begin_edit_session_mutation(&state.active_edit_sessions, &project_id, &session_id)?;
    let catalog_path = state.catalog_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _edit_session_mutation = mutation_guard;
        let repository = SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
        register_edit_outputs(&repository, &project_id, &session_id, &selected_path)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("Edit output discovery task did not complete: {error}"))?
}

/// A manual link is an explicit local provenance action only. It cannot approve an output or
/// alter culling, Production, originals, source FileInstances, or the returned derivative.
#[tauri::command(rename_all = "camelCase")]
fn manually_match_edit_output_command(
    state: State<'_, AppState>,
    project_id: String,
    output_id: String,
    work_item_id: String,
) -> Result<EditOutputView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    manually_match_edit_output(
        &*repository,
        &project_id,
        output_id.trim(),
        work_item_id.trim(),
    )
    .map_err(|error| error.to_string())
}

/// Approval and revision are intentionally the only accepted review transitions at the desktop
/// boundary. The core persists history and keeps all human culling decisions independent.
#[tauri::command(rename_all = "camelCase")]
fn review_edit_version_command(
    state: State<'_, AppState>,
    project_id: String,
    version_id: String,
    review_state: String,
) -> Result<persistence::EditVersionRecord, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let review_state = edit_version_review_state(&review_state)?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    review_edit_version(&*repository, &project_id, version_id.trim(), review_state)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn update_culling_decision_command(
    state: State<'_, AppState>,
    project_id: String,
    input: CullingDecisionInput,
) -> Result<CullingDecisionView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id =
        MediaAssetId::try_from(input.asset_id.as_str()).map_err(|error| error.to_string())?;
    let decision = match input.decision.as_deref() {
        Some("keep") => Some(CullingDecisionValue::Keep),
        Some("review") => Some(CullingDecisionValue::Review),
        Some("reject") => Some(CullingDecisionValue::Reject),
        Some(_) => return Err("Culling decision must be keep, review, or reject".into()),
        None => None,
    };
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    apply_culling_decision(
        &*repository,
        &project_id,
        &asset_id,
        &CullingDecisionUpdate {
            decision,
            clear_decision: input.clear_decision.unwrap_or(false),
            rating: input.rating,
            starred: input.starred,
            note: input.note.map(|note| note.trim().to_owned()),
            flags: input.flags,
            session_id: input.session_id,
        },
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn update_culling_position_command(
    state: State<'_, AppState>,
    project_id: String,
    input: CullingPositionInput,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = input
        .asset_id
        .as_deref()
        .map(MediaAssetId::try_from)
        .transpose()
        .map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    update_culling_position(
        &*repository,
        &project_id,
        &input.session_id,
        asset_id.as_ref(),
        input.group_id.as_deref(),
        &input.mode,
        input.filter_context.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn set_culling_group_representative_command(
    state: State<'_, AppState>,
    project_id: String,
    group_id: String,
    asset_id: String,
    session_id: Option<String>,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let asset_id = MediaAssetId::try_from(asset_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    set_culling_group_representative(
        &*repository,
        &project_id,
        &group_id,
        &asset_id,
        session_id.as_deref(),
    )
    .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn complete_culling_group_command(
    state: State<'_, AppState>,
    project_id: String,
    group_id: String,
    session_id: Option<String>,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    complete_culling_group(&*repository, &project_id, &group_id, session_id.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn finish_culling_review_command(
    state: State<'_, AppState>,
    project_id: String,
    session_id: String,
) -> Result<ReviewSessionView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    finish_culling_review(&*repository, &project_id, &session_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn export_culling_report_command(
    state: State<'_, AppState>,
    project_id: String,
    destination_path: String,
    format: String,
) -> Result<usize, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    if destination_path.trim().is_empty() {
        return Err("Choose a destination for the culling report".into());
    }
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    export_culling_report(
        &*repository,
        &project_id,
        Path::new(&destination_path),
        &format,
    )
    .map_err(|error| error.to_string())
}

/// Deliberately developer/research-only: this command is not wired into the photographer UI.
/// Its JSON contains relative IDs and technical/recommendation structure, never media/path data.
#[tauri::command(rename_all = "camelCase")]
fn export_studio_brain_preference_examples_command(
    state: State<'_, AppState>,
    project_id: String,
    destination_path: String,
) -> Result<usize, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    if destination_path.trim().is_empty() {
        return Err("Choose a destination for the local preference export".into());
    }
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    export_studio_brain_preference_examples(&*repository, &project_id, Path::new(&destination_path))
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
async fn prepare_media_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    query: VisualQueryInput,
) -> Result<MediaPreparationProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let project_key = project_id.to_string();
    let active_visual_projects = Arc::clone(&state.active_visual_projects);
    {
        let mut active = active_visual_projects
            .lock()
            .map_err(|_| "preview preparation lock was poisoned".to_owned())?;
        if !active.insert(project_key.clone()) {
            return Err("Preview preparation is already running for this project".into());
        }
    }
    let catalog_path = state.catalog_path.clone();
    let preview_cache_root = state.preview_cache_root.clone();
    let query = visual_query(query);
    let event_project_key = project_key.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // A dedicated SQLite connection lets grid reads continue during preparation (WAL mode).
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            prepare_visual_media(
                &repository,
                &project_id,
                &query,
                &preview_cache_root,
                |progress| {
                    let _ = app.emit(
                        "media-preparation-progress",
                        ProjectScopedEvent {
                            project_id: event_project_key.clone(),
                            progress,
                        },
                    );
                },
            )
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut active) = active_visual_projects.lock() {
            active.remove(&project_key);
        }
        result
    })
    .await
    .map_err(|error| format!("media preparation task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
async fn retry_failed_previews_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<MediaPreparationProgress, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let project_key = project_id.to_string();
    let active_visual_projects = Arc::clone(&state.active_visual_projects);
    {
        let mut active = active_visual_projects
            .lock()
            .map_err(|_| "preview preparation lock was poisoned".to_owned())?;
        if !active.insert(project_key.clone()) {
            return Err("Preview preparation is already running for this project".into());
        }
    }
    let catalog_path = state.catalog_path.clone();
    let preview_cache_root = state.preview_cache_root.clone();
    let event_project_key = project_key.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let repository =
                SqliteRepository::open(catalog_path).map_err(|error| error.to_string())?;
            retry_failed_visual_media(&repository, &project_id, &preview_cache_root, |progress| {
                let _ = app.emit(
                    "media-preparation-progress",
                    ProjectScopedEvent {
                        project_id: event_project_key.clone(),
                        progress,
                    },
                );
            })
            .map_err(|error| error.to_string())
        })();
        if let Ok(mut active) = active_visual_projects.lock() {
            active.remove(&project_key);
        }
        result
    })
    .await
    .map_err(|error| format!("preview retry task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn clear_preview_cache_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    clear_visual_cache(&*repository, &project_id, &state.preview_cache_root)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn media_capabilities() -> Vec<media_visual::Capability> {
    media_visual::capabilities()
}

#[tauri::command(rename_all = "camelCase")]
async fn index_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    selected_path: String,
) -> Result<JobView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let event_project_key = project_id.to_string();
    let repository = Arc::clone(&state.repository);
    tauri::async_runtime::spawn_blocking(move || {
        let repository = repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        index_local_folder(&*repository, &project_id, &selected_path, |job| {
            let _ = app.emit(
                "index-progress",
                ProjectScopedEvent {
                    project_id: event_project_key.clone(),
                    progress: job,
                },
            );
        })
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("index task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn preflight_ingest_command(
    state: State<'_, AppState>,
    project_id: String,
    request: IngestRequest,
    policy: IngestProtectionPolicy,
) -> Result<IngestPreflightView, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    preflight_ingest(&*repository, &project_id, request, policy).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
async fn start_ingest_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    request: IngestRequest,
    policy: IngestProtectionPolicy,
    start_request_id: String,
) -> Result<IngestReport, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    uuid::Uuid::parse_str(&start_request_id)
        .map_err(|_| "ingest start request ID must be a UUID".to_owned())?;
    let event_project_key = project_id.to_string();
    let repository = Arc::clone(&state.repository);
    tauri::async_runtime::spawn_blocking(move || {
        let repository = repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        start_ingest(
            &*repository,
            &project_id,
            request,
            policy,
            &start_request_id,
            |report| {
                let _ = app.emit(
                    "ingest-progress",
                    ProjectScopedEvent {
                        project_id: event_project_key.clone(),
                        progress: report,
                    },
                );
            },
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("ingest task did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
async fn restart_ingest_command(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    job_id: String,
) -> Result<IngestReport, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let job_id = IngestJobId::try_from(job_id.as_str()).map_err(|error| error.to_string())?;
    {
        let repository = state
            .repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        let job = repository
            .ingest_job(&job_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "ingest job not found".to_owned())?;
        if job.project_id != project_id {
            return Err("ingest job does not belong to the selected project".into());
        }
    }
    let event_project_key = project_id.to_string();
    let repository = Arc::clone(&state.repository);
    tauri::async_runtime::spawn_blocking(move || {
        let repository = repository
            .lock()
            .map_err(|_| "catalog lock was poisoned".to_owned())?;
        restart_ingest(&*repository, &job_id, |report| {
            let _ = app.emit(
                "ingest-progress",
                ProjectScopedEvent {
                    project_id: event_project_key.clone(),
                    progress: report,
                },
            );
        })
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("ingest retry did not complete: {error}"))?
}

#[tauri::command(rename_all = "camelCase")]
fn ingest_history_command(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<IngestJobSummary>, String> {
    let project_id = ProjectId::try_from(project_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    ingest_history(&*repository, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn ingest_report_command(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Option<IngestReport>, String> {
    let job_id = IngestJobId::try_from(job_id.as_str()).map_err(|error| error.to_string())?;
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    ingest_report(&*repository, &job_id).map_err(|error| error.to_string())
}

fn browser_filter(value: &str) -> MediaBrowserFilter {
    match value {
        "photos" => MediaBrowserFilter::Photos,
        "video" => MediaBrowserFilter::Video,
        "audio" => MediaBrowserFilter::Audio,
        "sidecars" => MediaBrowserFilter::Sidecars,
        "unknown" => MediaBrowserFilter::Unknown,
        _ => MediaBrowserFilter::All,
    }
}

fn visual_query(input: VisualQueryInput) -> VisualMediaQuery {
    VisualMediaQuery {
        filter: match input.filter.as_deref() {
            Some("photos") => VisualMediaFilter::Photos,
            Some("raw") => VisualMediaFilter::Raw,
            Some("jpegHeif") => VisualMediaFilter::JpegHeif,
            Some("video") => VisualMediaFilter::Video,
            Some("audio") => VisualMediaFilter::Audio,
            Some("offline") => VisualMediaFilter::Offline,
            Some("available") => VisualMediaFilter::Available,
            Some("strongCandidates") => VisualMediaFilter::StrongCandidates,
            Some("technicalIssues") => VisualMediaFilter::TechnicalIssues,
            Some("probableDuplicates") => VisualMediaFilter::ProbableDuplicates,
            Some("similarGroups") => VisualMediaFilter::SimilarGroups,
            Some("faces") => VisualMediaFilter::Faces,
            Some("possibleClosedEyes") => VisualMediaFilter::PossibleClosedEyes,
            Some("blurReview") => VisualMediaFilter::BlurReview,
            _ => VisualMediaFilter::All,
        },
        sort: match input.sort.as_deref() {
            Some("filename") => VisualMediaSort::Filename,
            Some("fileSize") => VisualMediaSort::FileSize,
            Some("dateIndexed") => VisualMediaSort::DateIndexed,
            Some("mediaType") => VisualMediaSort::MediaType,
            _ => VisualMediaSort::CaptureTime,
        },
        descending: input.descending.unwrap_or(false),
        search: clean_optional(input.search),
        camera_model: clean_optional(input.camera_model),
        lens_model: clean_optional(input.lens_model),
        captured_from: clean_optional(input.captured_from),
        captured_to: clean_optional(input.captured_to),
        moment_id: clean_optional(input.moment_id),
        limit: input.limit.unwrap_or(120).clamp(1, 250),
        offset: input.offset.unwrap_or(0),
    }
}

fn analysis_resource_mode(value: &str) -> Result<AnalysisResourceMode, String> {
    match value {
        "eco" => Ok(AnalysisResourceMode::Eco),
        "balanced" | "" => Ok(AnalysisResourceMode::Balanced),
        "fast" => Ok(AnalysisResourceMode::Fast),
        _ => Err("Analysis mode must be eco, balanced, or fast".into()),
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn app_data_directory(app: &AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let data_directory = app.path().app_local_data_dir()?;
    fs::create_dir_all(&data_directory)?;
    Ok(data_directory)
}

fn open_catalog(app: &AppHandle) -> Result<SqliteRepository, Box<dyn std::error::Error>> {
    Ok(SqliteRepository::open(
        app_data_directory(app)?.join("captureos.sqlite3"),
    )?)
}

fn preview_protocol_response(
    state: &AppState,
    request: &http::Request<Vec<u8>>,
) -> http::Response<Vec<u8>> {
    if request.method() != http::Method::GET {
        return preview_protocol_error(http::StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(artifact_id) = request.uri().path().strip_prefix('/') else {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    };
    if artifact_id.is_empty() || artifact_id.contains('/') || Uuid::parse_str(artifact_id).is_err()
    {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    }
    let artifact = match state.repository.lock() {
        Ok(repository) => repository
            .preview_render_artifact(artifact_id)
            .ok()
            .flatten(),
        Err(_) => None,
    };
    let Some(artifact) = artifact else {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    };
    let Some(path) =
        resolve_preview_artifact_path(&state.preview_cache_root, &artifact.cache_relative_path)
    else {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    };
    let Ok(bytes) = fs::read(&path) else {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    };
    let Some(content_type) = verified_preview_content_type(&path, &bytes) else {
        return preview_protocol_error(http::StatusCode::NOT_FOUND);
    };
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(bytes)
        .expect("static preview response is valid")
}

fn preview_protocol_error(status: http::StatusCode) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(Vec::new())
        .expect("static preview error response is valid")
}

fn resolve_preview_artifact_path(cache_root: &Path, relative: &str) -> Option<PathBuf> {
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
    let cache_root = cache_root.canonicalize().ok()?;
    let candidate = cache_root.join(relative).canonicalize().ok()?;
    (candidate.is_file() && candidate.starts_with(&cache_root)).then_some(candidate)
}

fn verified_preview_content_type(path: &Path, bytes: &[u8]) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str())? {
        "jpg" | "jpeg" if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) => Some("image/jpeg"),
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        _ => None,
    }
}

fn main() {
    tauri::Builder::default()
        .register_uri_scheme_protocol(PREVIEW_PROTOCOL, |context, request| {
            let state = context.app_handle().state::<AppState>();
            preview_protocol_response(&state, &request)
        })
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let repository = open_catalog(app.handle())?;
            recover_interrupted_ingests(&repository)?;
            recover_interrupted_visual_preparations(&repository)?;
            recover_interrupted_capture_intelligence(&repository)?;
            recover_interrupted_semantic_indexing(&repository)?;
            recover_interrupted_moment_analysis(&repository)?;
            recover_interrupted_studio_training(&repository)?;
            recover_interrupted_production_exports(&repository)?;
            let data_directory = app_data_directory(app.handle())?;
            let preview_cache_root = data_directory.join("preview-cache");
            // These roots contain only CaptureOS-managed derived data and operator-installed
            // static model packs. Nothing here is a source-media location or cloud cache.
            let semantic_model_root = data_directory.join("semantic-models");
            let semantic_index_root = data_directory.join("semantic-index");
            fs::create_dir_all(&preview_cache_root)?;
            fs::create_dir_all(&semantic_model_root)?;
            fs::create_dir_all(&semantic_index_root)?;
            app.manage(AppState {
                repository: Arc::new(Mutex::new(repository)),
                catalog_path: data_directory.join("captureos.sqlite3"),
                preview_cache_root,
                semantic_provider_cache: Arc::new(SiglipProviderCache::new(&semantic_model_root)),
                semantic_index_root,
                active_visual_projects: Arc::new(Mutex::new(HashSet::new())),
                active_metadata_refresh_projects: Arc::new(Mutex::new(HashSet::new())),
                active_intelligence_projects: Arc::new(Mutex::new(HashSet::new())),
                intelligence_pause_controls: Arc::new(Mutex::new(HashMap::new())),
                active_semantic_projects: Arc::new(Mutex::new(HashSet::new())),
                semantic_pause_controls: Arc::new(Mutex::new(HashMap::new())),
                active_moment_projects: Arc::new(Mutex::new(HashSet::new())),
                moment_pause_controls: Arc::new(Mutex::new(HashMap::new())),
                active_studio_profiles: Arc::new(Mutex::new(HashSet::new())),
                active_production_manifests: Arc::new(Mutex::new(HashSet::new())),
                production_cancel_controls: Arc::new(Mutex::new(HashMap::new())),
                active_edit_sessions: Arc::new(Mutex::new(HashSet::new())),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            project_library,
            create_project,
            project_home,
            visual_media_page,
            media_asset_detail_command,
            visual_preparation_summary_command,
            refresh_metadata_command,
            capture_intelligence_summary_command,
            prepare_media_command,
            retry_failed_previews_command,
            start_capture_intelligence_command,
            pause_capture_intelligence_command,
            semantic_index_status_command,
            start_semantic_index_command,
            pause_semantic_index_command,
            moment_timeline_status,
            start_moment_analysis,
            moment_timeline,
            moment_detail,
            rename_moment,
            set_moment_human_representative,
            merge_adjacent_moments,
            split_moment,
            moment_checklists,
            create_coverage_checklist_item,
            update_coverage_confirmation,
            moment_search,
            magic_search_command,
            find_similar_command,
            magic_search_history_command,
            clear_magic_search_history_command,
            similarity_group_command,
            save_human_intelligence_decision_command,
            culling_workspace_command,
            culling_progress_command,
            studio_brain_status_command,
            start_studio_brain_training_command,
            set_studio_brain_project_included_command,
            set_studio_brain_enabled_command,
            reset_studio_brain_personalization_command,
            production_workspace_command,
            create_production_plan_command,
            update_production_plan_configuration_command,
            set_production_plan_destination_command,
            set_production_plan_destination_reserve_command,
            set_production_plan_override_command,
            create_virtual_collection_command,
            set_static_virtual_collection_members_command,
            set_static_virtual_collection_member_command,
            production_plan_preview_command,
            create_production_manifest_command,
            production_manifest_preflight_command,
            start_production_export_command,
            cancel_production_export_command,
            edit_workspace_command,
            create_edit_session_command,
            edit_session_page_command,
            generate_edit_handoff_command,
            register_edit_outputs_command,
            manually_match_edit_output_command,
            review_edit_version_command,
            update_culling_decision_command,
            update_culling_position_command,
            set_culling_group_representative_command,
            complete_culling_group_command,
            finish_culling_review_command,
            export_culling_report_command,
            export_studio_brain_preference_examples_command,
            clear_preview_cache_command,
            media_capabilities,
            index_folder,
            preflight_ingest_command,
            start_ingest_command,
            restart_ingest_command,
            ingest_history_command,
            ingest_report_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running CaptureOS desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use persistence::PreviewArtifactRecord;
    use tempfile::tempdir;

    #[test]
    fn moment_scopes_are_preserved_only_as_bounded_backend_query_fields() {
        let visual = visual_query(VisualQueryInput {
            filter: Some("photos".into()),
            sort: Some("captureTime".into()),
            descending: None,
            search: None,
            camera_model: None,
            lens_model: None,
            captured_from: None,
            captured_to: None,
            moment_id: Some("  moment-1  ".into()),
            limit: Some(24),
            offset: Some(3),
        });
        assert_eq!(visual.moment_id.as_deref(), Some("moment-1"));
        assert_eq!(visual.limit, 24);
        assert_eq!(visual.offset, 3);

        let culling = culling_query(CullingQueryInput {
            mode: None,
            filter: None,
            group_id: None,
            moment_id: Some(" moment-1 ".into()),
            limit: Some(500),
            offset: None,
        });
        assert_eq!(culling.moment_id.as_deref(), Some("moment-1"));
        assert_eq!(culling.limit, 120);
        assert_eq!(culling.offset, 0);
    }

    #[test]
    fn moment_project_mutation_guard_rejects_a_second_same_project_action_until_release() {
        let active_projects = Arc::new(Mutex::new(HashSet::new()));
        let first = begin_moment_project_mutation(&active_projects, "project-1".into()).unwrap();
        assert!(begin_moment_project_mutation(&active_projects, "project-1".into()).is_err());
        assert!(begin_moment_project_mutation(&active_projects, "project-2".into()).is_ok());
        drop(first);
        assert!(begin_moment_project_mutation(&active_projects, "project-1".into()).is_ok());
    }

    #[test]
    fn production_manifest_guard_rejects_a_duplicate_start_until_the_worker_releases_it() {
        let active_manifests = Arc::new(Mutex::new(HashSet::new()));
        let first =
            begin_production_manifest_mutation(&active_manifests, "manifest-1".into()).unwrap();
        assert!(
            begin_production_manifest_mutation(&active_manifests, "manifest-1".into()).is_err()
        );
        assert!(begin_production_manifest_mutation(&active_manifests, "manifest-2".into()).is_ok());
        drop(first);
        assert!(begin_production_manifest_mutation(&active_manifests, "manifest-1".into()).is_ok());
    }

    #[test]
    fn edit_session_guard_rejects_overlapping_handoff_or_output_discovery() {
        let active_sessions = Arc::new(Mutex::new(HashSet::new()));
        let project_id = ProjectId::from_uuid(Uuid::from_u128(10));
        let first =
            begin_edit_session_mutation(&active_sessions, &project_id, "session-1").unwrap();
        assert!(begin_edit_session_mutation(&active_sessions, &project_id, "session-1").is_err());
        assert!(begin_edit_session_mutation(&active_sessions, &project_id, "session-2").is_ok());
        drop(first);
        assert!(begin_edit_session_mutation(&active_sessions, &project_id, "session-1").is_ok());
    }

    #[test]
    fn edit_command_values_map_only_supported_human_actions() {
        assert_eq!(
            edit_session_template("main_edit").unwrap(),
            EditSessionTemplate::WeddingMainEdit
        );
        assert_eq!(
            edit_session_template("client_revision").unwrap(),
            EditSessionTemplate::ClientRevisionRound
        );
        assert_eq!(
            expected_output_policy("one_per_work_item").unwrap(),
            ExpectedOutputPolicy::Required
        );
        assert_eq!(
            edit_handoff_mode("reference").unwrap(),
            HandoffMode::Reference
        );
        assert_eq!(
            edit_version_review_state("needs_revision").unwrap(),
            EditVersionReviewState::NeedsRevision
        );
        assert!(edit_session_template("invent_a_recipe").is_err());
        assert!(edit_handoff_mode("proprietary_catalog").is_err());
        assert!(edit_version_review_state("ready_for_review").is_err());
    }

    #[test]
    fn preview_bridge_serves_a_registered_ready_artifact_and_rejects_unknown_ids() {
        let directory = tempdir().unwrap();
        let cache_root = directory.path().join("preview-cache");
        let source_root = directory.path().join("indexed-source");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(
            source_root.join("IMG_0001.JPG"),
            b"read-only indexed source",
        )
        .unwrap();

        let repository = SqliteRepository::open_in_memory().unwrap();
        let project = create_local_project(&repository, "Preview bridge").unwrap();
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
            .pop()
            .unwrap();
        let artifact_id = Uuid::new_v4().to_string();
        let relative = "m3.1/asset/thumb.jpg";
        let artifact = cache_root.join(relative);
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
        repository
            .upsert_preview_artifact(&PreviewArtifactRecord {
                id: artifact_id.clone(),
                media_asset_id: item.asset_id,
                source_file_instance_id: item.file_instance_id,
                artifact_type: "thumbnail".into(),
                size_class: "small".into(),
                cache_relative_path: relative.into(),
                provider: "test".into(),
                generator_version: "test".into(),
                source_fingerprint: "test".into(),
                created_at: "2026-08-09T00:00:00Z".into(),
                status: "ready".into(),
                failure_reason: None,
            })
            .unwrap();
        let state = AppState {
            repository: Arc::new(Mutex::new(repository)),
            catalog_path: directory.path().join("captureos.sqlite3"),
            preview_cache_root: cache_root,
            semantic_provider_cache: Arc::new(SiglipProviderCache::new(
                directory.path().join("semantic-models"),
            )),
            semantic_index_root: directory.path().join("semantic-index"),
            active_visual_projects: Arc::new(Mutex::new(HashSet::new())),
            active_metadata_refresh_projects: Arc::new(Mutex::new(HashSet::new())),
            active_intelligence_projects: Arc::new(Mutex::new(HashSet::new())),
            intelligence_pause_controls: Arc::new(Mutex::new(HashMap::new())),
            active_semantic_projects: Arc::new(Mutex::new(HashSet::new())),
            semantic_pause_controls: Arc::new(Mutex::new(HashMap::new())),
            active_moment_projects: Arc::new(Mutex::new(HashSet::new())),
            moment_pause_controls: Arc::new(Mutex::new(HashMap::new())),
            active_studio_profiles: Arc::new(Mutex::new(HashSet::new())),
            active_production_manifests: Arc::new(Mutex::new(HashSet::new())),
            production_cancel_controls: Arc::new(Mutex::new(HashMap::new())),
            active_edit_sessions: Arc::new(Mutex::new(HashSet::new())),
        };

        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri(format!("{PREVIEW_PROTOCOL}://localhost/{artifact_id}"))
            .body(Vec::new())
            .unwrap();
        let response = preview_protocol_response(&state, &request);
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.headers()[http::header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(response.body(), &vec![0xFF, 0xD8, 0xFF, 0xD9]);

        let unknown = Uuid::new_v4();
        let unknown_request = http::Request::builder()
            .method(http::Method::GET)
            .uri(format!("{PREVIEW_PROTOCOL}://localhost/{unknown}"))
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            preview_protocol_response(&state, &unknown_request).status(),
            http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn preview_bridge_serves_only_cache_contained_image_artifacts() {
        let directory = tempdir().unwrap();
        let cache_root = directory.path().join("preview-cache");
        let artifact = cache_root.join("m3/asset/thumb.jpg");
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();

        let resolved = resolve_preview_artifact_path(&cache_root, "m3/asset/thumb.jpg").unwrap();
        assert_eq!(resolved, artifact.canonicalize().unwrap());
        assert_eq!(
            verified_preview_content_type(&resolved, &fs::read(&resolved).unwrap()),
            Some("image/jpeg")
        );
        assert!(resolve_preview_artifact_path(&cache_root, "../outside.jpg").is_none());
        assert!(resolve_preview_artifact_path(directory.path(), "/etc/passwd").is_none());
        assert!(verified_preview_content_type(&resolved, b"not a JPEG").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn preview_bridge_rejects_a_symlink_that_escapes_the_cache() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let cache_root = directory.path().join("preview-cache");
        let outside = directory.path().join("outside.jpg");
        fs::create_dir_all(&cache_root).unwrap();
        fs::write(&outside, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
        symlink(&outside, cache_root.join("escaped.jpg")).unwrap();

        assert!(resolve_preview_artifact_path(&cache_root, "escaped.jpg").is_none());
    }
}
