//! Explicit local-project validation for the already-approved M6 Magic Search workflow.
//!
//! This developer tool has deliberately no discovery, download, installation, or output-file
//! option. The caller must name the catalog, CaptureOS application-data root, and one project
//! exactly. It emits a redacted JSON projection to stdout: raw vectors and source-media paths
//! never leave the core service boundary.

use capture_core::{
    find_similar, index_semantic_embeddings, load_semantic_index_status, search_magic,
    FindSimilarRequest, MagicSearchRequest, MagicSearchResponse, SemanticIndexCounts,
    SemanticIndexProgress, SemanticModelIdentityView, SemanticModelStatus, SemanticStorageRoots,
    SiglipProviderCache,
};
use media_model::{AnalysisResourceMode, MediaAssetId, Project};
use persistence::{CatalogRepository, SqliteRepository};
use serde::Serialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FORMAT_VERSION: u32 = 1;
const DEFAULT_RESULT_LIMIT: u32 = 60;

const USAGE: &str = r#"Usage: cargo run -p magic-search-bench --bin m6_project_validate -- \
  --catalog /absolute/path/to/captureos.sqlite3 \
  --app-data-root /absolute/path/to/CaptureOS-data \
  --project "Exact project name" [--index] [--query "local query"]... \
  [--find-similar-asset MEDIA_ASSET_UUID]

This tool never downloads or installs a model. --index explicitly writes only M6-derived
embeddings/index state; every --query records local M6 search history for the exact project."#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    catalog: PathBuf,
    app_data_root: PathBuf,
    project: String,
    index: bool,
    queries: Vec<String>,
    find_similar_asset: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectValidationReport {
    format_version: u32,
    report_kind: &'static str,
    project: ProjectReport,
    mutation_scope: MutationScope,
    model: ModelState,
    index: IndexState,
    queries: Vec<QueryReport>,
    find_similar: Option<FindSimilarReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectReport {
    id: String,
    name: String,
}

/// State-changing work is deliberately explicit and limited to M6-owned derived records.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationScope {
    index_requested: bool,
    index_completed: bool,
    local_search_history_written: bool,
    local_model_provenance_may_be_registered: bool,
    m0_to_m5_data_mutated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelState {
    installed: bool,
    state: &'static str,
    identity: Option<SemanticModelIdentityView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexState {
    state: String,
    stage: String,
    active: bool,
    paused: bool,
    ready: bool,
    embedding_count: u64,
    completed: u64,
    total: u64,
    error_count: u64,
    counts: SemanticIndexCounts,
    /// End-to-end elapsed time for this explicit invocation only. A repeat index may reuse
    /// compatible durable embeddings, so this is never presented as a per-image inference rate.
    elapsed_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryReport {
    /// This is only the explicit text supplied with --query, never metadata-derived text.
    query: String,
    /// Product-path local elapsed time, including applicable catalog/index work for this query.
    elapsed_ms: u64,
    #[serde(flatten)]
    response: SearchResponseReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FindSimilarReport {
    source_asset_id: String,
    /// Product-path local elapsed time, including applicable catalog/index work for this request.
    elapsed_ms: u64,
    #[serde(flatten)]
    response: SearchResponseReport,
}

/// Deliberately redacted response projection. In particular, this omits every URL, selected
/// root, relative path, original-media path, preview location, and raw embedding vector.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponseReport {
    semantic_available: bool,
    semantic_applied: bool,
    identity_search_blocked: bool,
    parsed_filters: Vec<String>,
    total_results: u64,
    has_more: bool,
    results: Vec<SearchResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    asset_id: String,
    filename: String,
    /// This is a local normalized-dot ranking signal, never calibrated confidence or detection.
    semantic_score: Option<f32>,
    explanation: String,
    evidence: Vec<String>,
}

fn main() {
    let options = parse_options();
    match run(options) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Could not serialize M6 project validation report: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("M6 project validation failed: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_options() -> Options {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{USAGE}");
        std::process::exit(0);
    }
    parse_options_from(arguments).unwrap_or_else(|message| exit_usage(&message))
}

fn parse_options_from(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut arguments = arguments.into_iter();
    let mut catalog = None;
    let mut app_data_root = None;
    let mut project = None;
    let mut index = false;
    let mut queries = Vec::new();
    let mut find_similar_asset = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--catalog" => set_once(
                &mut catalog,
                PathBuf::from(next_value(&mut arguments, "--catalog")?),
                "--catalog",
            )?,
            "--app-data-root" => set_once(
                &mut app_data_root,
                PathBuf::from(next_value(&mut arguments, "--app-data-root")?),
                "--app-data-root",
            )?,
            "--project" => set_once(
                &mut project,
                next_value(&mut arguments, "--project")?,
                "--project",
            )?,
            "--index" => {
                if index {
                    return Err("--index may be supplied only once".into());
                }
                index = true;
            }
            "--query" => {
                let query = next_value(&mut arguments, "--query")?;
                if query.trim().is_empty() {
                    return Err("--query must not be blank".into());
                }
                queries.push(query);
            }
            "--find-similar-asset" => {
                let asset_id = next_value(&mut arguments, "--find-similar-asset")?;
                MediaAssetId::try_from(asset_id.as_str())
                    .map_err(|_| "--find-similar-asset must be a MediaAsset UUID".to_owned())?;
                set_once(&mut find_similar_asset, asset_id, "--find-similar-asset")?;
            }
            "--help" | "-h" => return Err("help requested".into()),
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }

    let catalog = catalog.ok_or_else(|| "--catalog is required".to_owned())?;
    let app_data_root = app_data_root.ok_or_else(|| "--app-data-root is required".to_owned())?;
    let project = project.ok_or_else(|| "--project is required".to_owned())?;
    if project.trim().is_empty() {
        return Err("--project must not be blank".into());
    }
    Ok(Options {
        catalog,
        app_data_root,
        project,
        index,
        queries,
        find_similar_asset,
    })
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} needs a value"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, option: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{option} may be supplied only once"));
    }
    *slot = Some(value);
    Ok(())
}

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}. {USAGE}");
    std::process::exit(2);
}

