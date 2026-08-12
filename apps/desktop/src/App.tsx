import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save as saveDialog } from "@tauri-apps/plugin-dialog";
import type {
  CaptureIntelligenceProgress,
  CoverageConfirmationState,
  CullingProgress,
  CullingDecision,
  CullingDecisionView,
  CullingFilter,
  CullingMediaRow,
  CullingMode,
  CullingWorkspaceQuery,
  CullingWorkspaceView,
  MomentAnalysisProgress,
  MomentChecklistView,
  MomentDetailView,
  MomentSearchRequest,
  MomentSearchResponse,
  MomentSummaryView,
  MomentTimelineView,
  FaceAnalysisEvidence,
  IngestJobSummary,
  IngestPolicy,
  IngestPreflightView,
  IngestReport,
  IngestRequest,
  IndexedMediaRow,
  JobView,
  MagicSearchHistoryEntry,
  MagicSearchRequest,
  MagicSearchResponse,
  MagicSearchResult,
  MagicSearchSort,
  MediaAssetDetail,
  MetadataRefreshProgress,
  MediaPreparationProgress,
  MediaFilter,
  ProjectHome,
  ProjectLibraryItem,
  ProjectView,
  ProductionExportProgress,
  ProductionPlanInput,
  ProductionPlanPreview,
  ProductionPlanType,
  ProductionWorkspaceView,
  ExportManifestView,
  ProductionPreflight,
  VirtualCollectionInput,
  SemanticIndexProgress,
  SemanticResourceMode,
  SimilarityGroupView,
  StudioBrainProgress,
  StudioBrainProjectStatus,
  VisualMediaFilter,
  VisualMediaPage,
  VisualMediaQuery,
  VisualMediaRow,
  VisualMediaSort,
} from "@captureos/contracts";
import { StatusCard } from "@captureos/ui";
import { type CSSProperties, type FormEvent, type ReactNode, type RefObject, useCallback, useEffect, useRef, useState } from "react";

const pageSize = 250;
const visualPageSize = 120;
const magicSearchPageSize = 120;
const filters: { id: MediaFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "photos", label: "Photos" },
  { id: "video", label: "Video" },
  { id: "audio", label: "Audio" },
  { id: "sidecars", label: "Sidecars" },
  { id: "unknown", label: "Unknown" },
];

type ProjectSurface = "media" | "ingest" | "cull" | "timeline" | "studio" | "production";
type AppRoute = { kind: "home" } | { kind: "project"; projectId: string; surface: ProjectSurface; momentId?: string };
type ProjectScopedEvent<T> = { projectId: string; progress: T };
type StudioProfileScopedEvent<T> = { projectId: string; profileId: string; progress: T };
type ProductionScopedEvent<T> = { projectId: string; manifestId: string; progress: T };

function routeHash(route: AppRoute) {
  if (route.kind === "home") return "#/";
  const momentSuffix = route.momentId ? `/${encodeURIComponent(route.momentId)}` : "";
  const suffix = route.surface === "ingest"
    ? "/ingest"
    : route.surface === "cull"
      ? `/cull${momentSuffix}`
      : route.surface === "timeline"
        ? `/timeline${momentSuffix}`
        : route.surface === "studio"
          ? "/studio"
          : route.surface === "production"
            ? "/production"
        : "";
  return `#/project/${encodeURIComponent(route.projectId)}${suffix}`;
}

function readRoute(): AppRoute {
  const segments = window.location.hash.replace(/^#\/?/, "").split("/").filter(Boolean);
  if (segments[0] !== "project" || !segments[1]) return { kind: "home" };
  const surface: ProjectSurface = segments[2] === "ingest"
    ? "ingest"
    : segments[2] === "cull"
      ? "cull"
      : segments[2] === "timeline"
        ? "timeline"
        : segments[2] === "studio"
          ? "studio"
          : segments[2] === "production"
            ? "production"
        : "media";
  return {
    kind: "project",
    projectId: decodeURIComponent(segments[1]),
    surface,
    ...(surface === "cull" || surface === "timeline"
      ? { momentId: segments[3] ? decodeURIComponent(segments[3]) : undefined }
      : {}),
  };
}

function useCaptureOsNavigation() {
  const [route, setRoute] = useState<AppRoute>({ kind: "home" });

  useEffect(() => {
    // Home is the deliberate startup destination. Project routes are user navigation history,
    // never an implicit "reopen the last project" preference.
    window.history.replaceState({ captureosRoute: true }, "", routeHash({ kind: "home" }));
    const syncFromHistory = () => setRoute(readRoute());
    window.addEventListener("popstate", syncFromHistory);
    window.addEventListener("hashchange", syncFromHistory);
    return () => {
      window.removeEventListener("popstate", syncFromHistory);
      window.removeEventListener("hashchange", syncFromHistory);
    };
  }, []);

  const navigate = useCallback((next: AppRoute, replace = false) => {
    const operation = replace ? "replaceState" : "pushState";
    window.history[operation]({ captureosRoute: true }, "", routeHash(next));
    setRoute(next);
  }, []);

  return { route, navigate };
}

export function App() {
  const { route, navigate } = useCaptureOsNavigation();
  const [projects, setProjects] = useState<ProjectLibraryItem[]>([]);
  const [home, setHome] = useState<ProjectHome | null>(null);
  const [cullingProgress, setCullingProgress] = useState<CullingProgress | null>(null);
  const [filter, setFilter] = useState<MediaFilter>("all");
  const [liveJob, setLiveJob] = useState<JobView | null>(null);
  const [ingestHistory, setIngestHistory] = useState<IngestJobSummary[]>([]);
  const [ingestReport, setIngestReport] = useState<IngestReport | null>(null);
  const [isIngesting, setIsIngesting] = useState(false);
  const [hasMoreMedia, setHasMoreMedia] = useState(false);
  const [isLoadingMore, setIsLoadingMore] = useState(false);
  const [isIndexing, setIsIndexing] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [showCreateProject, setShowCreateProject] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const activeHome = route.kind === "project" && home?.project.id === route.projectId ? home : null;
  const project = route.kind === "project"
    ? activeHome?.project ?? projects.find((item) => item.id === route.projectId) ?? null
    : null;
  const activeProjectId = route.kind === "project" ? route.projectId : null;

  const loadProjectLibrary = useCallback(async () => {
    const library = await invoke<ProjectLibraryItem[]>("project_library");
    setProjects(library);
  }, []);

  const loadHome = useCallback(
    async (projectId: string, nextFilter: MediaFilter) => {
      const nextHome = await invoke<ProjectHome>("project_home", {
        projectId,
        filter: nextFilter,
        offset: 0,
        limit: pageSize,
      });
      setHome(nextHome);
      setLiveJob(nextHome.latestJob);
      setHasMoreMedia(nextHome.media.length === pageSize);
    },
    [],
  );

  useEffect(() => {
    if (route.kind === "home") void loadProjectLibrary().catch(toError(setError));
  }, [loadProjectLibrary, route.kind]);

  useEffect(() => {
    if (route.kind === "home") {
      setHome(null);
      setCullingProgress(null);
      setLiveJob(null);
      setIngestHistory([]);
      setIngestReport(null);
      setIsIndexing(false);
      setIsIngesting(false);
      return;
    }
    let cancelled = false;
    void Promise.all([
      invoke<ProjectHome>("project_home", { projectId: route.projectId, filter, offset: 0, limit: pageSize }),
      invoke<IngestJobSummary[]>("ingest_history_command", { projectId: route.projectId }),
      invoke<CullingProgress>("culling_progress_command", { projectId: route.projectId }),
    ])
      .then(([nextHome, history, nextCullingProgress]) => {
        if (cancelled) return;
        setHome(nextHome);
        setLiveJob(nextHome.latestJob);
        setHasMoreMedia(nextHome.media.length === pageSize);
        setIngestHistory(history);
        setCullingProgress(nextCullingProgress);
        setIngestReport(null);
        setIsIndexing(false);
        setIsIngesting(false);
      })
      .catch(toError(setError));
    return () => { cancelled = true; };
  }, [filter, route]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<JobView>>("index-progress", ({ payload }) => {
      if (payload.projectId !== activeProjectId) return;
      setLiveJob(payload.progress);
      setIsIndexing(payload.progress.state === "running");
    }).then((stop) => {
      unlisten = stop;
    });
    return () => unlisten?.();
  }, [activeProjectId]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<IngestReport>>("ingest-progress", ({ payload }) => {
      if (payload.projectId !== activeProjectId) return;
      setIngestReport(payload.progress);
      setIsIngesting(payload.progress.job.state === "running");
    }).then((stop) => {
      unlisten = stop;
    });
    return () => unlisten?.();
  }, [activeProjectId]);

  async function createProject(event: FormEvent) {
    event.preventDefault();
    if (!newProjectName.trim()) return;
    try {
      const created = await invoke<ProjectView>("create_project", {
        name: newProjectName.trim(),
      });
      setNewProjectName("");
      setShowCreateProject(false);
      await loadProjectLibrary();
      navigate({ kind: "project", projectId: created.id, surface: "media" });
    } catch (reason) {
      toError(setError)(reason);
    }
  }

  async function selectFolderAndIndex() {
    if (!project || isIndexing) return;
    const projectId = project.id;
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Index a local media folder",
      });
      if (typeof selected !== "string") return;
      setError(null);
      setIsIndexing(true);
      const job = await invoke<JobView>("index_folder", {
        projectId,
        selectedPath: selected,
      });
      const currentRoute = readRoute();
      if (currentRoute.kind === "project" && currentRoute.projectId === projectId) {
        setLiveJob(job);
        await loadHome(projectId, filter);
        await loadProjectLibrary();
      }
    } catch (reason) {
      toError(setError)(reason);
    } finally {
      setIsIndexing(false);
    }
  }

  async function changeFilter(nextFilter: MediaFilter) {
    setFilter(nextFilter);
    if (!project) return;
    try {
      await loadHome(project.id, nextFilter);
    } catch (reason) {
      toError(setError)(reason);
    }
  }

  async function loadMoreMedia() {
    if (!project || !home || !hasMoreMedia || isLoadingMore) return;
    setIsLoadingMore(true);
    try {
      const nextHome = await invoke<ProjectHome>("project_home", {
        projectId: project.id,
        filter,
        offset: home.media.length,
        limit: pageSize,
      });
      setHome((current) =>
        current
          ? { ...nextHome, media: [...current.media, ...nextHome.media] }
          : nextHome,
      );
      setHasMoreMedia(nextHome.media.length === pageSize);
    } catch (reason) {
      toError(setError)(reason);
    } finally {
      setIsLoadingMore(false);
    }
  }

  return (
    <main className="shell">
      <header className="global-topbar">
        <button className="brand" onClick={() => navigate({ kind: "home" })}>CAPTUREOS</button>
        {project ? <div className="project-crumb"><button onClick={() => navigate({ kind: "home" })}>All Projects</button><span>/</span><strong>{project.name}</strong></div> : <span className="topbar-context">Project Library</span>}
        <div className="topbar-actions">
          <button className="topbar-placeholder" disabled title="Storage overview is the next global foundation">Storage</button>
          <button className="topbar-placeholder" disabled title="App settings are the next global foundation">Settings</button>
          <button className="primary" onClick={() => setShowCreateProject(true)}>New Project</button>
        </div>
      </header>

      {error ? <p className="error" role="alert">{error}</p> : null}
      {showCreateProject ? <ProjectCreation onCancel={() => setShowCreateProject(false)} name={newProjectName} onName={setNewProjectName} onSubmit={createProject} /> : null}
      {route.kind === "home" ? <ProjectLibrary projects={projects} onOpen={(projectId) => navigate({ kind: "project", projectId, surface: "media" })} onCreate={() => setShowCreateProject(true)} /> : null}
      {project && route.kind === "project" ? <ProjectHeader project={project} projects={projects} surface={route.surface} isIndexing={isIndexing} isIngesting={isIngesting} onHome={() => navigate({ kind: "home" })} onOpen={(projectId) => navigate({ kind: "project", projectId, surface: "media" })} onIndex={() => void selectFolderAndIndex()} onIngest={() => navigate({ kind: "project", projectId: project.id, surface: "ingest" })} onCull={() => navigate({ kind: "project", projectId: project.id, surface: "cull" })} onTimeline={() => navigate({ kind: "project", projectId: project.id, surface: "timeline" })} onStudio={() => navigate({ kind: "project", projectId: project.id, surface: "studio" })} onProduction={() => navigate({ kind: "project", projectId: project.id, surface: "production" })} /> : null}
      {project && route.kind === "project" && route.surface === "media" && activeHome ? <ProjectWorkspace key={project.id} home={activeHome} cullingProgress={cullingProgress} job={liveJob} filter={filter} onFilter={changeFilter} hasMoreMedia={hasMoreMedia} isLoadingMore={isLoadingMore} onLoadMore={loadMoreMedia} onIndex={() => void selectFolderAndIndex()} onIngest={() => navigate({ kind: "project", projectId: project.id, surface: "ingest" })} onCull={() => navigate({ kind: "project", projectId: project.id, surface: "cull" })} onMoments={() => navigate({ kind: "project", projectId: project.id, surface: "timeline" })} onStudio={() => navigate({ kind: "project", projectId: project.id, surface: "studio" })} onProduction={() => navigate({ kind: "project", projectId: project.id, surface: "production" })} /> : null}
      {project && route.kind === "project" && route.surface === "ingest" ? <IngestWorkspace key={project.id} project={project} history={ingestHistory} report={ingestReport} isIngesting={isIngesting} onReport={setIngestReport} onHistory={setIngestHistory} onIngesting={setIsIngesting} onError={setError} /> : null}
      {project && route.kind === "project" && route.surface === "cull" ? <CullingWorkspace key={`${project.id}:${route.momentId ?? "all"}`} project={project} momentId={route.momentId} onReturn={() => navigate({ kind: "project", projectId: project.id, surface: route.momentId ? "timeline" : "media", ...(route.momentId ? { momentId: route.momentId } : {}) })} onError={setError} /> : null}
      {project && route.kind === "project" && route.surface === "timeline" ? <MomentTimelineWorkspace key={`${project.id}:${route.momentId ?? "timeline"}`} project={project} initialMomentId={route.momentId} onReturn={() => navigate({ kind: "project", projectId: project.id, surface: "media" })} onShowTimeline={() => navigate({ kind: "project", projectId: project.id, surface: "timeline" })} onOpenMoment={(momentId) => navigate({ kind: "project", projectId: project.id, surface: "timeline", momentId })} onCullMoment={(momentId) => navigate({ kind: "project", projectId: project.id, surface: "cull", momentId })} onError={setError} /> : null}
      {project && route.kind === "project" && route.surface === "studio" ? <StudioBrainWorkspace key={project.id} project={project} onReturn={() => navigate({ kind: "project", projectId: project.id, surface: "media" })} /> : null}
      {project && route.kind === "project" && route.surface === "production" ? <ProductionWorkspace key={project.id} project={project} onReturn={() => navigate({ kind: "project", projectId: project.id, surface: "media" })} /> : null}
    </main>
  );
}

function ProjectCreation({ onCancel, name, onName, onSubmit }: { onCancel: () => void; name: string; onName: (name: string) => void; onSubmit: (event: FormEvent) => void }) {
  return <div className="project-dialog-backdrop" role="presentation"><section className="project-dialog" role="dialog" aria-modal="true" aria-labelledby="new-project-heading"><button className="dialog-close" aria-label="Close new project" onClick={onCancel}>×</button><p className="eyebrow">NEW PROJECT</p><h2 id="new-project-heading">Start a new shoot</h2><p className="muted">Only a name is needed. CaptureOS keeps each project separate in the local catalog.</p><form className="project-form" onSubmit={onSubmit}><label htmlFor="project-name">Project name</label><div><input autoFocus id="project-name" value={name} onChange={(event) => onName(event.target.value)} placeholder="e.g. AI Test" /><button className="primary" type="submit">Create project</button></div></form></section></div>;
}

function ProjectLibrary({ projects, onOpen, onCreate }: { projects: ProjectLibraryItem[]; onOpen: (projectId: string) => void; onCreate: () => void }) {
  return <section className="project-library"><div className="library-hero"><div><p className="eyebrow">CAPTUREOS</p><h1>Your shoots</h1><p className="lede">A local library for every production. Choose a project, or start a clean one.</p></div><button className="primary library-create" onClick={onCreate}>New Project</button></div><div className="library-heading"><h2>Recent projects</h2><small>{projects.length === 1 ? "1 project" : `${projects.length} projects`}</small></div>{projects.length ? <div className="project-cards">{projects.map((item) => <button className="project-card" key={item.id} onClick={() => onOpen(item.id)} aria-label={`Open ${item.name}`}><span className="project-cover" aria-hidden="true">COS</span><strong>{item.name}</strong><span className="project-card-meta"><span>Media <b>{item.mediaAssetCount}</b></span><span>{item.storageVolumeCount ? `${item.storageVolumeCount} storage volume${item.storageVolumeCount === 1 ? "" : "s"}` : "No storage indexed"}</span><span>Protection {protectionLabel(item.protectionState)}</span></span><small>Last activity {formatProjectDate(item.lastActivityAt)}</small></button>)}</div> : <StatusCard><div className="project-empty"><p className="eyebrow">YOUR SHOOT STARTS HERE</p><h2>There are no projects yet.</h2><p className="muted">Index existing media or ingest camera cards after you create your first project.</p><button className="primary" onClick={onCreate}>New Project</button></div></StatusCard>}</section>;
}

function ProjectHeader({ project, projects, surface, isIndexing, isIngesting, onHome, onOpen, onIndex, onIngest, onCull, onTimeline, onStudio, onProduction }: { project: ProjectView; projects: ProjectLibraryItem[]; surface: ProjectSurface; isIndexing: boolean; isIngesting: boolean; onHome: () => void; onOpen: (projectId: string) => void; onIndex: () => void; onIngest: () => void; onCull: () => void; onTimeline: () => void; onStudio: () => void; onProduction: () => void }) {
  const lede = surface === "ingest"
    ? "Safe ingest remains attached to this project."
    : surface === "cull"
      ? "A local, non-destructive workspace for your human review decisions."
    : surface === "timeline"
      ? "A local structural timeline. Suggested labels are evidence-grounded; your edits remain authoritative."
      : surface === "studio"
        ? "Local, explainable preference modeling from your explicit human decisions."
        : "Index and analyze local media in this selected project.";
  return <section className="project-header"><div><button className="back-link" onClick={onHome}>← All Projects</button><p className="eyebrow">PROJECT WORKSPACE</p><h1>{project.name}</h1><p className="lede">{lede}</p></div><div className="project-actions"><select aria-label="Switch project" value={project.id} onChange={(event) => onOpen(event.target.value)}>{projects.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select><button className={surface === "timeline" ? "primary" : "secondary"} onClick={onTimeline}>Moments</button><button className={surface === "cull" ? "primary" : "secondary"} onClick={onCull}>Smart Cull</button><button className={surface === "studio" ? "primary" : "secondary"} onClick={onStudio}>Studio Brain</button><button className={surface === "production" ? "primary" : "secondary"} onClick={onProduction}>Production</button><button className={surface === "ingest" ? "primary" : "secondary"} disabled={isIngesting} onClick={onIngest}>Ingest Shoot</button><button className={surface === "media" ? "primary" : "secondary"} disabled={isIndexing} onClick={onIndex}>{isIndexing ? "Indexing…" : "Index Folder"}</button></div></section>;
}

function formatProjectDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? "recorded locally" : new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", year: "numeric" }).format(date);
}

function protectionLabel(value: string) {
  if (value === "verified_copy_history") return "verified copy history";
  if (value === "ingest_history_recorded") return "ingest history recorded";
  return "not recorded";
}

