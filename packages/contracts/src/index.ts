export type MediaFilter =
  | "all"
  | "photos"
  | "video"
  | "audio"
  | "sidecars"
  | "unknown";

export interface ProjectView {
  id: string;
  name: string;
}

/** Small, global-library projection. Names are display values; `id` is the route identity. */
export interface ProjectLibraryItem {
  id: string;
  name: string;
  createdAt: string;
  lastActivityAt: string;
  mediaAssetCount: number;
  storageVolumeCount: number;
  protectionState: "verified_copy_history" | "ingest_history_recorded" | "not_recorded" | string;
}

export interface IndexRootView {
  id: string;
  selectedPath: string;
  status: string;
  lastIndexedAt: string | null;
  storageVolumeId: string;
}

export interface JobView {
  id: string;
  state: string;
  stage: string;
  filesDiscovered: number;
  filesProcessed: number;
  errorCount: number;
  startedAt: string;
  finishedAt: string | null;
  errorMessage: string | null;
}

export interface ProjectIndexSummary {
  mediaAssets: number;
  fileInstances: number;
  storageVolumes: number;
  filesDiscovered: number;
  supportedMediaCount: number;
  unknownCount: number;
  duplicateFastFingerprintCount: number;
  /** Active local Moment cards only; reading this count never starts analysis. */
  momentCount: number;
  lastIndexedFolder: string | null;
  storageVolumeIdentity: string | null;
}

export interface IndexedMediaRow {
  assetId: string;
  fileInstanceId: string;
  filename: string;
  mediaType: string;
  extension: string | null;
  relativePath: string;
  selectedRoot: string;
  byteSize: number | null;
  modifiedAt: string | null;
  storageVolume: string;
  storageVolumeId: string;
  fingerprintPresent: boolean;
  status: string;
}

export interface ProjectHome {
  project: ProjectView;
  summary: ProjectIndexSummary;
  roots: IndexRootView[];
  latestJob: JobView | null;
  media: IndexedMediaRow[];
}

export type VisualMediaFilter =
  | "all"
  | "photos"
  | "raw"
  | "jpegHeif"
  | "video"
  | "audio"
  | "offline"
  | "available"
  | "strongCandidates"
  | "technicalIssues"
  | "probableDuplicates"
  | "similarGroups"
  | "faces"
  | "possibleClosedEyes"
  | "blurReview";

export type VisualMediaSort =
  | "captureTime"
  | "filename"
  | "fileSize"
  | "dateIndexed"
  | "mediaType";

export interface VisualMediaQuery {
  filter: VisualMediaFilter;
  sort: VisualMediaSort;
  descending: boolean;
  search?: string;
  cameraModel?: string;
  lensModel?: string;
  capturedFrom?: string;
  capturedTo?: string;
  /** Optional active M7 Moment membership scope, validated by the local backend. */
  momentId?: string;
  limit: number;
  offset: number;
}

export interface VisualMediaRow {
  assetId: string;
  fileInstanceId: string;
  filename: string;
  mediaType: string;
  extension: string | null;
  byteSize: number | null;
  capturedAt: string | null;
  indexedAt: string;
  relativePath: string;
  selectedRoot: string | null;
  storageVolume: string;
  storageVolumeId: string;
  isAvailable: boolean;
  /** Runtime-only URL backed by the registered CaptureOS preview protocol. */
  thumbnailPreviewUrl: string | null;
  mediumPreviewUrl: string | null;
  previewPreviewUrl: string | null;
  previewStatus: "pending" | "ready" | "unsupported" | "offline" | "corrupt" | "failed" | "timeout" | "cancelled" | "stale" | string;
  previewFailureReason: string | null;
  width: number | null;
  height: number | null;
  durationMs: number | null;
  cameraModel: string | null;
  lensModel: string | null;
  codec: string | null;
  intelligence: IntelligenceSummary;
}

export interface IntelligenceSummary {
  status: "pending" | "ready" | "unsupported" | "corrupt" | "needs_original" | "failed" | "not_applicable" | "stale" | string | null;
  technicalQualityBand: "strong" | "good" | "review" | "technical_issue" | "not_applicable" | string | null;
  technicalQualityScore: number | null;
  recommendation: "strong_candidate" | "strong_alternative" | "review" | "probable_duplicate" | "technical_issue" | "not_applicable" | string | null;
  recommendationConfidence: number | null;
  similarityGroupId: string | null;
  similarityGroupKind: "exact_duplicate_set" | "near_duplicate_set" | "similar_set" | "burst" | string | null;
  similarCount: number;
  faceCount: number;
  openEyesCount: number;
  possibleClosedEyesCount: number;
  blurLevel: "low" | "moderate" | "high" | "uncertain" | "not_applicable" | string | null;
  sharpnessBand: string | null;
  confidence: number | null;
  unavailableReason: string | null;
}

