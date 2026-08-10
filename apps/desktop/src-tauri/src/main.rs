#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use capture_core::{
    analyze_capture_intelligence, apply_culling_decision, clear_visual_cache,
    complete_culling_group, create_local_project, export_culling_report,
    export_studio_brain_preference_examples, finish_culling_review, index_local_folder,
    ingest_history, ingest_report, list_local_projects, load_capture_intelligence_summary,
    load_culling_progress, load_culling_workspace, load_media_asset_detail, load_project_home,
    load_project_library, load_similarity_group, load_visual_media_page,
    load_visual_preparation_summary, preflight_ingest, prepare_visual_media,
    recover_interrupted_capture_intelligence, recover_interrupted_ingests,
    recover_interrupted_visual_preparations, restart_ingest, retry_failed_visual_media,
    save_human_intelligence_decision, set_culling_group_representative, start_ingest,
    update_culling_position, CaptureIntelligenceProgress, IngestPreflightView, JobView,
    MediaPreparationProgress, ProjectHome, ProjectLibraryItem, ProjectView,
};
use ingest::IngestRequest;
use media_model::{
    AnalysisResourceMode, CullingDecisionValue, HumanDecisionValue, IngestJobId,
    IngestProtectionPolicy, MediaAssetId, ProjectId,
};
use persistence::{
    CatalogRepository, CullingDecisionUpdate, CullingDecisionView, CullingProgress, CullingQuery,
    CullingWorkspaceView, IngestJobSummary, IngestReport, MediaAssetDetail, MediaBrowserFilter,
    ReviewSessionView, SimilarityGroupView, SqliteRepository, VisualMediaFilter, VisualMediaPage,
    VisualMediaQuery, VisualMediaSort,
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
    active_visual_projects: Arc<Mutex<HashSet<String>>>,
    active_intelligence_projects: Arc<Mutex<HashSet<String>>>,
    intelligence_pause_controls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
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

/// Tauri broadcasts are global to the webview. Keep the project identity beside each job update
/// so a background task from another shoot can never paint the current project workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectScopedEvent<T> {
    project_id: String,
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
    let query = CullingQuery {
        mode: query.mode.unwrap_or_else(|| "all_photos".into()),
        filter: query.filter.unwrap_or_else(|| "all".into()),
        group_id: query.group_id,
        limit: query.limit.unwrap_or(60).clamp(1, 120),
        offset: query.offset.unwrap_or(0),
    };
    let repository = state
        .repository
        .lock()
        .map_err(|_| "catalog lock was poisoned".to_owned())?;
    load_culling_workspace(&*repository, &project_id, &query, &state.preview_cache_root)
        .map_err(|error| error.to_string())
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
            let data_directory = app_data_directory(app.handle())?;
            let preview_cache_root = data_directory.join("preview-cache");
            fs::create_dir_all(&preview_cache_root)?;
            app.manage(AppState {
                repository: Arc::new(Mutex::new(repository)),
                catalog_path: data_directory.join("captureos.sqlite3"),
                preview_cache_root,
                active_visual_projects: Arc::new(Mutex::new(HashSet::new())),
                active_intelligence_projects: Arc::new(Mutex::new(HashSet::new())),
                intelligence_pause_controls: Arc::new(Mutex::new(HashMap::new())),
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
            capture_intelligence_summary_command,
            prepare_media_command,
            retry_failed_previews_command,
            start_capture_intelligence_command,
            pause_capture_intelligence_command,
            similarity_group_command,
            save_human_intelligence_decision_command,
            culling_workspace_command,
            culling_progress_command,
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
            active_visual_projects: Arc::new(Mutex::new(HashSet::new())),
            active_intelligence_projects: Arc::new(Mutex::new(HashSet::new())),
            intelligence_pause_controls: Arc::new(Mutex::new(HashMap::new())),
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