function ProjectWorkspace({ home, cullingProgress, job, filter, onFilter, hasMoreMedia, isLoadingMore, onLoadMore, onIndex, onIngest, onCull, onMoments, onStudio, onProduction }: { home: ProjectHome; cullingProgress: CullingProgress | null; job: JobView | null; filter: MediaFilter; onFilter: (filter: MediaFilter) => void; hasMoreMedia: boolean; isLoadingMore: boolean; onLoadMore: () => void; onIndex: () => void; onIngest: () => void; onCull: () => void; onMoments: () => void; onStudio: () => void; onProduction: () => void }) {
  const [visual, setVisual] = useState<VisualMediaPage | null>(null);
  const [visualFilter, setVisualFilter] = useState<VisualMediaFilter>("all");
  const [visualSort, setVisualSort] = useState<VisualMediaSort>("captureTime");
  const [descending, setDescending] = useState(false);
  const [search, setSearch] = useState("");
  const [cameraModel, setCameraModel] = useState("");
  const [lensModel, setLensModel] = useState("");
  const [density, setDensity] = useState<"small" | "medium" | "large">(() => (localStorage.getItem("captureos-grid-density") as "small" | "medium" | "large" | null) ?? "medium");
  const [view, setView] = useState<"grid" | "list">("grid");
  const [preparation, setPreparation] = useState<MediaPreparationProgress | null>(null);
  const [metadataRefreshProgress, setMetadataRefreshProgress] = useState<MetadataRefreshProgress | null>(null);
  const [isRefreshingMetadata, setIsRefreshingMetadata] = useState(false);
  const [metadataRefreshError, setMetadataRefreshError] = useState<string | null>(null);
  const [intelligenceProgress, setIntelligenceProgress] = useState<CaptureIntelligenceProgress | null>(null);
  const [analysisResourceMode, setAnalysisResourceMode] = useState<"eco" | "balanced" | "fast">("balanced");
  const [isStartingAnalysis, setIsStartingAnalysis] = useState(false);
  const [isPausingAnalysis, setIsPausingAnalysis] = useState(false);
  const [intelligenceError, setIntelligenceError] = useState<string | null>(null);
  const [selected, setSelected] = useState<MediaAssetDetail | null>(null);
  const [similarityGroup, setSimilarityGroup] = useState<SimilarityGroupView | null>(null);
  const [isLoadingSimilarityGroup, setIsLoadingSimilarityGroup] = useState(false);
  const [isSavingDecision, setIsSavingDecision] = useState(false);
  const [viewerOpen, setViewerOpen] = useState(false);
  const [showInspector, setShowInspector] = useState(true);
  const [showTechnical, setShowTechnical] = useState(false);
  const [isLoadingVisual, setIsLoadingVisual] = useState(false);
  const [semanticIndexProgress, setSemanticIndexProgress] = useState<SemanticIndexProgress | null>(null);
  const [semanticResourceMode, setSemanticResourceMode] = useState<SemanticResourceMode>("balanced");
  const [isStartingSemanticIndex, setIsStartingSemanticIndex] = useState(false);
  const [isPausingSemanticIndex, setIsPausingSemanticIndex] = useState(false);
  const [magicSearchOpen, setMagicSearchOpen] = useState(true);
  const [magicSearchQuery, setMagicSearchQuery] = useState("");
  const [magicSearchSort, setMagicSearchSort] = useState<MagicSearchSort>("relevance");
  const [magicSearchDescending, setMagicSearchDescending] = useState(true);
  const [magicSearchResponse, setMagicSearchResponse] = useState<MagicSearchResponse | null>(null);
  const [magicSearchMode, setMagicSearchMode] = useState<"query" | "similar">("query");
  // The reference asset is retained only while paginating Find Similar. It is never sent with a
  // text search, which prevents a later page from accidentally switching search modes.
  const [similarReferenceAssetId, setSimilarReferenceAssetId] = useState<string | null>(null);
  const [magicSearchHistory, setMagicSearchHistory] = useState<MagicSearchHistoryEntry[]>([]);
  const [isSearchingMagic, setIsSearchingMagic] = useState(false);
  const [magicSearchError, setMagicSearchError] = useState<string | null>(null);
  const magicSearchInputRef = useRef<HTMLInputElement>(null!);

  const query = useCallback((offset = 0): VisualMediaQuery => ({ filter: visualFilter, sort: visualSort, descending, search: search.trim() || undefined, cameraModel: cameraModel.trim() || undefined, lensModel: lensModel.trim() || undefined, limit: visualPageSize, offset }), [visualFilter, visualSort, descending, search, cameraModel, lensModel]);
  const loadVisual = useCallback(async (offset = 0, append = false, prepare = true, restoreSummary = true) => {
    setIsLoadingVisual(true);
    try {
      const pagePromise = invoke<VisualMediaPage>("visual_media_page", { projectId: home.project.id, query: query(offset) });
      const summaryPromise = restoreSummary
        ? invoke<MediaPreparationProgress | null>("visual_preparation_summary_command", { projectId: home.project.id })
        : Promise.resolve(null);
      const [page, summary] = await Promise.all([pagePromise, summaryPromise]);
      setVisual((current) => append && current ? { ...page, items: [...current.items, ...page.items] } : page);
      if (restoreSummary) setPreparation((current) => current?.state === "running" ? current : summary);
      if (prepare) void invoke<MediaPreparationProgress>("prepare_media_command", { projectId: home.project.id, query: query(offset) }).catch(() => undefined);
    } finally {
      setIsLoadingVisual(false);
    }
  }, [home.project.id, query]);

  const loadIntelligenceSummary = useCallback(async () => {
    const summary = await invoke<CaptureIntelligenceProgress | null>("capture_intelligence_summary_command", { projectId: home.project.id });
    if (isAnalysisResourceMode(summary?.resourceMode)) setAnalysisResourceMode(summary.resourceMode);
    setIntelligenceProgress((current) => current?.state === "running" ? current : summary);
  }, [home.project.id]);

  const loadSemanticIndexStatus = useCallback(async () => {
    const status = await invoke<SemanticIndexProgress | null>("semantic_index_status_command", { projectId: home.project.id });
    if (isSemanticResourceMode(status?.resourceMode)) setSemanticResourceMode(status.resourceMode);
    setSemanticIndexProgress((current) => current?.active ? current : status);
  }, [home.project.id]);

  const loadMagicSearchHistory = useCallback(async () => {
    const history = await invoke<MagicSearchHistoryEntry[]>("magic_search_history_command", { projectId: home.project.id, limit: 8 });
    setMagicSearchHistory(history);
  }, [home.project.id]);

  useEffect(() => { void loadVisual(); }, [loadVisual]);
  useEffect(() => { void loadIntelligenceSummary().catch((reason) => setIntelligenceError(toMessage(reason))); }, [loadIntelligenceSummary]);
  useEffect(() => { void loadSemanticIndexStatus().catch((reason) => setMagicSearchError(toMessage(reason))); }, [loadSemanticIndexStatus]);
  useEffect(() => { void loadMagicSearchHistory().catch((reason) => setMagicSearchError(toMessage(reason))); }, [loadMagicSearchHistory]);
  useEffect(() => { localStorage.setItem("captureos-grid-density", density); }, [density]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<MediaPreparationProgress>>("media-preparation-progress", ({ payload }) => {
      if (payload.projectId !== home.project.id) return;
      setPreparation(payload.progress);
      if (payload.progress.state === "completed") void loadVisual(0, false, false);
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [home.project.id, loadVisual]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<MetadataRefreshProgress>>("metadata-refresh-progress", ({ payload }) => {
      if (payload.projectId !== home.project.id) return;
      setMetadataRefreshProgress(payload.progress);
      setIsRefreshingMetadata(metadataRefreshIsActive(payload.progress));
      if (payload.progress.state === "completed") {
        void loadVisual(0, false, false);
      }
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [home.project.id, loadVisual]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<CaptureIntelligenceProgress>>("capture-intelligence-progress", ({ payload }) => {
      if (payload.projectId !== home.project.id) return;
      if (isAnalysisResourceMode(payload.progress.resourceMode)) setAnalysisResourceMode(payload.progress.resourceMode);
      setIntelligenceProgress(payload.progress);
      if (payload.progress.state === "completed" || payload.progress.state === "paused") {
        void loadVisual(0, false, false);
      }
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [home.project.id, loadVisual]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<SemanticIndexProgress>>("semantic-index-progress", ({ payload }) => {
      if (payload.projectId !== home.project.id) return;
      if (isSemanticResourceMode(payload.progress.resourceMode)) setSemanticResourceMode(payload.progress.resourceMode);
      setSemanticIndexProgress(payload.progress);
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [home.project.id]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k" && !isTypingElement(event.target)) {
        event.preventDefault();
        setMagicSearchOpen(true);
        window.requestAnimationFrame(() => magicSearchInputRef.current?.focus());
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  async function openMediaAsset(assetId: string) {
    const detail = await invoke<MediaAssetDetail | null>("media_asset_detail_command", { projectId: home.project.id, assetId });
    if (detail) {
      setSelected(detail);
      setViewerOpen(true);
    }
  }

  async function openMedia(item: VisualMediaRow) {
    await openMediaAsset(item.assetId);
  }

  async function clearCache() {
    await invoke("clear_preview_cache_command", { projectId: home.project.id });
    setPreparation(null);
    setSelected(null);
    setViewerOpen(false);
    setVisual(null);
    await loadVisual(0, false, true, false);
  }

  async function retryFailedPreviews() {
    await invoke<MediaPreparationProgress>("retry_failed_previews_command", { projectId: home.project.id });
    await loadVisual(0, false, false);
  }

  async function refreshCaptureMetadata() {
    if (isRefreshingMetadata || metadataRefreshIsActive(metadataRefreshProgress)) return;
    try {
      setMetadataRefreshError(null);
      setIsRefreshingMetadata(true);
      const progress = await invoke<MetadataRefreshProgress>("refresh_metadata_command", { projectId: home.project.id });
      setMetadataRefreshProgress(progress);
    } catch (reason) {
      setMetadataRefreshError(toMessage(reason));
    } finally {
      setIsRefreshingMetadata(false);
    }
  }

  async function startCaptureIntelligence() {
    if (isStartingAnalysis || intelligenceProgress?.state === "running") return;
    try {
      setIntelligenceError(null);
      setIsStartingAnalysis(true);
      const progress = await invoke<CaptureIntelligenceProgress>("start_capture_intelligence_command", {
        projectId: home.project.id,
        resourceMode: analysisResourceMode,
      });
      setIntelligenceProgress(progress);
      await loadVisual(0, false, false);
      if (selected) await refreshSelected(selected.item.assetId);
    } catch (reason) {
      setIntelligenceError(toMessage(reason));
    } finally {
      setIsStartingAnalysis(false);
    }
  }

  async function pauseCaptureIntelligence() {
    if (isPausingAnalysis || intelligenceProgress?.state !== "running") return;
    try {
      setIntelligenceError(null);
      setIsPausingAnalysis(true);
      await invoke("pause_capture_intelligence_command", { projectId: home.project.id });
    } catch (reason) {
      setIntelligenceError(toMessage(reason));
    } finally {
      setIsPausingAnalysis(false);
    }
  }

  async function startSemanticIndex() {
    if (isStartingSemanticIndex || semanticIndexProgress?.active) return;
    if (semanticIndexProgress?.model.installed === false) {
      setMagicSearchError(semanticIndexProgress.model.message ?? "Install an approved local semantic model pack before indexing photos.");
      return;
    }
    try {
      setMagicSearchError(null);
      setIsStartingSemanticIndex(true);
      const progress = await invoke<SemanticIndexProgress>("start_semantic_index_command", {
        projectId: home.project.id,
        resourceMode: semanticResourceMode,
      });
      setSemanticIndexProgress(progress);
    } catch (reason) {
      setMagicSearchError(toMessage(reason));
    } finally {
      setIsStartingSemanticIndex(false);
    }
  }

  async function pauseSemanticIndex() {
    if (isPausingSemanticIndex || !semanticIndexProgress?.active) return;
    try {
      setMagicSearchError(null);
      setIsPausingSemanticIndex(true);
      await invoke("pause_semantic_index_command", { projectId: home.project.id });
    } catch (reason) {
      setMagicSearchError(toMessage(reason));
    } finally {
      setIsPausingSemanticIndex(false);
    }
  }

  async function runMagicSearch(options: { query?: string; offset?: number; append?: boolean } = {}) {
    const queryText = (options.query ?? magicSearchQuery).trim();
    if (!queryText) {
      setMagicSearchResponse(null);
      setMagicSearchMode("query");
      return;
    }
    try {
      setMagicSearchError(null);
      setIsSearchingMagic(true);
      const request: MagicSearchRequest = {
        query: queryText,
        sort: magicSearchSort,
        descending: magicSearchDescending,
        limit: magicSearchPageSize,
        offset: options.offset ?? 0,
      };
      const response = await invoke<MagicSearchResponse>("magic_search_command", { projectId: home.project.id, request });
      setMagicSearchQuery(queryText);
      setMagicSearchMode("query");
      setSimilarReferenceAssetId(null);
      setMagicSearchResponse((current) => options.append && current
        ? { ...response, results: [...current.results, ...response.results] }
        : response);
      void loadMagicSearchHistory().catch((reason) => setMagicSearchError(toMessage(reason)));
    } catch (reason) {
      setMagicSearchError(toMessage(reason));
    } finally {
      setIsSearchingMagic(false);
    }
  }

  async function findSimilar(assetId: string, options: { offset?: number; append?: boolean } = {}) {
    try {
      setMagicSearchError(null);
      setIsSearchingMagic(true);
      const response = await invoke<MagicSearchResponse>("find_similar_command", {
        projectId: home.project.id,
        assetId,
        limit: magicSearchPageSize,
        offset: options.offset ?? 0,
      });
      setMagicSearchMode("similar");
      setSimilarReferenceAssetId(assetId);
      setMagicSearchResponse((current) => options.append && current
        ? { ...response, results: [...current.results, ...response.results] }
        : response);
    } catch (reason) {
      setMagicSearchError(toMessage(reason));
    } finally {
      setIsSearchingMagic(false);
    }
  }

  async function clearMagicSearchHistory() {
    try {
      setMagicSearchError(null);
      await invoke("clear_magic_search_history_command", { projectId: home.project.id });
      setMagicSearchHistory([]);
    } catch (reason) {
      setMagicSearchError(toMessage(reason));
    }
  }

  function clearMagicSearch() {
    setMagicSearchQuery("");
    setMagicSearchResponse(null);
    setMagicSearchMode("query");
    setSimilarReferenceAssetId(null);
    setMagicSearchError(null);
    magicSearchInputRef.current?.focus();
  }

  async function refreshSelected(assetId: string) {
    const detail = await invoke<MediaAssetDetail | null>("media_asset_detail_command", { projectId: home.project.id, assetId });
    if (detail) setSelected(detail);
  }

  async function openSimilarityGroup(assetId: string) {
    try {
      setIntelligenceError(null);
      setIsLoadingSimilarityGroup(true);
      const group = await invoke<SimilarityGroupView | null>("similarity_group_command", { projectId: home.project.id, assetId, limit: 24 });
      if (!group) {
        setIntelligenceError("No similar-frame group is stored for this media yet.");
        return;
      }
      setSimilarityGroup(group);
    } catch (reason) {
      setIntelligenceError(toMessage(reason));
    } finally {
      setIsLoadingSimilarityGroup(false);
    }
  }

  async function saveHumanDecision(assetId: string, decision: "keep" | "review" | "reject") {
    try {
      setIntelligenceError(null);
      setIsSavingDecision(true);
      await invoke("save_human_intelligence_decision_command", { projectId: home.project.id, input: { assetId, decision } });
      setSelected((current) => current?.item.assetId === assetId && current.intelligence
        ? { ...current, intelligence: { ...current.intelligence, humanDecision: decision } }
        : current);
    } catch (reason) {
      setIntelligenceError(toMessage(reason));
    } finally {
      setIsSavingDecision(false);
    }
  }

  const activeVisualItems = magicSearchResponse ? magicSearchResponse.results.map((result) => result.item) : visual?.items ?? [];
  const selectedIndex = selected ? activeVisualItems.findIndex((item) => item.assetId === selected.item.assetId) : -1;
  const navigate = (direction: number) => {
    if (selectedIndex < 0) return;
    const next = activeVisualItems[selectedIndex + direction];
    if (next) void openMedia(next);
  };

  return (
    <div className="workspace visual-workspace">
      {home.summary.mediaAssets === 0 ? <section className="project-first-step"><p className="eyebrow">YOUR SHOOT STARTS HERE</p><h2>Add the first media to this project.</h2><p className="muted">Index existing folders without changing originals, or ingest camera cards into verified destinations.</p><div><button className="primary" onClick={onIndex}>Index Existing Media</button><button className="secondary" onClick={onIngest}>Ingest Shoot</button></div></section> : null}
      <section className="metrics" aria-label="Project counts">
        <Metric label="Media assets" value={home.summary.mediaAssets} />
        <Metric label="File instances" value={home.summary.fileInstances} />
        <Metric label="Storage volumes" value={home.summary.storageVolumes} />
        <Metric label="Preview cache" value={formatSize(visual?.cacheBytes ?? 0)} />
      </section>

      <CaptureIntelligenceControls
        progress={intelligenceProgress}
        resourceMode={analysisResourceMode}
        isStarting={isStartingAnalysis}
        isPausing={isPausingAnalysis}
        onResourceMode={setAnalysisResourceMode}
        onStart={() => void startCaptureIntelligence()}
        onPause={() => void pauseCaptureIntelligence()}
      />
      <section className="culling-entry" aria-label="Capture-time metadata refresh">
        <div>
          <p className="section-label">Capture-time metadata</p>
          <h2>Refresh camera capture times</h2>
          <p className="muted">Runs only when you choose it. CaptureOS reads embedded metadata from available local copies, preserves unknown camera time zones as wall-clock time, and does not modify originals, previews, semantic data, or human decisions. Rebuild Moments afterward to use refreshed chronology.</p>
        </div>
        <button className="secondary" disabled={isRefreshingMetadata || metadataRefreshIsActive(metadataRefreshProgress)} onClick={() => void refreshCaptureMetadata()}>{isRefreshingMetadata || metadataRefreshIsActive(metadataRefreshProgress) ? "Refreshing metadata…" : "Refresh metadata"}</button>
      </section>
      {metadataRefreshProgress ? <p className="preparation" role="status" aria-live="polite">{metadataRefreshProgress.state === "running" || metadataRefreshProgress.state === "queued" ? <>Refreshing local capture metadata · {metadataRefreshProgress.itemsCompleted.toLocaleString()} / {metadataRefreshProgress.itemsTotal.toLocaleString()} logical media checked</> : metadataRefreshProgress.state === "completed" ? <>Capture-time metadata refresh complete · {metadataRefreshProgress.itemsCompleted.toLocaleString()} / {metadataRefreshProgress.itemsTotal.toLocaleString()} checked · {metadataRefreshProgress.resolvedCaptureTimeCount.toLocaleString()} resolved · {metadataRefreshProgress.highConfidenceCaptureTimeCount.toLocaleString()} high-confidence · {metadataRefreshProgress.copyConflictCount.toLocaleString()} copy conflicts. Rebuild Moments to use refreshed chronology.</> : <>Capture-time metadata refresh {displayLabel(metadataRefreshProgress.state)} · {metadataRefreshProgress.errorCount.toLocaleString()} issues{metadataRefreshProgress.message ? ` · ${metadataRefreshProgress.message}` : ""}</>}</p> : null}
      {metadataRefreshError ? <p className="error intelligence-error" role="alert">{metadataRefreshError}</p> : null}
      <MagicSearchControls
        isOpen={magicSearchOpen}
        onToggle={() => setMagicSearchOpen((value) => !value)}
        inputRef={magicSearchInputRef}
        query={magicSearchQuery}
        onQuery={setMagicSearchQuery}
        sort={magicSearchSort}
        onSort={setMagicSearchSort}
        descending={magicSearchDescending}
        onDescending={() => setMagicSearchDescending((value) => !value)}
        onSearch={() => void runMagicSearch()}
        onExample={(queryText) => { setMagicSearchQuery(queryText); void runMagicSearch({ query: queryText }); }}
        onClear={clearMagicSearch}
        isSearching={isSearchingMagic}
        response={magicSearchResponse}
        mode={magicSearchMode}
        status={semanticIndexProgress}
        resourceMode={semanticResourceMode}
        onResourceMode={setSemanticResourceMode}
        isStartingIndex={isStartingSemanticIndex}
        isPausingIndex={isPausingSemanticIndex}
        onStartIndex={() => void startSemanticIndex()}
        onPauseIndex={() => void pauseSemanticIndex()}
        history={magicSearchHistory}
        onHistory={(queryText) => { setMagicSearchQuery(queryText); void runMagicSearch({ query: queryText }); }}
        onClearHistory={() => void clearMagicSearchHistory()}
        error={magicSearchError}
      />
      <section className="culling-entry" aria-label="Moments timeline entry"><div><p className="section-label">Moment Brain</p><h2>Moments timeline</h2><p className="muted">{home.summary.momentCount ? `${home.summary.momentCount.toLocaleString()} local Moment${home.summary.momentCount === 1 ? "" : "s"} detected. ` : "No Moment analysis yet. "}Review a local structural timeline built from capture evidence. It never starts analysis while this project opens, never changes Similar Sets, and keeps your labels and split/merge choices authoritative.</p></div><button className="secondary" onClick={onMoments}>Open Moments</button></section>
      <section className="culling-entry" aria-label="Culling workspace entry"><div><p className="section-label">Human review</p><h2>Smart Culling Workspace</h2><p className="muted">{cullingProgress ? `${cullingProgress.reviewed.toLocaleString()} / ${cullingProgress.total.toLocaleString()} reviewed · Keep ${cullingProgress.keep.toLocaleString()} · Review ${cullingProgress.review.toLocaleString()} · Reject ${cullingProgress.reject.toLocaleString()}. ` : ""}AI technical evidence can help you begin, but Keep, Reject, Review, stars, ratings, notes, and representatives remain your local decisions.</p></div><button className="primary" onClick={onCull}>{cullingProgress?.reviewed ? "Resume Culling" : "Cull Photos"}</button></section>
      <StudioBrainEntry projectId={home.project.id} onOpen={onStudio} />
      <section className="culling-entry" aria-label="Production entry"><div><p className="section-label">Delivery Brain</p><h2>Production plans</h2><p className="muted">Create local delivery or editor worksets from your explicit human decisions. Preview a frozen manifest before any verified copy; originals and culling decisions remain unchanged.</p></div><button className="secondary" onClick={onProduction}>Open Production</button></section>
      {intelligenceError ? <p className="error intelligence-error" role="alert">{intelligenceError}</p> : null}

      <section className="visual-toolbar" aria-label="Visual media controls">
        <div><p className="section-label">Visual media</p><h2>Logical media</h2></div>
        <label className="search"><span className="sr-only">Search filename, camera, or lens</span><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search filename, camera, lens" /></label>
        <select aria-label="Sort media" value={visualSort} onChange={(event) => setVisualSort(event.target.value as VisualMediaSort)}><option value="captureTime">Capture time</option><option value="filename">Filename</option><option value="fileSize">File size</option><option value="dateIndexed">Date indexed</option><option value="mediaType">Media type</option></select>
        <button className="secondary" aria-label="Reverse sort direction" onClick={() => setDescending((value) => !value)}>{descending ? "Newest first" : "Oldest first"}</button>
        <div className="view-switch" aria-label="View mode"><button className={view === "grid" ? "active" : ""} onClick={() => setView("grid")}>Grid</button><button className={view === "list" ? "active" : ""} onClick={() => setView("list")}>List</button></div>
      </section>

      <section className="visual-filters" aria-label="Visual media filters">
        {([ ["all", "All"], ["photos", "Photos"], ["raw", "RAW"], ["jpegHeif", "JPEG / HEIF"], ["video", "Video"], ["audio", "Audio"], ["available", "Available"], ["offline", "Offline"] ] as [VisualMediaFilter, string][]).map(([id, label]) => <button key={id} className={visualFilter === id ? "active" : ""} onClick={() => setVisualFilter(id)}>{label}</button>)}
        <span className="intelligence-filter-divider" aria-hidden="true" />
        {([ ["strongCandidates", "Strong candidates"], ["technicalIssues", "Technical issues"], ["probableDuplicates", "Probable duplicates"], ["similarGroups", "Similar groups"], ["faces", "Faces"], ["possibleClosedEyes", "Possible closed eyes"], ["blurReview", "Blur review"] ] as [VisualMediaFilter, string][]).map(([id, label]) => <button key={id} className={`intelligence-filter ${visualFilter === id ? "active" : ""}`} onClick={() => setVisualFilter(id)}>{label}</button>)}
        <input aria-label="Camera model filter" value={cameraModel} onChange={(event) => setCameraModel(event.target.value)} placeholder="Camera model" />
        <input aria-label="Lens model filter" value={lensModel} onChange={(event) => setLensModel(event.target.value)} placeholder="Lens model" />
        <span className="toolbar-spacer" />
        <label>Density <select aria-label="Grid density" value={density} onChange={(event) => setDensity(event.target.value as "small" | "medium" | "large")}><option value="small">Small</option><option value="medium">Medium</option><option value="large">Large</option></select></label>
        <button className="secondary" onClick={() => setShowInspector((value) => !value)}>{showInspector ? "Hide inspector" : "Show inspector"}</button>
        <button className="secondary" onClick={() => void retryFailedPreviews()}>Retry failed previews</button>
        <button className="secondary" onClick={() => void clearCache()}>Clear preview cache</button>
      </section>

      {preparation ? <p className="preparation" role="status">{preparation.state === "running" ? <>Preparing previews · {preparation.itemsCompleted.toLocaleString()} / {preparation.itemsTotal.toLocaleString()} · {preparation.stage}{preparation.currentProvider ? ` · ${preparation.currentProvider}` : ""}</> : <>Preview preparation complete · {preparation.itemsCompleted.toLocaleString()} / {preparation.itemsTotal.toLocaleString()} processed · {preparation.readyCount} ready · {preparation.unsupportedCount} unsupported · {preparation.corruptCount} corrupt · {preparation.failedCount + preparation.timeoutCount} failed</>}</p> : null}
      {magicSearchResponse ? <MagicSearchResults
        response={magicSearchResponse}
        mode={magicSearchMode}
        density={density}
        view={view}
        showInspector={showInspector}
        selected={selected}
        onOpen={openMedia}
        onFindSimilar={(assetId) => void findSimilar(assetId)}
        isSearching={isSearchingMagic}
        onLoadMore={() => {
          const offset = magicSearchResponse.results.length;
          if (magicSearchMode === "similar") {
            if (!similarReferenceAssetId) {
              setMagicSearchError("Find Similar needs its original local photo before loading another page.");
              return;
            }
            void findSimilar(similarReferenceAssetId, { offset, append: true });
            return;
          }
          void runMagicSearch({ offset, append: true });
        }}
        onOpenViewer={selected ? () => setViewerOpen(true) : undefined}
        onOpenSimilarityGroup={openSimilarityGroup}
        isLoadingSimilarityGroup={isLoadingSimilarityGroup}
        onSaveHumanDecision={saveHumanDecision}
        isSavingDecision={isSavingDecision}
      /> : visual?.items.length ? <div className={`visual-layout ${showInspector ? "with-inspector" : ""}`}><section className={`media-grid ${density} ${view}`} aria-label="Visual media grid">{visual.items.map((item) => <MediaCard key={item.assetId} item={item} view={view} density={density} onOpen={openMedia} />)}{visual.hasMore ? <div className="load-more visual-load-more"><button onClick={() => void loadVisual(visual.items.length, true)} disabled={isLoadingVisual}>{isLoadingVisual ? "Loading…" : "Load next page"}</button></div> : null}</section>{showInspector ? <Inspector detail={selected} onOpen={selected ? () => setViewerOpen(true) : undefined} onOpenSimilarityGroup={openSimilarityGroup} isLoadingSimilarityGroup={isLoadingSimilarityGroup} onSaveHumanDecision={saveHumanDecision} isSavingDecision={isSavingDecision} /> : null}</div> : <div className="empty">{isLoadingVisual ? "Loading local media…" : "No media matches this view. Index a local folder to populate the catalog."}</div>}

      <StatusCard>
        <div className="panel-heading"><div><p className="section-label">Most recent index job</p><h2>{job?.state ?? "No index job yet"}</h2></div><span className={`badge ${job?.state ?? "idle"}`}>{job?.stage ?? "ready"}</span></div>
        {job ? <dl className="job-details"><Detail label="Files discovered" value={job.filesDiscovered} /><Detail label="Files processed" value={job.filesProcessed} /><Detail label="Issues" value={job.errorCount} /><Detail label="Started" value={formatDate(job.startedAt)} /><Detail label="Finished" value={job.finishedAt ? formatDate(job.finishedAt) : "—"} /></dl> : <p className="muted">Choose a local folder to start a read-only index.</p>}
      </StatusCard>

      <section className="summary"><div><p className="section-label">Project summary</p><p><strong>Last folder:</strong> {home.summary.lastIndexedFolder ?? "Not indexed yet"}</p><p><strong>Storage identity:</strong> {home.summary.storageVolumeIdentity ?? "Available after first index"}</p></div><div><p><strong>Duplicate logical assets:</strong> {home.summary.duplicateFastFingerprintCount}</p><p className="muted">Visual browsing shows one MediaAsset card. Its available/offline FileInstances appear in the inspector.</p></div></section>

      <section className="media-browser technical-browser"><div className="browser-heading"><div><p className="section-label">Advanced</p><h2>Physical file details</h2></div><button className="secondary" onClick={() => setShowTechnical((value) => !value)}>{showTechnical ? "Hide table" : "Show table"}</button></div>{showTechnical ? <><div className="filters" aria-label="Technical media filters">{filters.map((item) => <button key={item.id} className={filter === item.id ? "active" : ""} onClick={() => onFilter(item.id)}>{item.label}</button>)}</div>{home.media.length ? <><MediaTable rows={home.media} />{hasMoreMedia ? <div className="load-more"><button onClick={onLoadMore} disabled={isLoadingMore}>{isLoadingMore ? "Loading…" : "Load more files"}</button></div> : null}</> : <div className="empty">No physical files match this technical view.</div>}</> : null}</section>
      {viewerOpen && selected ? <MediaViewer detail={selected} nearby={activeVisualItems} onClose={() => setViewerOpen(false)} onPrevious={() => navigate(-1)} onNext={() => navigate(1)} onOpen={openMedia} onOpenSimilarityGroup={openSimilarityGroup} isLoadingSimilarityGroup={isLoadingSimilarityGroup} onSaveHumanDecision={saveHumanDecision} isSavingDecision={isSavingDecision} /> : null}
      {similarityGroup ? <SimilarityGroupDialog group={similarityGroup} onClose={() => setSimilarityGroup(null)} onOpenMember={(assetId) => { setSimilarityGroup(null); void openMediaAsset(assetId); }} /> : null}
    </div>
  );
}

const momentTimelinePageSize = 60;
const momentDetailPageSize = 120;

function StudioBrainEntry({ projectId, onOpen }: { projectId: string; onOpen: () => void }) {
  const [status, setStatus] = useState<StudioBrainProjectStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    void invoke<StudioBrainProjectStatus>("studio_brain_status_command", { projectId })
      .then((next) => { if (!cancelled) setStatus(next); })
      // A Studio status is additive. Project Home remains usable even if a future catalog needs
      // recovery, so this card shows an honest unavailable state rather than a raw DB string.
      .catch(() => { if (!cancelled) setStatus(null); });
    return () => { cancelled = true; };
  }, [projectId]);
  const state = status?.trainingStatus ?? "not_ready";
  const message = state === "ready"
    ? `Ready locally${status?.activeModelVersion ? ` · ${status.activeModelVersion}` : ""}`
    : state === "stale"
      ? "Update recommended"
      : state === "learning"
        ? "Learning from explicit decisions"
        : "Not enough evidence yet";
  return <section className="culling-entry" aria-label="Studio Brain entry"><div><p className="section-label">Studio Brain</p><h2>Local preference learning</h2><p className="muted">{status ? `${status.eligibleDecisionCount.toLocaleString()} explicit culling decision${status.eligibleDecisionCount === 1 ? "" : "s"} · ${message}. ` : "Status is loading locally. "}Studio Brain is advisory, runs offline, and never changes Keep, Reject, Review, ratings, representatives, Moments, or media.</p></div><button className="secondary" onClick={onOpen}>View Studio Brain</button></section>;
}

function StudioBrainWorkspace({ project, onReturn }: { project: ProjectView; onReturn: () => void }) {
  const [status, setStatus] = useState<StudioBrainProjectStatus | null>(null);
  const [progress, setProgress] = useState<StudioBrainProgress | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isMutating, setIsMutating] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [developerError, setDeveloperError] = useState<string | null>(null);
  const actionRef = useRef(false);

  const load = useCallback(async () => {
    const next = await invoke<StudioBrainProjectStatus>("studio_brain_status_command", { projectId: project.id });
    setStatus(next);
    return next;
  }, [project.id]);

  useEffect(() => {
    let cancelled = false;
    void load().catch((reason) => {
      if (!cancelled) {
        setNotice("Studio Brain status could not be loaded. Project media and human decisions remain available.");
        setDeveloperError(toMessage(reason));
      }
    }).finally(() => { if (!cancelled) setIsLoading(false); });
    let unlisten: (() => void) | undefined;
    void listen<StudioProfileScopedEvent<StudioBrainProgress>>("studio-brain-progress", ({ payload }) => {
      if (payload.projectId !== project.id) return;
      setStatus((current) => {
        if (current && current.profileId !== payload.profileId) return current;
        return current;
      });
      setProgress(payload.progress);
      if (payload.progress.state === "error") {
        setNotice(payload.progress.message ?? (payload.progress.activeModelVersion
          ? "Studio Brain update could not be completed. Your previous personalized model is still active."
          : "Studio Brain update could not be completed. No personalized model was activated; generic technical evidence remains available."));
        setDeveloperError(payload.progress.lastError);
      }
      if (!payload.progress.active) void load().catch(() => undefined);
    }).then((stop) => { unlisten = stop; });
    return () => { cancelled = true; unlisten?.(); };
  }, [load, project.id]);

  const actionsBlocked = isLoading || isMutating || progress?.active === true;
  const runMutation = useCallback(async (operation: () => Promise<StudioBrainProjectStatus>) => {
    if (actionRef.current || actionsBlocked) return;
    actionRef.current = true;
    setIsMutating(true);
    setNotice(null);
    try {
      setStatus(await operation());
    } catch (reason) {
      setNotice("Studio Brain settings could not be saved. Your human decisions and existing model remain unchanged.");
      setDeveloperError(toMessage(reason));
    } finally {
      actionRef.current = false;
      setIsMutating(false);
    }
  }, [actionsBlocked]);

  async function startTraining() {
    if (actionRef.current || actionsBlocked) return;
    actionRef.current = true;
    setIsMutating(true);
    setNotice(null);
    try {
      const queued = await invoke<StudioBrainProgress>("start_studio_brain_training_command", { projectId: project.id });
      setProgress(queued);
      setNotice(queued.message);
    } catch (reason) {
      setNotice(status?.activeModelVersion
        ? "Studio Brain update could not be started. Your previous personalized model is still active."
        : "Studio Brain update could not be started. No personalized model was activated; generic technical evidence remains available.");
      setDeveloperError(toMessage(reason));
    } finally {
      actionRef.current = false;
      setIsMutating(false);
    }
  }

  const readinessMessage = status?.readiness.message
    ?? (status?.trainingStatus === "ready" ? "A local personalized model is active." : "Not enough explicit human evidence is available for personalization yet.");
  const trainLabel = status?.activeModelVersion ? "Update Studio Brain" : "Train Studio Brain";
  const progressLabel = progress?.active ? `${displayLabel(progress.stage)} · ${progress.completed.toLocaleString()} / ${progress.total.toLocaleString()}` : null;
  return <section className="project-workspace studio-brain-workspace" aria-label="Studio Brain workspace">
    <header className="workspace-heading"><div><p className="eyebrow">STUDIO BRAIN</p><h2>Local preference learning</h2><p className="muted">Uses only explicit local human decisions and representative choices. It is offline, explainable, reversible, and advisory; it never automatically culls or changes your media.</p></div><button className="secondary" onClick={onReturn}>Return to Project</button></header>
    {notice ? <p className="preparation" role="status">{notice}</p> : null}
    <section className="intelligence-controls" aria-label="Studio Brain status and controls">
      <div className="intelligence-controls-copy"><p className="section-label">Local Studio Profile</p><h2>{status ? studioStatusLabel(status.trainingStatus) : "Loading"}</h2><p className="muted">{readinessMessage}</p></div>
      <div className="intelligence-controls-actions"><button className="primary" disabled={actionsBlocked} onClick={() => void startTraining()}>{actionsBlocked && progress?.active ? "Training locally…" : trainLabel}</button></div>
      <div className="intelligence-progress" role="status" aria-live="polite"><strong>{progress?.active ? "Training locally" : status ? studioStatusLabel(status.trainingStatus) : "Loading"}</strong><span>{progressLabel ?? `${(status?.eligibleDecisionCount ?? 0).toLocaleString()} eligible explicit decisions`}</span>{progress?.message ? <small>{progress.message}</small> : null}</div>
    </section>
    <section className="culling-entry"><div><p className="section-label">Training contribution</p><h2>Current project</h2><p className="muted">{status?.projectIncluded === false ? "This project's decisions are excluded from future Studio Brain training. They remain in Smart Cull unchanged." : "This project's explicit decisions may contribute to a future local training run. You can opt out without deleting any decisions."}</p></div><button className="secondary" disabled={actionsBlocked || !status} onClick={() => void runMutation(() => invoke<StudioBrainProjectStatus>("set_studio_brain_project_included_command", { projectId: project.id, included: !status?.projectIncluded }))}>{status?.projectIncluded === false ? "Include in learning" : "Exclude from learning"}</button></section>
    <section className="culling-entry"><div><p className="section-label">Recommendation use</p><h2>Personalized advice</h2><p className="muted">{status?.personalizationEnabled === false ? "Personalized advice is disabled. Smart Cull shows only existing generic technical evidence." : "Personalized advice is enabled when a valid local model is ready. Human choices always remain separate."}</p></div><button className="secondary" disabled={actionsBlocked || !status} onClick={() => void runMutation(() => invoke<StudioBrainProjectStatus>("set_studio_brain_enabled_command", { projectId: project.id, enabled: !status?.personalizationEnabled }))}>{status?.personalizationEnabled === false ? "Enable advice" : "Disable advice"}</button></section>
    <section className="culling-entry"><div><p className="section-label">Reset</p><h2>Reset personalization</h2><p className="muted">Removes only local Studio Brain models and advisory recommendations. It preserves all human decisions, ratings, stars, Similar Set representatives, Moments, technical evidence, previews, and original media.</p></div><button className="secondary" disabled={actionsBlocked || !status} onClick={() => { if (window.confirm("Reset local Studio Brain personalization? Human decisions will be preserved.")) void runMutation(() => invoke<StudioBrainProjectStatus>("reset_studio_brain_personalization_command", { projectId: project.id, confirmed: true })); }}>Reset personalization</button></section>
    <section className="metrics-strip" aria-label="Studio Brain training data"><Metric label="Keep" value={status?.keepCount ?? 0} /><Metric label="Review" value={status?.reviewCount ?? 0} /><Metric label="Reject" value={status?.rejectCount ?? 0} /><Metric label="Ratings" value={status?.ratingCount ?? 0} /><Metric label="Stars" value={status?.starredCount ?? 0} /><Metric label="Representatives" value={status?.representativeCount ?? 0} /></section>
    <details><summary>Developer Details</summary><p>Profile {status?.profileName ?? "unavailable"} · active model {status?.activeModelVersion ?? "none"} · contributing projects {status?.contributingProjectCount ?? 0}.</p>{status?.readiness.conditions?.length ? <ul>{status.readiness.conditions.map((condition) => <li key={condition.key}>{condition.met ? "Met" : "Not met"}: {condition.message}</li>)}</ul> : null}{developerError ? <pre>{developerError}</pre> : null}</details>
  </section>;
}

const emptyProductionRules = () => ({
  decisions: [] as string[],
  minimumRating: null as number | null,
  starredOnly: false,
  momentIds: [] as string[],
  staticAssetIds: [] as string[],
  virtualCollectionId: null as string | null,
});
const gibibyte = 1024 * 1024 * 1024;
const minimumProductionReserveGiB = 0.125;

function productionPlanTemplate(type: ProductionPlanType): ProductionPlanInput {
  const names: Record<ProductionPlanType, string> = {
    client_delivery: "Client Delivery",
    editor_workset: "Editor Workset",
    portfolio_selects: "Portfolio Selects",
    proof_gallery: "Proof Gallery",
    backup_archive: "Backup Archive",
    custom: "Custom Production Plan",
  };
  return {
    name: names[type],
    planType: type,
    selectionRules: {
      ...emptyProductionRules(),
      decisions: type === "editor_workset" ? ["keep", "review"] : ["keep"],
    },
    organization: "by_moment",
    filenameStrategy: { kind: "preserve_original" },
  };
}

function productionByteLabel(bytes: number | null | undefined) {
  if (bytes === null || bytes === undefined) return "Unavailable";
  if (bytes >= 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${bytes.toLocaleString()} B`;
}

function ProductionWorkspace({
  project,
  onReturn,
}: {
  project: ProjectView;
  onReturn: () => void;
}) {
  const [workspace, setWorkspace] = useState<ProductionWorkspaceView | null>(
    null,
  );
  const [selectedPlanId, setSelectedPlanId] = useState<string | null>(null);
  const [planName, setPlanName] = useState("");
  const [planType, setPlanType] =
    useState<ProductionPlanType>("client_delivery");
  const [decisions, setDecisions] = useState<string[]>(["keep"]);
  const [minimumRating, setMinimumRating] = useState<number | null>(null);
  const [starredOnly, setStarredOnly] = useState(false);
  const [momentIds, setMomentIds] = useState<string[]>([]);
  const [planMoments, setPlanMoments] = useState<MomentTimelineView | null>(
    null,
  );
  const [isLoadingPlanMoments, setIsLoadingPlanMoments] = useState(false);
  const [organization, setOrganization] = useState<
    "single_folder" | "by_moment"
  >("by_moment");
  const [filenameKind, setFilenameKind] = useState("preserve_original");
  const [filenameTemplate, setFilenameTemplate] = useState("{original}");
  const [destinationPath, setDestinationPath] = useState("");
  const [destinationReserveGiB, setDestinationReserveGiB] = useState("1");
  const [virtualCollectionId, setVirtualCollectionId] = useState<string | null>(null);
  const [staticCollectionId, setStaticCollectionId] = useState<string | null>(null);
  const [preview, setPreview] = useState<ProductionPlanPreview | null>(null);
  const [manifest, setManifest] = useState<ExportManifestView | null>(null);
  const [preflight, setPreflight] = useState<ProductionPreflight | null>(null);
  const [progress, setProgress] = useState<ProductionExportProgress | null>(
    null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const [developerError, setDeveloperError] = useState<string | null>(null);
  const [isMutating, setIsMutating] = useState(false);
  const actionRef = useRef(false);

  const selectedPlan =
    workspace?.plans.find((plan) => plan.id === selectedPlanId) ?? null;
  const exportActive =
    progress?.state === "queued" || progress?.state === "running";

  const load = useCallback(async () => {
    const next = await invoke<ProductionWorkspaceView>(
      "production_workspace_command",
      { projectId: project.id },
    );
    setWorkspace(next);
    setSelectedPlanId((current) =>
      current && next.plans.some((plan) => plan.id === current)
        ? current
        : (next.plans[0]?.id ?? null),
    );
    return next;
  }, [project.id]);

  useEffect(() => {
    void load().catch((reason) => {
      setNotice(
        "Production Plans could not be loaded. Project media and human decisions remain available.",
      );
      setDeveloperError(toMessage(reason));
    });
  }, [load]);
  useEffect(() => {
    if (!selectedPlan) return;
    setPlanName(selectedPlan.name);
    setPlanType(selectedPlan.planType as ProductionPlanType);
    setDecisions(selectedPlan.selectionRules.decisions);
    setMinimumRating(selectedPlan.selectionRules.minimumRating);
    setStarredOnly(selectedPlan.selectionRules.starredOnly);
    setMomentIds(selectedPlan.selectionRules.momentIds);
    setPlanMoments(null);
    setOrganization(selectedPlan.organization as "single_folder" | "by_moment");
    setFilenameKind(selectedPlan.filenameStrategy.kind);
    setFilenameTemplate(
      selectedPlan.filenameStrategy.kind === "custom_template"
        ? selectedPlan.filenameStrategy.template
        : "{original}",
    );
    setDestinationPath(selectedPlan.destinationPath ?? "");
    setDestinationReserveGiB(
      (selectedPlan.destinationReserveBytes / gibibyte)
        .toFixed(3)
        .replace(/\.?0+$/, ""),
    );
    setVirtualCollectionId(selectedPlan.selectionRules.virtualCollectionId);
    setPreview(null);
    setManifest(null);
    setPreflight(null);
    setProgress(null);
  }, [selectedPlan?.id]);
  useEffect(() => {
    const staticCollections = workspace?.collections.filter((collection) => collection.kind === "static") ?? [];
    setStaticCollectionId((current) =>
      current && staticCollections.some((collection) => collection.id === current)
        ? current
        : (staticCollections[0]?.id ?? null),
    );
  }, [workspace?.collections]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProductionScopedEvent<ProductionExportProgress>>(
      "production-export-progress",
      ({ payload }) => {
        if (payload.projectId !== project.id) return;
        setProgress(payload.progress);
        setManifest((current) =>
          current?.id === payload.manifestId ? current : current,
        );
        if (payload.progress.message) setNotice(payload.progress.message);
        if (!["queued", "running"].includes(payload.progress.state))
          void load().catch(() => undefined);
      },
    ).then((stop) => {
      unlisten = stop;
    });
    return () => {
      unlisten?.();
    };
  }, [load, project.id]);

  const currentInput = (): ProductionPlanInput => ({
    name: planName.trim(),
    planType,
    selectionRules: {
      ...emptyProductionRules(),
      decisions,
      minimumRating,
      starredOnly,
      momentIds,
      staticAssetIds: selectedPlan?.selectionRules.staticAssetIds ?? [],
      virtualCollectionId,
    },
    organization,
    filenameStrategy:
      filenameKind === "custom_template"
        ? { kind: "custom_template", template: filenameTemplate }
        : {
            kind: filenameKind as
              | "preserve_original"
              | "sequential"
              | "project_sequence"
              | "moment_sequence",
          },
  });
  const changeDecision = (decision: string, checked: boolean) =>
    setDecisions((current) =>
      checked
        ? [...new Set([...current, decision])]
        : current.filter((value) => value !== decision),
    );
  const changeMoment = (momentId: string, checked: boolean) =>
    setMomentIds((current) =>
      checked
        ? [...new Set([...current, momentId])]
        : current.filter((value) => value !== momentId),
    );
  async function loadPlanMoments() {
    if (isLoadingPlanMoments) return;
    setIsLoadingPlanMoments(true);
    setNotice(null);
    setDeveloperError(null);
    try {
      const next = await invoke<MomentTimelineView>("moment_timeline", {
        projectId: project.id,
        limit: 120,
        offset: 0,
      });
      setPlanMoments(next);
    } catch (reason) {
      setNotice(
        "Moments could not be loaded for this optional selection rule. Existing plan settings and media are unchanged.",
      );
      setDeveloperError(toMessage(reason));
    } finally {
      setIsLoadingPlanMoments(false);
    }
  }
  const run = async (action: () => Promise<void>) => {
    if (actionRef.current || isMutating || exportActive) return;
    actionRef.current = true;
    setIsMutating(true);
    setNotice(null);
    setDeveloperError(null);
    try {
      await action();
    } catch (reason) {
      setNotice(
        "Production Plan could not be updated. No source media or human decisions were changed.",
      );
      setDeveloperError(toMessage(reason));
    } finally {
      actionRef.current = false;
      setIsMutating(false);
    }
  };
  async function createPlan(type: ProductionPlanType) {
    await run(async () => {
      const created = await invoke<{ id: string }>(
        "create_production_plan_command",
        { projectId: project.id, input: productionPlanTemplate(type) },
      );
      await load();
      setSelectedPlanId(created.id);
      setNotice(
        `${productionPlanTemplate(type).name} is ready to configure. No files have been selected or copied.`,
      );
    });
  }
  async function saveConfiguration() {
    if (!selectedPlan || !planName.trim()) return;
    await run(async () => {
      const updated = await invoke<{ id: string }>(
        "update_production_plan_configuration_command",
        {
          projectId: project.id,
          planId: selectedPlan.id,
          input: currentInput(),
        },
      );
      await load();
      setSelectedPlanId(updated.id);
      setNotice(
        "Plan configuration saved. Create a fresh preview before any export.",
      );
    });
  }
  async function chooseDestination() {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Choose local Delivery destination",
    });
    if (typeof selected === "string") setDestinationPath(selected);
  }
  async function saveDestination() {
    if (!selectedPlan) return;
    await run(async () => {
      const updated = await invoke<{ id: string }>(
        "set_production_plan_destination_command",
        {
          projectId: project.id,
          planId: selectedPlan.id,
          destinationPath: destinationPath.trim() || null,
        },
      );
      await load();
      setSelectedPlanId(updated.id);
      setNotice(
        destinationPath.trim()
          ? "Local destination saved. Preview checks it without copying media."
          : "Destination cleared. This plan cannot export until you choose another local folder.",
      );
    });
  }
  async function saveDestinationReserve() {
    if (!selectedPlan) return;
    const requestedGiB = Number(destinationReserveGiB);
    if (
      !Number.isFinite(requestedGiB) ||
      requestedGiB < minimumProductionReserveGiB
    ) {
      setNotice(
        "Safety reserve must be at least 0.125 GiB. This is required local free-space headroom, not a delivery size limit.",
      );
      return;
    }
    const reserveBytes = Math.round(requestedGiB * gibibyte);
    await run(async () => {
      const updated = await invoke<{ id: string }>(
        "set_production_plan_destination_reserve_command",
        { projectId: project.id, planId: selectedPlan.id, reserveBytes },
      );
      await load();
      setSelectedPlanId(updated.id);
      setNotice(
        "Safety reserve saved. Preview again to measure the current destination headroom before export.",
      );
    });
  }
  async function previewPlan() {
    if (!selectedPlan) return;
    await run(async () => {
      const next = await invoke<ProductionPlanPreview>(
        "production_plan_preview_command",
        { projectId: project.id, planId: selectedPlan.id },
      );
      setPreview(next);
      setManifest(null);
      setPreflight(null);
      setNotice(
        next.blockers.length
          ? "Preview found items that must be resolved before a frozen manifest can be created."
          : "Dry run is ready. No media was copied or changed.",
      );
    });
  }
  async function applyPlanOverride(
    assetId: string,
    kind: "force_include" | "force_exclude" | null,
  ) {
    if (!selectedPlan) return;
    await run(async () => {
      await invoke("set_production_plan_override_command", {
        projectId: project.id,
        planId: selectedPlan.id,
        assetId,
        kind,
      });
      setPreview(null);
      setManifest(null);
      setPreflight(null);
      await load();
      setNotice(
        kind === "force_include"
          ? "Included for this Production Plan only. The human Smart Cull decision is unchanged. Preview again to validate it."
          : kind === "force_exclude"
            ? "Excluded from this Production Plan only. The human Smart Cull decision is unchanged. Preview again to validate it."
            : "The plan-local exception was removed. Preview again to apply the plan rules.",
      );
    });
  }
  async function createManifest() {
    if (!selectedPlan) return;
    await run(async () => {
      const next = await invoke<ExportManifestView>(
        "create_production_manifest_command",
        { projectId: project.id, planId: selectedPlan.id },
      );
      const nextPreflight = await invoke<ProductionPreflight>(
        "production_manifest_preflight_command",
        { projectId: project.id, manifestId: next.id },
      );
      setManifest(next);
      setPreflight(nextPreflight);
      await load();
      setNotice(
        nextPreflight.blockers.length
          ? "The immutable manifest was saved, but export remains blocked until preflight is clear."
          : "Immutable manifest saved. Review the final preflight, then start verified local copy.",
      );
    });
  }
  async function startExport() {
    if (!manifest || preflight?.blockers.length) return;
    await run(async () => {
      const queued = await invoke<ProductionExportProgress>(
        "start_production_export_command",
        { projectId: project.id, manifestId: manifest.id },
      );
      setProgress(queued);
      setNotice(queued.message);
    });
  }
  async function resumeFrozenExport(manifestId: string) {
    await run(async () => {
      const nextPreflight = await invoke<ProductionPreflight>(
        "production_manifest_preflight_command",
        { projectId: project.id, manifestId },
      );
      setManifest(nextPreflight.manifest);
      setPreflight(nextPreflight);
      if (nextPreflight.blockers.length) {
        setNotice(
          "This frozen manifest remains intact, but its current local preflight is blocked. Reconnect the drive or resolve the reported condition before resuming.",
        );
        return;
      }
      const queued = await invoke<ProductionExportProgress>(
        "start_production_export_command",
        { projectId: project.id, manifestId },
      );
      setProgress(queued);
      setNotice(
        "Resume is queued from the same immutable manifest. Already verified identical files will be recognized rather than blindly recopied.",
      );
    });
  }
  async function cancelExport() {
    if (!manifest || !exportActive) return;
    try {
      await invoke("cancel_production_export_command", {
        manifestId: manifest.id,
      });
      setNotice(
        "Cancellation requested. Any already verified destination files remain valid; incomplete partial files are not treated as completed exports.",
      );
    } catch (reason) {
      setNotice(
        "Export cancellation could not be requested. The current local export may already have finished.",
      );
      setDeveloperError(toMessage(reason));
    }
  }
  async function createCollection(kind: "dynamic" | "static") {
    await run(async () => {
      const input: VirtualCollectionInput = {
        name: kind === "dynamic" ? "Human Keeps" : "Static Collection",
        kind,
        rules:
          kind === "dynamic"
            ? { ...emptyProductionRules(), decisions: ["keep"] }
            : emptyProductionRules(),
      };
      await invoke("create_virtual_collection_command", {
        projectId: project.id,
        input,
      });
      await load();
      setNotice(
        kind === "dynamic"
          ? "Dynamic Human Keeps collection created from explicit culling decisions."
          : "Empty static collection created. It stores only asset references and never duplicates media.",
      );
    });
  }
  async function setStaticCollectionMember(assetId: string, included: boolean) {
    if (!staticCollectionId) return;
    await run(async () => {
      await invoke("set_static_virtual_collection_member_command", {
        projectId: project.id,
        collectionId: staticCollectionId,
        assetId,
        included,
      });
      await load();
      setNotice(
        included
          ? "Asset reference added to the selected static collection. No media or Smart Cull decision changed."
          : "Asset reference removed from the selected static collection. No media or Smart Cull decision changed.",
      );
    });
  }

  const disabled = isMutating || exportActive;
  return (
    <section
      className="project-workspace production-workspace"
      aria-label="Production workspace"
    >
      <header className="workspace-heading">
        <div>
          <p className="eyebrow">DELIVERY BRAIN</p>
          <h2>Production plans</h2>
          <p className="muted">
            Organize explicit human selections into local worksets. Studio Brain
            remains advisory; this workspace never changes Keep, Reject, Review,
            ratings, notes, Moments, originals, or card media.
          </p>
        </div>
        <button className="secondary" onClick={onReturn}>
          Return to Project
        </button>
      </header>
      {notice ? (
        <p className="preparation" role="status">
          {notice}
        </p>
      ) : null}
      <section
        className="intelligence-controls"
        aria-label="Create Production Plan"
      >
        <div className="intelligence-controls-copy">
          <p className="section-label">Plan templates</p>
          <h2>Start from human decisions</h2>
          <p className="muted">
            Templates are editable. Client Delivery starts with Keep; Editor
            Workset starts with Keep and Review. Nothing uses a Studio
            recommendation as a final select.
          </p>
        </div>
        <div className="intelligence-controls-actions">
          <button
            className="secondary"
            disabled={disabled}
            onClick={() => void createPlan("client_delivery")}
          >
            Client Delivery
          </button>
          <button
            className="secondary"
            disabled={disabled}
            onClick={() => void createPlan("editor_workset")}
          >
            Editor Workset
          </button>
          <button
            className="secondary"
            disabled={disabled}
            onClick={() => void createPlan("custom")}
          >
            Custom Plan
          </button>
        </div>
      </section>
      {workspace?.plans.length ? (
        <section className="status-card" aria-label="Production Plan editor">
          <div className="panel-heading">
            <div>
              <p className="section-label">Production Plan</p>
              <h2>{selectedPlan?.name ?? "Select a plan"}</h2>
              <p className="muted">
                Plans, manifests, and export jobs are distinct durable records.
                Changing a plan makes its prior manifest stale; a running export
                keeps its original frozen snapshot.
              </p>
            </div>
            <label>
              Plan{" "}
              <select
                aria-label="Select Production Plan"
                value={selectedPlanId ?? ""}
                disabled={disabled}
                onChange={(event) => setSelectedPlanId(event.target.value)}
              >
                {workspace.plans.map((plan) => (
                  <option key={plan.id} value={plan.id}>
                    {plan.name} · {displayLabel(plan.status)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {selectedPlan ? (
            <>
              <div className="visual-toolbar">
                <label>
                  Name{" "}
                  <input
                    aria-label="Production Plan name"
                    value={planName}
                    disabled={disabled}
                    onChange={(event) => setPlanName(event.target.value)}
                  />
                </label>
                <label>
                  Type{" "}
                  <select
                    value={planType}
                    disabled={disabled}
                    onChange={(event) =>
                      setPlanType(event.target.value as ProductionPlanType)
                    }
                  >
                    {(
                      [
                        "client_delivery",
                        "editor_workset",
                        "portfolio_selects",
                        "proof_gallery",
                        "backup_archive",
                        "custom",
                      ] as ProductionPlanType[]
                    ).map((type) => (
                      <option key={type} value={type}>
                        {displayLabel(type)}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Organization{" "}
                  <select
                    value={organization}
                    disabled={disabled}
                    onChange={(event) =>
                      setOrganization(
                        event.target.value as "single_folder" | "by_moment",
                      )
                    }
                  >
                    <option value="by_moment">Moment folders</option>
                    <option value="single_folder">Single folder</option>
                  </select>
                </label>
                <label>
                  Filename{" "}
                  <select
                    value={filenameKind}
                    disabled={disabled}
                    onChange={(event) => setFilenameKind(event.target.value)}
                  >
                    <option value="preserve_original">Preserve original</option>
                    <option value="sequential">Sequential</option>
                    <option value="project_sequence">Project sequence</option>
                    <option value="moment_sequence">Moment sequence</option>
                    <option value="custom_template">
                      Custom safe template
                    </option>
                  </select>
                </label>
                {filenameKind === "custom_template" ? (
                  <label>
                    Template{" "}
                    <input
                      aria-label="Safe filename template"
                      value={filenameTemplate}
                      disabled={disabled}
                      onChange={(event) =>
                        setFilenameTemplate(event.target.value)
                      }
                      placeholder="{project}_{sequence}"
                    />
                  </label>
                ) : null}
                <button
                  className="secondary"
                  disabled={disabled || !planName.trim()}
                  onClick={() => void saveConfiguration()}
                >
                  Save plan settings
                </button>
              </div>
              <fieldset className="production-rule-set" disabled={disabled}>
                <legend>Explicit human selection rules</legend>
                <label>
                  <input
                    type="checkbox"
                    checked={decisions.includes("keep")}
                    onChange={(event) =>
                      changeDecision("keep", event.target.checked)
                    }
                  />{" "}
                  Keep
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={decisions.includes("review")}
                    onChange={(event) =>
                      changeDecision("review", event.target.checked)
                    }
                  />{" "}
                  Review
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={decisions.includes("reject")}
                    onChange={(event) =>
                      changeDecision("reject", event.target.checked)
                    }
                  />{" "}
                  Reject
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={starredOnly}
                    onChange={(event) => setStarredOnly(event.target.checked)}
                  />{" "}
                  Starred only
                </label>
                <label>
                  Minimum rating{" "}
                  <select
                    value={minimumRating ?? ""}
                    onChange={(event) =>
                      setMinimumRating(
                        event.target.value ? Number(event.target.value) : null,
                      )
                    }
                  >
                    <option value="">Any</option>
                    {[1, 2, 3, 4, 5].map((rating) => (
                      <option key={rating} value={rating}>
                        {rating} star{rating === 1 ? "" : "s"}+
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Virtual Collection{" "}
                  <select
                    aria-label="Production Virtual Collection"
                    value={virtualCollectionId ?? ""}
                    onChange={(event) =>
                      setVirtualCollectionId(event.target.value || null)
                    }
                  >
                    <option value="">No collection restriction</option>
                    {(workspace?.collections ?? []).map((collection) => (
                      <option key={collection.id} value={collection.id}>
                        {collection.name} · {displayLabel(collection.kind)}
                      </option>
                    ))}
                  </select>
                </label>
                <div className="production-moment-rule">
                  <button
                    className="secondary"
                    type="button"
                    disabled={disabled || isLoadingPlanMoments}
                    onClick={() => void loadPlanMoments()}
                  >
                    {isLoadingPlanMoments
                      ? "Loading Moments…"
                      : "Choose Moments"}
                  </button>
                  <span>
                    {momentIds.length
                      ? `${momentIds.length.toLocaleString()} Moment${
                          momentIds.length === 1 ? "" : "s"
                        } selected`
                      : "All Moments"}
                  </span>
                </div>
                {planMoments ? (
                  <div
                    className="production-moment-options"
                    aria-label="Production Moment filters"
                  >
                    {planMoments.moments.length ? (
                      planMoments.moments.map((moment) => (
                        <label key={moment.id}>
                          <input
                            type="checkbox"
                            checked={momentIds.includes(moment.id)}
                            onChange={(event) =>
                              changeMoment(moment.id, event.target.checked)
                            }
                          />{" "}
                          {String(moment.ordinal).padStart(2, "0")} ·{" "}
                          {moment.label.displayLabel} ({moment.assetCount} files)
                        </label>
                      ))
                    ) : (
                      <p className="muted">
                        No local Moments are available. This rule remains
                        optional; it never starts Moment analysis.
                      </p>
                    )}
                    {planMoments.hasMore ? (
                      <p className="muted">
                        Showing the first 120 Moments. Use the Moments workspace
                        to inspect the complete project timeline.
                      </p>
                    ) : null}
                  </div>
                ) : null}
                <p className="muted">
                  Collection and Moment references intersect the human rules
                  above. They are separate from Smart Cull; a plan-local
                  include/exclude exception never rewrites its human decision.
                </p>
              </fieldset>
              <div className="visual-toolbar">
                <label className="search">
                  <span className="sr-only">Local delivery destination</span>
                  <input
                    aria-label="Local delivery destination"
                    value={destinationPath}
                    disabled={disabled}
                    onChange={(event) => setDestinationPath(event.target.value)}
                    placeholder="Choose a local destination folder"
                  />
                </label>
                <button
                  className="secondary"
                  disabled={disabled}
                  onClick={() => void chooseDestination()}
                >
                  Choose folder
                </button>
                <button
                  className="secondary"
                  disabled={disabled}
                  onClick={() => void saveDestination()}
                >
                  Save destination
                </button>
                <label className="production-reserve">
                  Safety reserve (GiB)
                  <input
                    aria-label="Production safety reserve in GiB"
                    type="number"
                    min={minimumProductionReserveGiB}
                    step="0.125"
                    value={destinationReserveGiB}
                    disabled={disabled}
                    onChange={(event) =>
                      setDestinationReserveGiB(event.target.value)
                    }
                  />
                </label>
                <button
                  className="secondary"
                  disabled={disabled}
                  onClick={() => void saveDestinationReserve()}
                >
                  Save reserve
                </button>
                <button
                  className="primary"
                  disabled={disabled}
                  onClick={() => void previewPlan()}
                >
                  Preview dry run
                </button>
              </div>
            </>
          ) : null}
        </section>
      ) : (
        <div className="empty">
          Create a Client Delivery, Editor Workset, or Custom Plan. The new plan
          contains no copy job until you explicitly create its manifest.
        </div>
      )}
      {preview ? (
        <section className="status-card" aria-label="Production Plan dry run">
          <div className="panel-heading">
            <div>
              <p className="section-label">Dry run</p>
              <h2>
                {preview.manifestSummary.selectedFileCount.toLocaleString()}{" "}
                files ·{" "}
                {productionByteLabel(preview.manifestSummary.estimatedBytes)}
              </h2>
              <p className="muted">
                Destination is{" "}
                {preview.destinationWritable
                  ? "locally writable"
                  : "not writable"}
                . {preview.availableSourceCount.toLocaleString()} sources
                available · {preview.offlineSourceCount.toLocaleString()}{" "}
                offline · {preview.existingIdenticalCount.toLocaleString()}{" "}
                already identical at destination.
              </p>
            </div>
            <button
              className="primary"
              disabled={disabled || preview.blockers.length > 0}
              onClick={() => void createManifest()}
            >
              Freeze manifest
            </button>
          </div>
          {preview.blockers.length ? (
            <ul className="culling-reasons">
              {preview.blockers.map((blocker) => (
                <li key={blocker}>
                  <strong>Blocked:</strong> {blocker}
                </li>
              ))}
            </ul>
          ) : (
            <p className="human-override">
              Ready to freeze. This preview has not written any media.
            </p>
          )}
          {preview.warnings.length ? (
            <ul className="culling-reasons">
              {preview.warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
          ) : null}
          <div className="production-path-examples">
            <strong>Safe destination examples</strong>
            {preview.namingExamples.length ? (
              <ul>
                {preview.namingExamples.map((entry) => (
                  <li key={entry.destinationRelativePath}>
                    <code>{entry.originalFilename}</code> →{" "}
                    <code>{entry.destinationRelativePath}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted">
                No assets meet the current explicit human selection rules.
              </p>
            )}
          </div>
          <div className="production-inspection" aria-label="Production collection inspection">
            <div>
              <strong>Selection review</strong>
              <p className="muted">
                Included {preview.inspection.includedCount.toLocaleString()} · Excluded {preview.inspection.excludedCount.toLocaleString()} · Blocked {preview.inspection.blockedCount.toLocaleString()}. Review is bounded to the first {preview.inspection.items.length.toLocaleString()} local records; it never loads a whole large plan into the browser.
              </p>
            </div>
            {preview.inspection.items.length ? (
              <ul>
                {preview.inspection.items.map((item) => (
                  <li key={item.assetId} className={`production-inspection-${item.state}`}>
                    <div>
                      <strong>{item.originalFilename}</strong>
                      <small>
                        {displayLabel(item.state)} · {item.humanDecision ? `Human ${displayLabel(item.humanDecision)}` : "No human decision"}
                        {item.destinationRelativePath ? ` · ${item.destinationRelativePath}` : ""}
                      </small>
                      {item.reason ? <small>{item.reason}</small> : null}
                    </div>
                    <button
                      className="secondary"
                      disabled={disabled}
                      onClick={() => void applyPlanOverride(
                        item.assetId,
                        item.planOverride
                          ? null
                          : item.state === "included"
                            ? "force_exclude"
                            : "force_include",
                      )}
                    >
                      {item.planOverride
                        ? "Use plan rules"
                        : item.state === "included"
                          ? "Exclude from plan"
                          : "Include in plan"}
                    </button>
                    {staticCollectionId ? (
                      <span className="production-inspection-actions">
                        <button
                          className="secondary"
                          disabled={disabled}
                          onClick={() =>
                            void setStaticCollectionMember(item.assetId, true)
                          }
                        >
                          Add to static
                        </button>
                        <button
                          className="secondary"
                          disabled={disabled}
                          onClick={() =>
                            void setStaticCollectionMember(item.assetId, false)
                          }
                        >
                          Remove static
                        </button>
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            ) : null}
            {preview.inspection.remainingCount ? (
              <p className="muted">{preview.inspection.remainingCount.toLocaleString()} additional records are represented in the counts above.</p>
            ) : null}
          </div>
        </section>
      ) : null}
      {manifest && preflight ? (
        <section className="status-card" aria-label="Frozen Delivery Manifest">
          <div className="panel-heading">
            <div>
              <p className="section-label">
                Frozen manifest v{manifest.manifestVersion}
              </p>
              <h2>
                {manifest.selectedFileCount.toLocaleString()} planned files ·{" "}
                {productionByteLabel(manifest.estimatedBytes)}
              </h2>
              <p className="muted">
                Checksum {manifest.checksum.slice(0, 16)}… · selection will not
                be re-evaluated while copying.
              </p>
            </div>
            {exportActive ? (
              <button className="secondary" onClick={() => void cancelExport()}>
                Cancel export
              </button>
            ) : (
              <button
                className="primary"
                disabled={disabled || preflight.blockers.length > 0}
                onClick={() => void startExport()}
              >
                Start verified export
              </button>
            )}
          </div>
          {preflight.blockers.length ? (
            <ul className="culling-reasons">
              {preflight.blockers.map((blocker) => (
                <li key={blocker}>
                  <strong>Blocked:</strong> {blocker}
                </li>
              ))}
            </ul>
          ) : (
            <p className="human-override">
              Final preflight is clear. Copy uses streaming BLAKE3
              source-to-destination verification and never overwrites a
              different existing file.
            </p>
          )}
          {preflight.warnings.length ? (
            <ul className="culling-reasons">
              {preflight.warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
          ) : null}
        </section>
      ) : null}
      {progress ? (
        <section
          className="intelligence-controls"
          aria-label="Production export progress"
        >
          <div className="intelligence-controls-copy">
            <p className="section-label">Verified local export</p>
            <h2>{displayLabel(progress.state)}</h2>
            <p className="muted">
              {progress.itemsCompleted.toLocaleString()} /{" "}
              {progress.itemsTotal.toLocaleString()} processed ·{" "}
              {progress.verifiedCount.toLocaleString()} verified ·{" "}
              {progress.skippedIdenticalCount.toLocaleString()} already
              identical · {progress.failedCount.toLocaleString()} not verified
            </p>
          </div>
          <div
            className="intelligence-progress"
            role="status"
            aria-live="polite"
          >
            <strong>{displayLabel(progress.stage)}</strong>
            <span>
              {progress.currentFilename ??
                progress.message ??
                "Waiting for local worker"}
            </span>
            <small>
              {productionByteLabel(progress.verifiedBytes)} verified
            </small>
          </div>
        </section>
      ) : null}
      {workspace?.recentExports.length ? (
        <section className="status-card" aria-label="Local Delivery history">
          <div className="panel-heading">
            <div>
              <p className="section-label">Local history</p>
              <h2>Verified delivery executions</h2>
              <p className="muted">
                This is local audit history only, not cloud delivery tracking.
                Restarting an interrupted or partial frozen manifest safely
                reuses already verified destination matches and never overwrites
                a different file.
              </p>
            </div>
          </div>
          <ul className="culling-reasons">
            {workspace.recentExports.map((job) => (
              <li key={job.id}>
                <strong>
                  {workspace.plans.find((plan) => plan.id === job.planId)
                    ?.name ?? "Production Plan"}
                </strong>{" "}
                · {displayLabel(job.state)} ·{" "}
                {job.verifiedCount.toLocaleString()} verified ·{" "}
                {productionByteLabel(job.verifiedBytes)}
                {job.failedCount
                  ? ` · ${job.failedCount.toLocaleString()} not verified`
                  : ""}
                {job.errorMessage ? <small> · {job.errorMessage}</small> : null}
                {["interrupted", "failed", "partially_completed", "cancelled"].includes(job.state) ? (
                  <button
                    className="secondary"
                    disabled={disabled}
                    onClick={() => void resumeFrozenExport(job.manifestId)}
                  >
                    Resume frozen manifest
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      <section className="status-card" aria-label="Virtual Collections">
        <div className="panel-heading">
          <div>
            <p className="section-label">Virtual Collections</p>
            <h2>Logical media groups</h2>
            <p className="muted">
              Collections reference existing MediaAssets only. They do not copy,
              move, rename, hide, or alter media. Dynamic rules use explicit
              human state only.
            </p>
          </div>
          <div>
            <button
              className="secondary"
              disabled={disabled}
              onClick={() => void createCollection("dynamic")}
            >
              Create Human Keeps
            </button>
            <button
              className="secondary"
              disabled={disabled}
              onClick={() => void createCollection("static")}
            >
              Create static collection
            </button>
          </div>
        </div>
        {workspace?.collections.some((collection) => collection.kind === "static") ? (
          <label className="production-static-target">
            Active static collection{" "}
            <select
              aria-label="Active static Virtual Collection"
              value={staticCollectionId ?? ""}
              disabled={disabled}
              onChange={(event) => setStaticCollectionId(event.target.value || null)}
            >
              {(workspace?.collections ?? [])
                .filter((collection) => collection.kind === "static")
                .map((collection) => (
                  <option key={collection.id} value={collection.id}>
                    {collection.name} · {collection.assetCount.toLocaleString()} references
                  </option>
                ))}
            </select>
            <small>Use the bounded plan review above to add or remove individual asset references.</small>
          </label>
        ) : null}
        {workspace?.collections.length ? (
          <ul className="culling-reasons">
            {workspace.collections.map((collection) => (
              <li key={collection.id}>
                <strong>{collection.name}</strong> ·{" "}
                {displayLabel(collection.kind)} ·{" "}
                {collection.assetCount.toLocaleString()} asset reference
                {collection.assetCount === 1 ? "" : "s"}
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted">
            No collections yet. They are optional organization, separate from
            source media and human culling.
          </p>
        )}
      </section>
      {developerError ? (
        <details className="advanced">
          <summary>Developer Details</summary>
          <pre>{developerError}</pre>
        </details>
      ) : null}
    </section>
  );
}

function MomentTimelineWorkspace({ project, initialMomentId, onReturn, onShowTimeline, onOpenMoment, onCullMoment, onError }: {
  project: ProjectView;
  initialMomentId?: string;
  onReturn: () => void;
  onShowTimeline: () => void;
  onOpenMoment: (momentId: string) => void;
  onCullMoment: (momentId: string) => void;
  onError: (message: string | null) => void;
}) {
  const [progress, setProgress] = useState<MomentAnalysisProgress | null>(null);
  const [timeline, setTimeline] = useState<MomentTimelineView | null>(null);
  const [detail, setDetail] = useState<MomentDetailView | null>(null);
  const [momentMedia, setMomentMedia] = useState<VisualMediaPage | null>(null);
  const [checklists, setChecklists] = useState<MomentChecklistView[]>([]);
  const [isStarting, setIsStarting] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [momentStatusLoaded, setMomentStatusLoaded] = useState(false);
  const [momentRecoveryMessage, setMomentRecoveryMessage] = useState<string | null>(null);
  const [isSearching, setIsSearching] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResponse, setSearchResponse] = useState<MagicSearchResponse | null>(null);
  const [momentSearchQuery, setMomentSearchQuery] = useState("");
  const [momentSearchResponse, setMomentSearchResponse] = useState<MomentSearchResponse | null>(null);
  const [isMomentSearching, setIsMomentSearching] = useState(false);
  const [checklistCandidateSearch, setChecklistCandidateSearch] = useState<{ phrase: string; response: MagicSearchResponse } | null>(null);
  const [isChecklistSearching, setIsChecklistSearching] = useState(false);
  const [momentResourceMode, setMomentResourceMode] = useState<SemanticResourceMode>("balanced");
  const [humanLabel, setHumanLabel] = useState("");
  const [newChecklistPhrase, setNewChecklistPhrase] = useState("");
  const analysisStartInFlight = useRef(false);
  const momentMutationInFlight = useRef(false);

  const loadStatus = useCallback(async () => {
    const next = await invoke<MomentAnalysisProgress | null>("moment_timeline_status", { projectId: project.id });
    setProgress((current) => current?.active ? current : next);
    setMomentStatusLoaded(true);
  }, [project.id]);

  const loadTimeline = useCallback(async () => {
    const next = await invoke<MomentTimelineView>("moment_timeline", {
      projectId: project.id,
      limit: momentTimelinePageSize,
      offset: 0,
    });
    setTimeline(next);
    setProgress((current) => current?.active ? current : next.progress);
  }, [project.id]);

  const loadChecklists = useCallback(async () => {
    const next = await invoke<MomentChecklistView[]>("moment_checklists", { projectId: project.id });
    setChecklists(next);
  }, [project.id]);

  const loadDetail = useCallback(async () => {
    if (!initialMomentId) {
      setDetail(null);
      setMomentMedia(null);
      setSearchResponse(null);
      return;
    }
    const [next, media] = await Promise.all([
      invoke<MomentDetailView | null>("moment_detail", {
        projectId: project.id,
        momentId: initialMomentId,
        limit: momentDetailPageSize,
        offset: 0,
      }),
      invoke<VisualMediaPage>("visual_media_page", {
        projectId: project.id,
        query: {
          filter: "photos",
          sort: "captureTime",
          descending: false,
          momentId: initialMomentId,
          limit: momentDetailPageSize,
          offset: 0,
        },
      }),
    ]);
    setDetail(next);
    setMomentMedia(media);
    setSearchResponse(null);
  }, [initialMomentId, project.id]);

  const refresh = useCallback(async () => {
    await Promise.all([loadStatus(), loadTimeline(), loadChecklists(), loadDetail()]);
  }, [loadChecklists, loadDetail, loadStatus, loadTimeline]);

  useEffect(() => { void refresh().catch(toError(onError)); }, [onError, refresh]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ProjectScopedEvent<MomentAnalysisProgress>>("moment-analysis-progress", ({ payload }) => {
      if (payload.projectId !== project.id) return;
      setProgress(payload.progress);
      if (payload.progress.state === "completed" || payload.progress.state === "paused" || payload.progress.state === "failed") {
        void Promise.all([loadTimeline(), loadChecklists(), loadDetail()]).catch(toError(onError));
      }
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [loadChecklists, loadDetail, loadTimeline, onError, project.id]);
  useEffect(() => { setHumanLabel(detail?.moment.label.humanLabel ?? ""); }, [detail?.moment.id, detail?.moment.label.humanLabel]);
  useEffect(() => {
    if (isSemanticResourceMode(progress?.resourceMode)) setMomentResourceMode(progress.resourceMode);
  }, [progress?.resourceMode]);

  const momentActionsBlocked = !momentStatusLoaded || isStarting || isSaving || Boolean(progress?.active);

  function handleMomentActionError(reason: unknown) {
    const message = toMessage(reason);
    if (message.startsWith("Timeline update could not be saved.")) {
      setMomentRecoveryMessage(message);
      onError(null);
      return;
    }
    toError(onError)(reason);
  }

  async function startAnalysis(rebuild: boolean) {
    if (momentActionsBlocked || analysisStartInFlight.current || momentMutationInFlight.current) return;
    try {
      analysisStartInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsStarting(true);
      const next = await invoke<MomentAnalysisProgress>("start_moment_analysis", { projectId: project.id, rebuild, resourceMode: momentResourceMode });
      if (isSemanticResourceMode(next.resourceMode)) setMomentResourceMode(next.resourceMode);
      setProgress(next);
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsStarting(false);
      analysisStartInFlight.current = false;
    }
  }

  async function renameMoment() {
    if (!detail || !humanLabel.trim() || momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("rename_moment", { projectId: project.id, momentId: detail.moment.id, label: humanLabel.trim() });
      await refresh();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function chooseRepresentative(assetId: string) {
    if (!detail || momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("set_moment_human_representative", { projectId: project.id, momentId: detail.moment.id, assetId });
      await refresh();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function mergeMoments(leftMomentId: string, rightMomentId: string) {
    if (momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current || !window.confirm("Merge these adjacent Moments? Your structural override will be retained when local analysis is rebuilt.")) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("merge_adjacent_moments", { projectId: project.id, leftMomentId, rightMomentId });
      onShowTimeline();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function splitMoment(afterAssetId: string) {
    if (!detail || momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current || !window.confirm("Split this Moment after the selected photo? Your structural override will remain protected on future analysis.")) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("split_moment", { projectId: project.id, momentId: detail.moment.id, afterAssetId });
      onShowTimeline();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function addChecklistPhrase(event: FormEvent) {
    event.preventDefault();
    if (!newChecklistPhrase.trim() || momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("create_coverage_checklist_item", { projectId: project.id, input: { phrase: newChecklistPhrase.trim() } });
      setNewChecklistPhrase("");
      await loadChecklists();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function updateCoverage(checklistItemId: string, state: CoverageConfirmationState) {
    if (!detail || momentActionsBlocked || momentMutationInFlight.current || analysisStartInFlight.current) return;
    try {
      momentMutationInFlight.current = true;
      onError(null);
      setMomentRecoveryMessage(null);
      setIsSaving(true);
      await invoke("update_coverage_confirmation", {
        projectId: project.id,
        input: { checklistItemId, state, momentId: detail.moment.id },
      });
      await loadChecklists();
    } catch (reason) {
      handleMomentActionError(reason);
    } finally {
      setIsSaving(false);
      momentMutationInFlight.current = false;
    }
  }

  async function searchWithinMoment(event: FormEvent) {
    event.preventDefault();
    if (!detail || !searchQuery.trim() || isSearching) return;
    try {
      onError(null);
      setIsSearching(true);
      const request: MagicSearchRequest = {
        query: searchQuery.trim(),
        sort: "relevance",
        descending: true,
        limit: 24,
        offset: 0,
        momentId: detail.moment.id,
      };
      setSearchResponse(await invoke<MagicSearchResponse>("magic_search_command", { projectId: project.id, request }));
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      setIsSearching(false);
    }
  }

  async function searchAcrossMoments(event: FormEvent) {
    event.preventDefault();
    if (!momentSearchQuery.trim() || isMomentSearching) return;
    try {
      onError(null);
      setIsMomentSearching(true);
      const request: MomentSearchRequest = { query: momentSearchQuery.trim(), limit: 24 };
      setMomentSearchResponse(await invoke<MomentSearchResponse>("moment_search", { projectId: project.id, request }));
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      setIsMomentSearching(false);
    }
  }

  async function findChecklistCandidates(phrase: string) {
    if (!phrase.trim() || isChecklistSearching) return;
    try {
      onError(null);
      setIsChecklistSearching(true);
      const request: MagicSearchRequest = {
        query: phrase.trim(),
        sort: "relevance",
        descending: true,
        limit: 24,
        offset: 0,
      };
      const response = await invoke<MagicSearchResponse>("magic_search_command", { projectId: project.id, request });
      setChecklistCandidateSearch({ phrase: phrase.trim(), response });
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      setIsChecklistSearching(false);
    }
  }

  const moments = timeline?.moments ?? [];
  const selectedMoment = detail?.moment ?? (initialMomentId ? moments.find((moment) => moment.id === initialMomentId) ?? null : null);
  const checklistItems = checklists.flatMap((checklist) => checklist.items.map((item) => ({ ...item, checklistName: checklist.name })));
  const primaryActionLabel = progress?.timelineReady ? "Update timeline" : "Analyze timeline";

  return <div className="workspace moment-timeline-workspace">
    <section className="project-first-step" aria-label="Moments timeline">
      <p className="eyebrow">MOMENT BRAIN</p>
      <h2>{selectedMoment ? selectedMoment.label.displayLabel : "Moments timeline"}</h2>
      <p className="muted">A local structural timeline based on capture evidence. It does not identify people, assert event stages, change Similar Sets, or make culling decisions.</p>
      <div>
        <button className="secondary" onClick={onReturn}>Return to Project</button>
        {selectedMoment ? <button className="secondary" onClick={onShowTimeline}>All Moments</button> : null}
        <span className="resource-mode" role="group" aria-label="Moment analysis resource mode">
          {([ ["eco", "ECO"], ["balanced", "BALANCED"], ["fast", "FAST"] ] as [SemanticResourceMode, string][]).map(([mode, label]) => <button key={mode} type="button" className={momentResourceMode === mode ? "active" : ""} aria-pressed={momentResourceMode === mode} disabled={momentActionsBlocked} onClick={() => setMomentResourceMode(mode)}>{label}</button>)}
        </span>
        <button className="primary" disabled={momentActionsBlocked} onClick={() => void startAnalysis(false)}>{isStarting ? "Starting…" : progress?.active ? "Analyzing locally…" : primaryActionLabel}</button>
        {progress?.timelineReady ? <button className="secondary" disabled={momentActionsBlocked} onClick={() => void startAnalysis(true)}>Rebuild AI timeline</button> : null}
      </div>
    </section>

    <section className="status-card" aria-label="Moment analysis status">
      <div className="panel-heading"><div><p className="section-label">Local analysis status</p><h2>{momentAnalysisStatusLabel(progress)}</h2><p className="muted">Analysis runs in the background only after you request it. Opening this project never starts or blocks it.</p></div><span className={`badge ${progress?.active ? "running" : progress?.timelineReady ? "completed" : "idle"}`}>{progress?.stage ?? "not started"}</span></div>
      {momentRecoveryMessage ? <p className="error" role="alert">{momentRecoveryMessage}</p> : null}
      {progress ? <><div className="intelligence-progress" role="status" aria-live="polite"><span>{progress.message ?? (progress.timelineReady ? `${progress.momentCount.toLocaleString()} local Moments are ready.` : "No completed local Moment timeline is stored.")}</span>{progress.active ? <small>{progress.completed.toLocaleString()} / {progress.total.toLocaleString()} processed · {progress.errorCount.toLocaleString()} issues · {displayLabel(progress.resourceMode)}</small> : <small>{progress.momentCount.toLocaleString()} Moments · {progress.ungroupedAssetCount.toLocaleString()} photos with insufficient timeline evidence · {displayLabel(progress.resourceMode)}</small>}</div>{progress.lastError ? <details className="advanced"><summary>Developer Details</summary><p>Local persistence diagnostic: {progress.lastError}</p></details> : null}</> : <p className="muted">Checking local Moment analysis status…</p>}
    </section>

    {!selectedMoment ? <>
      <section className="status-card" aria-label="Coverage checklist">
        <div className="panel-heading"><div><p className="section-label">Coverage checklist</p><h2>Photographer-provided expectations</h2><p className="muted">Checklist phrases remain local. They can supply optional label candidates; only you confirm coverage.</p></div></div>
        <form className="visual-toolbar" onSubmit={addChecklistPhrase}><label className="search"><span className="sr-only">Add coverage checklist phrase</span><input value={newChecklistPhrase} onChange={(event) => setNewChecklistPhrase(event.target.value)} placeholder="Add a local checklist phrase" /></label><button className="secondary" type="submit" disabled={momentActionsBlocked || !newChecklistPhrase.trim()}>Add checklist item</button></form>
        {checklistItems.length ? <ul className="culling-reasons">{checklistItems.map((item) => <li key={item.id}><strong>{item.phrase}</strong> · {coverageStateLabel(item.state)}{item.checklistName ? ` · ${item.checklistName}` : ""}<div><button className="secondary" type="button" disabled={isChecklistSearching} onClick={() => void findChecklistCandidates(item.phrase)}>{isChecklistSearching ? "Finding candidates…" : "Find candidates"}</button></div></li>)}</ul> : <p className="muted">No checklist items yet. Add only the expectations you want to review; Moment Brain never invents missing coverage.</p>}
        {checklistCandidateSearch ? <section className="magic-search-results" aria-label="Checklist candidate results"><p className="section-label">Candidate lookup</p><h3>Local candidates for “{checklistCandidateSearch.phrase}”</h3><p className="muted">This is retrieval only. It does not assert coverage or change the checklist; confirm coverage yourself from a Moment.</p><MomentSearchResults response={checklistCandidateSearch.response} emptyMessage="No local candidate photos were returned for this checklist phrase." /></section> : null}
      </section>
      <section className="status-card" aria-label="Search Moments">
        <div className="panel-heading"><div><p className="section-label">Moment search</p><h2>Search local Moments</h2><p className="muted">Cards are ranked only from compatible local Moment centroids. This is retrieval, not proof of an object, person, relationship, or event.</p></div></div>
        <form className="visual-toolbar" onSubmit={searchAcrossMoments}><label className="search"><span className="sr-only">Search local Moments</span><input value={momentSearchQuery} onChange={(event) => setMomentSearchQuery(event.target.value)} placeholder="Describe a visual Moment" /></label><button className="secondary" type="submit" disabled={isMomentSearching || !momentSearchQuery.trim()}>{isMomentSearching ? "Searching…" : "Search Moments"}</button></form>
        {momentSearchResponse ? <MomentCardSearchResults response={momentSearchResponse} onOpen={onOpenMoment} /> : null}
      </section>
      <section aria-label="Timeline moments"><div className="panel-heading"><div><p className="section-label">Timeline</p><h2>{moments.length ? `${timeline?.totalMoments.toLocaleString()} local Moments` : "No timeline ready"}</h2><p className="muted">Each card is a structural sequence, not a claim about a person, relationship, event, or creative quality.</p></div></div>{moments.length ? <div className="media-grid medium" aria-label="Moment cards">{moments.map((moment, index) => <MomentCard key={moment.id} moment={moment} previous={index ? moments[index - 1] : null} onOpen={onOpenMoment} onMerge={(leftMomentId, rightMomentId) => void mergeMoments(leftMomentId, rightMomentId)} disabled={momentActionsBlocked} />)}</div> : <div className="empty">{progress?.active ? "Building the local structural timeline in the background…" : "Choose Analyze timeline when you are ready. This does not change media, decisions, or Similar Sets."}</div>}</section>
      {timeline?.ungroupedAssetCount ? <p className="preparation">{timeline.ungroupedAssetCount.toLocaleString()} photo{timeline.ungroupedAssetCount === 1 ? " has" : "s have"} insufficient capture-time or local evidence for a Moment. They remain in the project and can be reviewed normally.</p> : null}
      {timeline?.timelineGaps.length ? <section className="status-card" aria-label="Observed timeline gaps"><div className="panel-heading"><div><p className="section-label">Observed timeline gaps</p><h3>No recorded capture activity</h3><p className="muted">These are factual intervals in the local capture timeline, not coverage conclusions.</p></div></div><ul className="culling-reasons">{timeline.timelineGaps.map((gap) => <li key={`${gap.startedAt}:${gap.endedAt}`}><strong>{formatDate(gap.startedAt)} – {formatDate(gap.endedAt)}</strong><small>{formatDuration(gap.durationSeconds * 1000)} · {gap.explanation}</small></li>)}</ul></section> : null}
      {timeline?.clockDiagnostics.length ? <details className="advanced"><summary>Camera time diagnostics</summary><p className="muted">These are advisory observations only. CaptureOS does not rewrite capture timestamps.</p>{timeline.clockDiagnostics.map((diagnostic) => <p key={`${diagnostic.cameraLabel}:${diagnostic.summary}`}><strong>{diagnostic.cameraLabel}</strong> · {diagnostic.summary}</p>)}</details> : null}
    </> : <section className="status-card" aria-label="Moment detail">
      <div className="panel-heading"><div><p className="section-label">Moment detail</p><h2>{selectedMoment.label.displayLabel}</h2><p className="muted">{formatMomentTimeRange(selectedMoment.capturedFrom, selectedMoment.capturedTo, selectedMoment.captureTimeState)} · {selectedMoment.assetCount.toLocaleString()} local photos</p></div><button className="primary" onClick={() => onCullMoment(selectedMoment.id)}>Cull this Moment</button></div>
      {selectedMoment.label.humanLabel ? <p className="human-override">Human label: {selectedMoment.label.humanLabel}. The original local suggestion is preserved in Developer Details.</p> : null}
      <form className="visual-toolbar" onSubmit={(event) => { event.preventDefault(); void renameMoment(); }}><label className="search"><span className="sr-only">Human Moment label</span><input value={humanLabel} onChange={(event) => setHumanLabel(event.target.value)} placeholder="Name this Moment" /></label><button className="secondary" type="submit" disabled={momentActionsBlocked || !humanLabel.trim()}>Save human label</button></form>
      {detail ? <>
        {momentMedia?.items.length ? <section className="media-grid medium" aria-label="Moment photos">{momentMedia.items.map((item, index) => <article key={item.assetId}><MediaCard item={item} view="grid" density="medium" onOpen={() => undefined} /><div className="card-caption"><strong>{detail.moment.representative?.assetId === item.assetId && detail.moment.representative.source === "human" ? "Human representative" : detail.moment.representative?.assetId === item.assetId ? "Suggested starting point" : item.filename}</strong><div><button className="secondary" disabled={momentActionsBlocked} onClick={() => void chooseRepresentative(item.assetId)}>Choose representative</button>{index < momentMedia.items.length - 1 ? <button className="secondary" disabled={momentActionsBlocked} onClick={() => void splitMoment(item.assetId)}>Split after this photo</button> : null}</div></div></article>)}</section> : <p className="muted">No bounded local photo page is available for this Moment.</p>}
        {momentMedia?.hasMore ? <p className="preparation">This Moment contains more photos than the current bounded page. Open Smart Cull or the visual grid to continue review without loading the entire catalog here.</p> : null}
        <section className="status-card" aria-label="Magic Search within this Moment"><div className="panel-heading"><div><p className="section-label">Magic Search</p><h3>Search this Moment locally</h3><p className="muted">This request is scoped to the current Moment. Similar Sets remain separate.</p></div></div><form className="visual-toolbar" onSubmit={searchWithinMoment}><label className="search"><span className="sr-only">Magic Search this Moment</span><input value={searchQuery} onChange={(event) => setSearchQuery(event.target.value)} placeholder="Describe a photo in this Moment" /></label><button className="secondary" type="submit" disabled={isSearching || !searchQuery.trim()}>{isSearching ? "Searching…" : "Search this Moment"}</button></form>{searchResponse ? <MomentSearchResults response={searchResponse} /> : null}</section>
        <section className="status-card" aria-label="Moment coverage checklist"><div className="panel-heading"><div><p className="section-label">Coverage confirmation</p><h3>Human confirmation only</h3><p className="muted">A result or label is not proof of coverage. Confirm, review, or mark not covered yourself.</p></div></div>{checklistItems.length ? <ul className="culling-reasons">{checklistItems.map((item) => <li key={item.id}><strong>{item.phrase}</strong> · {coverageStateLabel(item.state)}<div><button className="secondary" disabled={momentActionsBlocked} onClick={() => void updateCoverage(item.id, "confirmed_covered")}>Confirm covered here</button><button className="secondary" disabled={momentActionsBlocked} onClick={() => void updateCoverage(item.id, "needs_review")}>Needs review</button><button className="secondary" disabled={momentActionsBlocked} onClick={() => void updateCoverage(item.id, "not_covered")}>Not covered</button></div></li>)}</ul> : <p className="muted">Add checklist phrases from the timeline overview before confirming coverage.</p>}</section>
        <details className="advanced"><summary>Developer Details</summary><p><strong>Displayed label:</strong> {detail.moment.label.displayLabel}</p><p><strong>AI suggested label:</strong> {detail.moment.label.aiSuggestedLabel ?? "No supported suggestion — Untitled Moment."}</p><p><strong>Label source:</strong> {displayLabel(detail.moment.label.source)}</p><p><strong>Suggestion support:</strong> {displayLabel(detail.moment.label.strength)}</p>{detail.moment.label.evidence.length ? <ul>{detail.moment.label.evidence.map((evidence) => <li key={evidence}>{evidence}</li>)}</ul> : <p>No label evidence is available.</p>}{detail.boundaryEvidence.map((boundary, index) => <div key={`${boundary.summary}:${index}`}><strong>{displayLabel(boundary.strength)} boundary evidence</strong><p>{boundary.summary}</p>{boundary.signals.length ? <ul>{boundary.signals.map((signal) => <li key={signal}>{signal}</li>)}</ul> : null}</div>)}<p>Structural analysis uses local metadata and compatible local embeddings only. It does not identify people or claim an event occurred.</p></details>
      </> : <div className="empty">Loading local Moment details…</div>}
    </section>}
  </div>;
}

function MomentCard({ moment, previous, onOpen, onMerge, disabled }: { moment: MomentSummaryView; previous: MomentSummaryView | null; onOpen: (momentId: string) => void; onMerge: (leftMomentId: string, rightMomentId: string) => void; disabled: boolean }) {
  const representative = moment.representative;
  return <article className="media-card moment-card">
    <button onClick={() => onOpen(moment.id)} aria-label={`Open Moment ${moment.label.displayLabel}`}><span className="media-thumbnail">{representative ? <PreviewImage url={representative.thumbnailPreviewUrl} alt="" fallback={<span className="media-placeholder">◫</span>} /> : <span className="media-placeholder">◫</span>}</span><span className="media-card-title"><strong>{moment.label.displayLabel}</strong><small>{formatMomentTimeRange(moment.capturedFrom, moment.capturedTo, moment.captureTimeState)}</small></span></button>
    <div className="card-caption">
      <strong>{moment.assetCount.toLocaleString()} local photos</strong>
      {moment.label.humanLabel ? <small>Human label</small> : moment.label.aiSuggestedLabel ? <small>Local suggested label</small> : <small>Untitled until evidence supports a label</small>}
      {representative ? <small>{representative.source === "human" ? "Human representative" : "Suggested starting point"} · {representative.filename}</small> : null}
      <small>{moment.similarSetCount.toLocaleString()} Similar Set{moment.similarSetCount === 1 ? "" : "s"} · {moment.starredCount.toLocaleString()} starred</small>
      <small>{moment.keepCount.toLocaleString()} Keep · {moment.rejectCount.toLocaleString()} Reject · {moment.reviewCount.toLocaleString()} Review · {moment.unreviewedCount.toLocaleString()} unreviewed · {moment.technicalIssueCount.toLocaleString()} technical issue{moment.technicalIssueCount === 1 ? "" : "s"}</small>
      {moment.boundaryBefore ? <small>{displayLabel(moment.boundaryBefore.strength)} boundary · {moment.boundaryBefore.summary}</small> : null}
      {moment.hasHumanStructureOverride ? <small>Human split/merge override protected</small> : null}
      <div><button className="secondary" onClick={() => onOpen(moment.id)}>Open Moment</button>{previous ? moment.canMergeWithPrevious ? <button className="secondary" disabled={disabled} onClick={() => onMerge(previous.id, moment.id)}>Merge with previous</button> : <span className="muted">These adjacent Moments are from different local analysis runs. Rebuild AI timeline before merging across this boundary.</span> : null}</div>
    </div>
  </article>;
}

function MomentCardSearchResults({ response, onOpen }: { response: MomentSearchResponse; onOpen: (momentId: string) => void }) {
  const availability = response.identitySearchBlocked
    ? "Identity search is unavailable. CaptureOS does not identify or match people."
    : response.semanticApplied
      ? "Moment cards are ranked from compatible local centroids. This is a retrieval signal, not proof of a concept or event."
      : response.semanticUnavailableReason ?? "Moment-card semantic retrieval is unavailable.";
  return <div className="magic-search-results" aria-live="polite">
    <p className="muted">{response.message ?? availability}</p>
    {response.results.length ? <div className="media-grid medium" aria-label="Moment search cards">{response.results.map((moment) => <MomentCard key={moment.id} moment={moment} previous={null} onOpen={onOpen} onMerge={() => undefined} disabled />)}</div> : <p className="muted">{availability}</p>}
    {response.hasMore ? <p className="preparation">The local Moment-card result list is bounded. Refine the visual description to narrow it further.</p> : null}
  </div>;
}

function MomentSearchResults({ response, emptyMessage = "No local photos in this Moment matched that request." }: { response: MagicSearchResponse; emptyMessage?: string }) {
  const availability = response.semanticApplied
    ? "Ranked from local embedding similarity. This is a retrieval signal, not proof of an object, identity, or event."
    : response.semanticUnavailableReason ?? "Showing local deterministic matching within this Moment.";
  return <div className="magic-search-results"><p className="muted">{response.message ?? availability}</p>{response.results.length ? <ul className="culling-reasons">{response.results.map((result) => <li key={result.item.assetId}><strong>{result.item.filename}</strong> · {result.scoreLabel ? `${result.scoreLabel} local match` : "Local filter match"}<small>{result.explanation}</small>{result.matchedEvidence.length ? <small>{result.matchedEvidence.join(" · ")}</small> : null}</li>)}</ul> : <p className="muted">{emptyMessage}</p>}</div>;
}

type CullingPatch = { decision?: CullingDecision; clearDecision?: boolean; rating?: number; starred?: boolean; note?: string; flags?: string[] };
type CullingHistoryEntry = { assetId: string; before: CullingDecisionView; after: CullingDecisionView };

const cullingFilters: { id: CullingFilter; label: string }[] = [
  { id: "all", label: "All" }, { id: "unreviewed", label: "Unreviewed" }, { id: "keep", label: "Keep" }, { id: "reject", label: "Reject" }, { id: "review", label: "Review" }, { id: "starred", label: "Starred" }, { id: "five_star", label: "5 Star" }, { id: "four_plus", label: "4+ Star" },
  { id: "strong_candidates", label: "Strong candidates" }, { id: "technical_issues", label: "Technical issues" }, { id: "possible_duplicates", label: "Possible duplicates" }, { id: "similar_groups", label: "Similar groups" }, { id: "faces", label: "Faces" }, { id: "blur_review", label: "Blur review" },
];

function CullingWorkspace({ project, momentId, onReturn, onError }: { project: ProjectView; momentId?: string; onReturn: () => void; onError: (message: string | null) => void }) {
  const [workspace, setWorkspace] = useState<CullingWorkspaceView | null>(null);
  const [mode, setMode] = useState<CullingMode>("all_photos");
  const [filter, setFilter] = useState<CullingFilter>("all");
  const [groupId, setGroupId] = useState<string | undefined>();
  const [currentIndex, setCurrentIndex] = useState(0);
  const [surface, setSurface] = useState<"focus" | "grid" | "compare" | "face">("focus");
  const [compareIds, setCompareIds] = useState<string[]>([]);
  const [autoAdvance, setAutoAdvance] = useState(() => localStorage.getItem("captureos-culling-auto-advance") !== "false");
  const [undo, setUndo] = useState<CullingHistoryEntry[]>([]);
  const [redo, setRedo] = useState<CullingHistoryEntry[]>([]);
  const [saving, setSaving] = useState(false);
  const [isFinishing, setIsFinishing] = useState(false);
  const [finished, setFinished] = useState(false);

  const load = useCallback(async () => {
    const query: CullingWorkspaceQuery = {
      mode,
      filter,
      ...(groupId ? { groupId } : {}),
      ...(momentId ? { momentId } : {}),
      limit: 80,
      offset: 0,
    };
    const next = await invoke<CullingWorkspaceView>("culling_workspace_command", { projectId: project.id, query });
    setWorkspace(next);
    setCurrentIndex((previous) => {
      const resumed = next.session.lastAssetId ? next.items.findIndex((item) => item.media.assetId === next.session.lastAssetId) : -1;
      return Math.max(0, Math.min(resumed >= 0 ? resumed : previous, Math.max(0, next.items.length - 1)));
    });
    if (mode === "similar_sets" && !groupId && next.groups[0]) setGroupId(next.groups[0].id);
  }, [filter, groupId, mode, momentId, project.id]);

  useEffect(() => { void load().catch(toError(onError)); }, [load, onError]);
  useEffect(() => { localStorage.setItem("captureos-culling-auto-advance", String(autoAdvance)); }, [autoAdvance]);

  const active = workspace?.items[currentIndex] ?? null;
  const group = active?.similarityGroupId ? workspace?.groups.find((candidate) => candidate.id === active.similarityGroupId) ?? null : null;
  const selectedForCompare = workspace?.items.filter((item) => compareIds.includes(item.media.assetId)) ?? [];

  const persistPosition = useCallback((asset: CullingMediaRow | null) => {
    if (!workspace) return;
    void invoke("update_culling_position_command", {
      projectId: project.id,
      input: { sessionId: workspace.session.id, assetId: asset?.media.assetId, groupId: asset?.similarityGroupId, mode, filterContext: momentId ? `moment:${momentId}:${filter}` : filter },
    }).catch(toError(onError));
  }, [filter, mode, momentId, onError, project.id, workspace]);

  function chooseIndex(nextIndex: number) {
    if (!workspace?.items.length) return;
    const bounded = Math.max(0, Math.min(nextIndex, workspace.items.length - 1));
    setCurrentIndex(bounded);
    persistPosition(workspace.items[bounded] ?? null);
  }

  function localPatch(value: CullingDecisionView, patch: CullingPatch): CullingDecisionView {
    return {
      ...value,
      decision: patch.clearDecision ? null : patch.decision ?? value.decision,
      rating: patch.rating ?? value.rating,
      starred: patch.starred ?? value.starred,
      note: patch.note === undefined ? value.note : patch.note || null,
      flags: patch.flags ?? value.flags,
      updatedAt: new Date().toISOString(),
    };
  }

  function patchWorkspace(assetId: string, decision: CullingDecisionView) {
    setWorkspace((current) => {
      if (!current) return current;
      const affected = current.items.find((item) => item.media.assetId === assetId);
      const previous = affected?.decision;
      const items = current.items.map((item) => item.media.assetId === assetId ? { ...item, decision } : item);
      if (!previous) return { ...current, items };
      const progress = { ...current.progress };
      const changed = (from: boolean, to: boolean) => Number(to) - Number(from);
      progress.reviewed += changed(previous.decision !== null, decision.decision !== null);
      progress.keep += changed(previous.decision === "keep", decision.decision === "keep");
      progress.reject += changed(previous.decision === "reject", decision.decision === "reject");
      progress.review += changed(previous.decision === "review", decision.decision === "review");
      progress.unreviewed = Math.max(0, progress.total - progress.reviewed);
      progress.starred += changed(previous.starred, decision.starred);
      progress.fiveStar += changed(previous.rating === 5, decision.rating === 5);
      let setCompletionDelta = 0;
      const groups = affected?.similarityGroupId
        ? current.groups.map((candidate) => {
          if (candidate.id !== affected.similarityGroupId) return candidate;
          const reviewedCount = Math.max(0, candidate.reviewedCount + changed(previous.decision !== null, decision.decision !== null));
          if (candidate.completionKind === "explicit_user_completion") return { ...candidate, reviewedCount };
          const completed = reviewedCount === candidate.memberCount;
          setCompletionDelta = Number(completed) - Number(candidate.completed);
          return { ...candidate, reviewedCount, completed, completionKind: completed ? "auto_all_reviewed" as const : null };
        })
        : current.groups;
      return { ...current, items, groups, progress: { ...progress, setsReviewed: Math.max(0, progress.setsReviewed + setCompletionDelta) } };
    });
  }

  async function apply(asset: CullingMediaRow, patch: CullingPatch, recordHistory = true, advance = false) {
    if (!workspace) return;
    const before = asset.decision;
    const after = localPatch(before, patch);
    patchWorkspace(asset.media.assetId, after);
    if (recordHistory) {
      setUndo((entries) => [...entries, { assetId: asset.media.assetId, before, after }]);
      setRedo([]);
    }
    if (advance && autoAdvance) chooseIndex(currentIndex + 1);
    try {
      setSaving(true);
      const saved = await invoke<CullingDecisionView>("update_culling_decision_command", {
        projectId: project.id,
        input: { assetId: asset.media.assetId, ...patch, sessionId: workspace.session.id },
      });
      patchWorkspace(asset.media.assetId, saved);
    } catch (reason) {
      patchWorkspace(asset.media.assetId, before);
      toError(onError)(reason);
    } finally {
      setSaving(false);
    }
  }

  async function undoDecision() {
    const entry = undo.at(-1);
    if (!entry || !workspace) return;
    const target = workspace.items.find((item) => item.media.assetId === entry.assetId);
    if (!target) return;
    setUndo((entries) => entries.slice(0, -1));
    setRedo((entries) => [...entries, entry]);
    await apply(target, { decision: entry.before.decision ?? undefined, clearDecision: entry.before.decision === null, rating: entry.before.rating, starred: entry.before.starred, note: entry.before.note ?? "", flags: entry.before.flags }, false);
  }

  async function redoDecision() {
    const entry = redo.at(-1);
    if (!entry || !workspace) return;
    const target = workspace.items.find((item) => item.media.assetId === entry.assetId);
    if (!target) return;
    setRedo((entries) => entries.slice(0, -1));
    setUndo((entries) => [...entries, entry]);
    await apply(target, { decision: entry.after.decision ?? undefined, clearDecision: entry.after.decision === null, rating: entry.after.rating, starred: entry.after.starred, note: entry.after.note ?? "", flags: entry.after.flags }, false);
  }

  function toggleCompare(assetId: string) {
    setCompareIds((selected) => selected.includes(assetId) ? selected.filter((id) => id !== assetId) : selected.length < 4 ? [...selected, assetId] : selected);
  }

  async function applyBulk(patch: CullingPatch) {
    // This remains intentionally conservative: only frames the photographer explicitly selected
    // can be changed, and every member gets its own durable human-decision history entry.
    for (const item of selectedForCompare) await apply(item, patch, true, false);
  }

  async function setRepresentative(asset: CullingMediaRow) {
    if (!asset.similarityGroupId || !workspace) return;
    try {
      await invoke("set_culling_group_representative_command", { projectId: project.id, groupId: asset.similarityGroupId, assetId: asset.media.assetId, sessionId: workspace.session.id });
      await load();
    } catch (reason) { toError(onError)(reason); }
  }

  async function completeGroup() {
    if (!group || !workspace) return;
    setWorkspace((current) => {
      if (!current) return current;
      const groups = current.groups.map((candidate) => candidate.id === group.id
        ? { ...candidate, completed: true, completionKind: "explicit_user_completion" as const }
        : candidate);
      return { ...current, groups, progress: { ...current.progress, setsReviewed: current.progress.setsReviewed + Number(!group.completed) } };
    });
    try {
      await invoke("complete_culling_group_command", { projectId: project.id, groupId: group.id, sessionId: workspace.session.id });
      await load();
    } catch (reason) { await load().catch(toError(onError)); toError(onError)(reason); }
  }

  async function exportReport() {
    const format = "csv";
    const selected = await saveDialog({ title: "Export Culling Report", defaultPath: `${project.name.replaceAll(/[\\/:*?"<>|]/g, "-")}-culling-report.csv`, filters: [{ name: "CSV", extensions: ["csv"] }] });
    if (typeof selected !== "string") return;
    try {
      const count = await invoke<number>("export_culling_report_command", { projectId: project.id, destinationPath: selected, format });
      onError(`Culling report exported locally: ${count} rows. Originals and sidecars were not modified.`);
    } catch (reason) { toError(onError)(reason); }
  }

  async function finish() {
    if (!workspace) return;
    if (workspace.progress.unreviewed > 0 && !window.confirm(`${workspace.progress.unreviewed.toLocaleString()} photos remain unreviewed. Finish this session anyway?`)) return;
    try {
      setIsFinishing(true);
      await invoke("finish_culling_review_command", { projectId: project.id, sessionId: workspace.session.id });
      setFinished(true);
    } catch (reason) { toError(onError)(reason); } finally { setIsFinishing(false); }
  }

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || isTypingElement(event.target)) return;
      if (!active) return;
      const key = event.key.toLowerCase();
      if (key === "k") { event.preventDefault(); void apply(active, { decision: "keep" }, true, true); }
      else if (key === "x") { event.preventDefault(); void apply(active, { decision: "reject" }, true, true); }
      else if (key === "r") { event.preventDefault(); void apply(active, { decision: "review" }, true, true); }
      else if (key === "s") { event.preventDefault(); void apply(active, { starred: !active.decision.starred }); }
      else if (/^[0-5]$/.test(key)) { event.preventDefault(); void apply(active, { rating: Number(key) }); }
      else if (event.key === "ArrowRight") { event.preventDefault(); chooseIndex(currentIndex + 1); }
      else if (event.key === "ArrowLeft") { event.preventDefault(); chooseIndex(currentIndex - 1); }
      else if (event.key === " ") { event.preventDefault(); setSurface((current) => current === "focus" ? "grid" : "focus"); }
      else if (key === "c") { event.preventDefault(); setSurface("compare"); }
      else if (key === "f") { event.preventDefault(); setSurface("face"); }
      else if (key === "g") { event.preventDefault(); setSurface("grid"); }
      else if (key === "u") { event.preventDefault(); void undoDecision(); }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active, currentIndex, workspace, autoAdvance, mode, filter]);

  if (finished && workspace) return <section className="cull-complete"><p className="eyebrow">CULL COMPLETE</p><h2>Review session finished</h2><div className="metrics"><Metric label="Photos reviewed" value={workspace.progress.reviewed} /><Metric label="Kept" value={workspace.progress.keep} /><Metric label="Rejected" value={workspace.progress.reject} /><Metric label="Review" value={workspace.progress.review} /></div><p className="muted">All decisions are local CaptureOS metadata. No source file or sidecar was modified.</p><div><button className="primary" onClick={onReturn}>Return to Project</button><button className="secondary" onClick={() => void exportReport()}>Export Culling Report</button></div></section>;
  if (!workspace) return <div className="empty">Opening the local Culling Workspace…</div>;

  return <div className="culling-workspace">
    <header className="culling-topbar"><div><p className="eyebrow">SMART CULL</p><h2>{project.name}</h2><p className="muted">{momentId ? "Moment-scoped human review · " : ""}{workspace.progress.reviewed.toLocaleString()} / {workspace.progress.total.toLocaleString()} reviewed · Human decisions are authoritative.</p></div><div className="culling-actions"><label>Mode <select value={mode} onChange={(event) => { setMode(event.target.value as CullingMode); setGroupId(undefined); setCurrentIndex(0); }}><option value="all_photos">All Photos</option><option value="similar_sets">Similar Sets</option><option value="ai_review_queue">AI Review Queue</option></select></label><button className="secondary" disabled={!undo.length || saving} onClick={() => void undoDecision()}>Undo</button><button className="secondary" disabled={!redo.length || saving} onClick={() => void redoDecision()}>Redo</button><button className="primary" disabled={isFinishing} onClick={() => void finish()}>{isFinishing ? "Finishing…" : "Finish Review"}</button></div></header>
    <section className="culling-progress" aria-label="Culling progress"><Metric label="Reviewed" value={`${workspace.progress.reviewed} / ${workspace.progress.total}`} /><Metric label="Keep" value={workspace.progress.keep} /><Metric label="Reject" value={workspace.progress.reject} /><Metric label="Review" value={workspace.progress.review} /><Metric label="Unreviewed" value={workspace.progress.unreviewed} /><Metric label="Starred" value={workspace.progress.starred} /><Metric label="Sets reviewed" value={`${workspace.progress.setsReviewed} / ${workspace.progress.setsTotal}`} /></section>
    <p className="agreement" aria-label="AI and human agreement">AI/Human Agreement · strong candidate → kept {workspace.progress.strongCandidateKept} · technical issue → kept {workspace.progress.technicalIssueKept} · strong candidate → rejected {workspace.progress.strongCandidateRejected}. These are local workflow observations, not accuracy.</p>
    <nav className="culling-filters" aria-label="Culling filters">{cullingFilters.map((item) => <button key={item.id} className={filter === item.id ? "active" : ""} onClick={() => { setFilter(item.id); setCurrentIndex(0); }}>{item.label}</button>)}<label className="auto-advance"><input type="checkbox" checked={autoAdvance} onChange={(event) => setAutoAdvance(event.target.checked)} /> Auto Advance</label></nav>
    {mode === "similar_sets" ? <aside className="set-queue" aria-label="Similar sets"><p className="section-label">Similar Sets</p>{workspace.groups.length ? workspace.groups.map((candidate) => <button key={candidate.id} className={groupId === candidate.id ? "active" : ""} onClick={() => { setGroupId(candidate.id); setCurrentIndex(0); }}>{candidate.memberCount} frames <small>{candidate.completed ? "✓ Set Complete" : `${candidate.reviewedCount} reviewed`}</small></button>) : <p className="muted">No stored similar sets yet. You can still review All Photos while local analysis finishes.</p>}</aside> : null}
    {group ? <section className="set-summary"><div><p className="section-label">{group.kind.replaceAll("_", " ")} · {group.memberCount} · {group.reviewedCount} / {group.memberCount} reviewed</p><span>AI suggested starting point</span><strong>{group.aiRepresentativeFilename}</strong>{group.studioStartingPointAssetId ? <><span>Studio Brain starting point</span><strong>{group.studioStartingPointReason ?? "Local comparable-set advice"}</strong></> : null}<span>Your representative</span><strong>{group.humanRepresentativeFilename ?? "Not chosen"}</strong><small>{group.completed ? "✓ Set Complete" : "Set remains open"}</small></div><button className="secondary" disabled={group.completed} onClick={() => void completeGroup()}>{group.completed ? "✓ Set Complete" : "Mark Set Complete"}</button></section> : null}
    {selectedForCompare.length ? <section className="culling-bulk" aria-label="Selected frames"><strong>{selectedForCompare.length} selected</strong><button onClick={() => void applyBulk({ decision: "keep" })}>Keep selected</button><button onClick={() => void applyBulk({ decision: "reject" })}>Reject selected</button><button onClick={() => void applyBulk({ decision: "review" })}>Review selected</button><button onClick={() => void applyBulk({ rating: 5 })}>Rate 5</button><button onClick={() => setCompareIds([])}>Clear selection</button></section> : null}
    <main className="culling-stage">
      <aside className="culling-sidebar"><p className="section-label">Queue</p>{workspace.items.slice(0, 80).map((item, index) => <button key={item.media.assetId} className={index === currentIndex ? "active" : ""} onClick={() => chooseIndex(index)}><span>{decisionGlyph(item.decision.decision)} {item.media.filename}</span><small>{item.decision.rating ? `${item.decision.rating}★` : item.decision.starred ? "★" : item.media.intelligence.recommendation?.replaceAll("_", " ") ?? "unreviewed"}</small></button>)}</aside>
      <section className="culling-center" aria-live="polite">
        <nav className="culling-surface-tabs"><button className={surface === "focus" ? "active" : ""} onClick={() => setSurface("focus")}>Focus</button><button className={surface === "grid" ? "active" : ""} onClick={() => setSurface("grid")}>Set Grid</button><button className={surface === "compare" ? "active" : ""} onClick={() => setSurface("compare")}>Compare {compareIds.length ? `(${compareIds.length})` : ""}</button><button className={surface === "face" ? "active" : ""} onClick={() => setSurface("face")}>Face View</button></nav>
        {surface === "focus" && active ? <CullingFocus item={active} onPrevious={() => chooseIndex(currentIndex - 1)} onNext={() => chooseIndex(currentIndex + 1)} /> : null}
        {surface === "grid" ? <CullingSetGrid items={workspace.items} activeId={active?.media.assetId} compareIds={compareIds} onOpen={(assetId) => chooseIndex(workspace.items.findIndex((item) => item.media.assetId === assetId))} onCompare={toggleCompare} /> : null}
        {surface === "compare" ? <CullingCompare items={selectedForCompare} onOpen={(assetId) => chooseIndex(workspace.items.findIndex((item) => item.media.assetId === assetId))} onNext={() => chooseIndex(currentIndex + 1)} /> : null}
        {surface === "face" ? <CullingFaceView items={selectedForCompare.length ? selectedForCompare : workspace.items} /> : null}
      </section>
      <aside className="culling-inspector">{active ? <><p className="section-label">Decision & Evidence</p><h3>{active.media.filename}</h3><div className="quick-decisions"><button className={active.decision.decision === "keep" ? "active keep" : ""} onClick={() => void apply(active, { decision: "keep" }, true, true)}>K Keep</button><button className={active.decision.decision === "reject" ? "active reject" : ""} onClick={() => void apply(active, { decision: "reject" }, true, true)}>X Reject</button><button className={active.decision.decision === "review" ? "active review" : ""} onClick={() => void apply(active, { decision: "review" }, true, true)}>R Review</button><button className={active.decision.starred ? "active" : ""} onClick={() => void apply(active, { starred: !active.decision.starred })}>S {active.decision.starred ? "Starred" : "Star"}</button></div><div className="rating-controls" aria-label="Rating">{[1, 2, 3, 4, 5].map((rating) => <button key={rating} className={active.decision.rating === rating ? "active" : ""} onClick={() => void apply(active, { rating })}>{rating}</button>)}<button onClick={() => void apply(active, { rating: 0 })}>0 clear</button></div><label className="culling-note">Note <textarea value={active.decision.note ?? ""} onChange={(event) => patchWorkspace(active.media.assetId, localPatch(active.decision, { note: event.target.value }))} onBlur={(event) => void apply(active, { note: event.target.value }, false)} placeholder="Client requested this one" /></label><dl className="culling-evidence"><Detail label="AI recommendation" value={recommendationLabel(active.media.intelligence.recommendation) ?? "No current recommendation"} /><Detail label="Technical score" value={active.media.intelligence.technicalQualityScore === null ? "Unavailable" : `${Math.round(active.media.intelligence.technicalQualityScore)} / 100`} /><Detail label="Sharpness" value={displayLabel(active.media.intelligence.sharpnessBand ?? "unavailable")} /><Detail label="Blur" value={displayLabel(active.media.intelligence.blurLevel ?? "unavailable")} /><Detail label="Faces" value={active.media.intelligence.faceCount} /><Detail label="Eye state" value={active.media.intelligence.openEyesCount ? "Open evidence available" : "NOT_ANALYZABLE / unavailable"} /></dl><StudioRecommendationPanel recommendation={active.studioBrain} />{active.relativeEvidence.length ? <ul className="culling-reasons">{active.relativeEvidence.map((reason) => <li key={reason}>{reason}</li>)}</ul> : null}{active.similarityGroupId ? <button className="secondary" onClick={() => void setRepresentative(active)}>{active.isHumanRepresentative ? `Human representative: ${active.media.filename}` : "Choose as human representative"}</button> : null}{active.decision.decision && active.media.intelligence.recommendation ? <p className="human-override">AI recommendation: {recommendationLabel(active.media.intelligence.recommendation)}<br />Your decision: {humanDecisionLabel(active.decision.decision)}</p> : null}</> : <p className="muted">No media matches this culling view.</p>}<details><summary>Keyboard Shortcuts</summary><p>K Keep · X Reject · R Review · S Star · 1–5 rate · 0 clear rating · ←/→ navigate · Space focus/grid · C compare · F Face View · G Set Grid · U Undo</p></details></aside>
    </main>
    <nav className="culling-filmstrip" aria-label="Culling filmstrip">{workspace.items.map((item, index) => { const compareOrder = compareIds.indexOf(item.media.assetId) + 1; return <button key={item.media.assetId} className={`${index === currentIndex ? "active" : ""} ${compareOrder ? "compare-selected" : ""}`} onClick={() => chooseIndex(index)} onDoubleClick={() => toggleCompare(item.media.assetId)} aria-label={`Open ${item.media.filename}; ${compareOrder ? `Compare selection ${compareOrder}` : "double click to select for compare"}`}><PreviewImage url={item.media.thumbnailPreviewUrl} alt="" fallback={<span>{mediaSymbol(item.media)}</span>} /><small className="decision-state" title={`Decision: ${item.decision.decision ? humanDecisionLabel(item.decision.decision) : "Unreviewed"}`}>{decisionGlyph(item.decision.decision)}</small>{compareOrder ? <b className="compare-order" aria-hidden="true">{["①", "②", "③", "④"][compareOrder - 1]}</b> : null}</button>; })}</nav>
    <footer className="culling-footer"><span>{saving ? "Saving local decision…" : "Decisions persist locally in the background."}</span><button className="secondary" onClick={() => void exportReport()}>Export Culling Report</button><button className="secondary" onClick={onReturn}>Return to Project</button></footer>
  </div>;
}

function StudioRecommendationPanel({ recommendation }: { recommendation: CullingMediaRow["studioBrain"] }) {
  if (!recommendation) return <section className="studio-recommendation"><p className="section-label">Studio Brain</p><p className="muted">Personalized advice is unavailable until a valid local model is ready.</p></section>;
  const label = studioRecommendationLabel(recommendation.recommendation);
  return <section className="studio-recommendation"><p className="section-label">Studio Brain</p><strong>{label}</strong><p className="muted">{recommendation.confidenceBand === "unavailable" ? "Not enough local evidence for a confident personalized recommendation." : `${displayLabel(recommendation.confidenceBand)} confidence · ${recommendation.agreement === "differs" ? "Differs from generic technical advice" : recommendation.agreement === "agrees" ? "Agrees with generic technical advice" : "Generic comparison unavailable"}`}</p>{recommendation.explanationFactors.length ? <ul className="culling-reasons">{recommendation.explanationFactors.map((factor) => <li key={factor}>{factor}</li>)}</ul> : null}<small>Advisory only — it does not change your decision, rating, star, representative, Moment, or media.</small></section>;
}

function CullingFocus({ item, onPrevious, onNext }: { item: CullingMediaRow; onPrevious: () => void; onNext: () => void }) {
  const preview = item.media.previewPreviewUrl ?? item.media.mediumPreviewUrl ?? item.media.thumbnailPreviewUrl;
  return <div className="culling-focus"><header><button onClick={onPrevious} aria-label="Previous culling photo">←</button><strong>{item.media.filename}</strong><button onClick={onNext} aria-label="Next culling photo">→</button></header><div className="culling-photo">{preview ? <PreviewImage url={preview} alt={item.media.filename} fallback={<span className="media-placeholder">{mediaSymbol(item.media)}</span>} /> : <span className="media-placeholder">{mediaSymbol(item.media)}</span>}</div><span className={`decision-overlay ${item.decision.decision ?? ""}`}>{item.decision.decision ? humanDecisionLabel(item.decision.decision).toUpperCase() : ""}</span></div>;
}

function CullingSetGrid({ items, activeId, compareIds, onOpen, onCompare }: { items: CullingMediaRow[]; activeId?: string; compareIds: string[]; onOpen: (assetId: string) => void; onCompare: (assetId: string) => void }) {
  return <div className="culling-set-grid">{items.map((item) => <article key={item.media.assetId} className={item.media.assetId === activeId ? "active" : ""}><button onClick={() => onOpen(item.media.assetId)}><PreviewImage url={item.media.mediumPreviewUrl ?? item.media.thumbnailPreviewUrl} alt={item.media.filename} fallback={<span className="media-placeholder">{mediaSymbol(item.media)}</span>} /></button><footer><strong>{item.media.filename}</strong><small>{item.isAiRepresentative ? "Suggested starting point" : item.isHumanRepresentative ? "Human representative" : recommendationLabel(item.media.intelligence.recommendation) ?? "Comparable technical candidate"}</small><label><input type="checkbox" checked={compareIds.includes(item.media.assetId)} onChange={() => onCompare(item.media.assetId)} /> Compare</label></footer></article>)}</div>;
}

function CullingCompare({ items, onOpen, onNext }: { items: CullingMediaRow[]; onOpen: (assetId: string) => void; onNext: () => void }) {
  const [zoom, setZoom] = useState<"fit" | "full">("fit");
  const [showEvidence, setShowEvidence] = useState(true);
  if (items.length < 2) return <div className="culling-empty-state"><h3>Select 2–4 related frames</h3><p>Double-click filmstrip frames or use Set Grid to add them. The comparison is visual and technical; CaptureOS does not judge expression or composition.</p></div>;
  return <div className="culling-compare"><header className="compare-controls"><strong>Compare {items.length} related frames</strong><div><button className={zoom === "fit" ? "active" : ""} onClick={() => setZoom("fit")}>Fit</button><button className={zoom === "full" ? "active" : ""} onClick={() => setZoom("full")}>100%</button><button onClick={onNext}>Next candidate</button><button onClick={() => setShowEvidence((current) => !current)}>{showEvidence ? "Hide evidence" : "Show evidence"}</button></div></header><div className={`compare-photos count-${items.length} ${zoom}`}>{items.map((item) => <button key={item.media.assetId} onClick={() => onOpen(item.media.assetId)}><PreviewImage url={item.media.previewPreviewUrl ?? item.media.mediumPreviewUrl} alt={item.media.filename} fallback={<span className="media-placeholder">{mediaSymbol(item.media)}</span>} /><strong>{item.media.filename}</strong></button>)}</div>{showEvidence ? <table><thead><tr><th>Technical evidence</th>{items.map((item) => <th key={item.media.assetId}>{item.media.filename}</th>)}</tr></thead><tbody><tr><th>Sharpness</th>{items.map((item) => <td key={item.media.assetId}>{item.media.intelligence.technicalQualityScore === null ? "—" : Math.round(item.media.intelligence.technicalQualityScore)}</td>)}</tr><tr><th>Face sharpness</th>{items.map((item) => <td key={item.media.assetId}>{item.faces[0]?.faceSharpness === null || item.faces[0]?.faceSharpness === undefined ? "—" : Math.round(item.faces[0].faceSharpness)}</td>)}</tr><tr><th>Exposure / blur</th>{items.map((item) => <td key={item.media.assetId}>{displayLabel(item.media.intelligence.technicalQualityBand ?? "unavailable")} · {displayLabel(item.media.intelligence.blurLevel ?? "unavailable")}</td>)}</tr><tr><th>Faces / eyes</th>{items.map((item) => <td key={item.media.assetId}>{item.faces.length || "—"} · {item.faces.some((face) => face.eyeState === "open") ? "open evidence" : "—"}</td>)}</tr></tbody></table> : null}<p className="muted">Fit and 100% apply the same zoom to each cached preview. Synchronized pointer panning is deferred until the preview canvas gains direct pointer transforms.</p></div>;
}

function CullingFaceView({ items }: { items: CullingMediaRow[] }) {
  const withFaces = items.filter((item) => item.faces.length);
  if (!withFaces.length) return <div className="culling-empty-state"><h3>Face View unavailable for these frames</h3><p>No current local face detection boxes are available. This is not a claim that a person is absent.</p></div>;
  return <section className="culling-face-view"><header><p className="section-label">Face View</p><h3>Detector crops across related frames</h3><p className="muted">Face positions are local observations only. CaptureOS does not identify or match people.</p></header><div>{withFaces.map((item) => <article key={item.media.assetId}><strong>{item.media.filename}</strong><div className="face-crops">{item.faces.map((face, index) => <div key={face.id}><FaceCrop url={item.media.mediumPreviewUrl ?? item.media.thumbnailPreviewUrl} face={face} label={`Face ${index + 1} from ${item.media.filename}`} /><small>Face {index + 1} · {face.faceSharpness === null ? "sharpness unavailable" : `${Math.round(face.faceSharpness)} sharpness`} · {face.eyeState === "not_analyzable" ? "Eyes not analyzable" : displayLabel(face.eyeState)}</small></div>)}</div></article>)}</div></section>;
}

function decisionGlyph(decision: CullingDecision | null) { return decision === "keep" ? "✓" : decision === "reject" ? "×" : decision === "review" ? "?" : "·"; }
function isTypingElement(target: EventTarget | null) { return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement || (target instanceof HTMLElement && target.isContentEditable); }

function CaptureIntelligenceControls({ progress, resourceMode, isStarting, isPausing, onResourceMode, onStart, onPause }: {
  progress: CaptureIntelligenceProgress | null;
  resourceMode: "eco" | "balanced" | "fast";
  isStarting: boolean;
  isPausing: boolean;
  onResourceMode: (mode: "eco" | "balanced" | "fast") => void;
  onStart: () => void;
  onPause: () => void;
}) {
  const running = progress?.state === "running";
  const resumable = progress?.state === "paused" || progress?.state === "interrupted" || progress?.state === "failed";
  const actionLabel = resumable ? "Resume analysis" : progress?.state === "completed" ? "Analyze new media" : "Analyze local photos";
  const completed = progress?.itemsCompleted ?? 0;
  const total = progress?.itemsTotal ?? 0;

  return <section className="intelligence-controls" aria-label="Capture Intelligence controls">
    <div className="intelligence-controls-copy">
      <p className="section-label">Capture Intelligence</p>
      <h2>Local technical evidence</h2>
      <p className="muted">Resolves eligible local analysis images automatically, reusing safe cache artifacts or creating one read-only from an available copy. Evidence is advisory, never an artistic judgment or an automatic cull.</p>
    </div>
    <div className="intelligence-controls-actions">
      <div className="resource-mode" role="group" aria-label="Analysis resource mode">
        {([ ["eco", "ECO"], ["balanced", "BALANCED"], ["fast", "FAST"] ] as ["eco" | "balanced" | "fast", string][]).map(([mode, label]) => <button key={mode} className={resourceMode === mode ? "active" : ""} aria-pressed={resourceMode === mode} disabled={running || isStarting} onClick={() => onResourceMode(mode)}>{label}</button>)}
      </div>
      {running ? <button className="secondary" disabled={isPausing} onClick={onPause}>{isPausing ? "Pausing…" : "Pause analysis"}</button> : <button className="primary" disabled={isStarting} onClick={onStart}>{isStarting ? "Starting…" : actionLabel}</button>}
    </div>
    <div className="intelligence-progress" role="status" aria-live="polite">
      {progress ? <>
        <strong>{intelligenceStateLabel(progress.state)}</strong>
        <span>{completed.toLocaleString()} / {total.toLocaleString()} analyzed{running ? ` · ${progress.currentStageDetail ?? progress.stage}` : ""}</span>
        {progress.message ? <small>{progress.message}</small> : null}
        {!running && progress.state !== "queued" ? <small>{(progress.readyCount ?? 0).toLocaleString()} ready · {(progress.unsupportedCount ?? 0).toLocaleString()} unsupported · {(progress.corruptCount ?? 0).toLocaleString()} corrupt · {(progress.needsOriginalCount ?? 0).toLocaleString()} needs original · {(progress.failedCount ?? 0).toLocaleString()} failed</small> : null}
      </> : <><strong>Not analyzed</strong><span>No completed local analysis is stored for this project.</span></>}
    </div>
  </section>;
}

function MagicSearchControls({
  isOpen,
  onToggle,
  inputRef,
  query,
  onQuery,
  sort,
  onSort,
  descending,
  onDescending,
  onSearch,
  onExample,
  onClear,
  isSearching,
  response,
  mode,
  status,
  resourceMode,
  onResourceMode,
  isStartingIndex,
  isPausingIndex,
  onStartIndex,
  onPauseIndex,
  history,
  onHistory,
  onClearHistory,
  error,
}: {
  isOpen: boolean;
  onToggle: () => void;
  inputRef: RefObject<HTMLInputElement>;
  query: string;
  onQuery: (value: string) => void;
  sort: MagicSearchSort;
  onSort: (value: MagicSearchSort) => void;
  descending: boolean;
  onDescending: () => void;
  onSearch: () => void;
  onExample: (query: string) => void;
  onClear: () => void;
  isSearching: boolean;
  response: MagicSearchResponse | null;
  mode: "query" | "similar";
  status: SemanticIndexProgress | null;
  resourceMode: SemanticResourceMode;
  onResourceMode: (mode: SemanticResourceMode) => void;
  isStartingIndex: boolean;
  isPausingIndex: boolean;
  onStartIndex: () => void;
  onPauseIndex: () => void;
  history: MagicSearchHistoryEntry[];
  onHistory: (query: string) => void;
  onClearHistory: () => void;
  error: string | null;
}) {
  const modelInstalled = status?.model.installed === true;
  const modelMessage = status?.model.message;
  const modelNotInstalled = status?.model.installed === false
    && (!modelMessage || modelMessage.startsWith("Semantic model not installed"));
  const unavailableHeading = status === null
    ? "Checking local semantic model"
    : modelNotInstalled ? "Semantic model not installed" : "Semantic model unavailable";
  const indexActive = status?.active === true;
  const canStartIndex = modelInstalled && !indexActive && !isStartingIndex;
  const modelIdentity = status?.model.identity;
  const statusText = status === null
    ? "Checking local semantic model status…"
    : modelInstalled
      ? status.indexReady
        ? `${status.indexEmbeddingCount.toLocaleString()} local image embeddings indexed`
        : "Local model is installed; index eligible still-photo previews to enable semantic matching."
      : modelMessage ?? "Semantic model not installed. Metadata and technical filters remain available.";

  return <section className="status-card" aria-label="Magic Search">
    <div className="panel-heading">
      <div>
        <p className="section-label">Magic Search</p>
        <h2>Search this project locally</h2>
        <p className="muted">Still-photo semantic matching and deterministic filters stay on this device. Similar Sets remain a separate related-frame tool.</p>
      </div>
      <button className="secondary" aria-expanded={isOpen} onClick={onToggle}>{isOpen ? "Hide search" : "Open search"}</button>
    </div>
    {isOpen ? <>
      <form className="visual-toolbar" onSubmit={(event) => { event.preventDefault(); onSearch(); }}>
        <label className="search"><span className="sr-only">Magic Search this project</span><input ref={inputRef} value={query} onChange={(event) => onQuery(event.target.value)} placeholder="Describe a photo or add filters" /></label>
        <select aria-label="Magic Search sort" value={sort} onChange={(event) => onSort(event.target.value as MagicSearchSort)}>
          <option value="relevance">Relevance</option>
          <option value="captureTime">Capture time</option>
          <option value="technicalQuality">Technical quality</option>
          <option value="rating">Rating</option>
        </select>
        <button className="secondary" type="button" aria-label="Reverse Magic Search sort direction" onClick={onDescending}>{descending ? "Descending" : "Ascending"}</button>
        <button className="primary" type="submit" disabled={isSearching || !query.trim()}>{isSearching ? "Searching…" : modelInstalled ? "Search local photos" : "Search filters"}</button>
        {response ? <button className="secondary" type="button" onClick={onClear}>Clear results</button> : null}
      </form>
      <div className="visual-filters" aria-label="Magic Search examples">
        <span className="section-label">Try</span>
        {(["2 faces", "5 stars", "kept", "sharp", "camera Sony"] as const).map((example) => <button key={example} type="button" onClick={() => onExample(example)}>{example}</button>)}
      </div>
      <div className="intelligence-controls-actions">
        <div className="resource-mode" role="group" aria-label="Semantic index resource mode">
          {([ ["eco", "ECO"], ["balanced", "BALANCED"], ["fast", "FAST"] ] as [SemanticResourceMode, string][]).map(([nextMode, label]) => <button key={nextMode} type="button" className={resourceMode === nextMode ? "active" : ""} aria-pressed={resourceMode === nextMode} disabled={!modelInstalled || indexActive || isStartingIndex} onClick={() => onResourceMode(nextMode)}>{label}</button>)}
        </div>
        {indexActive ? <button className="secondary" type="button" disabled={isPausingIndex} onClick={onPauseIndex}>{isPausingIndex ? "Pausing…" : "Pause indexing"}</button> : <button className="secondary" type="button" disabled={!canStartIndex} title={modelInstalled ? undefined : "An approved local semantic model pack is required."} onClick={onStartIndex}>{isStartingIndex ? "Starting…" : status?.indexReady ? "Index new photos" : "Index local photos"}</button>}
      </div>
      <div className="intelligence-progress" role="status" aria-live="polite">
        <strong>{modelInstalled ? status?.indexReady ? "Semantic index ready" : indexActive ? "Indexing locally" : "Semantic index not ready" : unavailableHeading}</strong>
        <span>{statusText}</span>
        {!modelInstalled && modelMessage ? <small>{modelMessage}</small> : null}
        {status?.active ? <small>{status.completed.toLocaleString()} / {status.total.toLocaleString()} · {status.stage}</small> : null}
        {!status?.active && status ? <small>{status.counts.ready.toLocaleString()} ready · {status.counts.unsupported.toLocaleString()} unsupported · {status.counts.corrupt.toLocaleString()} corrupt · {status.counts.needsOriginal.toLocaleString()} needs original · {status.counts.failed.toLocaleString()} failed</small> : null}
      </div>
      {modelNotInstalled ? <aside className="preparation" aria-label="Local semantic model installation">
        <strong>Optional local model pack</strong>
        <span>Google SigLIP Base Patch16-224 source · Apache-2.0 model-repository metadata · about 813 MB download / 816 MB installed.</span>
        <small>Install it explicitly with the documented local pack workflow. CaptureOS never downloads a semantic model automatically.</small>
      </aside> : null}
      {modelIdentity ? <details className="advanced"><summary>Local model details</summary><small>{modelIdentity.provider} · {modelIdentity.modelId} {modelIdentity.modelVersion}</small><small>{modelIdentity.embeddingDimension ? `${modelIdentity.embeddingDimension}-dimension image/text embedding space` : "Embedding dimension recorded locally"}</small>{modelIdentity.installedBytes !== null ? <small>{formatSize(modelIdentity.installedBytes)} checksum-validated local pack</small> : null}{modelIdentity.licenseUrl ? <small>License {modelIdentity.licenseUrl}</small> : null}</details> : null}
      {history.length ? <details className="advanced"><summary>Local search history ({history.length})</summary><div className="visual-filters">{history.map((entry) => <button key={entry.id} type="button" onClick={() => onHistory(entry.query)}>{entry.query}</button>)}<button className="secondary" type="button" onClick={onClearHistory}>Clear history</button></div></details> : null}
      {mode === "similar" && response ? <p className="preparation">Find Similar uses the separate local semantic nearest-neighbor index. It does not create or alter Similar Sets.</p> : null}
      {error ? <p className="error intelligence-error" role="alert">{error}</p> : null}
    </> : null}
  </section>;
}

function MagicSearchResults({ response, mode, density, view, showInspector, selected, onOpen, onFindSimilar, isSearching, onLoadMore, onOpenViewer, onOpenSimilarityGroup, isLoadingSimilarityGroup, onSaveHumanDecision, isSavingDecision }: {
  response: MagicSearchResponse;
  mode: "query" | "similar";
  density: "small" | "medium" | "large";
  view: "grid" | "list";
  showInspector: boolean;
  selected: MediaAssetDetail | null;
  onOpen: (item: VisualMediaRow) => void;
  onFindSimilar: (assetId: string) => void;
  isSearching: boolean;
  onLoadMore: () => void;
  onOpenViewer?: () => void;
  onOpenSimilarityGroup: (assetId: string) => void;
  isLoadingSimilarityGroup: boolean;
  onSaveHumanDecision: (assetId: string, decision: "keep" | "review" | "reject") => void;
  isSavingDecision: boolean;
}) {
  const searchLabel = mode === "similar" ? "Find Similar" : "Magic Search";
  const resultBadge = response.identitySearchBlocked
    ? "Identity unavailable"
    : response.semanticApplied ? "Semantic matches" : "Filters only";
  const availabilityMessage = response.identitySearchBlocked
    ? "Identity search is unavailable. CaptureOS does not identify or match people."
    : response.semanticApplied
    ? "Ranked from local image/text embedding similarity. A match is not an object, identity, or localized detection claim."
    : response.semanticAvailable
      ? "Showing deterministic local filter matches; this query did not request semantic ranking."
      : response.semanticUnavailableReason ?? "Semantic search is unavailable. Deterministic local filters remain available.";

  return <section aria-label={`${searchLabel} results`}>
    <div className="panel-heading">
      <div>
        <p className="section-label">{searchLabel}</p>
        <h2>{response.results.length ? `${response.results.length.toLocaleString()} local result${response.results.length === 1 ? "" : "s"}` : "No local matches"}</h2>
        <p className="muted">{response.message ?? availabilityMessage}</p>
      </div>
      <span className={`badge ${response.semanticApplied ? "completed" : ""}`}>{resultBadge}</span>
    </div>
    {response.parsedFilters.chips.length ? <div className="visual-filters" aria-label="Applied Magic Search filters">{response.parsedFilters.chips.map((chip) => <span className="status" key={chip}>{chip}</span>)}</div> : null}
    {response.results.length ? <div className={`visual-layout ${showInspector ? "with-inspector" : ""}`}>
      <section className={`media-grid ${density} ${view}`} aria-label={`${searchLabel} media results`}>
        {response.results.map((result) => <MagicSearchResultCard key={result.item.assetId} result={result} semanticApplied={response.semanticApplied} onOpen={onOpen} onFindSimilar={onFindSimilar} findSimilarAvailable={response.semanticAvailable} isSearching={isSearching} view={view} density={density} />)}
        {response.hasMore ? <div className="load-more visual-load-more"><button onClick={onLoadMore} disabled={isSearching}>{isSearching ? "Loading…" : "Load more results"}</button></div> : null}
      </section>
      {showInspector ? <Inspector detail={selected} onOpen={onOpenViewer} onOpenSimilarityGroup={onOpenSimilarityGroup} isLoadingSimilarityGroup={isLoadingSimilarityGroup} onSaveHumanDecision={onSaveHumanDecision} isSavingDecision={isSavingDecision} /> : null}
    </div> : <div className="empty">{availabilityMessage}</div>}
  </section>;
}

function MagicSearchResultCard({ result, semanticApplied, onOpen, onFindSimilar, findSimilarAvailable, isSearching, view, density }: {
  result: MagicSearchResult;
  semanticApplied: boolean;
  onOpen: (item: VisualMediaRow) => void;
  onFindSimilar: (assetId: string) => void;
  findSimilarAvailable: boolean;
  isSearching: boolean;
  view: "grid" | "list";
  density: "small" | "medium" | "large";
}) {
  const score = semanticApplied && result.scoreLabel ? `${result.scoreLabel} semantic match` : "Local filter match";
  const semanticScore = semanticApplied && typeof result.semanticScore === "number" && Number.isFinite(result.semanticScore)
    ? result.semanticScore.toFixed(3)
    : null;
  return <article>
    <MediaCard item={result.item} view={view} density={density} onOpen={onOpen} />
    <div className="card-caption">
      <strong>{score}</strong>
      <small>{result.explanation}</small>
      {semanticScore ? <small>Local similarity {semanticScore} · ranking signal only, not confidence or proof of an object, person, or identity.</small> : null}
      {result.matchedEvidence.length ? <small>{result.matchedEvidence.join(" · ")}</small> : null}
      <button className="secondary" type="button" disabled={!findSimilarAvailable || isSearching} title={findSimilarAvailable ? "Search local semantic neighbors" : "Find Similar requires an available local semantic model."} onClick={() => onFindSimilar(result.item.assetId)}>{findSimilarAvailable ? "Find Similar" : "Find Similar unavailable"}</button>
    </div>
  </article>;
}

type DraftSource = { label: string; selectedPath: string };

function IngestWorkspace({ project, history, report, isIngesting, onReport, onHistory, onIngesting, onError }: { project: ProjectView; history: IngestJobSummary[]; report: IngestReport | null; isIngesting: boolean; onReport: (report: IngestReport | null) => void; onHistory: (history: IngestJobSummary[]) => void; onIngesting: (value: boolean) => void; onError: (error: string | null) => void }) {
  const [sources, setSources] = useState<DraftSource[]>([{ label: "Camera A", selectedPath: "" }]);
  const [masterPath, setMasterPath] = useState("");
  const [backupPaths, setBackupPaths] = useState<string[]>([]);
  const [policy, setPolicy] = useState<IngestPolicy>("standard");
  const [preflight, setPreflight] = useState<IngestPreflightView | null>(null);
  const [startRequestId, setStartRequestId] = useState<string | null>(null);
  const [isReviewing, setIsReviewing] = useState(false);
  const startInFlight = useRef(false);

  const request = (): IngestRequest => ({
    projectName: project.name,
    sources: sources.filter((source) => source.selectedPath).map((source) => ({ label: source.label, selectedPath: source.selectedPath })),
    master: { role: "master", selectedPath: masterPath },
    backups: backupPaths.filter(Boolean).map((selectedPath) => ({ role: "backup", selectedPath })),
  });

  async function chooseFolder(assign: (path: string) => void, title: string) {
    const selected = await open({ directory: true, multiple: false, title });
    if (typeof selected === "string") assign(selected);
  }

  async function reviewPreflight() {
    if (!sources.some((source) => source.selectedPath) || !masterPath) {
      onError("Choose at least one source and a master destination before pre-flight.");
      return;
    }
    try {
      setIsReviewing(true);
      onError(null);
      const next = await invoke<IngestPreflightView>("preflight_ingest_command", { projectId: project.id, request: request(), policy });
      setPreflight(next);
      setStartRequestId(next.canStart ? crypto.randomUUID() : null);
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      setIsReviewing(false);
    }
  }

  async function start() {
    if (!preflight?.canStart || !startRequestId || isIngesting || startInFlight.current) return;
    try {
      startInFlight.current = true;
      onError(null);
      onIngesting(true);
      const next = await invoke<IngestReport>("start_ingest_command", { projectId: project.id, request: request(), policy, startRequestId });
      onReport(next);
      onHistory(await invoke<IngestJobSummary[]>("ingest_history_command", { projectId: project.id }));
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      onIngesting(false);
      startInFlight.current = false;
      setPreflight(null);
      setStartRequestId(null);
    }
  }

  async function restart(jobId: string) {
    try {
      onIngesting(true);
      const next = await invoke<IngestReport>("restart_ingest_command", { projectId: project.id, jobId });
      onReport(next);
      onHistory(await invoke<IngestJobSummary[]>("ingest_history_command", { projectId: project.id }));
    } catch (reason) {
      toError(onError)(reason);
    } finally {
      onIngesting(false);
    }
  }

  async function openReport(jobId: string) {
    try {
      const next = await invoke<IngestReport | null>("ingest_report_command", { jobId });
      if (!next) throw new Error("The persisted ingest report is unavailable.");
      onReport(next);
    } catch (reason) {
      toError(onError)(reason);
    }
  }

  return <div className="workspace ingest-workspace">
    <section className="ingest-hero"><div><p className="eyebrow">CAPTUREOS / MAGIC INGEST</p><h2>Safe multi-source ingest</h2><p className="muted">Copy, cryptographically verify, register, and assess protection—without changing your source media.</p></div><GuardianBadge report={report} /></section>
    <section className="ingest-grid">
      <StatusCard><div className="panel-heading"><div><p className="section-label">Sources</p><h2>Camera and recorder folders</h2></div><button className="secondary" onClick={() => { setSources((current) => [...current, { label: `Source ${current.length + 1}`, selectedPath: "" }]); setPreflight(null); setStartRequestId(null); }}>Add source</button></div>
        <div className="source-list">{sources.map((source, index) => <div className="source-row" key={`${index}-${source.selectedPath}`}><input aria-label={`Source ${index + 1} label`} value={source.label} onChange={(event) => { const next = [...sources]; next[index] = { ...source, label: event.target.value }; setSources(next); setPreflight(null); setStartRequestId(null); }} /><button onClick={() => void chooseFolder((selectedPath) => { const next = [...sources]; next[index] = { ...source, selectedPath }; setSources(next); setPreflight(null); setStartRequestId(null); }, "Select source media folder")}>{source.selectedPath ? "Change folder" : "Select folder"}</button><small>{source.selectedPath || "No source selected"}</small>{sources.length > 1 ? <button className="icon-button" aria-label={`Remove source ${index + 1}`} onClick={() => { setSources((current) => current.filter((_, item) => item !== index)); setPreflight(null); setStartRequestId(null); }}>×</button> : null}</div>)}</div>
      </StatusCard>
      <StatusCard><p className="section-label">Destinations</p><h2>Master and backup copies</h2><DestinationPicker label="MASTER" path={masterPath} onChoose={() => void chooseFolder((selectedPath) => { setMasterPath(selectedPath); setPreflight(null); setStartRequestId(null); }, "Select master destination") } />{backupPaths.map((path, index) => <DestinationPicker key={`${index}-${path}`} label={`BACKUP ${index + 1}`} path={path} onChoose={() => void chooseFolder((selectedPath) => { const next = [...backupPaths]; next[index] = selectedPath; setBackupPaths(next); setPreflight(null); setStartRequestId(null); }, "Select backup destination") } onRemove={() => { setBackupPaths((current) => current.filter((_, item) => item !== index)); setPreflight(null); setStartRequestId(null); }} />)}<button className="secondary add-backup" onClick={() => { setBackupPaths((current) => [...current, ""]); setPreflight(null); setStartRequestId(null); }}>Add backup destination</button><label className="policy"><span>Protection policy</span><select value={policy} onChange={(event) => { setPolicy(event.target.value as IngestPolicy); setPreflight(null); setStartRequestId(null); }}><option value="standard">Standard — verified master + independent backup</option><option value="basic">Basic — verified master only (reduced protection)</option></select></label></StatusCard>
    </section>
    <section className="transfer-map" aria-label="Ingest transfer map"><div className="transfer-sources">{sources.filter((source) => source.selectedPath).map((source) => <span key={source.selectedPath}>{source.label || "Source"}</span>)}{!sources.some((source) => source.selectedPath) ? <span>Sources</span> : null}</div><div className="transfer-arrow">→</div><div className="transfer-destinations"><strong>MASTER</strong>{backupPaths.filter(Boolean).length ? <strong>+ BACKUP</strong> : <small>Optional backup</small>}</div></section>
    <section className="ingest-actions"><button className="primary" disabled={isReviewing || isIngesting} onClick={() => void reviewPreflight()}>{isReviewing ? "Reviewing…" : "Review pre-flight"}</button>{preflight ? <button className="primary" disabled={!preflight.canStart || !startRequestId || isIngesting || startInFlight.current} onClick={() => void start()}>{isIngesting ? "Ingesting…" : "Start verified ingest"}</button> : null}</section>
    {preflight ? <PreflightPanel view={preflight} /> : null}
    {report ? <IngestReportPanel report={report} /> : null}
    <section className="ingest-history"><div><p className="section-label">Ingest history</p><h2>Previous ingest jobs</h2></div>{history.length ? <div className="history-list">{history.map((job) => <div key={job.id} className="history-row"><div><strong>{formatDate(job.createdAt)}</strong><small>{job.filesVerified} / {job.filesTotal} verified · {job.guardianState.replaceAll("_", " ")}</small></div><span className={`guardian ${job.guardianState}`}>{job.guardianState.replaceAll("_", " ")}</span><div className="history-actions"><button onClick={() => void openReport(job.id)}>Open report</button>{job.state === "needs_attention" || job.state === "interrupted" ? <button onClick={() => void restart(job.id)} disabled={isIngesting}>Retry failed</button> : null}</div></div>)}</div> : <p className="muted">No ingest jobs in this project yet.</p>}</section>
  </div>;
}

function DestinationPicker({ label, path, onChoose, onRemove }: { label: string; path: string; onChoose: () => void; onRemove?: () => void }) { return <div className="destination-picker"><div><span>{label}</span><small>{path || "No folder selected"}</small></div><button onClick={onChoose}>{path ? "Change" : "Select"}</button>{onRemove ? <button className="icon-button" aria-label={`Remove ${label}`} onClick={onRemove}>×</button> : null}</div>; }

function GuardianBadge({ report }: { report: IngestReport | null }) { const state = report?.job.guardianState ?? "unprotected"; return <div className={`guardian guardian-large ${state}`}><span>CaptureGuardian</span><strong>{state.replaceAll("_", " ")}</strong></div>; }

function PreflightPanel({ view }: { view: IngestPreflightView }) { return <section className="preflight"><div className="panel-heading"><div><p className="section-label">Pre-flight report</p><h2>{view.canStart ? "Ready for verified ingest" : "Resolve blocking issues"}</h2></div><strong>{formatSize(view.report.totalSourceBytes)} required per destination</strong></div><div className="preflight-grid"><div><h3>Sources</h3>{view.report.sources.map((source) => <p key={source.selectedPath}><strong>{source.label}</strong><br /><small>{source.fileCount} files · {formatSize(source.totalBytes)} · {source.detectedMediaTypes.join(", ") || "unknown"}</small></p>)}</div><div><h3>Destinations</h3>{view.report.destinations.map((destination) => <p key={`${destination.role}-${destination.selectedPath}`}><strong>{destination.role}</strong><br /><small>{formatSize(destination.requiredBytes)} required · {destination.availableBytes === null ? "space unavailable" : `${formatSize(destination.availableBytes)} available`} · {destination.headroomBytes === null ? "headroom unavailable" : `${formatSize(destination.headroomBytes)} headroom`} · {destination.writable ? "writable" : "not writable"}</small></p>)}</div></div>{view.report.issues.length ? <div className="issues">{view.report.issues.map((issue) => <p className={issue.severity} key={`${issue.code}-${issue.message}`}><strong>{issue.severity}</strong> — {issue.message}</p>)}</div> : <p className="verified-copy">No blocking configuration issues found. Copy and verification can begin.</p>}</section>; }

function IngestReportPanel({ report }: { report: IngestReport }) { return <section className="final-report"><div className="panel-heading"><div><p className="section-label">Ingest report</p><h2>{report.job.state.replaceAll("_", " ")}</h2></div><GuardianBadge report={report} /></div><div className="report-metrics"><Metric label="Verified files" value={`${report.job.filesVerified} / ${report.job.filesTotal}`} /><Metric label="Verified bytes" value={`${formatSize(report.job.bytesVerified)} / ${formatSize(report.job.bytesTotal)}`} /><Metric label="Failures" value={report.job.filesFailed} /></div><div className="report-sources">{report.sources.map((source) => <div key={source.id}><strong>{source.label}</strong><small>{source.fileCount} discovered · {source.status.replaceAll("_", " ")}</small></div>)}</div><div className="report-destinations">{report.destinations.map((destination) => <div key={destination.id}><strong>{destination.role} · {destination.storageVolumeName}</strong><small>{destination.selectedPath}</small></div>)}</div>{report.sameVolumeWarning ? <p className="warning">Two verified copies are on the same physical storage volume. This is not independent device protection.</p> : null}{report.recentErrors.length ? <div className="issues">{report.recentErrors.map((error) => <p className="error" key={error}>{error}</p>)}</div> : null}<p className={report.job.safeToEject ? "safe-eject" : "muted"}>{report.job.safeToEject ? "Copy and verification requirements are complete. Source media can be manually ejected." : "Source media is not yet ready for ejection under the selected protection policy."}</p></section>; }

function MediaTable({ rows }: { rows: IndexedMediaRow[] }) {
  return <div className="table-wrap"><table><thead><tr><th>File</th><th>Type</th><th>Path</th><th>Root</th><th>Size</th><th>Modified</th><th>Storage</th><th>Fingerprint</th><th>Status</th><th>IDs</th></tr></thead><tbody>{rows.map((row) => <tr key={row.fileInstanceId}><td><strong>{row.filename}</strong><small>.{row.extension ?? "—"}</small></td><td>{row.mediaType.replace("_", " ")}</td><td><code>{row.relativePath}</code></td><td title={row.selectedRoot}><code>{row.selectedRoot}</code></td><td>{formatSize(row.byteSize)}</td><td>{row.modifiedAt ? formatDate(row.modifiedAt) : "—"}</td><td>{row.storageVolume}</td><td>{row.fingerprintPresent ? "Fast" : "None"}</td><td><span className={`status ${row.status}`}>{row.status}</span></td><td><small title={row.assetId}>asset {shortId(row.assetId)}</small><small title={row.fileInstanceId}>file {shortId(row.fileInstanceId)}</small></td></tr>)}</tbody></table></div>;
}

function MediaCard({ item, view, density, onOpen }: { item: VisualMediaRow; view: "grid" | "list"; density: "small" | "medium" | "large"; onOpen: (item: VisualMediaRow) => void }) {
  const thumbnail = density === "small" ? item.thumbnailPreviewUrl : item.mediumPreviewUrl ?? item.thumbnailPreviewUrl;
  const label = `${item.filename}, ${item.mediaType.replaceAll("_", " ")}${item.isAvailable ? "" : ", original offline"}`;
  return <button className={`media-card ${view}`} onClick={() => void onOpen(item)} aria-label={`Open ${label}`}>
    <span className="thumbnail-frame"><PreviewImage url={thumbnail} alt="" loading="lazy" fallback={<span className={`media-placeholder ${item.mediaType}`}>{mediaSymbol(item)}</span>} />{item.mediaType === "raw_photo" ? <span className="media-kind">RAW</span> : null}{item.mediaType === "video" ? <span className="media-kind">VIDEO</span> : null}{item.mediaType === "audio" ? <span className="media-kind">AUDIO</span> : null}{!item.isAvailable ? <span className="offline-mark" title="Original offline">Offline original</span> : null}</span>
    <span className="card-caption"><strong>{item.filename}</strong><small>{item.durationMs ? formatDuration(item.durationMs) : item.width && item.height ? `${item.width} × ${item.height}` : item.mediaType.replaceAll("_", " ")}</small><IntelligenceCardIndicators intelligence={item.intelligence} />{item.previewStatus !== "ready" && item.previewStatus !== "pending" ? <small className="preview-state">{previewStatusLabel(item)}</small> : null}{view === "list" ? <small>{item.cameraModel ?? item.storageVolume} · {formatSize(item.byteSize)}</small> : null}</span>
  </button>;
}

function IntelligenceCardIndicators({ intelligence }: { intelligence?: VisualMediaRow["intelligence"] | null }) {
  if (intelligence?.status !== "ready") return null;
  const indicators: { kind: string; label: string }[] = [];
  if (intelligence.recommendation === "strong_candidate") indicators.push({ kind: "strong", label: "★ Strong" });
  if (intelligence.recommendation === "technical_issue") indicators.push({ kind: "review", label: "⚠ Issue" });
  if (intelligence.recommendation === "review") indicators.push({ kind: "review", label: "⚠ Review" });
  if (intelligence.recommendation === "probable_duplicate") indicators.push({ kind: "duplicate", label: "◉ Duplicate" });
  if (intelligence.similarityGroupId && intelligence.similarCount > 1) indicators.push({ kind: "similar", label: `◉ ${intelligence.similarCount} related` });
  if (intelligence.faceCount > 0) indicators.push({ kind: "faces", label: `${intelligence.faceCount} face${intelligence.faceCount === 1 ? "" : "s"}` });
  if (intelligence.possibleClosedEyesCount > 0) indicators.push({ kind: "review", label: "Eyes?" });
  if (intelligence.blurLevel === "high" || intelligence.blurLevel === "moderate") indicators.push({ kind: "review", label: "Blur review" });
  if (intelligence.blurLevel === "uncertain") indicators.push({ kind: "neutral", label: "Blur?" });
  return indicators.length ? <span className="intelligence-indicators" aria-label="Capture Intelligence evidence">{indicators.slice(0, 3).map((indicator) => <span className={indicator.kind} key={`${indicator.kind}-${indicator.label}`}>{indicator.label}</span>)}</span> : null;
}

function previewStatusLabel(item: Pick<VisualMediaRow, "mediaType" | "previewStatus" | "previewFailureReason">) {
  if (item.previewStatus === "corrupt") return "Corrupt media — unable to decode";
  if (item.previewStatus === "timeout") return "Preview provider timed out";
  if (item.previewStatus === "failed") return item.mediaType === "video" ? "Poster generation failed" : "Preview generation failed";
  if (item.previewStatus === "unsupported") return item.mediaType === "raw_photo" ? "RAW preview unavailable" : "Preview unavailable";
  if (item.previewStatus === "offline") return "Original offline";
  if (item.previewStatus === "cancelled") return "Preview preparation cancelled";
  return item.previewFailureReason ?? item.previewStatus.replaceAll("_", " ");
}

function Inspector({ detail, onOpen, onOpenSimilarityGroup, isLoadingSimilarityGroup, onSaveHumanDecision, isSavingDecision }: { detail: MediaAssetDetail | null; onOpen?: () => void; onOpenSimilarityGroup?: (assetId: string) => void; isLoadingSimilarityGroup?: boolean; onSaveHumanDecision?: (assetId: string, decision: "keep" | "review" | "reject") => void; isSavingDecision?: boolean }) {
  if (!detail) return <aside className="inspector empty-inspector"><p className="section-label">Inspector</p><h2>Select media</h2><p className="muted">Technical metadata, storage, Capture Intelligence evidence, and every physical copy appear here.</p></aside>;
  const { item, metadata, copies } = detail;
  const preview = item.mediumPreviewUrl ?? item.thumbnailPreviewUrl;
  const captureTimeCopyConflict = hasCaptureTimeCopyConflict(metadata?.rawMetadata);

  return <aside className="inspector" aria-label="Media metadata inspector">
    <div className="panel-heading"><div><p className="section-label">Inspector</p><h2>{item.filename}</h2></div>{onOpen ? <button className="secondary" onClick={onOpen}>Open viewer</button> : null}</div>
    <div className="inspector-preview"><PreviewImage url={preview} alt="" fallback={<span className="media-placeholder">{mediaSymbol(item)}</span>} /></div>
    <CaptureIntelligenceEvidence detail={detail} onOpenSimilarityGroup={onOpenSimilarityGroup} isLoadingSimilarityGroup={isLoadingSimilarityGroup} onSaveHumanDecision={onSaveHumanDecision} isSavingDecision={isSavingDecision} />
    <dl className="metadata-grid">
      <Detail label="Original" value={item.isAvailable ? "Available" : "Offline original"} />
      <Detail label="Storage" value={item.storageVolume} />
      {metadata?.cameraModel ? <Detail label="Camera" value={metadata.cameraModel} /> : null}
      {metadata?.lensModel ? <Detail label="Lens" value={metadata.lensModel} /> : null}
      {metadata?.focalLengthMm ? <Detail label="Focal length" value={`${metadata.focalLengthMm} mm`} /> : null}
      {metadata?.aperture ? <Detail label="Aperture" value={`f/${metadata.aperture}`} /> : null}
      {metadata?.shutterSpeed ? <Detail label="Shutter" value={metadata.shutterSpeed} /> : null}
      {metadata?.iso ? <Detail label="ISO" value={metadata.iso} /> : null}
      {metadata?.width && metadata?.height ? <Detail label="Dimensions" value={`${metadata.width} × ${metadata.height}`} /> : null}
      {metadata?.durationMs ? <Detail label="Duration" value={formatDuration(metadata.durationMs)} /> : null}
      {metadata?.codec ? <Detail label="Codec" value={metadata.codec} /> : null}
      {metadata?.sampleRate ? <Detail label="Audio" value={`${metadata.sampleRate} Hz · ${metadata.channels ?? "?"} ch`} /> : null}
      {metadata?.capturedAtLocal ? <Detail label="Captured" value={formatDate(metadata.capturedAtLocal)} /> : null}
      {metadata?.gpsPresent ? <Detail label="Location" value="Location metadata available" /> : null}
    </dl>
    <section className="copy-list"><p className="section-label">Copies</p>{copies.map((copy) => <div key={copy.fileInstanceId}><strong>{copy.storageVolume}</strong><small>{copy.isAvailable ? "Available" : "Offline"} · {copy.selectedRoot ?? "No index root"}</small><small>{copy.relativePath}</small></div>)}</section>
    <details className="advanced">
      <summary>Advanced / Developer Details</summary>
      <small>Asset {item.assetId}</small>
      <small>Preferred file instance {item.fileInstanceId}</small>
      <small>Metadata source {metadata?.extractor ?? "not prepared"} {metadata?.extractorVersion ?? ""}</small>
      <small>Fingerprint {metadata?.sourceFingerprint ?? "not prepared"}</small>
      {metadata?.capturedAtLocal ? <>
        <small>Capture time source {captureTimeSourceDetail(metadata.captureTimeSource)} · {metadata.captureTimeConfidence ? `${displayLabel(metadata.captureTimeConfidence)} confidence` : "confidence unavailable"}</small>
        <small>Capture timezone {metadata.captureTimezone === "unknown" ? "Unknown — camera wall time preserved" : metadata.captureTimezone ?? "Unavailable"}</small>
      </> : <small>Capture time provenance unavailable</small>}
      {captureTimeCopyConflict ? <small>Capture-time copy diagnostic: available copies reported conflicting embedded capture times; a deterministic local resolution was retained.</small> : null}
      {detail.intelligence ? <>
        <small>Intelligence provider {detail.intelligence.provider} {detail.intelligence.providerVersion}</small>
        <small>Analysis settings {detail.intelligence.settingsVersion}</small>
        <small>Analysis input {detail.intelligence.inputFingerprint}</small>
        <small>Face detection chain {detail.intelligence.faceProvider} {detail.intelligence.faceProviderVersion} · {analysisStatusLabel(detail.intelligence.faceAnalysisStatus)}</small>
        <small>Resolved face provider {detail.intelligence.faceResolvedProvider} {detail.intelligence.faceResolvedProviderVersion}</small>
        <small>Face landmarks · {analysisStatusLabel(detail.intelligence.faceLandmarkStatus)}</small>
        {detail.intelligence.faceAnalysisError ? <small>Face detection detail {detail.intelligence.faceAnalysisError}</small> : null}
        {detail.intelligence.faceProviderAttemptError ? <small>Earlier face provider attempt {detail.intelligence.faceProviderAttemptError}</small> : null}
        {detail.intelligence.faceLandmarkError ? <small>Face landmarks detail {detail.intelligence.faceLandmarkError}</small> : null}
      </> : null}
    </details>
  </aside>;
}

function CaptureIntelligenceEvidence({ detail, onOpenSimilarityGroup, isLoadingSimilarityGroup, onSaveHumanDecision, isSavingDecision }: { detail: MediaAssetDetail; onOpenSimilarityGroup?: (assetId: string) => void; isLoadingSimilarityGroup?: boolean; onSaveHumanDecision?: (assetId: string, decision: "keep" | "review" | "reject") => void; isSavingDecision?: boolean }) {
  const intelligence = detail.intelligence;
  if (!intelligence) return <section className="intelligence-evidence unavailable"><p className="section-label">Capture Intelligence</p><h3>Not analyzed</h3><p className="muted">No completed local intelligence artifact is stored for this media.</p></section>;
  const { summary, technical, faces } = intelligence;
  if (summary.status !== "ready") return <section className="intelligence-evidence unavailable"><p className="section-label">Capture Intelligence</p><h3>{analysisStatusLabel(summary.status)}</h3><p className="muted">{summary.unavailableReason ?? "No technical recommendation was made for this media."}</p></section>;
  const analyzableEyes = faces.filter((face) => face.eyeState !== "not_analyzable");
  const openEyes = faces.filter((face) => face.eyeState === "open").length;
  const possiblyClosedEyes = faces.filter((face) => face.eyeState === "closed").length;
  const uncertainEyes = faces.filter((face) => face.eyeState === "uncertain").length;
  const faceDetectionReady = intelligence.faceAnalysisStatus === "ready";
  const faceDetectionUnavailable = !faceDetectionReady;
  const faceDetectionLabel = intelligence.faceAnalysisStatus === "failed" ? "Face detection failed" : intelligence.faceAnalysisStatus === "stale" ? "Face detection stale" : "Face detection unavailable";
  const faceDetectionDetail = faceDetectionUnavailable
    ? [intelligence.faceProvider, shortDetail(intelligence.faceAnalysisError)].filter(Boolean).join(" · ") || undefined
    : undefined;
  const exposureEvidence = technical ? `Highlights ${formatPercent(technical.highlightClippingPercent)} · Shadows ${formatPercent(technical.shadowClippingPercent)}` : "Unavailable";
  const landmarksReady = intelligence.faceLandmarkStatus === "ready";
  const eyeEvidence = faceDetectionUnavailable ? "Unavailable" : faces.length === 0 ? "No faces detected" : !landmarksReady ? "Not analyzable" : analyzableEyes.length === 0 ? "Not analyzable" : `${openEyes} open${possiblyClosedEyes ? ` · ${possiblyClosedEyes} possibly closed` : ""}${uncertainEyes ? ` · ${uncertainEyes} uncertain` : ""}`;
  const faceSharpness = faces.map((face) => face.faceSharpness).filter((value): value is number => value !== null).sort((left, right) => right - left)[0];
  return <section className="intelligence-evidence"><div className="intelligence-evidence-heading"><div><p className="section-label">Capture Intelligence</p><h3>{recommendationLabel(summary.recommendation) ?? technicalQualityLabel(summary.technicalQualityBand) ?? "Technical evidence"}</h3></div>{summary.confidence !== null ? <span className="confidence">{formatConfidence(summary.confidence)} confidence</span> : null}</div><p className="intelligence-disclaimer">Explainable technical evidence only — it does not judge creative quality.</p><dl className="intelligence-evidence-grid"><EvidenceRow label="Technical quality" value={technicalQualityLabel(summary.technicalQualityBand) ?? "Unavailable"} detail={technical?.technicalQualityScore === null || technical?.technicalQualityScore === undefined ? undefined : `${Math.round(technical.technicalQualityScore)} / 100`} /><EvidenceRow label="Sharpness" value={technical ? displayLabel(technical.sharpnessBand) : "Unavailable"} detail={technical?.globalSharpness === null || technical?.globalSharpness === undefined ? undefined : `${Math.round(technical.globalSharpness)} / 100`} /><EvidenceRow label="Exposure" value={exposureEvidence} /><EvidenceRow label="Motion blur" value={technical ? displayLabel(technical.blurLevel) : "Unavailable"} /><EvidenceRow label="Face detection" value={faceDetectionUnavailable ? faceDetectionLabel : faces.length ? `${faces.length} detected` : "No faces detected"} detail={faceDetectionDetail} /><EvidenceRow label="Face landmarks" value={landmarksReady ? "Ready" : analysisStatusLabel(intelligence.faceLandmarkStatus)} /><EvidenceRow label="Eye state" value={eyeEvidence} /><EvidenceRow label="Face sharpness" value={faceSharpness === undefined ? "Unavailable" : "Measured"} detail={faceSharpness === undefined ? undefined : `${Math.round(faceSharpness)} / 100`} /></dl>{summary.similarityGroupId && summary.similarCount > 1 ? <button className="similar-group-link" disabled={!onOpenSimilarityGroup || isLoadingSimilarityGroup} onClick={() => onOpenSimilarityGroup?.(detail.item.assetId)}>{isLoadingSimilarityGroup ? "Loading related frames…" : `Similar ${summary.similarCount}`}</button> : null}{intelligence.recommendationReasons.length ? <ul className="intelligence-reasons">{intelligence.recommendationReasons.map((reason) => <li key={reason}>{reason}</li>)}</ul> : null}{onSaveHumanDecision ? <section className="human-decision"><div><strong>Your decision</strong><small>{intelligence.humanDecision ? `Saved as ${humanDecisionLabel(intelligence.humanDecision)}` : "Optional override — AI evidence is preserved."}</small></div><div>{([ ["keep", "Keep"], ["review", "Review"], ["reject", "Reject"] ] as ["keep" | "review" | "reject", string][]).map(([decision, label]) => <button key={decision} className={intelligence.humanDecision === decision ? "active" : ""} aria-pressed={intelligence.humanDecision === decision} disabled={isSavingDecision} onClick={() => onSaveHumanDecision(detail.item.assetId, decision)}>{label}</button>)}</div></section> : null}</section>;
}

function EvidenceRow({ label, value, detail }: { label: string; value: string; detail?: string }) { return <div><dt>{label}</dt><dd>{value}{detail ? <small>{detail}</small> : null}</dd></div>; }

function MediaViewer({ detail, nearby, onClose, onPrevious, onNext, onOpen, onOpenSimilarityGroup, isLoadingSimilarityGroup, onSaveHumanDecision, isSavingDecision }: { detail: MediaAssetDetail; nearby: VisualMediaRow[]; onClose: () => void; onPrevious: () => void; onNext: () => void; onOpen: (item: VisualMediaRow) => void; onOpenSimilarityGroup?: (assetId: string) => void; isLoadingSimilarityGroup?: boolean; onSaveHumanDecision?: (assetId: string, decision: "keep" | "review" | "reject") => void; isSavingDecision?: boolean }) {
  const [zoom, setZoom] = useState(1);
  const [showMetadata, setShowMetadata] = useState(true);
  const [showFilmstrip, setShowFilmstrip] = useState(true);
  const item = detail.item;
  const preview = item.previewPreviewUrl ?? item.mediumPreviewUrl ?? item.thumbnailPreviewUrl;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
      if (event.key === "Escape" || event.key === " ") { event.preventDefault(); onClose(); }
      if (event.key === "ArrowLeft") onPrevious();
      if (event.key === "ArrowRight") onNext();
      if (event.key === "+") setZoom((value) => Math.min(4, value + .25));
      if (event.key === "-") setZoom((value) => Math.max(.25, value - .25));
      if (event.key.toLowerCase() === "i") setShowMetadata((value) => !value);
      if (event.key.toLowerCase() === "g") setShowFilmstrip((value) => !value);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose, onNext, onPrevious]);
  return <div className="viewer" role="dialog" aria-modal="true" aria-label={`Viewer for ${item.filename}`}><header className="viewer-header"><div><p className="section-label">Viewer</p><strong>{item.filename}</strong></div><div><button onClick={onPrevious} aria-label="Previous media">←</button><button onClick={onNext} aria-label="Next media">→</button><button onClick={() => setZoom(1)}>Fit</button><button onClick={() => setZoom(1)}>100%</button><button onClick={() => setShowMetadata((value) => !value)}>Metadata</button><button onClick={() => setShowFilmstrip((value) => !value)}>Filmstrip</button><button className="primary" onClick={onClose}>Close</button></div></header><div className="viewer-body"><div className="viewer-canvas">{preview ? <PreviewImage url={preview} alt={item.filename} style={{ transform: `scale(${zoom})` }} fallback={<div className="viewer-unavailable" data-preview-error="true"><span>{mediaSymbol(item)}</span><strong>{previewStatusLabel(item)}</strong><small>{item.previewFailureReason ?? "The generated preview is unavailable."}</small></div>} /> : <div className="viewer-unavailable"><span>{mediaSymbol(item)}</span><strong>{item.previewStatus === "pending" ? "Preview is preparing" : previewStatusLabel(item)}</strong><small>{item.previewFailureReason ?? "Metadata and the original remain available when mounted."}</small></div>}</div>{showMetadata ? <Inspector detail={detail} onOpenSimilarityGroup={onOpenSimilarityGroup} isLoadingSimilarityGroup={isLoadingSimilarityGroup} onSaveHumanDecision={onSaveHumanDecision} isSavingDecision={isSavingDecision} /> : null}</div>{showFilmstrip ? <nav className="filmstrip" aria-label="Nearby media">{nearby.map((nearbyItem) => <button key={nearbyItem.assetId} className={nearbyItem.assetId === item.assetId ? "active" : ""} onClick={() => void onOpen(nearbyItem)} aria-label={`Open ${nearbyItem.filename}`}>{nearbyItem.thumbnailPreviewUrl ? <PreviewImage url={nearbyItem.thumbnailPreviewUrl} alt="" fallback={<span>{mediaSymbol(nearbyItem)}</span>} /> : <span>{mediaSymbol(nearbyItem)}</span>}</button>)}</nav> : null}</div>;
}

function SimilarityGroupDialog({ group, onClose, onOpenMember }: { group: SimilarityGroupView; onClose: () => void; onOpenMember: (assetId: string) => void }) {
  const [showFaceView, setShowFaceView] = useState(false);
  const hasFaceBoxes = group.members.some((member) => member.faces?.length);
  return <div className="similarity-dialog-backdrop" role="presentation"><section className="similarity-dialog" role="dialog" aria-modal="true" aria-label="Similar frames"><header><div><p className="section-label">{similarityGroupLabel(group.kind)}</p><h2>Related frames</h2><p className="muted">{group.members.length} local members · {formatConfidence(group.similarityConfidence)} similarity confidence{group.timeProximitySeconds !== null ? ` · ${formatDuration(group.timeProximitySeconds * 1000)} apart` : ""}</p></div><div className="similarity-dialog-actions">{hasFaceBoxes ? <button className={`secondary ${showFaceView ? "active" : ""}`} aria-pressed={showFaceView} onClick={() => setShowFaceView((value) => !value)}>Face view</button> : null}<button className="secondary" onClick={onClose}>Close</button></div></header><p className="similarity-method">{displayLabel(group.groupingMethod)} · {group.groupingVersion}</p>{showFaceView ? <SimilarityFaceCompare members={group.members} /> : <div className="similarity-compare">{group.members.map((member) => <button className="similarity-member" key={member.assetId} onClick={() => onOpenMember(member.assetId)} aria-label={`Open ${member.filename}`}><span className="similarity-member-preview"><PreviewImage url={member.mediumPreviewUrl} alt="" fallback={<span className="media-placeholder">◫</span>} /></span><strong>{member.filename}</strong><small>{member.isRepresentative ? "Representative" : `${formatConfidence(member.similarityConfidence)} similar`}</small><SimilarityMemberEvidence intelligence={member.intelligence} /></button>)}</div>}<p className="muted similarity-dialog-note">Open a frame to inspect its local technical evidence. This compact comparison does not alter originals or recommendations.</p></section></div>;
}

function SimilarityFaceCompare({ members }: { members: SimilarityGroupView["members"] }) {
  const visibleMembers = members.filter((member) => member.faces?.length);
  return <section className="similarity-face-compare" aria-label="Face crop comparison"><div><p className="section-label">Face view</p><h3>Face crops — per-frame, not identity-matched</h3><p className="muted">Each crop is a local detector box from its own frame. CaptureOS does not recognize or match people.</p></div><div className="similarity-face-members">{visibleMembers.map((member) => <article className="similarity-face-member" key={member.assetId}><strong>{member.filename}</strong><div className="face-crops">{member.faces.slice(0, 2).map((face, index) => <FaceCrop key={face.id} url={member.mediumPreviewUrl} face={face} label={`Face ${index + 1} from ${member.filename}`} />)}</div>{member.faces.length > 2 ? <small>Showing 2 of {member.faces.length} detected faces</small> : null}</article>)}</div></section>;
}

function FaceCrop({ url, face, label }: { url: string | null; face: FaceAnalysisEvidence; label: string }) {
  const box = safeFaceBox(face);
  const imageStyle: CSSProperties = { height: `${100 / box.height}%`, left: `${-(box.x / box.width) * 100}%`, maxWidth: "none", position: "absolute", top: `${-(box.y / box.height) * 100}%`, width: `${100 / box.width}%` };
  return <span className="face-crop" aria-label={label}><PreviewImage url={url} alt="" style={imageStyle} fallback={<span className="face-crop-unavailable">Preview unavailable</span>} /></span>;
}

function SimilarityMemberEvidence({ intelligence }: { intelligence?: VisualMediaRow["intelligence"] | null }) {
  const label = recommendationLabel(intelligence?.recommendation ?? null);
  return label ? <small className="similarity-member-evidence">{label}</small> : null;
}

function PreviewImage({ url, alt, fallback, loading, style }: { url: string | null; alt: string; fallback: ReactNode; loading?: "lazy" | "eager"; style?: CSSProperties }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  return url && !failed ? <img src={url} alt={alt} loading={loading} style={style} onError={() => setFailed(true)} /> : <>{fallback}</>;
}

function mediaSymbol(item: Pick<VisualMediaRow, "mediaType">) { if (item.mediaType === "audio") return "♫"; if (item.mediaType === "video") return "▶"; if (item.mediaType === "raw_photo") return "RAW"; return "◫"; }
function formatDuration(milliseconds: number) { const seconds = Math.round(milliseconds / 1000); return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`; }
function formatPercent(value: number | null | undefined) { return value === null || value === undefined || !Number.isFinite(value) ? "Unavailable" : `${value.toFixed(1)}%`; }
function formatConfidence(value: number | null | undefined) { return value === null || value === undefined || !Number.isFinite(value) ? "Unavailable" : `${Math.round(value * 100)}%`; }
function formatMomentTimeRange(from: string | null, to: string | null, state: string) {
  if (state === "unavailable" || (!from && !to)) return "Capture time unavailable";
  if (!from) return `Capture time through ${formatDate(to!)}`;
  if (!to || to === from) return `Captured ${formatDate(from)}`;
  return `${formatDate(from)} – ${formatDate(to)}`;
}
function momentAnalysisStatusLabel(progress: MomentAnalysisProgress | null) {
  if (!progress) return "Checking local status";
  if (progress.active || progress.state === "running") return "Analyzing locally";
  if (progress.state === "queued") return "Analysis queued";
  if (progress.state === "paused") return "Analysis paused";
  if (progress.state === "failed") return "Analysis needs review";
  if (progress.state === "interrupted") return "Analysis interrupted";
  if (progress.timelineReady) return "Local timeline ready";
  return "Not analyzed";
}
function coverageStateLabel(state: string) {
  if (state === "confirmed_covered") return "Confirmed covered";
  if (state === "needs_review") return "Needs review";
  if (state === "not_covered") return "Not covered";
  return "Unreviewed";
}
function shortDetail(value: string | null | undefined) { const compact = value?.replaceAll(/\s+/g, " ").trim(); return compact && compact.length > 110 ? `${compact.slice(0, 107)}…` : compact; }
function safeFaceBox(face: FaceAnalysisEvidence) { const width = Math.min(Math.max(Number.isFinite(face.width) ? face.width : 0.01, 0.01), 1); const height = Math.min(Math.max(Number.isFinite(face.height) ? face.height : 0.01, 0.01), 1); return { width, height, x: Math.min(Math.max(Number.isFinite(face.x) ? face.x : 0, 0), 1 - width), y: Math.min(Math.max(Number.isFinite(face.y) ? face.y : 0, 0), 1 - height) }; }
function isAnalysisResourceMode(value: unknown): value is "eco" | "balanced" | "fast" { return value === "eco" || value === "balanced" || value === "fast"; }
function isSemanticResourceMode(value: unknown): value is SemanticResourceMode { return isAnalysisResourceMode(value); }
function metadataRefreshIsActive(progress: MetadataRefreshProgress | null) { return progress?.state === "queued" || progress?.state === "running"; }
function captureTimeSourceDetail(source: string | null | undefined) {
  if (source === "exif_datetime_original") return "EXIF DateTimeOriginal";
  if (source === "exif_datetime_digitized") return "EXIF DateTimeDigitized";
  if (source === "exif_create_date") return "EXIF CreateDate";
  if (source === "platform_content_creation_date") return "Platform content creation date";
  if (source === "platform_sips_creation_date") return "Platform image creation date";
  if (source === "filesystem_modified_time") return "Filesystem modified time";
  if (source === "filesystem_created_time") return "Filesystem created time";
  return source ? displayLabel(source) : "unavailable";
}
function hasCaptureTimeCopyConflict(rawMetadata: Record<string, unknown> | undefined) {
  const resolution = rawMetadata?.captureTimeResolution;
  if (!isRecord(resolution) || !Array.isArray(resolution.diagnostics)) return false;
  return resolution.diagnostics.some((diagnostic) => isRecord(diagnostic) && diagnostic.kind === "embedded_capture_time_conflict");
}
function isRecord(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function displayLabel(value: string) { return value.split("_").filter(Boolean).map((part) => `${part.slice(0, 1).toUpperCase()}${part.slice(1)}`).join(" "); }
function intelligenceStateLabel(value: string) { if (value === "running") return "Analyzing locally"; if (value === "paused") return "Analysis paused"; if (value === "completed") return "Analysis complete"; if (value === "interrupted") return "Analysis interrupted"; if (value === "failed") return "Analysis needs review"; if (value === "queued") return "Analysis queued"; return displayLabel(value); }
function analysisStatusLabel(value: string | null | undefined) { if (value === "needs_original") return "Needs original"; if (value === "unsupported") return "Analysis unsupported"; if (value === "corrupt") return "Corrupt media"; if (value === "not_applicable") return "Not applicable"; if (value === "stale") return "Analysis stale"; if (value === "failed") return "Analysis failed"; if (value === "pending") return "Analysis pending"; return value ? displayLabel(value) : "Analysis unavailable"; }
function recommendationLabel(value: string | null | undefined) { if (value === "strong_candidate") return "Strong technical candidate"; if (value === "strong_alternative") return "Strong alternative"; if (value === "review") return "Needs review"; if (value === "probable_duplicate") return "Probable duplicate"; if (value === "technical_issue") return "Technical issue"; return null; }
function studioRecommendationLabel(value: string | null | undefined) { if (value === "likely_keep") return "Likely Keep"; if (value === "likely_review") return "Likely Review"; if (value === "likely_reject") return "Likely Reject"; return "Not enough evidence"; }
function studioStatusLabel(value: string | null | undefined) { if (value === "ready") return "Ready"; if (value === "stale") return "Update recommended"; if (value === "learning") return "Learning"; if (value === "error") return "Needs attention"; return "Not ready"; }
function technicalQualityLabel(value: string | null | undefined) { if (value === "strong") return "Strong"; if (value === "good") return "Good"; if (value === "review") return "Review"; if (value === "technical_issue") return "Technical issue"; return null; }
function humanDecisionLabel(value: string) { if (value === "keep") return "Keep"; if (value === "review") return "Review"; if (value === "reject") return "Reject"; return displayLabel(value); }
function similarityGroupLabel(value: string) { if (value === "exact_duplicate_set") return "Exact duplicate set"; if (value === "near_duplicate_set") return "Near-duplicate set"; if (value === "burst") return "Burst sequence"; if (value === "similar_set") return "Similar set"; return displayLabel(value); }

function Metric({ label, value }: { label: string; value: string | number }) { return <div><span>{label}</span><strong>{typeof value === "number" ? value.toLocaleString() : value}</strong></div>; }
function Detail({ label, value }: { label: string; value: string | number }) { return <div><dt>{label}</dt><dd>{value}</dd></div>; }
function formatSize(value: number | null) { if (value === null) return "—"; if (value < 1024) return `${value} B`; if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KB`; return `${(value / 1024 ** 2).toFixed(1)} MB`; }
function formatDate(value: string) {
  const localWallClock = parseLocalWallClock(value);
  if (localWallClock) {
    // Zone-less camera metadata is a local wall clock, not UTC. Formatting it in a neutral UTC
    // frame preserves its numeric fields without applying the workstation time zone.
    const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" }).format(localWallClock);
    const time = new Intl.DateTimeFormat(undefined, { timeStyle: "short", timeZone: "UTC" }).format(localWallClock);
    return `${date} ${time}`;
  }
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}
function parseLocalWallClock(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,9}))?)?$/.exec(value);
  if (!match) return null;
  const [, yearText, monthText, dayText, hourText, minuteText, secondText = "0", fraction = ""] = match;
  const year = Number(yearText);
  const month = Number(monthText);
  const day = Number(dayText);
  const hour = Number(hourText);
  const minute = Number(minuteText);
  const second = Number(secondText);
  const milliseconds = Number(fraction.padEnd(3, "0").slice(0, 3));
  const date = new Date(Date.UTC(year, month - 1, day, hour, minute, second, milliseconds));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day && date.getUTCHours() === hour && date.getUTCMinutes() === minute && date.getUTCSeconds() === second ? date : null;
}
function shortId(value: string) { return value.slice(0, 8); }
function toMessage(reason: unknown) { return reason instanceof Error ? reason.message : typeof reason === "string" ? reason : "The local catalog did not respond."; }
function toError(setError: (message: string) => void) { return (reason: unknown) => setError(toMessage(reason)); }