export interface VisualMediaPage {
  items: VisualMediaRow[];
  hasMore: boolean;
  cacheBytes: number;
}

export type CullingMode = "all_photos" | "similar_sets" | "ai_review_queue";
export type CullingFilter = "all" | "unreviewed" | "keep" | "reject" | "review" | "starred" | "five_star" | "four_plus" | "strong_candidates" | "technical_issues" | "possible_duplicates" | "similar_groups" | "faces" | "blur_review";
export type CullingDecision = "keep" | "review" | "reject";

/**
 * A bounded human-review query. `momentId` is an optional project-owned
 * Moment scope; it never changes culling decisions, Similar Sets, or review
 * history semantics.
 */
export interface CullingWorkspaceQuery {
  mode: CullingMode;
  filter: CullingFilter;
  groupId?: string;
  momentId?: string;
  limit: number;
  offset: number;
}

export interface CullingDecisionView {
  decision: CullingDecision | null;
  rating: number;
  starred: boolean;
  note: string | null;
  flags: string[];
  updatedAt: string | null;
}

export interface CullingMediaRow {
  media: VisualMediaRow;
  decision: CullingDecisionView;
  /** Detector boxes only; no identity matching or persistent face crops. */
  faces: FaceAnalysisEvidence[];
  relativeEvidence: string[];
  similarityGroupId: string | null;
  isAiRepresentative: boolean;
  isHumanRepresentative: boolean;
  /** Separate local preference advice. It never replaces or mutates `decision`. */
  studioBrain: StudioRecommendationView | null;
}

export interface CullingProgress {
  total: number;
  reviewed: number;
  keep: number;
  reject: number;
  review: number;
  unreviewed: number;
  starred: number;
  fiveStar: number;
  setsTotal: number;
  setsReviewed: number;
  strongCandidateKept: number;
  technicalIssueKept: number;
  strongCandidateRejected: number;
}

export interface CullingGroupSummary {
  id: string;
  kind: string;
  memberCount: number;
  aiRepresentativeAssetId: string;
  aiRepresentativeFilename: string;
  humanRepresentativeAssetId: string | null;
  humanRepresentativeFilename: string | null;
  reviewedCount: number;
  completed: boolean;
  completionKind: "auto_all_reviewed" | "explicit_user_completion" | null;
  /** Advisory only; this does not change generic or human representatives. */
  studioStartingPointAssetId: string | null;
  studioStartingPointReason: string | null;
}

export type StudioTrainingStatus = "not_ready" | "learning" | "ready" | "stale" | "error" | string;

/** Small, UI-safe local Studio Profile status. No source path, note text, raw feature vector,
 * model coefficient, probability, or biometric/identity signal is exposed here. */
export interface StudioBrainProjectStatus {
  profileId: string;
  profileName: string;
  trainingStatus: StudioTrainingStatus;
  personalizationEnabled: boolean;
  projectIncluded: boolean;
  eligibleDecisionCount: number;
  keepCount: number;
  reviewCount: number;
  rejectCount: number;
  ratingCount: number;
  starredCount: number;
  representativeCount: number;
  contributingProjectCount: number;
  activeModelVersion: string | null;
  lastTrainedAt: string | null;
  readiness: { state?: string; message?: string; conditions?: { key: string; met: boolean; message: string }[]; reasons?: string[] };
  lastError: string | null;
}

export interface StudioBrainProgress {
  profileId: string;
  state: StudioTrainingStatus | "queued" | "training" | "evaluating" | string;
  active: boolean;
  stage: string;
  completed: number;
  total: number;
  errorCount: number;
  activeModelVersion: string | null;
  message: string | null;
  /** Developer Details only; normal UI must use the friendly message. */
  lastError: string | null;
}