fn run(options: Options) -> Result<ProjectValidationReport, String> {
    validate_existing_file(&options.catalog, "--catalog")?;
    validate_existing_directory(&options.app_data_root, "--app-data-root")?;

    // The normal desktop process owns the same SQLite catalog. The caller should not run the
    // validator concurrently with an active desktop write or index job.
    let repository = SqliteRepository::open(&options.catalog)
        .map_err(|error| format!("could not open the explicit catalog: {error}"))?;
    let project = exact_project(&repository, &options.project)?;
    let model_root = options.app_data_root.join("semantic-models");
    let index_root = options.app_data_root.join("semantic-index");
    let preview_cache_root = options.app_data_root.join("preview-cache");
    let provider_cache = SiglipProviderCache::new(model_root);

    let index_elapsed_ms = if options.index {
        let started = Instant::now();
        index_semantic_embeddings(
            &repository,
            &project.id,
            SemanticStorageRoots {
                preview_cache_root: &preview_cache_root,
                index_root: &index_root,
            },
            &provider_cache,
            AnalysisResourceMode::Balanced,
            || false,
            |_| {},
        )
        .map_err(|error| format!("explicit M6 semantic index operation failed: {error}"))?;
        Some(elapsed_millis(started.elapsed()))
    } else {
        None
    };

    // Status intentionally goes through the product's admission boundary. It never downloads;
    // a missing or invalid pack is reported as unavailable rather than fabricated as usable.
    // Reading it after an optional rebuild reports durable state rather than assumed completion.
    let index_progress =
        load_semantic_index_status(&repository, &project.id, &provider_cache, &index_root)
            .map_err(|error| format!("could not read final M6 semantic state: {error}"))?;

    let queries = options
        .queries
        .iter()
        .map(|query| {
            let started = Instant::now();
            let response = search_magic(
                &repository,
                &project.id,
                &MagicSearchRequest {
                    query: query.clone(),
                    sort: "relevance".into(),
                    descending: true,
                    limit: DEFAULT_RESULT_LIMIT,
                    offset: 0,
                    // M6 project validation always searches the whole selected project. M7
                    // Moment scoping is an optional navigation context and must not narrow or
                    // otherwise change the established MagicSearchBench behavior.
                    moment_id: None,
                },
                &provider_cache,
                &index_root,
                &preview_cache_root,
            )
            .map_err(|error| format!("Magic Search query could not run: {error}"))?;
            Ok(QueryReport {
                query: query.clone(),
                elapsed_ms: elapsed_millis(started.elapsed()),
                response: redact_response(response),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let find_similar = options
        .find_similar_asset
        .as_deref()
        .map(|source_asset_id| {
            let started = Instant::now();
            let asset_id = MediaAssetId::try_from(source_asset_id)
                .map_err(|error| format!("invalid --find-similar-asset: {error}"))?;
            let response = find_similar(
                &repository,
                &project.id,
                FindSimilarRequest {
                    asset_id: &asset_id,
                    limit: DEFAULT_RESULT_LIMIT,
                    offset: 0,
                },
                SemanticStorageRoots {
                    preview_cache_root: &preview_cache_root,
                    index_root: &index_root,
                },
                &provider_cache,
            )
            .map_err(|error| format!("Find Similar could not run: {error}"))?;
            Ok::<FindSimilarReport, String>(FindSimilarReport {
                source_asset_id: source_asset_id.to_owned(),
                elapsed_ms: elapsed_millis(started.elapsed()),
                response: redact_response(response),
            })
        })
        .transpose()?;

    let model = model_state(&index_progress.model);
    Ok(ProjectValidationReport {
        format_version: FORMAT_VERSION,
        report_kind: "captureos.m6.local-project-validation.v1",
        project: ProjectReport {
            id: project.id.to_string(),
            name: project.name,
        },
        mutation_scope: MutationScope {
            index_requested: options.index,
            // `run` returns an error rather than emitting a success report if the explicit
            // index operation did not complete.
            index_completed: options.index,
            local_search_history_written: !options.queries.is_empty(),
            // The product service may upsert this M6 provenance metadata after a successful
            // locally admitted pack; it never installs/downloads a pack.
            local_model_provenance_may_be_registered: model.installed,
            m0_to_m5_data_mutated: false,
        },
        model,
        index: index_state(&index_progress, index_elapsed_ms),
        queries,
        find_similar,
    })
}

fn validate_existing_file(path: &Path, option: &str) -> Result<(), String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(format!("{option} must name an existing file")),
        Err(error) => Err(format!(
            "{option} is not an accessible existing file: {error}"
        )),
    }
}

fn validate_existing_directory(path: &Path, option: &str) -> Result<(), String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!("{option} must name an existing directory")),
        Err(error) => Err(format!(
            "{option} is not an accessible existing directory: {error}"
        )),
    }
}

