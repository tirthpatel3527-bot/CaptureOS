import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const eventListeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, callback: (event: { payload: unknown }) => void) => {
    eventListeners.set(event, callback);
    return vi.fn();
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { App } from "./App";

const home = {
  project: { id: "project-1", name: "Golden Wedding" },
  summary: {
    mediaAssets: 2,
    fileInstances: 3,
    storageVolumes: 1,
    filesDiscovered: 3,
    supportedMediaCount: 3,
    unknownCount: 0,
    duplicateFastFingerprintCount: 1,
    momentCount: 0,
    lastIndexedFolder: "/Volumes/Golden",
    storageVolumeIdentity: "unix-device:1",
  },
  roots: [],
  latestJob: {
    id: "job-1",
    state: "completed",
    stage: "finalize",
    filesDiscovered: 3,
    filesProcessed: 3,
    errorCount: 0,
    startedAt: "2026-01-01T00:00:00Z",
    finishedAt: "2026-01-01T00:00:01Z",
    errorMessage: null,
  },
  media: [{ assetId: "asset-1", fileInstanceId: "file-1", filename: "IMG_0001.jpg", mediaType: "jpeg", extension: "jpg", relativePath: "RAW/IMG_0001.jpg", selectedRoot: "/Volumes/Golden", byteSize: 1024, modifiedAt: "2026-01-01T00:00:00Z", storageVolume: "Golden", storageVolumeId: "volume-1", fingerprintPresent: true, status: "available" }],
};

const visual = {
  items: [{ assetId: "asset-1", fileInstanceId: "file-1", filename: "IMG_0001.jpg", mediaType: "jpeg", extension: "jpg", byteSize: 1024, capturedAt: "2026-01-01T00:00:00Z", indexedAt: "2026-01-01T00:00:00Z", relativePath: "RAW/IMG_0001.jpg", selectedRoot: "/Volumes/Golden", storageVolume: "Golden", storageVolumeId: "volume-1", isAvailable: true, thumbnailPreviewUrl: "captureos-preview://localhost/artifact-small", mediumPreviewUrl: "captureos-preview://localhost/artifact-medium", previewPreviewUrl: "captureos-preview://localhost/artifact-preview", previewStatus: "ready", previewFailureReason: null, width: null, height: null, durationMs: null, cameraModel: null, lensModel: null, codec: null }],
  hasMore: false,
  cacheBytes: 0,
};

const detail = {
  item: visual.items[0],
  metadata: { mediaAssetId: "asset-1", sourceFileInstanceId: "file-1", sourceFingerprint: "fast", extractor: "test", extractorVersion: "1", status: "ready", failureReason: null, extractedAt: "2026-01-01T00:00:00Z", mimeType: "image/jpeg", byteSize: 1024, capturedAtRaw: null, capturedAtLocal: null, captureTimezone: null, captureTimeSource: null, captureTimeConfidence: null, width: 1200, height: 800, orientation: "1", cameraMake: "Sony", cameraModel: "A7 IV", lensMake: null, lensModel: "FE 85mm F1.8", focalLengthMm: 85, focalLengthEquivalentMm: null, aperture: 1.8, shutterSpeed: "1/320", iso: 800, exposureCompensation: null, flash: null, whiteBalance: null, colorSpace: "sRGB", gpsPresent: false, durationMs: null, frameRate: null, codec: null, pixelFormat: null, bitrate: null, audioStreams: null, videoStreams: null, sampleRate: null, bitDepth: null, channels: null, rawMetadata: {} },
  copies: [{ fileInstanceId: "file-1", relativePath: "RAW/IMG_0001.jpg", selectedRoot: "/Volumes/Golden", storageVolume: "Golden", isAvailable: true, observedAt: "2026-01-01T00:00:00Z" }],
};

const readyVideoItem = {
  ...visual.items[0],
  filename: "Movie on 7-31-26 at 4.22PM.mov",
  mediaType: "video",
  extension: "mov",
  previewStatus: "ready",
  previewFailureReason: null,
};
const readyVideoVisual = { ...visual, items: [readyVideoItem] };
const readyVideoDetail = { ...detail, item: readyVideoItem };
const capturedLocalDetail = {
  ...detail,
  metadata: {
    ...detail.metadata,
    capturedAtRaw: "2019:10:29 21:46:24",
    capturedAtLocal: "2019-10-29T21:46:24.120",
    captureTimezone: "unknown",
    captureTimeSource: "exif_datetime_original",
    captureTimeConfidence: "high",
    rawMetadata: {
      captureTimeResolution: {
        diagnostics: [{ kind: "embedded_capture_time_conflict" }],
      },
    },
  },
};
const readyVideoSummary = {
  state: "completed", stage: "finalize", itemsCompleted: 1, itemsTotal: 1, errorCount: 0,
  readyCount: 1, unsupportedCount: 0, corruptCount: 0, offlineCount: 0, failedCount: 0,
  timeoutCount: 0, cancelledCount: 0, currentAssetId: null, currentFileInstanceId: null,
  currentProvider: null, startedAt: "2026-01-01T00:00:00Z", finishedAt: "2026-01-01T00:00:01Z", message: null,
};

const intelligenceProgress = {
  state: "completed", stage: "finalize", resourceMode: "balanced", itemsCompleted: 1, itemsTotal: 1,
  errorCount: 0, readyCount: 1, unsupportedCount: 0, corruptCount: 0, needsOriginalCount: 0,
  failedCount: 0, notApplicableCount: 0, staleCount: 0, currentAssetId: null, currentStageDetail: null,
  startedAt: "2026-01-01T00:00:00Z", finishedAt: "2026-01-01T00:00:01Z", message: "Local evidence is ready.",
};

const metadataRefreshProgress = {
  state: "completed",
  itemsCompleted: 2,
  itemsTotal: 2,
  errorCount: 0,
  resolvedCaptureTimeCount: 2,
  highConfidenceCaptureTimeCount: 2,
  copyConflictCount: 0,
  currentAssetId: null,
  startedAt: "2026-01-01T00:00:00Z",
  finishedAt: "2026-01-01T00:00:01Z",
  message: "Local capture-time metadata refresh is complete.",
};

const semanticIndexUnavailable = {
  state: "idle", model: { installed: false, message: "Semantic model not installed. Metadata and technical filters remain available.", identity: null },
  counts: { ready: 0, unsupported: 0, corrupt: 0, needsOriginal: 0, failed: 0, pending: 0, stale: 0 },
  active: false, paused: false, completed: 0, total: 0, stage: "semantic_embedding", resourceMode: "balanced",
  indexReady: false, indexEmbeddingCount: 0, lastError: null,
};
const semanticIndexReady = {
  ...semanticIndexUnavailable,
  model: {
    installed: true,
    message: "Local semantic search model is ready.",
    identity: { modelId: "test-siglip", modelVersion: "1", provider: "test-local", licenseUrl: "https://example.invalid/license", embeddingDimension: 3, installedBytes: 815600873 },
  },
  counts: { ...semanticIndexUnavailable.counts, ready: 2 },
  indexReady: true,
  indexEmbeddingCount: 2,
};

const relatedVisualItem = { ...visual.items[0], assetId: "asset-2", fileInstanceId: "file-2", filename: "IMG_0002.jpg" };
const magicSearchResult = {
  query: "local scene",
  results: [{ item: visual.items[0], scoreLabel: "High", semanticScore: 0.81234, explanation: "Local embedding similarity.", matchedEvidence: ["Local image/text embedding similarity."] }],
  hasMore: false,
  totalResults: 1,
  semanticAvailable: true,
  semanticApplied: true,
  semanticUnavailableReason: null,
  identitySearchBlocked: false,
  parsedFilters: { chips: [] },
  message: null,
};

const intelligenceSummary = {
  status: "ready", technicalQualityBand: "strong", technicalQualityScore: 86,
  recommendation: "strong_candidate", recommendationConfidence: 0.82, similarityGroupId: "group-1",
  similarityGroupKind: "similar_set", similarCount: 3, faceCount: 1, openEyesCount: 1,
  possibleClosedEyesCount: 0, blurLevel: "low", sharpnessBand: "excellent", confidence: 0.82,
  unavailableReason: null,
};

const analyzedVisual = { ...visual, items: [{ ...visual.items[0], intelligence: intelligenceSummary }] };
const analyzedDetail = {
  ...detail,
  item: analyzedVisual.items[0],
  intelligence: {
    summary: intelligenceSummary,
    inputFingerprint: "analysis-fingerprint",
    provider: "captureos-deterministic-image",
    providerVersion: "1",
    settingsVersion: "m4.test",
    generatedAt: "2026-01-01T00:00:01Z",
    technical: {
      globalSharpness: 86, sharpnessBand: "excellent", directionalBlurRatio: 0.77, blurLevel: "low",
      meanLuminance: 0.52, medianLuminance: 0.49, highlightClippingPercent: 0.8, shadowClippingPercent: 1.2,
      channelClippingPercent: 0.2, technicalQualityScore: 86, technicalQualityBand: "strong", confidence: 0.82,
      status: "ready", errorMessage: null,
    },
    faceProvider: "macos-vision",
    faceProviderVersion: "1",
    faceResolvedProvider: "macos-vision",
    faceResolvedProviderVersion: "1",
    faceAnalysisStatus: "ready",
    faceAnalysisError: null,
    faceProviderAttemptError: null,
    faceLandmarkStatus: "ready",
    faceLandmarkError: null,
    faces: [{ id: "face-1", x: 0.2, y: 0.2, width: 0.3, height: 0.3, detectionConfidence: 0.9, relativeSize: 0.09, visibility: "good", pose: "frontal", faceSharpness: 82, eyeState: "open", eyeConfidence: 0.91 }],
    recommendationReasons: ["Sharpness evidence: excellent"],
    humanDecision: null,
  },
};

const cullingWorkspace = {
  session: { id: "session-1", projectId: "project-1", startedAt: "2026-01-01T00:00:00Z", endedAt: null, mode: "all_photos", lastAssetId: null, lastGroupId: null, filterContext: "all", photosReviewed: 0, setsReviewed: 0 },
  progress: { total: 1, reviewed: 0, keep: 0, reject: 0, review: 0, unreviewed: 1, starred: 0, fiveStar: 0, setsTotal: 1, setsReviewed: 0, strongCandidateKept: 0, technicalIssueKept: 0, strongCandidateRejected: 0 },
  items: [{ media: analyzedVisual.items[0], decision: { decision: null, rating: 0, starred: false, note: null, flags: [], updatedAt: null }, faces: analyzedDetail.intelligence.faces, relativeEvidence: ["Sharpness evidence: excellent"], similarityGroupId: "group-1", isAiRepresentative: true, isHumanRepresentative: false }],
  groups: [{ id: "group-1", kind: "similar_set", memberCount: 1, aiRepresentativeAssetId: "asset-1", aiRepresentativeFilename: "IMG_0001.jpg", humanRepresentativeAssetId: null, humanRepresentativeFilename: null, reviewedCount: 0, completed: false, completionKind: null }],
  hasMore: false,
};

const momentProgress = {
  state: "completed",
  active: false,
  paused: false,
  stage: "finalize",
  resourceMode: "balanced",
  completed: 2,
  total: 2,
  errorCount: 0,
  timelineReady: true,
  momentCount: 2,
  ungroupedAssetCount: 0,
  lastError: null,
  message: "Local structural timeline is ready.",
};
const firstMoment = {
  id: "moment-1",
  ordinal: 1,
  canMergeWithPrevious: false,
  label: {
    displayLabel: "Outdoor portraits",
    aiSuggestedLabel: "Outdoor portraits",
    humanLabel: null,
    source: "generic_visual_vocabulary",
    strength: "strong",
    evidence: ["Supported local concepts: outdoor, portraits."],
  },
  capturedFrom: "2026-01-01T10:00:00Z",
  capturedTo: "2026-01-01T10:04:00Z",
  captureTimeState: "observed",
  assetCount: 2,
  similarSetCount: 1,
  keepCount: 0,
  rejectCount: 0,
  reviewCount: 0,
  unreviewedCount: 2,
  starredCount: 0,
  technicalIssueCount: 0,
  representative: { assetId: "asset-1", filename: "IMG_0001.jpg", thumbnailPreviewUrl: "captureos-preview://localhost/artifact-small", source: "ai_suggested", evidence: ["Central local representative."] },
  boundaryBefore: null,
  hasHumanStructureOverride: false,
};
const secondMoment = {
  ...firstMoment,
  id: "moment-2",
  ordinal: 2,
  canMergeWithPrevious: true,
  label: { ...firstMoment.label, displayLabel: "Untitled Moment", aiSuggestedLabel: null, source: "none", strength: "unavailable", evidence: [] },
  capturedFrom: "2026-01-01T10:18:00Z",
  capturedTo: "2026-01-01T10:22:00Z",
  boundaryBefore: { strength: "strong", summary: "Observed capture gap with a local visual change.", signals: ["Capture-time continuity changed."] },
  hasHumanStructureOverride: true,
};
const momentTimeline = {
  progress: momentProgress,
  moments: [firstMoment, secondMoment],
  hasMore: false,
  totalMoments: 2,
  ungroupedAssetCount: 0,
  timelineGaps: [],
  clockDiagnostics: [],
};
const momentSearchResult = {
  query: "water",
  results: [secondMoment],
  hasMore: false,
  totalResults: 1,
  semanticAvailable: true,
  semanticApplied: true,
  semanticUnavailableReason: null,
  identitySearchBlocked: false,
  message: "Moment cards are ranked from compatible local centroids.",
};
const momentDetail = {
  moment: firstMoment,
  items: [visual.items[0], { ...visual.items[0], assetId: "asset-2", fileInstanceId: "file-2", filename: "IMG_0002.jpg" }],
  hasMore: false,
  totalAssets: 2,
  boundaryEvidence: [{ strength: "moderate", summary: "Structural continuity is based on available local evidence.", signals: ["Compatible local semantic evidence was available."] }],
};
const momentChecklists = [{
  id: "checklist-1",
  name: "Coverage",
  items: [{ id: "checklist-item-1", phrase: "Portraits", state: "unreviewed", confirmedMomentId: null, confirmedAssetId: null, updatedAt: null }],
}];

const twoFrameCullingWorkspace = {
  ...cullingWorkspace,
  progress: { ...cullingWorkspace.progress, total: 2, unreviewed: 2, setsTotal: 1 },
  items: [
    { ...cullingWorkspace.items[0], media: { ...cullingWorkspace.items[0].media, assetId: "asset-400", filename: "DSC03400.JPG" }, similarityGroupId: "group-400", decision: { ...cullingWorkspace.items[0].decision } },
    { ...cullingWorkspace.items[0], media: { ...cullingWorkspace.items[0].media, assetId: "asset-401", filename: "DSC03401.JPG" }, similarityGroupId: "group-400", decision: { ...cullingWorkspace.items[0].decision, decision: null } },
  ],
  groups: [{ id: "group-400", kind: "similar_set", memberCount: 2, aiRepresentativeAssetId: "asset-401", aiRepresentativeFilename: "DSC03401.JPG", humanRepresentativeAssetId: "asset-400", humanRepresentativeFilename: "DSC03400.JPG", reviewedCount: 0, completed: false, completionKind: null }],
};

const completedTwoFrameCullingWorkspace = {
  ...twoFrameCullingWorkspace,
  progress: { ...twoFrameCullingWorkspace.progress, reviewed: 2, keep: 1, review: 1, unreviewed: 0, setsReviewed: 1 },
  items: [
    { ...twoFrameCullingWorkspace.items[0], similarityGroupId: "group-400", decision: { ...twoFrameCullingWorkspace.items[0].decision, decision: "keep" } },
    { ...twoFrameCullingWorkspace.items[1], similarityGroupId: "group-400", decision: { ...twoFrameCullingWorkspace.items[1].decision, decision: "review" } },
  ],
  groups: [{ ...twoFrameCullingWorkspace.groups[0], reviewedCount: 2, completed: true, completionKind: "auto_all_reviewed" as const }],
};

const similarityGroup = {
  id: "group-1", kind: "similar_set", representativeAssetId: "asset-1", groupingMethod: "deterministic-lsh", groupingVersion: "1",
  similarityConfidence: 0.91, timeProximitySeconds: 4, visualSimilarity: 0.91,
  members: [{ assetId: "asset-1", filename: "IMG_0001.jpg", mediumPreviewUrl: "captureos-preview://localhost/artifact-medium", similarityConfidence: 1, timeProximitySeconds: 0, isRepresentative: true, intelligence: intelligenceSummary, faces: [] }],
};

const faceSimilarityGroup = {
  ...similarityGroup,
  members: [{
    ...similarityGroup.members[0],
    faces: [
      { id: "face-1", x: 0.15, y: 0.2, width: 0.3, height: 0.3, detectionConfidence: 0.9, relativeSize: 0.09, visibility: "good", pose: "frontal", faceSharpness: 82, eyeState: "open", eyeConfidence: 0.91 },
      { id: "face-2", x: 0.55, y: 0.25, width: 0.25, height: 0.25, detectionConfidence: 0.88, relativeSize: 0.06, visibility: "good", pose: "frontal", faceSharpness: 75, eyeState: "open", eyeConfidence: 0.84 },
      { id: "face-3", x: 0.7, y: 0.55, width: 0.16, height: 0.16, detectionConfidence: 0.72, relativeSize: 0.03, visibility: "partial", pose: "profile", faceSharpness: 51, eyeState: "not_analyzable", eyeConfidence: null },
    ],
  }],
};

const zeroFaceSummary = { ...intelligenceSummary, faceCount: 0, openEyesCount: 0, possibleClosedEyesCount: 0 };
const zeroFaceVisual = { ...visual, items: [{ ...visual.items[0], intelligence: zeroFaceSummary }] };
const unavailableFaceDetail = {
  ...analyzedDetail,
  item: zeroFaceVisual.items[0],
  intelligence: {
    ...analyzedDetail.intelligence,
    summary: zeroFaceSummary,
    faceProvider: "local-vision",
    faceAnalysisStatus: "not_applicable",
    faceAnalysisError: "The approved local face provider is not available on this platform.",
    faceProviderAttemptError: null,
    faceLandmarkStatus: "not_applicable",
    faceLandmarkError: "No local landmark provider is enabled.",
    faces: [],
  },
};
const fallbackFaceDetail = {
  ...analyzedDetail,
  intelligence: {
    ...analyzedDetail.intelligence,
    faceProvider: "captureos-local-face-detection",
    faceProviderVersion: "m4.face-detection-chain.v3",
    faceResolvedProvider: "ultraface-rfb-320",
    faceResolvedProviderVersion: "version-RFB-320",
    faceAnalysisStatus: "ready",
    faceAnalysisError: null,
    faceProviderAttemptError: "Apple Vision failed: transient host service unavailable",
    faceLandmarkStatus: "not_applicable",
    faceLandmarkError: "No landmark provider is enabled.",
  },
};
const goldenProject = {
  id: "project-1", name: "Golden Wedding", createdAt: "2026-01-01T00:00:00Z",
  lastActivityAt: "2026-01-02T00:00:00Z", mediaAssetCount: 2, storageVolumeCount: 1,
  protectionState: "not_recorded",
};
const aiProject = {
  id: "project-2", name: "AI Test", createdAt: "2026-01-03T00:00:00Z",
  lastActivityAt: "2026-01-03T00:00:00Z", mediaAssetCount: 0, storageVolumeCount: 0,
  protectionState: "not_recorded",
};
const goldenIngestHistory = [{ id: "golden-job", state: "completed", policy: "standard", guardianState: "protected", safeToEject: true, filesTotal: 1, filesVerified: 1, filesFailed: 0, bytesTotal: 100, bytesVerified: 100, createdAt: "2026-01-02T00:00:00Z", updatedAt: "2026-01-02T00:00:01Z", finishedAt: "2026-01-02T00:00:01Z" }];
const aiIngestHistory = [{ id: "ai-job", state: "completed", policy: "standard", guardianState: "partially_protected", safeToEject: false, filesTotal: 2, filesVerified: 2, filesFailed: 0, bytesTotal: 200, bytesVerified: 200, createdAt: "2026-01-03T00:00:00Z", updatedAt: "2026-01-03T00:00:01Z", finishedAt: "2026-01-03T00:00:01Z" }];
const blockedPreflight = { canStart: false, report: { sources: [], destinations: [], issues: [{ severity: "error", code: "fixture", message: "Fixture only" }], totalSourceBytes: 0 } };
const emptyVisual = { items: [], hasMore: false, cacheBytes: 0 };
const aiHome = { ...home, project: { id: "project-2", name: "AI Test" }, summary: { ...home.summary, mediaAssets: 0, fileInstances: 0, storageVolumes: 0 }, media: [] };
const studioStatus = {
  profileId: "studio-profile-1", profileName: "Local Studio Profile", trainingStatus: "learning",
  personalizationEnabled: true, projectIncluded: true, eligibleDecisionCount: 4,
  keepCount: 2, reviewCount: 1, rejectCount: 1, ratingCount: 0, starredCount: 0,
  representativeCount: 0, contributingProjectCount: 1, activeModelVersion: null,
  lastTrainedAt: null, readiness: { state: "learning", message: "Not enough evidence for personalization." }, lastError: null,
};

let projectLibrary = [goldenProject];
let commandOverrides: Record<string, unknown> = {};

function setCommandOverrides(overrides: Record<string, unknown>) {
  commandOverrides = overrides;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function renderProject(name = "Golden Wedding") {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: `Open ${name}` }));
}