export interface StudioRecommendationView {
  recommendation: "likely_keep" | "likely_review" | "likely_reject" | "not_enough_evidence" | string;
  confidenceBand: "high" | "moderate" | "low" | "unavailable" | string;
  modelVersion: string;
  explanationFactors: string[];
  genericRecommendation: string | null;
  agreement: "agrees" | "differs" | "unavailable" | string;
  generatedAt: string;
}

export interface ReviewSessionView {
  id: string;
  projectId: string;
  startedAt: string;
  endedAt: string | null;
  mode: CullingMode | string;
  lastAssetId: string | null;
  lastGroupId: string | null;
  filterContext: string | null;
  photosReviewed: number;
  setsReviewed: number;
}

export interface CullingWorkspaceView {
  session: ReviewSessionView;
  progress: CullingProgress;
  items: CullingMediaRow[];
  groups: CullingGroupSummary[];
  hasMore: boolean;
}

export interface MediaMetadata {
  mediaAssetId: string;
  sourceFileInstanceId: string;
  sourceFingerprint: string;
  extractor: string;
  extractorVersion: string;
  status: string;
  failureReason: string | null;
  extractedAt: string;
  mimeType: string | null;
  byteSize: number | null;
  capturedAtRaw: string | null;
  capturedAtLocal: string | null;
  captureTimezone: string | null;
  captureTimeSource: string | null;
  /** Qualitative provenance assessment for the resolved local capture time. */
  captureTimeConfidence: string | null;
  width: number | null;
  height: number | null;
  orientation: string | null;
  cameraMake: string | null;
  cameraModel: string | null;
  lensMake: string | null;
  lensModel: string | null;
  focalLengthMm: number | null;
  focalLengthEquivalentMm: number | null;
  aperture: number | null;
  shutterSpeed: string | null;
  iso: number | null;
  exposureCompensation: string | null;
  flash: string | null;
  whiteBalance: string | null;
  colorSpace: string | null;
  gpsPresent: boolean | null;
  durationMs: number | null;
  frameRate: string | null;
  codec: string | null;
  pixelFormat: string | null;
  bitrate: number | null;
  audioStreams: number | null;
  videoStreams: number | null;
  sampleRate: number | null;
  bitDepth: number | null;
  channels: number | null;
  rawMetadata: Record<string, unknown>;
}

export interface MediaCopyView {
  fileInstanceId: string;
  relativePath: string;
  selectedRoot: string | null;
  storageVolume: string;
  isAvailable: boolean;
  observedAt: string;
}

export interface MediaAssetDetail {
  item: VisualMediaRow;
  metadata: MediaMetadata | null;
  copies: MediaCopyView[];
  intelligence: CaptureIntelligenceDetail | null;
}

export interface TechnicalQualityEvidence {
  globalSharpness: number | null;
  sharpnessBand: string;
  directionalBlurRatio: number | null;
  blurLevel: string;
  meanLuminance: number | null;
  medianLuminance: number | null;
  highlightClippingPercent: number | null;
  shadowClippingPercent: number | null;
  channelClippingPercent: number | null;
  technicalQualityScore: number | null;
  technicalQualityBand: string;
  confidence: number;
  status: string;
  errorMessage: string | null;
}

export interface FaceAnalysisEvidence {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  detectionConfidence: number;
  relativeSize: number;
  visibility: string | null;
  pose: string | null;
  faceSharpness: number | null;
  eyeState: "open" | "closed" | "uncertain" | "not_analyzable" | string;
  eyeConfidence: number | null;
}

export interface CaptureIntelligenceDetail {
  summary: IntelligenceSummary;
  inputFingerprint: string;
  provider: string;
  providerVersion: string;
  settingsVersion: string;
  generatedAt: string;
  technical: TechnicalQualityEvidence | null;
  faceProvider: string;
  faceProviderVersion: string;
  faceResolvedProvider: string;
  faceResolvedProviderVersion: string;
  faceAnalysisStatus: "ready" | "not_applicable" | "failed" | "unsupported" | string;
  faceAnalysisError: string | null;
  /** Diagnostic only: an earlier local provider attempt may fail before a fallback succeeds. */
  faceProviderAttemptError: string | null;
  faceLandmarkStatus: "ready" | "not_applicable" | "failed" | "unsupported" | string;
  faceLandmarkError: string | null;
  faces: FaceAnalysisEvidence[];
  recommendationReasons: string[];
  humanDecision: "keep" | "review" | "reject" | string | null;
}