fn exact_project(repository: &SqliteRepository, name: &str) -> Result<Project, String> {
    let mut matches = repository
        .projects()
        .map_err(|error| format!("could not list projects from the explicit catalog: {error}"))?
        .into_iter()
        .filter(|project| project.name == name);
    let project = matches
        .next()
        .ok_or_else(|| "no project matched --project exactly".to_owned())?;
    if matches.next().is_some() {
        return Err("more than one project matched --project exactly; refusing ambiguity".into());
    }
    Ok(project)
}

fn model_state(status: &SemanticModelStatus) -> ModelState {
    ModelState {
        installed: status.installed,
        state: if status.installed {
            "ready"
        } else {
            "unavailable"
        },
        identity: status.identity.clone(),
    }
}

fn index_state(progress: &SemanticIndexProgress, elapsed_ms: Option<u64>) -> IndexState {
    IndexState {
        state: progress.state.clone(),
        stage: progress.stage.clone(),
        active: progress.active,
        paused: progress.paused,
        ready: progress.index_ready,
        embedding_count: progress.index_embedding_count,
        completed: progress.completed,
        total: progress.total,
        error_count: progress.error_count,
        counts: progress.counts.clone(),
        elapsed_ms,
    }
}

fn elapsed_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn redact_response(response: MagicSearchResponse) -> SearchResponseReport {
    SearchResponseReport {
        semantic_available: response.semantic_available,
        semantic_applied: response.semantic_applied,
        identity_search_blocked: response.identity_search_blocked,
        parsed_filters: response.parsed_filters.chips,
        total_results: response.total_results,
        has_more: response.has_more,
        results: response
            .results
            .into_iter()
            .map(|result| SearchResult {
                asset_id: result.item.asset_id,
                filename: result.item.filename,
                semantic_score: result.semantic_score,
                explanation: result.explanation,
                evidence: result.matched_evidence,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required_arguments() -> Vec<String> {
        [
            "--catalog",
            "/controlled/catalog.sqlite3",
            "--app-data-root",
            "/controlled/app-data",
            "--project",
            "Exact project",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn parser_accepts_repeatable_explicit_queries_and_index() {
        let mut arguments = required_arguments();
        arguments.extend(
            [
                "--index",
                "--query",
                "first arbitrary query",
                "--query",
                "second arbitrary query",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let options = parse_options_from(arguments).expect("explicit arguments parse");
        assert!(options.index);
        assert_eq!(options.queries.len(), 2);
        assert_eq!(options.project, "Exact project");
    }

    #[test]
    fn parser_requires_each_project_locator() {
        let error = parse_options_from(
            ["--catalog", "/controlled/catalog.sqlite3"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap_err();
        assert_eq!(error, "--app-data-root is required");
    }

    #[test]
    fn parser_rejects_an_ambiguous_duplicate_project_option() {
        let mut arguments = required_arguments();
        arguments.extend(
            ["--project", "Other exact project"]
                .into_iter()
                .map(str::to_owned),
        );
        let error = parse_options_from(arguments).unwrap_err();
        assert_eq!(error, "--project may be supplied only once");
    }

    #[test]
    fn parser_rejects_a_non_uuid_find_similar_asset() {
        let mut arguments = required_arguments();
        arguments.extend(
            ["--find-similar-asset", "not-a-media-asset-id"]
                .into_iter()
                .map(str::to_owned),
        );
        let error = parse_options_from(arguments).unwrap_err();
        assert_eq!(error, "--find-similar-asset must be a MediaAsset UUID");
    }

    #[test]
    fn exact_project_lookup_never_falls_back_to_a_partial_or_case_insensitive_name() {
        let repository = SqliteRepository::open_in_memory().expect("temporary catalog opens");
        repository
            .create_project("Exact project")
            .expect("project is stored");

        let exact = exact_project(&repository, "Exact project").expect("exact project matches");
        assert_eq!(exact.name, "Exact project");
        let error = exact_project(&repository, "exact project").unwrap_err();
        assert_eq!(error, "no project matched --project exactly");
    }

    #[test]
    fn redacted_result_projection_has_no_path_or_vector_fields() {
        let result = SearchResult {
            asset_id: "asset-id".into(),
            filename: "visible-filename.jpg".into(),
            semantic_score: Some(0.5),
            explanation: "Local ranking evidence.".into(),
            evidence: vec!["Local image/text embedding similarity".into()],
        };
        let json = serde_json::to_string(&result).expect("safe projection serializes");
        assert!(!json.contains("relativePath"));
        assert!(!json.contains("selectedRoot"));
        assert!(!json.contains("vector"));
        assert!(!json.contains("previewUrl"));
    }
}