describe("CaptureOS application shell and local media engine", () => {
  beforeEach(() => {
    window.history.replaceState({}, "", "#/");
    projectLibrary = [goldenProject];
    commandOverrides = {};
    eventListeners.clear();
    vi.mocked(open).mockReset();
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      const override = commandOverrides[command];
      if (typeof override === "function") return override(args);
      if (override !== undefined) return override;
      if (command === "project_library") return projectLibrary;
      if (command === "create_project") {
        const name = (args as { name: string }).name;
        const created = { ...aiProject, id: `project-${projectLibrary.length + 1}`, name };
        projectLibrary = [created, ...projectLibrary];
        return { id: created.id, name: created.name };
      }
      if (command === "project_home") return home;
      if (command === "visual_media_page") return visual;
      if (command === "visual_preparation_summary_command") return null;
      if (command === "capture_intelligence_summary_command") return null;
      if (command === "semantic_index_status_command") return null;
      if (command === "magic_search_history_command") return [];
      if (command === "prepare_media_command") return { state: "completed", stage: "finalize", itemsCompleted: 1, itemsTotal: 1, errorCount: 0, message: null };
      if (command === "media_asset_detail_command") return detail;
      if (command === "culling_progress_command") return cullingWorkspace.progress;
      if (command === "studio_brain_status_command") return studioStatus;
      if (command === "culling_workspace_command") return cullingWorkspace;
      if (command === "update_culling_decision_command") {
        const input = (args as { input: { decision?: string; clearDecision?: boolean; rating?: number; starred?: boolean; note?: string } }).input;
        return { decision: input.clearDecision ? null : input.decision ?? null, rating: input.rating ?? 0, starred: input.starred ?? false, note: input.note ?? null, flags: [], updatedAt: "2026-01-01T00:00:02Z" };
      }
      if (command === "update_culling_position_command" || command === "set_culling_group_representative_command" || command === "complete_culling_group_command") return undefined;
      if (command === "ingest_history_command") return [];
      throw new Error(`unexpected command: ${command}`);
    });
  });

  it("starts at Home and shows an existing project library card", async () => {
    render(<App />);
    expect(await screen.findByRole("heading", { name: "Your shoots" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open Golden Wedding" })).toBeTruthy();
    expect(screen.getByText("Media")).toBeTruthy();
    expect(screen.getByText("Protection not recorded")).toBeTruthy();
  });

  it("creates exactly one project, opens it empty, and returns to Home", async () => {
    setCommandOverrides({
      project_home: (args: { projectId: string }) => args.projectId === "project-2" ? aiHome : home,
      visual_media_page: (args: { projectId: string }) => args.projectId === "project-2" ? emptyVisual : visual,
    });
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "New Project" }))[0]);
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "AI Test" } });
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(await screen.findByRole("heading", { name: "AI Test" })).toBeTruthy();
    expect(screen.getByText("YOUR SHOOT STARTS HERE")).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "create_project")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "← All Projects" }));
    expect(await screen.findByRole("button", { name: "Open AI Test" })).toBeTruthy();
  });

  it("switches projects by stable ID without leaking media or ingest history", async () => {
    projectLibrary = [aiProject, goldenProject];
    setCommandOverrides({
      project_home: (args: { projectId: string }) => args.projectId === "project-2" ? aiHome : home,
      visual_media_page: (args: { projectId: string }) => args.projectId === "project-2" ? emptyVisual : visual,
      ingest_history_command: (args: { projectId: string }) => args.projectId === "project-2" ? aiIngestHistory : goldenIngestHistory,
    });
    await renderProject();
    expect(await screen.findByText("IMG_0001.jpg")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Switch project"), { target: { value: "project-2" } });
    expect(await screen.findByRole("heading", { name: "AI Test" })).toBeTruthy();
    expect(screen.queryByText("IMG_0001.jpg")).toBeNull();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("project_home", expect.objectContaining({ projectId: "project-2" })));
    fireEvent.click(screen.getAllByRole("button", { name: "Ingest Shoot" })[0]);
    await waitFor(() => expect(document.querySelector(".history-row")?.textContent).toContain("2 / 2 verified"));
    expect(document.querySelector(".history-row")?.textContent).not.toContain("1 / 1 verified");
  });

  it("keeps equal project display names separate by ID", async () => {
    projectLibrary = [{ ...goldenProject, id: "project-2" }, goldenProject];
    render(<App />);
    expect((await screen.findAllByRole("button", { name: "Open Golden Wedding" })).length).toBe(2);
    fireEvent.click(screen.getAllByRole("button", { name: "Open Golden Wedding" })[0]);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("project_home", expect.objectContaining({ projectId: "project-2" })));
  });

  it("keeps Ingest Mode separate from the read-only Index Mode", async () => {
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Ingest Shoot" }));
    expect(await screen.findByText("Safe multi-source ingest")).toBeTruthy();
    expect(screen.getByText("Camera and recorder folders")).toBeTruthy();
    expect(screen.getByText("Master and backup copies")).toBeTruthy();
  });

  it("keeps browser Back inside normal project navigation and returns to Home", async () => {
    await renderProject();
    expect(await screen.findByRole("heading", { name: "Golden Wedding" })).toBeTruthy();
    window.history.back();
    fireEvent(window, new PopStateEvent("popstate"));
    expect(await screen.findByRole("heading", { name: "Your shoots" })).toBeTruthy();
  });

  it("indexes only the currently selected project", async () => {
    projectLibrary = [aiProject, goldenProject];
    setCommandOverrides({
      project_home: (args: { projectId: string }) => args.projectId === "project-2" ? aiHome : home,
      visual_media_page: (args: { projectId: string }) => args.projectId === "project-2" ? emptyVisual : visual,
      index_folder: { ...home.latestJob, state: "completed" },
    });
    vi.mocked(open).mockResolvedValue("/fixture/ai-test");
    await renderProject("AI Test");
    fireEvent.click(screen.getByRole("button", { name: "Index Folder" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("index_folder", { projectId: "project-2", selectedPath: "/fixture/ai-test" }));
  });

  it("does not refresh capture metadata on project open and starts it only after an explicit choice", async () => {
    setCommandOverrides({ refresh_metadata_command: metadataRefreshProgress });
    await renderProject();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "refresh_metadata_command")).toHaveLength(0);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh metadata" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("refresh_metadata_command", { projectId: "project-1" }));
    expect(await screen.findByText(/Capture-time metadata refresh complete/)).toBeTruthy();
    expect(screen.getByText(/Rebuild Moments to use refreshed chronology/)).toBeTruthy();
  });

  it("sends ingest pre-flight only to the currently selected project", async () => {
    projectLibrary = [aiProject, goldenProject];
    setCommandOverrides({
      project_home: (args: { projectId: string }) => args.projectId === "project-2" ? aiHome : home,
      visual_media_page: (args: { projectId: string }) => args.projectId === "project-2" ? emptyVisual : visual,
      ingest_history_command: () => aiIngestHistory,
      preflight_ingest_command: blockedPreflight,
    });
    vi.mocked(open)
      .mockResolvedValueOnce("/fixture/camera")
      .mockResolvedValueOnce("/fixture/master");
    await renderProject("AI Test");
    fireEvent.click(screen.getByRole("button", { name: "Ingest Shoot" }));
    fireEvent.click(await screen.findByRole("button", { name: "Select folder" }));
    await screen.findByText("/fixture/camera");
    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    await screen.findByText("/fixture/master");
    fireEvent.click(screen.getByRole("button", { name: "Review pre-flight" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("preflight_ingest_command", expect.objectContaining({ projectId: "project-2" })));
  });

  it("opens logical media in the viewer with inspector copy details", async () => {
    await renderProject();
    await screen.findByText("IMG_0001.jpg");
    fireEvent.click(screen.getByRole("button", { name: /Open IMG_0001.jpg/i }));
    expect(await screen.findByRole("dialog", { name: /Viewer for IMG_0001.jpg/i })).toBeTruthy();
    expect(screen.getAllByText("A7 IV").length).toBeGreaterThan(0);
    expect(screen.getAllByText("FE 85mm F1.8").length).toBeGreaterThan(0);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("media_asset_detail_command", { projectId: "project-1", assetId: "asset-1" }));
  });

  it("preserves an unknown camera capture time as local wall-clock metadata and exposes provenance diagnostics", async () => {
    setCommandOverrides({ media_asset_detail_command: capturedLocalDetail });
    await renderProject();
    fireEvent.click(await screen.findByRole("button", { name: /Open IMG_0001.jpg/i }));
    const localWallClock = new Date(Date.UTC(2019, 9, 29, 21, 46, 24, 120));
    const expected = `${new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" }).format(localWallClock)} ${new Intl.DateTimeFormat(undefined, { timeStyle: "short", timeZone: "UTC" }).format(localWallClock)}`;
    expect((await screen.findAllByText(expected)).length).toBeGreaterThan(0);
    fireEvent.click(screen.getAllByText("Advanced / Developer Details")[0]);
    expect((await screen.findAllByText(/Capture time source EXIF DateTimeOriginal · High confidence/)).length).toBeGreaterThan(0);
    expect((await screen.findAllByText("Capture timezone Unknown — camera wall time preserved")).length).toBeGreaterThan(0);
    expect((await screen.findAllByText(/Capture-time copy diagnostic: available copies reported conflicting embedded capture times/)).length).toBeGreaterThan(0);
  });

  it("uses the registered preview bridge and a safe fallback", async () => {
    await renderProject();
    await screen.findByText("IMG_0001.jpg");
    const gridImage = document.querySelector('img[src="captureos-preview://localhost/artifact-medium"]');
    expect(gridImage).toBeTruthy();
    fireEvent.error(gridImage!);
    expect(document.querySelector('img[src="captureos-preview://localhost/artifact-medium"]')).toBeNull();
    expect(document.querySelector(".media-placeholder.jpeg")).toBeTruthy();
  });

  it("does not show a failed-poster subtitle when a usable video poster is ready", async () => {
    setCommandOverrides({ visual_media_page: readyVideoVisual, visual_preparation_summary_command: readyVideoSummary, prepare_media_command: readyVideoSummary, media_asset_detail_command: readyVideoDetail });
    await renderProject();
    await screen.findByText("Movie on 7-31-26 at 4.22PM.mov");
    expect(screen.queryByText("Poster generation failed")).toBeNull();
  });

  it("lets the photographer choose an explicit local analysis resource mode", async () => {
    setCommandOverrides({ capture_intelligence_summary_command: intelligenceProgress, start_capture_intelligence_command: intelligenceProgress });
    await renderProject();
    expect(await screen.findByText("Analysis complete")).toBeTruthy();
    fireEvent.click(within(screen.getByRole("group", { name: "Analysis resource mode" })).getByRole("button", { name: "FAST" }));
    fireEvent.click(screen.getByRole("button", { name: "Analyze new media" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("start_capture_intelligence_command", { projectId: "project-1", resourceMode: "fast" }));
  });

  it("keeps deterministic Magic Search available when a semantic pack is unavailable", async () => {
    setCommandOverrides({
      semantic_index_status_command: semanticIndexUnavailable,
      magic_search_command: {
        ...magicSearchResult,
        query: "2 faces",
        semanticAvailable: false,
        semanticApplied: false,
        semanticUnavailableReason: "No approved local semantic model pack is installed.",
        results: [{ item: visual.items[0], scoreLabel: null, semanticScore: null, explanation: "Matched local face-count evidence.", matchedEvidence: ["Faces: 2"] }],
        parsedFilters: { chips: ["Faces: 2"] },
      },
    });
    await renderProject();
    const input = await screen.findByRole("textbox", { name: "Magic Search this project" });
    fireEvent.change(input, { target: { value: "2 faces" } });
    fireEvent.click(screen.getByRole("button", { name: "Search filters" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("magic_search_command", {
      projectId: "project-1",
      request: { query: "2 faces", sort: "relevance", descending: true, limit: 120, offset: 0 },
    }));
    expect(await screen.findByText("Filters only")).toBeTruthy();
    expect(screen.getAllByText("Semantic model not installed")).not.toHaveLength(0);
    expect(screen.getByText("Install it explicitly with the documented local pack workflow. CaptureOS never downloads a semantic model automatically.")).toBeTruthy();
    expect(screen.queryByText(/Local similarity/)).toBeNull();
    expect(screen.getAllByText("Faces: 2")).not.toHaveLength(0);
  });

  it("labels a semantic score as a local ranking signal rather than confidence", async () => {
    setCommandOverrides({ semantic_index_status_command: semanticIndexReady, magic_search_command: magicSearchResult });
    await renderProject();
    const input = await screen.findByRole("textbox", { name: "Magic Search this project" });
    fireEvent.change(input, { target: { value: "local scene" } });
    fireEvent.click(screen.getByRole("button", { name: "Search local photos" }));
    expect(await screen.findByText("Local similarity 0.812 · ranking signal only, not confidence or proof of an object, person, or identity.")).toBeTruthy();
  });

  it("paginates Find Similar with its original local asset instead of issuing a text search", async () => {
    const secondRelatedVisualItem = { ...relatedVisualItem, assetId: "asset-3", fileInstanceId: "file-3", filename: "IMG_0003.jpg" };
    setCommandOverrides({
      semantic_index_status_command: semanticIndexReady,
      magic_search_command: magicSearchResult,
      find_similar_command: (args: { assetId: string; offset: number }) => args.offset === 0
        ? {
          ...magicSearchResult,
          query: "",
          results: [{ item: relatedVisualItem, scoreLabel: "High", semanticScore: 0.79, explanation: "Related local visual embedding.", matchedEvidence: ["Related local visual embedding."] }],
          hasMore: true,
          totalResults: 2,
        }
        : {
          ...magicSearchResult,
          query: "",
          results: [{ item: secondRelatedVisualItem, scoreLabel: "Medium", semanticScore: 0.71, explanation: "Related local visual embedding.", matchedEvidence: ["Related local visual embedding."] }],
          hasMore: false,
          totalResults: 2,
        },
    });
    await renderProject();
    const input = await screen.findByRole("textbox", { name: "Magic Search this project" });
    fireEvent.change(input, { target: { value: "local scene" } });
    fireEvent.click(screen.getByRole("button", { name: "Search local photos" }));
    await screen.findByRole("button", { name: "Find Similar" });
    fireEvent.click(screen.getByRole("button", { name: "Find Similar" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("find_similar_command", {
      projectId: "project-1", assetId: "asset-1", limit: 120, offset: 0,
    }));
    await screen.findByText("Find Similar uses the separate local semantic nearest-neighbor index. It does not create or alter Similar Sets.");
    fireEvent.click(await screen.findByRole("button", { name: "Load more results" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("find_similar_command", {
      projectId: "project-1", assetId: "asset-1", limit: 120, offset: 1,
    }));
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "magic_search_command")).toEqual([
      ["magic_search_command", { projectId: "project-1", request: { query: "local scene", sort: "relevance", descending: true, limit: 120, offset: 0 } }],
    ]);
    expect(await screen.findByText("IMG_0003.jpg")).toBeTruthy();
  });

  it("does not load or start Moment analysis until the photographer opens Moments", async () => {
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: {
        ...momentTimeline,
        timelineGaps: [{ startedAt: "2026-01-01T10:04:00Z", endedAt: "2026-01-01T10:18:00Z", durationSeconds: 840, explanation: "No locally recorded capture activity in this interval." }],
      },
      moment_checklists: momentChecklists,
      start_moment_analysis: { ...momentProgress, state: "queued", active: true, message: "Queued local Moment analysis." },
    });
    await renderProject();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "moment_timeline_status" || command === "moment_timeline")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    expect(await screen.findByText("Outdoor portraits")).toBeTruthy();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("moment_timeline_status", { projectId: "project-1" }));
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_moment_analysis")).toHaveLength(0);
    expect(screen.getByText(/Suggested labels are evidence-grounded; your edits remain authoritative/)).toBeTruthy();
    expect(screen.getByText(/only you confirm coverage/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: "No recorded capture activity" })).toBeTruthy();
    expect(screen.getByText(/not coverage conclusions/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Update timeline" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("start_moment_analysis", { projectId: "project-1", rebuild: false, resourceMode: "balanced" }));
  });

  it("forwards the photographer-selected local Moment scheduling mode", async () => {
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_checklists: momentChecklists,
      start_moment_analysis: { ...momentProgress, state: "queued", active: true, resourceMode: "fast" },
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    await screen.findByText("Outdoor portraits");
    fireEvent.click(within(screen.getByRole("group", { name: "Moment analysis resource mode" })).getByRole("button", { name: "FAST" }));
    fireEvent.click(screen.getByRole("button", { name: "Update timeline" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("start_moment_analysis", {
      projectId: "project-1",
      rebuild: false,
      resourceMode: "fast",
    }));
  });

  it("synchronously ignores duplicate Moment update clicks while the request is in flight", async () => {
    const pendingStart = deferred<typeof momentProgress>();
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_checklists: momentChecklists,
      start_moment_analysis: () => pendingStart.promise,
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    await screen.findByText("Outdoor portraits");
    const update = screen.getByRole("button", { name: "Update timeline" });
    fireEvent.click(update);
    fireEvent.click(update);
    await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_moment_analysis")).toHaveLength(1));
    expect(screen.getByRole("button", { name: "Rebuild AI timeline" }).hasAttribute("disabled")).toBe(true);
    await act(async () => {
      pendingStart.resolve({ ...momentProgress, state: "queued", active: true, message: "Queued local Moment analysis." });
      await pendingStart.promise;
    });
    await waitFor(() => expect(screen.getByRole("button", { name: "Analyzing locally…" }).hasAttribute("disabled")).toBe(true));
  });

  it("blocks timeline updates while a structural mutation is pending", async () => {
    const pendingMerge = deferred<void>();
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_checklists: momentChecklists,
      merge_adjacent_moments: () => pendingMerge.promise,
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    try {
      await renderProject();
      fireEvent.click(screen.getByRole("button", { name: "Moments" }));
      await screen.findByText("Outdoor portraits");
      const merge = screen.getByRole("button", { name: "Merge with previous" });
      fireEvent.click(merge);
      fireEvent.click(merge);
      await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "merge_adjacent_moments")).toHaveLength(1));
      expect(screen.getByRole("button", { name: "Update timeline" }).hasAttribute("disabled")).toBe(true);
      fireEvent.click(screen.getByRole("button", { name: "Update timeline" }));
      expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_moment_analysis")).toHaveLength(0);
    } finally {
      await act(async () => {
        pendingMerge.resolve();
        await pendingMerge.promise;
      });
      confirm.mockRestore();
    }
  });

  it("explains why a cross-run Moment boundary must be rebuilt before merging", async () => {
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: {
        ...momentTimeline,
        moments: [
          firstMoment,
          { ...secondMoment, canMergeWithPrevious: false },
        ],
      },
      moment_checklists: momentChecklists,
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    expect(await screen.findByText("These adjacent Moments are from different local analysis runs. Rebuild AI timeline before merging across this boundary.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Merge with previous" })).toBeNull();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "merge_adjacent_moments")).toHaveLength(0);
  });

  it("shows a recoverable Moment persistence failure without placing SQLite text in the primary status", async () => {
    const sqliteDetail = "database error: UNIQUE constraint failed: moment_records.run_id, moment_records.ordinal";
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_checklists: momentChecklists,
      start_moment_analysis: {
        ...momentProgress,
        state: "failed",
        active: false,
        lastError: sqliteDetail,
        message: "Timeline update could not be saved. Your previous timeline is still available.",
      },
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    await screen.findByText("Outdoor portraits");
    fireEvent.click(screen.getByRole("button", { name: "Update timeline" }));
    const status = screen.getByRole("status");
    await waitFor(() => expect(status.textContent).toContain("Your previous timeline is still available."));
    expect(status.textContent).not.toContain("UNIQUE constraint failed");
    expect([...document.querySelectorAll("details")].some((details) => details.textContent?.includes(sqliteDetail))).toBe(true);
    expect(screen.getByText("Developer Details")).toBeTruthy();
    expect(screen.getByText("Outdoor portraits")).toBeTruthy();
  });

  it("renders a persisted Moment failure received through the background progress event safely", async () => {
    const sqliteDetail = "database error: UNIQUE constraint failed: moment_records.run_id, moment_records.ordinal";
    const failedProgress = {
      ...momentProgress,
      state: "failed",
      active: false,
      lastError: sqliteDetail,
      message: "Timeline update could not be saved. Your previous timeline is still available.",
    };
    let persistedFailure = false;
    setCommandOverrides({
      moment_timeline_status: () => persistedFailure ? failedProgress : momentProgress,
      moment_timeline: () => ({ ...momentTimeline, progress: persistedFailure ? failedProgress : momentProgress }),
      moment_checklists: momentChecklists,
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    await screen.findByText("Outdoor portraits");
    await waitFor(() => expect(eventListeners.get("moment-analysis-progress")).toBeDefined());
    persistedFailure = true;
    await act(async () => {
      eventListeners.get("moment-analysis-progress")?.({
        payload: { projectId: "project-1", progress: failedProgress },
      });
    });
    const status = screen.getByRole("status");
    await waitFor(() => expect(status.textContent).toContain("Your previous timeline is still available."));
    expect(status.textContent).not.toContain("UNIQUE constraint failed");
    expect([...document.querySelectorAll("details")].some((details) => details.textContent?.includes(sqliteDetail))).toBe(true);
  });

  it("keeps Moment-card and checklist candidate retrieval local, scoped, and non-confirming", async () => {
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_checklists: momentChecklists,
      moment_search: momentSearchResult,
      magic_search_command: magicSearchResult,
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    const momentSearch = await screen.findByRole("textbox", { name: "Search local Moments" });
    fireEvent.change(momentSearch, { target: { value: "water" } });
    fireEvent.click(screen.getByRole("button", { name: "Search Moments" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("moment_search", {
      projectId: "project-1",
      request: { query: "water", limit: 24 },
    }));
    expect(await screen.findByText("Moment cards are ranked from compatible local centroids.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Find candidates" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("magic_search_command", {
      projectId: "project-1",
      request: { query: "Portraits", sort: "relevance", descending: true, limit: 24, offset: 0 },
    }));
    expect(await screen.findByText(/This is retrieval only\. It does not assert coverage/)).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_coverage_confirmation")).toHaveLength(0);
  });

  it("keeps Moment labels, local search, coverage confirmation, and culling explicitly project and Moment scoped", async () => {
    setCommandOverrides({
      moment_timeline_status: momentProgress,
      moment_timeline: momentTimeline,
      moment_detail: momentDetail,
      moment_checklists: momentChecklists,
      magic_search_command: magicSearchResult,
      update_coverage_confirmation: undefined,
      culling_workspace_command: cullingWorkspace,
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Moments" }));
    fireEvent.click(await screen.findByRole("button", { name: "Open Moment Outdoor portraits" }));
    expect((await screen.findAllByRole("heading", { name: "Outdoor portraits" })).length).toBeGreaterThan(0);
    expect(screen.queryByText("bride")).toBeNull();
    const search = screen.getByRole("textbox", { name: "Magic Search this Moment" });
    fireEvent.change(search, { target: { value: "portraits" } });
    fireEvent.click(screen.getByRole("button", { name: "Search this Moment" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("magic_search_command", {
      projectId: "project-1",
      request: { query: "portraits", sort: "relevance", descending: true, limit: 24, offset: 0, momentId: "moment-1" },
    }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm covered here" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("update_coverage_confirmation", {
      projectId: "project-1",
      input: { checklistItemId: "checklist-item-1", state: "confirmed_covered", momentId: "moment-1" },
    }));
    fireEvent.click(screen.getByRole("button", { name: "Cull this Moment" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("culling_workspace_command", {
      projectId: "project-1",
      query: { mode: "all_photos", filter: "all", momentId: "moment-1", limit: 80, offset: 0 },
    }));
    expect(await screen.findByText(/Moment-scoped human review/)).toBeTruthy();
  });

  it("shows persisted intelligence evidence and preserves a separate human decision", async () => {
    setCommandOverrides({ visual_media_page: analyzedVisual, capture_intelligence_summary_command: intelligenceProgress, media_asset_detail_command: analyzedDetail, similarity_group_command: similarityGroup, save_human_intelligence_decision_command: undefined });
    await renderProject();
    expect(await screen.findByText("★ Strong")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Open IMG_0001.jpg/i }));
    await screen.findByRole("dialog", { name: /Viewer for IMG_0001.jpg/i });
    fireEvent.click(screen.getAllByRole("button", { name: "Keep" })[0]);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_human_intelligence_decision_command", { projectId: "project-1", input: { assetId: "asset-1", decision: "keep" } }));
    fireEvent.click(screen.getAllByRole("button", { name: "Similar 3" })[0]);
    expect(await screen.findByRole("dialog", { name: "Similar frames" })).toBeTruthy();
  });

  it("shows at most two per-frame face crops without identity matching", async () => {
    setCommandOverrides({ visual_media_page: analyzedVisual, capture_intelligence_summary_command: intelligenceProgress, media_asset_detail_command: analyzedDetail, similarity_group_command: faceSimilarityGroup });
    await renderProject();
    await screen.findByText("IMG_0001.jpg");
    fireEvent.click(screen.getByRole("button", { name: /Open IMG_0001.jpg/i }));
    fireEvent.click((await screen.findAllByRole("button", { name: "Similar 3" }))[0]);
    fireEvent.click(await screen.findByRole("button", { name: "Face view" }));
    expect(await screen.findByText("Face crops — per-frame, not identity-matched")).toBeTruthy();
    expect(document.querySelectorAll(".face-crop")).toHaveLength(2);
  });

  it("reports unavailable, ready-empty, and stale face evidence honestly", async () => {
    setCommandOverrides({ visual_media_page: zeroFaceVisual, capture_intelligence_summary_command: intelligenceProgress, media_asset_detail_command: unavailableFaceDetail });
    await renderProject();
    await screen.findByText("IMG_0001.jpg");
    fireEvent.click(screen.getByRole("button", { name: /Open IMG_0001.jpg/i }));
    expect((await screen.findAllByText("Face detection unavailable")).length).toBeGreaterThan(0);
    expect(screen.queryByText("No faces detected")).toBeNull();
  });

  it("keeps the card and inspector face state ready when a local fallback succeeds", async () => {
    setCommandOverrides({ visual_media_page: analyzedVisual, capture_intelligence_summary_command: intelligenceProgress, media_asset_detail_command: fallbackFaceDetail });
    await renderProject();
    expect(await screen.findByText("★ Strong")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Open IMG_0001.jpg/i }));
    expect((await screen.findAllByText("1 detected")).length).toBeGreaterThan(0);
    expect(screen.queryByText("Face detection failed")).toBeNull();
    expect((await screen.findAllByText("Not applicable")).length).toBeGreaterThan(0);
    fireEvent.click(screen.getAllByText("Advanced / Developer Details")[0]);
    expect((await screen.findAllByText(/Resolved face provider ultraface-rfb-320/i)).length).toBeGreaterThan(0);
    expect((await screen.findAllByText(/Earlier face provider attempt Apple Vision failed/i)).length).toBeGreaterThan(0);
  });

  it("supports keyboard-first local culling without shortcuts leaking into a note field", async () => {
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Smart Cull" }));
    expect(await screen.findByText(/Human decisions are authoritative/)).toBeTruthy();
    fireEvent.keyDown(window, { key: "k" });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("update_culling_decision_command", expect.objectContaining({ projectId: "project-1", input: expect.objectContaining({ assetId: "asset-1", decision: "keep", sessionId: "session-1" }) })));
    const callsBeforeTyping = vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command").length;
    const note = screen.getByPlaceholderText("Client requested this one");
    fireEvent.keyDown(note, { key: "k" });
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command")).toHaveLength(callsBeforeTyping);
    fireEvent.click(screen.getByRole("button", { name: "Face View" }));
    expect(await screen.findByText("Detector crops across related frames")).toBeTruthy();
    fireEvent.keyDown(window, { key: "u" });
    await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command").length).toBeGreaterThan(callsBeforeTyping));
  });

  it("updates a completed set immediately and renders filenames instead of internal asset IDs", async () => {
    setCommandOverrides({ culling_workspace_command: completedTwoFrameCullingWorkspace });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Smart Cull" }));
    expect((await screen.findAllByText("DSC03401.JPG")).length).toBeGreaterThan(0);
    expect(screen.getAllByText("DSC03400.JPG").length).toBeGreaterThan(0);
    expect(screen.getAllByText("✓ Set Complete").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "✓ Set Complete" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByRole("button", { name: "Mark set complete" })).toBeNull();
    expect(screen.queryByText("asset-400")).toBeNull();
    expect(screen.queryByText("asset-401")).toBeNull();
  });

  it("marks a two-frame set complete as soon as both frames receive review decisions", async () => {
    setCommandOverrides({ culling_workspace_command: twoFrameCullingWorkspace });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Smart Cull" }));
    fireEvent.click(await screen.findByRole("button", { name: "K Keep" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "DSC03401.JPG" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "R Review" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "✓ Set Complete" }).hasAttribute("disabled")).toBe(true));
    expect(screen.getAllByText(/2 \/ 2 reviewed/).length).toBeGreaterThan(0);
  });

  it("keeps compare selection separate from human review decisions", async () => {
    setCommandOverrides({ culling_workspace_command: twoFrameCullingWorkspace });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Smart Cull" }));
    const decisionCallsBeforeSelection = vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command").length;
    fireEvent.doubleClick(await screen.findByRole("button", { name: /Open DSC03400.JPG; double click to select for compare/ }));
    expect(await screen.findByRole("button", { name: /Open DSC03400.JPG; Compare selection 1/ })).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command")).toHaveLength(decisionCallsBeforeSelection);
  });

  it("opens Studio Brain in an honest learning state without silently starting training", async () => {
    await renderProject();
    await screen.findByText("Local preference learning");
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_studio_brain_training_command")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Studio Brain" }));
    expect(await screen.findByRole("heading", { name: "Learning" })).toBeTruthy();
    expect(screen.getByText(/4 eligible explicit decisions/)).toBeTruthy();
    expect(screen.getByText(/never automatically culls/i)).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_studio_brain_training_command")).toHaveLength(0);
  });

  it("keeps Production dry-run and verified export explicit, human-rule based, and non-blocking", async () => {
    const plan = {
      id: "plan-1", projectId: "project-1", name: "Client Delivery", planType: "client_delivery", status: "draft",
      selectionRules: { decisions: ["keep"], minimumRating: null, starredOnly: false, momentIds: [], staticAssetIds: [], virtualCollectionId: null },
      organization: "by_moment", filenameStrategy: { kind: "preserve_original" }, destinationPath: "/Volumes/Delivery",
      destinationReserveBytes: 1024, estimatedFileCount: 0, estimatedBytes: 0, currentManifestId: null,
      createdAt: "2026-01-01T00:00:00Z", updatedAt: "2026-01-01T00:00:00Z",
    };
    const preview = {
      plan, destinationPath: "/Volumes/Delivery", destinationWritable: true, availableBytes: 1_000_000,
      requiredBytes: 2_048, reserveBytes: 1_024, headroomBytes: 999_000, availableSourceCount: 2,
      offlineSourceCount: 0, existingIdenticalCount: 0, collisionCount: 0, blockers: [], warnings: [],
      namingExamples: [{ originalFilename: "IMG_0001.JPG", destinationRelativePath: "01_Portraits/IMG_0001.JPG" }],
      inspection: { includedCount: 2, excludedCount: 1, blockedCount: 0, remainingCount: 0, items: [{ assetId: "asset-1", originalFilename: "IMG_0001.JPG", humanDecision: "keep", state: "included", destinationRelativePath: "01_Portraits/IMG_0001.JPG", reason: null, planOverride: null }] },
      manifestSummary: { selectedFileCount: 2, estimatedBytes: 2_048, checksum: "a".repeat(64), blockingIssueCount: 0, warningIssueCount: 0 },
    };
    const manifest = {
      id: "manifest-1", planId: "plan-1", projectId: "project-1", manifestVersion: 1, sourceRevision: 4,
      status: "ready", destinationPath: "/Volumes/Delivery", selectedFileCount: 2, estimatedBytes: 2_048,
      checksum: "a".repeat(64), createdAt: "2026-01-01T00:00:00Z",
    };
    setCommandOverrides({
      production_workspace_command: { plans: [plan], collections: [], recentExports: [] },
      moment_timeline: momentTimeline,
      update_production_plan_configuration_command: { id: "plan-1" },
      set_production_plan_destination_reserve_command: { id: "plan-1" },
      production_plan_preview_command: preview,
      create_production_manifest_command: manifest,
      production_manifest_preflight_command: {
        manifest, destinationWritable: true, availableBytes: 1_000_000, requiredBytes: 2_048,
        reserveBytes: 1_024, headroomBytes: 999_000, availableSourceCount: 2, offlineSourceCount: 0,
        existingIdenticalCount: 0, collisionCount: 0, blockers: [], warnings: [],
      },
      start_production_export_command: {
        exportJobId: "", manifestId: "manifest-1", state: "queued", stage: "queued", itemsCompleted: 0,
        itemsTotal: 2, verifiedCount: 0, skippedIdenticalCount: 0, failedCount: 0, verifiedBytes: 0,
        currentFilename: null, message: "Verified local export is queued and will not block project browsing.",
      },
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Production" }));
    expect(await screen.findByRole("heading", { name: "Production plans" })).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_production_export_command")).toHaveLength(0);
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "production_plan_preview_command")).toHaveLength(0);
    expect(screen.getByText(/Studio Brain remains advisory/)).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "moment_timeline")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Choose Moments" }));
    fireEvent.click(await screen.findByRole("checkbox", { name: /01 · Outdoor portraits \(2 files\)/ }));
    fireEvent.click(screen.getByRole("button", { name: "Save plan settings" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("update_production_plan_configuration_command", expect.objectContaining({
      projectId: "project-1",
      planId: "plan-1",
      input: expect.objectContaining({
        selectionRules: expect.objectContaining({ momentIds: ["moment-1"] }),
      }),
    })));
    fireEvent.change(screen.getByLabelText("Production safety reserve in GiB"), {
      target: { value: "2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save reserve" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("set_production_plan_destination_reserve_command", {
      projectId: "project-1",
      planId: "plan-1",
      reserveBytes: 2 * 1024 * 1024 * 1024,
    }));
    fireEvent.click(screen.getByRole("button", { name: "Preview dry run" }));
    await screen.findByText(/Dry run is ready/);
    fireEvent.click(screen.getByRole("button", { name: "Freeze manifest" }));
    await screen.findByText(/Immutable manifest saved/);
    fireEvent.click(screen.getByRole("button", { name: "Start verified export" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("start_production_export_command", {
      projectId: "project-1", manifestId: "manifest-1",
    }));
  });

  it("queues one explicit Studio Brain train action and never turns it into a culling decision", async () => {
    setCommandOverrides({
      start_studio_brain_training_command: {
        profileId: "studio-profile-1", state: "queued", active: true, stage: "queued", completed: 0,
        total: 0, errorCount: 0, activeModelVersion: null,
        message: "Studio Brain training is queued locally and will not block project browsing.", lastError: null,
      },
    });
    await renderProject();
    fireEvent.click(screen.getByRole("button", { name: "Studio Brain" }));
    await screen.findByRole("button", { name: "Train Studio Brain" });
    fireEvent.click(screen.getByRole("button", { name: "Train Studio Brain" }));
    fireEvent.click(screen.getByRole("button", { name: "Train Studio Brain" }));
    await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_studio_brain_training_command")).toHaveLength(1));
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "update_culling_decision_command")).toHaveLength(0);
  });
});