export interface SimilarityGroupMemberView {
  assetId: string;
  filename: string;
  mediumPreviewUrl: string | null;
  similarityConfidence: number;
  timeProximitySeconds: number | null;
  isRepresentative: boolean;
  intelligence: IntelligenceSummary;
  /** Per-frame detection boxes for a local crop view; never identity-corresponded. */
  faces: FaceAnalysisEvidence[];
}

export interface SimilarityGroupView {
  id: string;
  kind: string;
  representativeAssetId: string;
  groupingMethod: string;
  groupingVersion: string;
  similarityConfidence: number;
  timeProximitySeconds: number | null;
  visualSimilarity: number | null;
  members: SimilarityGroupMemberView[];
}

export interface CaptureIntelligenceProgress {
  state: "queued" | "running" | "paused" | "completed" | "failed" | "cancelled" | "interrupted" | string;
  stage: string;
  resourceMode: "eco" | "balanced" | "fast" | string;
  itemsCompleted: number;
  itemsTotal: number;
  errorCount: number;
  readyCount: number;
  unsupportedCount: number;
  corruptCount: number;
  needsOriginalCount: number;
  failedCount: number;
  notApplicableCount: number;
  staleCount: number;
  currentAssetId: string | null;
  currentStageDetail: string | null;
  startedAt: string;
  finishedAt: string | null;
  message: string | null;
}

export interface MediaPreparationProgress {
  state: string;
  stage: string;
  itemsCompleted: number;
  itemsTotal: number;
  errorCount: number;
  readyCount: number;
  unsupportedCount: number;
  corruptCount: number;
  offlineCount: number;
  failedCount: number;
  timeoutCount: number;
  cancelledCount: number;
  currentAssetId: string | null;
  currentFileInstanceId: string | null;
  currentProvider: string | null;
  startedAt: string;
  finishedAt: string | null;
  message: string | null;
}

/**
 * Progress for a photographer-requested, local-only capture-time metadata refresh.
 * It reads available copies without changing originals, previews, semantic data, or
 * human review decisions. A completed refresh does not implicitly rebuild Moments.
 */
export interface MetadataRefreshProgress {
  state: "queued" | "running" | "completed" | "failed" | "interrupted" | string;
  itemsCompleted: number;
  itemsTotal: number;
  errorCount: number;
  resolvedCaptureTimeCount: number;
  highConfidenceCaptureTimeCount: number;
  copyConflictCount: number;
  currentAssetId: string | null;
  startedAt: string;
  finishedAt: string | null;
  message: string | null;
}

export interface MediaCapability {
  format: string;
  metadata: boolean;
  thumbnail: boolean;
  viewer: boolean;
  note: string;
}

export type IngestPolicy = "basic" | "standard";
export type IngestDestinationRole = "master" | "backup";

export interface IngestSourceInput {
  label: string;
  selectedPath: string;
}

export interface IngestDestinationInput {
  role: IngestDestinationRole;
  selectedPath: string;
}

export interface IngestRequest {
  projectName: string;
  sources: IngestSourceInput[];
  master: IngestDestinationInput;
  backups: IngestDestinationInput[];
}

export interface SourceInventory {
  label: string;
  selectedPath: string;
  fileCount: number;
  totalBytes: number;
  detectedMediaTypes: string[];
}

export interface DestinationPreflight {
  role: IngestDestinationRole;
  selectedPath: string;
  availableBytes: number | null;
  requiredBytes: number;
  headroomBytes: number | null;
  writable: boolean;
}

export interface PreflightIssue {
  severity: "warning" | "error";
  code: string;
  message: string;
}

export interface PreflightReport {
  sources: SourceInventory[];
  destinations: DestinationPreflight[];
  totalSourceBytes: number;
  issues: PreflightIssue[];
}

export interface IngestPreflightView {
  report: PreflightReport;
  canStart: boolean;
  policy: IngestPolicy;
}

export interface IngestJobSummary {
  id: string;
  state: string;
  policy: IngestPolicy;
  guardianState: string;
  safeToEject: boolean;
  filesTotal: number;
  filesVerified: number;
  filesFailed: number;
  bytesTotal: number;
  bytesVerified: number;
  createdAt: string;
  updatedAt: string;
  finishedAt: string | null;
}

export interface IngestSourceSummary {
  id: string;
  label: string;
  selectedPath: string;
  storageVolumeId: string;
  fileCount: number;
  totalBytes: number;
  status: string;
  warnings: string[];
}

export interface IngestDestinationSummary {
  id: string;
  role: IngestDestinationRole;
  selectedPath: string;
  storageVolumeId: string;
  storageVolumeName: string;
  availableBytes: number | null;
  requiredBytes: number;
  writable: boolean;
  status: string;
}

export interface IngestReport {
  job: IngestJobSummary;
  sources: IngestSourceSummary[];
  destinations: IngestDestinationSummary[];
  recentErrors: string[];
  sameVolumeWarning: boolean;
}

/**
 * Milestone 6 is deliberately limited to a locally installed, project-scoped
 * still-image semantic index. These types never contain embedding vectors or
 * filesystem paths: those remain local derived data owned by the backend.
 */
export type SemanticResourceMode = "eco" | "balanced" | "fast";

export interface SemanticModelIdentity {
  modelId: string;
  modelVersion: string;
  provider: string;
  licenseUrl: string | null;
  embeddingDimension: number | null;
  /** Size of the checksum-validated, closed local pack; never a filesystem path. */
  installedBytes: number | null;
}

export interface SemanticModelStatus {
  /** False means deterministic metadata filters still work, but semantic matching does not. */
  installed: boolean;
  message: string | null;
  identity: SemanticModelIdentity | null;
}

export interface SemanticIndexCounts {
  ready: number;
  unsupported: number;
  corrupt: number;
  needsOriginal: number;
  failed: number;
  pending: number;
  stale: number;
}

export interface SemanticIndexProgress {
  state: "idle" | "queued" | "running" | "paused" | "completed" | "failed" | "interrupted" | string;
  model: SemanticModelStatus;
  counts: SemanticIndexCounts;
  active: boolean;
  paused: boolean;
  completed: number;
  total: number;
  stage: string;
  resourceMode: SemanticResourceMode | string;
  indexReady: boolean;
  indexEmbeddingCount: number;
  lastError: string | null;
}

/**
 * Milestone 7 Moment Brain is a local, project-scoped structural timeline.
 * These projections deliberately contain display evidence instead of source
 * paths, embedding vectors, numeric semantic scores, or identity claims.
 */
export type MomentAnalysisState = "idle" | "queued" | "running" | "paused" | "completed" | "failed" | "interrupted" | string;
export type MomentBoundaryStrength = "continuous" | "moderate" | "strong" | "unavailable" | string;
export type MomentLabelSource = "human" | "generic_visual_vocabulary" | "project_checklist" | "none" | string;
export type MomentLabelStrength = "strong" | "moderate" | "weak" | "unavailable" | string;
export type CoverageConfirmationState = "unreviewed" | "confirmed_covered" | "needs_review" | "not_covered" | string;

export interface MomentAnalysisProgress {
  state: MomentAnalysisState;
  active: boolean;
  paused: boolean;
  stage: string;
  /** Existing local scheduler mode for an explicitly requested Moment analysis. */
  resourceMode: SemanticResourceMode | string;
  completed: number;
  total: number;
  errorCount: number;
  timelineReady: boolean;
  momentCount: number;
  ungroupedAssetCount: number;
  lastError: string | null;
  message: string | null;
}

/** A qualitative, evidence-grounded boundary explanation; never a probability. */
export interface MomentBoundaryEvidenceView {
  strength: MomentBoundaryStrength;
  summary: string;
  signals: string[];
}

/**
 * `displayLabel` is selected in this order: human label, supported local
 * suggestion, then "Untitled Moment". The underlying AI suggestion remains
 * available for developer details after a human rename.
 */
export interface MomentLabelView {
  displayLabel: string;
  aiSuggestedLabel: string | null;
  humanLabel: string | null;
  source: MomentLabelSource;
  strength: MomentLabelStrength;
  evidence: string[];
}

export interface MomentRepresentativeView {
  assetId: string;
  filename: string;
  thumbnailPreviewUrl: string | null;
  /** An AI suggestion is a starting point only; a human choice is authoritative. */
  source: "ai_suggested" | "human" | string;
  evidence: string[];
}

export interface MomentSummaryView {
  id: string;
  ordinal: number;
  /** False at an incremental-run boundary; use a full local rebuild before merging there. */
  canMergeWithPrevious: boolean;
  label: MomentLabelView;
  capturedFrom: string | null;
  capturedTo: string | null;
  captureTimeState: "observed" | "partially_observed" | "unavailable" | string;
  assetCount: number;
  /** Project-local factual review context. These counters never change any decision or set. */
  similarSetCount: number;
  keepCount: number;
  rejectCount: number;
  reviewCount: number;
  unreviewedCount: number;
  starredCount: number;
  technicalIssueCount: number;
  representative: MomentRepresentativeView | null;
  boundaryBefore: MomentBoundaryEvidenceView | null;
  /** True when a human split or merge constraint shaped this materialized Moment. */
  hasHumanStructureOverride: boolean;
}

export interface MomentClockDiagnosticView {
  cameraLabel: string;
  summary: string;
  state: "possible_offset" | "unavailable" | string;
}

/** A factual interval with no locally recorded capture activity; never a missing-coverage claim. */
export interface MomentTimelineGapView {
  startedAt: string;
  endedAt: string;
  durationSeconds: number;
  explanation: string;
}

export interface MomentTimelineRequest {
  limit: number;
  offset: number;
}

export interface MomentTimelineView {
  progress: MomentAnalysisProgress | null;
  moments: MomentSummaryView[];
  hasMore: boolean;
  totalMoments: number;
  ungroupedAssetCount: number;
  timelineGaps: MomentTimelineGapView[];
  clockDiagnostics: MomentClockDiagnosticView[];
}

/** Bounded semantic retrieval of current-project Moment cards only. */
export interface MomentSearchRequest {
  query: string;
  limit: number;
}

/**
 * No vectors, numeric similarity scores, paths, or object/identity claims leave the backend.
 * Results are local retrieval rankings of compatible durable Moment centroids only.
 */
export interface MomentSearchResponse {
  query: string;
  results: MomentSummaryView[];
  hasMore: boolean;
  totalResults: number;
  semanticAvailable: boolean;
  semanticApplied: boolean;
  semanticUnavailableReason: string | null;
  identitySearchBlocked: boolean;
  message: string | null;
}

export interface MomentDetailRequest {
  momentId: string;
  limit: number;
  offset: number;
}

export interface MomentDetailView {
  moment: MomentSummaryView;
  boundaryEvidence: MomentBoundaryEvidenceView[];
}

export interface CoverageChecklistItemView {
  id: string;
  phrase: string;
  state: CoverageConfirmationState;
  confirmedMomentId: string | null;
  confirmedAssetId: string | null;
  updatedAt: string | null;
}

export interface MomentChecklistView {
  id: string;
  name: string;
  items: CoverageChecklistItemView[];
}

export interface CreateCoverageChecklistItemInput {
  checklistId?: string;
  phrase: string;
}

export interface UpdateCoverageConfirmationInput {
  checklistItemId: string;
  state: CoverageConfirmationState;
  momentId?: string;
  assetId?: string;
}

export type MagicSearchSort = "relevance" | "captureTime" | "technicalQuality" | "rating";

export interface MagicSearchRequest {
  query: string;
  sort: MagicSearchSort;
  descending: boolean;
  limit: number;
  offset: number;
  /** Optional project-owned Moment scope. The backend must validate ownership. */
  momentId?: string;
}

/**
 * User-facing evidence only. `semanticScore` is a local similarity ranking
 * signal, not confidence or an object, identity, or localized detection claim.
 */
export interface MagicSearchResult {
  item: VisualMediaRow;
  scoreLabel: "High" | "Medium" | "Low" | string | null;
  /** Null when the result was produced only by deterministic filters. */
  semanticScore: number | null;
  explanation: string;
  matchedEvidence: string[];
}

export interface MagicSearchParsedFilters {
  chips: string[];
}

export interface MagicSearchResponse {
  query: string;
  results: MagicSearchResult[];
  hasMore: boolean;
  /** Count in the backend's current bounded result set; vectors and paths never leave the backend. */
  totalResults: number;
  semanticAvailable: boolean;
  semanticApplied: boolean;
  semanticUnavailableReason: string | null;
  /** True when the request asked for person identity recognition, which M6 does not provide. */
  identitySearchBlocked: boolean;
  parsedFilters: MagicSearchParsedFilters;
  message: string | null;
}

export interface MagicSearchHistoryEntry {
  id: string;
  query: string;
  createdAt: string;
  parsedFilters: MagicSearchParsedFilters;
}
