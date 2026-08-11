//! Local, private visual-semantic search primitives for CaptureOS.
//!
//! This crate has no network client and never opens a customer source path. The production
//! provider accepts only an already-decoded `AnalysisImage` supplied by CaptureOS's managed
//! preview pipeline. Model files are an opt-in, checksum-verified static ONNX pack underneath
//! an application-owned root; arbitrary user paths, Python, and pickle are deliberately absent.

use capture_intelligence::AnalysisImage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokenizers::Tokenizer;
use tract_onnx::prelude::{
    Framework, InferenceModelExt, TVec, Tensor, TypedModel, TypedRunnableModel,
};

pub const SUPPORTED_SIGLIP_MODEL_ID: &str = "google-siglip-base-patch16-224";
pub const SUPPORTED_SIGLIP_PACK_DIRECTORY: &str = "google-siglip-base-patch16-224";
pub const SEMANTIC_EMBEDDING_VERSION: &str = "m6.semantic-embedding.v1";
pub const SIGLIP_PREPROCESSING_VERSION: &str = "siglip-rgb224-pillow-bicubic-scale05.v1";
pub const SIGLIP_NORMALIZED_DOT_METRIC: &str = "siglip-normalized-dot.v1";
pub const SIGLIP_INPUT_SIZE: u32 = 224;
pub const SIGLIP_EMBEDDING_DIMENSION: usize = 768;
pub const SIGLIP_TEXT_SEQUENCE_LENGTH: usize = 64;

const SUPPORTED_SIGLIP_PACK_ID: &str = "captureos.semantic.siglip-base-p16-224.v1";
const SUPPORTED_SIGLIP_SOURCE_URL: &str = "https://huggingface.co/google/siglip-base-patch16-224";
const SIGLIP_EOS_AND_PAD_TOKEN_ID: i64 = 1;
const MAX_REFERENCE_IMAGE_EDGE: u32 = 4_096;
const MAX_REFERENCE_IMAGE_PIXELS: u64 = 4_096 * 4_096;
const SUPPORTED_SIGLIP_CAPABILITIES: &[&str] = &["image_text_embedding"];
const APPROVED_SIGLIP_PACK_DESCRIPTOR_JSON: &str =
    include_str!("../../../model-packs/google-siglip-base-patch16-224/approved-pack.json");

// Reference vectors are part of the static pack admission gate. The tolerance is deliberately
// fixed in CaptureOS code rather than pack-controlled, so a pack cannot weaken the check by
// declaring a larger error allowance in its own manifest.
const REFERENCE_VECTOR_MAX_ABSOLUTE_ERROR: f32 = 0.002;

const INDEX_FORMAT_VERSION: u32 = 1;
const INDEX_TABLES: usize = 4;
const INDEX_BITS_PER_TABLE: usize = 16;
const EXACT_SEARCH_ASSET_LIMIT: usize = 4_096;
const MAX_APPROXIMATE_CANDIDATES: usize = 8_192;

#[derive(Debug, Error)]
pub enum MagicSearchError {
    #[error("semantic model is not installed: {0}")]
    NotInstalled(String),
    #[error("semantic model pack is invalid: {0}")]
    InvalidModelPack(String),
    #[error("semantic model checksum did not match for {path}")]
    ChecksumMismatch { path: String },
    #[error("semantic embedding is invalid: {0}")]
    InvalidEmbedding(String),
    #[error("semantic query is invalid: {0}")]
    InvalidQuery(String),
    #[error("local ONNX provider failed: {0}")]
    Provider(String),
    #[error("local vector index is corrupt: {0}")]
    CorruptIndex(String),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticProviderIdentity {
    pub model_id: String,
    pub model_family: String,
    pub provider: String,
    pub model_version: String,
    pub embedding_version: String,
    pub preprocessing_version: String,
    pub metric: String,
    pub dimensions: usize,
}

impl SemanticProviderIdentity {
    pub fn cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.model_id,
            self.provider,
            self.model_version,
            self.embedding_version,
            self.preprocessing_version,
            self.metric,
            self.dimensions,
        )
    }
}

/// A real image/text embedding boundary. It exists separately from the M4 luminance descriptor
/// used by Similar Sets. Implementations must return vectors in one shared space.
pub trait SemanticEmbeddingProvider: Send + Sync {
    fn identity(&self) -> &SemanticProviderIdentity;
    fn embed_image(&self, image: &AnalysisImage) -> Result<Vec<f32>, MagicSearchError>;
    fn embed_text(&self, query: &str) -> Result<Vec<f32>, MagicSearchError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelCapability {
    pub installed: bool,
    pub message: String,
    pub identity: Option<SemanticProviderIdentity>,
}

/// Audited installation metadata copied from a pack only after its fixed manifest, static file
/// checksums, tokenizer parity check, and ONNX graphs have all validated. It is intentionally
/// descriptive rather than an executable-path escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedModelPackMetadata {
    pub source_revision: String,
    pub source_weights_sha256: String,
    pub license: String,
    pub license_url: String,
    pub input_size: u32,
    pub embedding_dimension: usize,
    /// Total bytes of the closed installed pack, including its compiled manifest. This is
    /// provenance/UI metadata only; admission still verifies every individual artifact.
    pub installed_bytes: u64,
}

/// Returns an honest capability state without downloading or executing a model. A pack can only
/// be opened from the fixed app-owned root and the fixed allow-listed model directory.
pub fn local_siglip_capability(model_root: &Path) -> LocalModelCapability {
    match discover_siglip_provider(model_root) {
        Ok(Some(provider)) => LocalModelCapability {
            installed: true,
            message: "Local semantic search model is ready.".into(),
            identity: Some(provider.identity().clone()),
        },
        Ok(None) => LocalModelCapability {
            installed: false,
            message:
                "Semantic model not installed. Metadata and technical filters remain available."
                    .into(),
            identity: None,
        },
        Err(error) => LocalModelCapability {
            installed: false,
            message: format!("Semantic model unavailable: {error}"),
            identity: None,
        },
    }
}

/// Discovers the only product-approved M6 pack location. This deliberately does not offer a UI
/// path picker: executable model material must be managed beneath CaptureOS's data directory.
pub fn discover_siglip_provider(
    model_root: &Path,
) -> Result<Option<SiglipOnnxProvider>, MagicSearchError> {
    let Some(pack_root) = siglip_pack_root(model_root)? else {
        return Ok(None);
    };
    SiglipOnnxProvider::open(&pack_root).map(Some)
}

/// Process-local cache for the one approved M6.1 semantic provider.
///
/// A cache hit is never based on a model name alone. Each acquisition first reads a closed,
/// lightweight stamp for the fixed pack directory: the exact manifest digest, the closed entry
/// set, and metadata identities for every admitted artifact. A missing, changed, extra, or
/// symbolic-link entry discards the cached provider. The next use then goes through the existing
/// full admission path, including every artifact checksum, tokenizer checks, ONNX loading, and
/// reference image/text inference before it can be cached again.
///
/// The stamp avoids re-reading the 815 MB pack on every local search, but it is deliberately not
/// presented as a cryptographic content check on every hit. A hostile local actor able to replace
/// bytes while preserving all inspected filesystem metadata requires a full checksum pass; the
/// full pass still occurs for every observed replacement, while an observed removal simply makes
/// the provider unavailable.
pub struct SiglipProviderCache {
    model_root: PathBuf,
    cached: Mutex<Option<CachedSiglipProvider>>,
}

struct CachedProvider<T> {
    stamp: SiglipPackStamp,
    provider: Arc<T>,
}

type CachedSiglipProvider = CachedProvider<SiglipOnnxProvider>;

impl SiglipProviderCache {
    /// The root is CaptureOS-owned application data. Callers do not provide a pack subdirectory:
    /// `acquire` always admits only the fixed product directory below this root.
    pub fn new(model_root: impl Into<PathBuf>) -> Self {
        Self {
            model_root: model_root.into(),
            cached: Mutex::new(None),
        }
    }

    /// Returns a shared provider only after a stable, fully validated admission. It returns
    /// `Ok(None)` when the fixed optional pack is absent, preserving deterministic-only search
    /// fallback without starting a download.
    pub fn acquire(&self) -> Result<Option<Arc<SiglipOnnxProvider>>, MagicSearchError> {
        // Keep a single cold admission in flight. Loading both full graphs is intentionally
        // serialized so concurrent status/search/index requests do not multiply memory use or
        // admit different replacement states.
        let mut cached = self.cached.lock().map_err(|_| {
            MagicSearchError::Provider("semantic provider cache lock was poisoned".into())
        })?;

        for _ in 0..2 {
            let before = match siglip_pack_stamp(&self.model_root) {
                Ok(Some(stamp)) => stamp,
                Ok(None) => {
                    *cached = None;
                    return Ok(None);
                }
                Err(error) => {
                    *cached = None;
                    return Err(error);
                }
            };
            if let Some(provider) = cached_provider_if_current(&cached, &before) {
                return Ok(Some(provider));
            }

            // Never retain a provider after its backing pack has changed, even when a later
            // admission fails. Existing Arc leases may finish an already-started operation, but
            // every new acquire observes the changed/missing pack and cannot reuse this entry.
            *cached = None;
            let provider = match discover_siglip_provider(&self.model_root)? {
                Some(provider) => provider,
                None => return Ok(None),
            };
            let after = match siglip_pack_stamp(&self.model_root) {
                Ok(Some(stamp)) => stamp,
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            };
            if before == after {
                let provider = Arc::new(provider);
                *cached = Some(CachedSiglipProvider {
                    stamp: after,
                    provider: Arc::clone(&provider),
                });
                return Ok(Some(provider));
            }
        }

        *cached = None;
        Err(MagicSearchError::InvalidModelPack(
            "model pack changed while it was being validated".into(),
        ))
    }
}

fn cached_provider_if_current<T>(
    cached: &Option<CachedProvider<T>>,
    stamp: &SiglipPackStamp,
) -> Option<Arc<T>> {
    cached
        .as_ref()
        .filter(|entry| entry.stamp == *stamp)
        .map(|entry| Arc::clone(&entry.provider))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SiglipPackStamp {
    manifest_sha256: String,
    pack_root: PackEntryStamp,
    files: BTreeMap<String, PackEntryStamp>,
}

/// File identities are intentionally metadata-only: full cryptographic verification remains in
/// `SiglipOnnxProvider::open`, while this stamp cheaply detects the normal atomic
/// install/replacement/removal path before a cached runtime is reused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackEntryStamp {
    length: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

fn siglip_pack_root(model_root: &Path) -> Result<Option<PathBuf>, MagicSearchError> {
    let pack_root = model_root.join(SUPPORTED_SIGLIP_PACK_DIRECTORY);
    let metadata = match fs::symlink_metadata(&pack_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MagicSearchError::InvalidModelPack(format!(
                "model pack root is unavailable: {error}"
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack root must not be a symlink".into(),
        ));
    }
    if !metadata.is_dir() {
        // Preserve discovery's historical missing-pack behavior for a non-directory at the
        // fixed location. A normal installer never creates this state.
        return Ok(None);
    }
    let root = pack_root.canonicalize().map_err(|error| {
        MagicSearchError::InvalidModelPack(format!("model pack root is unavailable: {error}"))
    })?;
    if !root.is_dir() {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack root is not a directory".into(),
        ));
    }
    Ok(Some(root))
}

fn siglip_pack_stamp(model_root: &Path) -> Result<Option<SiglipPackStamp>, MagicSearchError> {
    let Some(root) = siglip_pack_root(model_root)? else {
        return Ok(None);
    };
    let manifest_path = safe_pack_file(&root, "captureos-semantic-model.json")?;
    let manifest_bytes = fs::read(manifest_path)?;
    let manifest: SiglipPackManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    validate_approved_pack_admission(&manifest_bytes, &manifest)?;
    closed_siglip_pack_stamp(&root, &manifest, sha256_bytes(&manifest_bytes)).map(Some)
}

fn closed_siglip_pack_stamp(
    root: &Path,
    manifest: &SiglipPackManifest,
    manifest_sha256: String,
) -> Result<SiglipPackStamp, MagicSearchError> {
    validate_pack_root_contents(root, manifest)?;
    let mut files = BTreeMap::new();
    let manifest_path = "captureos-semantic-model.json";
    files.insert(
        manifest_path.into(),
        pack_entry_stamp(&safe_pack_file(root, manifest_path)?)?,
    );
    for file in [
        &manifest.image_model,
        &manifest.text_model,
        &manifest.tokenizer,
        &manifest.license_file,
        &manifest.notice_file,
        &manifest.conversion_provenance_file,
        &manifest.reference_vectors.image.rgb24,
    ] {
        let path = safe_pack_file(root, &file.path)?;
        let entry_stamp = pack_entry_stamp(&path)?;
        if files.insert(file.path.clone(), entry_stamp).is_some() {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack artifacts must use distinct fixed paths".into(),
            ));
        }
    }
    Ok(SiglipPackStamp {
        manifest_sha256,
        pack_root: pack_entry_stamp(root)?,
        files,
    })
}

fn pack_entry_stamp(path: &Path) -> Result<PackEntryStamp, MagicSearchError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        MagicSearchError::InvalidModelPack(format!(
            "model pack entry is unavailable for cache revalidation: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack entry was not a normal file or directory during cache revalidation".into(),
        ));
    }
    let modified = metadata.modified().map_err(|error| {
        MagicSearchError::InvalidModelPack(format!(
            "model pack entry modification time was unavailable for cache revalidation: {error}"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(PackEntryStamp {
            length: metadata.len(),
            modified,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(PackEntryStamp {
            length: metadata.len(),
            modified,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ModelPackFile {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct TokenIds {
    eos: i64,
    pad: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct TokenizerSelfTest {
    text: String,
    expected_ids: Vec<i64>,
}

/// A small, static RGB24 raster stored as a checksum-verified pack file. It deliberately has a
/// non-224 size, so provider admission exercises the reviewed bicubic resize path rather than
/// only an identity resize. The decoder boundary remains deliberately narrow: raw RGB24 only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ReferenceImage {
    width: u32,
    height: u32,
    rgb24: ModelPackFile,
}

/// Versioned local inference checks. The expected outputs are normalized vectors generated from
/// the reviewed immutable model revision. They are checked after the image/text ONNX graphs and
/// tokenizer have loaded, before the provider can be reported as installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ReferenceVectors {
    format_version: u32,
    image: ReferenceImage,
    text_query: String,
    expected_image_embedding: Vec<f32>,
    expected_text_embedding: Vec<f32>,
}

/// The static pack manifest is intentionally stricter than a generic ONNX manifest. This lets
/// CaptureOS reject unknown model families and unreviewed architecture changes before inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct SiglipPackManifest {
    format_version: u32,
    pack_id: String,
    model_id: String,
    model_family: String,
    runtime: String,
    model_version: String,
    source_url: String,
    source_revision: String,
    source_weights_sha256: String,
    license: String,
    license_url: String,
    image_model: ModelPackFile,
    text_model: ModelPackFile,
    tokenizer: ModelPackFile,
    license_file: ModelPackFile,
    notice_file: ModelPackFile,
    conversion_provenance_file: ModelPackFile,
    input_size: u32,
    embedding_dimension: usize,
    preprocessing_version: String,
    supported_capabilities: Vec<String>,
    text_sequence_length: usize,
    token_ids: TokenIds,
    /// The exact ONNX graph input order. M6.1 admits only `input_ids`; masks, token types, and
    /// future graph inputs are intentionally rejected rather than guessed at runtime.
    text_inputs: Vec<String>,
    image_output_index: usize,
    text_output_index: usize,
    /// M6.1 admits only a graph that emits its pooled text vector directly.
    text_pooling: String,
    tokenizer_self_tests: Vec<TokenizerSelfTest>,
    reference_vectors: ReferenceVectors,
}

/// A compact descriptor compiled into CaptureOS. The static digest pins every remaining manifest
/// field, including tokenizer regression IDs and reference vectors, without duplicating those
/// large vectors in source code. The explicit artifact list keeps file admission understandable
/// and independently protects against a manifest-digest mismatch caused by a bad installer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ApprovedSiglipPackDescriptor {
    format_version: u32,
    pack_id: String,
    model_id: String,
    model_family: String,
    runtime: String,
    model_version: String,
    source_url: String,
    source_revision: String,
    source_weights_sha256: String,
    license: String,
    license_url: String,
    preprocessing_version: String,
    supported_capabilities: Vec<String>,
    manifest_sha256: String,
    files: ApprovedSiglipPackFiles,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
struct ApprovedSiglipPackFiles {
    image_model: ModelPackFile,
    text_model: ModelPackFile,
    tokenizer: ModelPackFile,
    license_file: ModelPackFile,
    notice_file: ModelPackFile,
    conversion_provenance_file: ModelPackFile,
    reference_rgb24: ModelPackFile,
}

type RunnableModel = TypedRunnableModel<TypedModel>;

/// CPU-only, static ONNX SigLIP adapter. It contains no network implementation and never loads
/// a model selected by raw user text. The runtime is intentionally narrow so a future provider
/// (for example an audited CoreML accelerator) can sit behind the same trait.
pub struct SiglipOnnxProvider {
    identity: SemanticProviderIdentity,
    manifest: SiglipPackManifest,
    installed_bytes: u64,
    image_model: Mutex<RunnableModel>,
    text_model: Mutex<RunnableModel>,
    tokenizer: StaticSiglipTokenizer,
}

impl SiglipOnnxProvider {
    fn open(pack_root: &Path) -> Result<Self, MagicSearchError> {
        if fs::symlink_metadata(pack_root)
            .map_err(|error| {
                MagicSearchError::InvalidModelPack(format!(
                    "model pack root is unavailable: {error}"
                ))
            })?
            .file_type()
            .is_symlink()
        {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack root must not be a symlink".into(),
            ));
        }
        let root = pack_root.canonicalize().map_err(|error| {
            MagicSearchError::InvalidModelPack(format!("model pack root is unavailable: {error}"))
        })?;
        if !root.is_dir() {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack root is not a directory".into(),
            ));
        }
        let manifest_path = safe_pack_file(&root, "captureos-semantic-model.json")?;
        let manifest_bytes = fs::read(manifest_path)?;
        let manifest: SiglipPackManifest = serde_json::from_slice(&manifest_bytes)?;
        validate_manifest(&manifest)?;
        validate_approved_pack_admission(&manifest_bytes, &manifest)?;
        validate_pack_root_contents(&root, &manifest)?;
        let image_path = verified_pack_file(&root, &manifest.image_model)?;
        let text_path = verified_pack_file(&root, &manifest.text_model)?;
        let tokenizer_path = verified_pack_file(&root, &manifest.tokenizer)?;
        let _license_path = verified_pack_file(&root, &manifest.license_file)?;
        let _notice_path = verified_pack_file(&root, &manifest.notice_file)?;
        let _conversion_provenance_path =
            verified_pack_file(&root, &manifest.conversion_provenance_file)?;
        let reference_image_path =
            verified_pack_file(&root, &manifest.reference_vectors.image.rgb24)?;
        let reference_image =
            load_reference_image(&manifest.reference_vectors.image, &reference_image_path)?;
        let tokenizer = StaticSiglipTokenizer::from_json(&fs::read(tokenizer_path)?)?;
        for self_test in &manifest.tokenizer_self_tests {
            let self_test_ids = tokenizer.encode(&self_test.text)?;
            if self_test_ids != self_test.expected_ids {
                return Err(MagicSearchError::InvalidModelPack(
                    "tokenizer self-test did not match the approved pack".into(),
                ));
            }
        }
        let image_model = load_runnable_model(&image_path)?;
        let text_model = load_runnable_model(&text_path)?;
        let artifact_bytes = [
            &manifest.image_model,
            &manifest.text_model,
            &manifest.tokenizer,
            &manifest.license_file,
            &manifest.notice_file,
            &manifest.conversion_provenance_file,
            &manifest.reference_vectors.image.rgb24,
        ]
        .into_iter()
        .try_fold(0_u64, |total, file| {
            total.checked_add(file.bytes).ok_or_else(|| {
                MagicSearchError::InvalidModelPack(
                    "installed pack byte count overflowed u64".into(),
                )
            })
        })?;
        let installed_bytes = u64::try_from(manifest_bytes.len())
            .map_err(|_| {
                MagicSearchError::InvalidModelPack("manifest length overflowed u64".into())
            })?
            .checked_add(artifact_bytes)
            .ok_or_else(|| {
                MagicSearchError::InvalidModelPack(
                    "installed pack byte count overflowed u64".into(),
                )
            })?;
        let provider = Self {
            identity: SemanticProviderIdentity {
                model_id: manifest.model_id.clone(),
                model_family: manifest.model_family.clone(),
                provider: manifest.runtime.clone(),
                model_version: manifest.model_version.clone(),
                embedding_version: SEMANTIC_EMBEDDING_VERSION.into(),
                preprocessing_version: SIGLIP_PREPROCESSING_VERSION.into(),
                metric: SIGLIP_NORMALIZED_DOT_METRIC.into(),
                dimensions: manifest.embedding_dimension,
            },
            manifest,
            installed_bytes,
            image_model: Mutex::new(image_model),
            text_model: Mutex::new(text_model),
            tokenizer,
        };
        provider.validate_reference_vectors(&reference_image)?;
        Ok(provider)
    }

    /// Reject a pack before it is available to the UI when the actual image/text graph outputs
    /// do not match its reviewed static reference vectors. This is deliberately performed at
    /// discovery time, not deferred until a photographer's first search or embedding job.
    fn validate_reference_vectors(
        &self,
        reference_image: &AnalysisImage,
    ) -> Result<(), MagicSearchError> {
        let image_embedding = self.embed_image(reference_image).map_err(|error| {
            MagicSearchError::InvalidModelPack(format!(
                "reference image inference did not validate the local pack: {error}"
            ))
        })?;
        let text_embedding = self
            .embed_text(&self.manifest.reference_vectors.text_query)
            .map_err(|error| {
                MagicSearchError::InvalidModelPack(format!(
                    "reference text inference did not validate the local pack: {error}"
                ))
            })?;
        validate_reference_outputs(
            &self.manifest.reference_vectors,
            self.identity.dimensions,
            &image_embedding,
            &text_embedding,
        )
    }

    pub fn pack_metadata(&self) -> ValidatedModelPackMetadata {
        ValidatedModelPackMetadata {
            source_revision: self.manifest.source_revision.clone(),
            source_weights_sha256: self.manifest.source_weights_sha256.clone(),
            license: self.manifest.license.clone(),
            license_url: self.manifest.license_url.clone(),
            input_size: self.manifest.input_size,
            embedding_dimension: self.manifest.embedding_dimension,
            installed_bytes: self.installed_bytes,
        }
    }
}

impl SemanticEmbeddingProvider for SiglipOnnxProvider {
    fn identity(&self) -> &SemanticProviderIdentity {
        &self.identity
    }

    fn embed_image(&self, image: &AnalysisImage) -> Result<Vec<f32>, MagicSearchError> {
        let input = Tensor::from_shape(
            &[
                1,
                3,
                self.manifest.input_size as usize,
                self.manifest.input_size as usize,
            ],
            &siglip_image_input(image, self.manifest.input_size),
        )
        .map_err(|error| {
            MagicSearchError::Provider(format!("image tensor construction failed: {error}"))
        })?;
        let outputs = self
            .image_model
            .lock()
            .map_err(|_| MagicSearchError::Provider("image model lock was poisoned".into()))?
            .run(TVec::from_vec(vec![input.into()]))
            .map_err(|error| {
                MagicSearchError::Provider(format!("image inference failed: {error}"))
            })?;
        normalize_embedding(select_pooled_output_embedding(
            &outputs,
            self.manifest.image_output_index,
            self.identity.dimensions,
            "image",
        )?)
    }

    fn embed_text(&self, query: &str) -> Result<Vec<f32>, MagicSearchError> {
        let token_ids = self.tokenizer.encode(query)?;
        let ids = Tensor::from_shape(&[1, self.manifest.text_sequence_length], &token_ids)
            .map_err(|error| {
                MagicSearchError::Provider(format!("text token tensor failed: {error}"))
            })?;
        let outputs = self
            .text_model
            .lock()
            .map_err(|_| MagicSearchError::Provider("text model lock was poisoned".into()))?
            .run(TVec::from_vec(vec![ids.into()]))
            .map_err(|error| {
                MagicSearchError::Provider(format!("text inference failed: {error}"))
            })?;
        normalize_embedding(select_pooled_output_embedding(
            &outputs,
            self.manifest.text_output_index,
            self.identity.dimensions,
            "text",
        )?)
    }
}

fn validate_manifest(manifest: &SiglipPackManifest) -> Result<(), MagicSearchError> {
    if manifest.format_version != 2
        || manifest.pack_id != SUPPORTED_SIGLIP_PACK_ID
        || manifest.model_id != SUPPORTED_SIGLIP_MODEL_ID
        || manifest.model_family != "siglip"
        || manifest.runtime != "tract-onnx"
        || manifest.source_url != SUPPORTED_SIGLIP_SOURCE_URL
        || manifest.license != "Apache-2.0"
        || manifest.input_size != SIGLIP_INPUT_SIZE
        || manifest.embedding_dimension != SIGLIP_EMBEDDING_DIMENSION
        || manifest.preprocessing_version != SIGLIP_PREPROCESSING_VERSION
        || manifest.supported_capabilities
            != SUPPORTED_SIGLIP_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect::<Vec<_>>()
        || manifest.text_sequence_length != SIGLIP_TEXT_SEQUENCE_LENGTH
        || manifest.model_version.trim().is_empty()
        || manifest.source_revision.trim().is_empty()
        || !valid_sha256(&manifest.source_weights_sha256)
        || manifest.license_url.trim().is_empty()
    {
        return Err(MagicSearchError::InvalidModelPack(
            "manifest did not match CaptureOS's reviewed SigLIP descriptor".into(),
        ));
    }
    if manifest.token_ids.eos != SIGLIP_EOS_AND_PAD_TOKEN_ID
        || manifest.token_ids.pad != SIGLIP_EOS_AND_PAD_TOKEN_ID
        || manifest.text_inputs.len() != 1
        || manifest.text_inputs.first().map(String::as_str) != Some("input_ids")
        || manifest.image_output_index != 0
        || manifest.text_output_index != 0
        || manifest.text_pooling != "pooled"
        || manifest.tokenizer_self_tests.is_empty()
        || manifest.tokenizer_self_tests.len() > 8
        || manifest.tokenizer_self_tests.iter().any(|self_test| {
            self_test.text.trim().is_empty()
                || self_test.text.chars().count() > 512
                || !valid_siglip_token_sequence(&self_test.expected_ids)
        })
    {
        return Err(MagicSearchError::InvalidModelPack(
            "manifest has an unsupported static text-model or tokenizer contract".into(),
        ));
    }
    validate_reference_vector_contract(&manifest.reference_vectors, manifest.embedding_dimension)?;
    for file in [
        &manifest.image_model,
        &manifest.text_model,
        &manifest.tokenizer,
        &manifest.license_file,
        &manifest.notice_file,
        &manifest.conversion_provenance_file,
        &manifest.reference_vectors.image.rgb24,
    ] {
        if file.bytes == 0 || !valid_sha256(&file.sha256) || !is_safe_relative_path(&file.path) {
            return Err(MagicSearchError::InvalidModelPack(
                "manifest contains an unsafe file location or checksum".into(),
            ));
        }
    }
    Ok(())
}

fn approved_siglip_pack_descriptor() -> Result<ApprovedSiglipPackDescriptor, MagicSearchError> {
    let descriptor: ApprovedSiglipPackDescriptor =
        serde_json::from_str(APPROVED_SIGLIP_PACK_DESCRIPTOR_JSON).map_err(|error| {
            MagicSearchError::InvalidModelPack(format!(
                "CaptureOS's compiled SigLIP descriptor is invalid: {error}"
            ))
        })?;
    if descriptor.format_version != 1
        || descriptor.pack_id != SUPPORTED_SIGLIP_PACK_ID
        || descriptor.model_id != SUPPORTED_SIGLIP_MODEL_ID
        || descriptor.model_family != "siglip"
        || descriptor.runtime != "tract-onnx"
        || descriptor.source_url != SUPPORTED_SIGLIP_SOURCE_URL
        || descriptor.license != "Apache-2.0"
        || descriptor.preprocessing_version != SIGLIP_PREPROCESSING_VERSION
        || descriptor.supported_capabilities
            != SUPPORTED_SIGLIP_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect::<Vec<_>>()
        || descriptor.model_version.trim().is_empty()
        || descriptor.source_revision.trim().is_empty()
        || !valid_sha256(&descriptor.source_weights_sha256)
        || !valid_sha256(&descriptor.manifest_sha256)
        || descriptor.license_url.trim().is_empty()
    {
        return Err(MagicSearchError::InvalidModelPack(
            "CaptureOS's compiled SigLIP descriptor did not match the reviewed model contract"
                .into(),
        ));
    }
    for file in [
        &descriptor.files.image_model,
        &descriptor.files.text_model,
        &descriptor.files.tokenizer,
        &descriptor.files.license_file,
        &descriptor.files.notice_file,
        &descriptor.files.conversion_provenance_file,
        &descriptor.files.reference_rgb24,
    ] {
        if file.bytes == 0 || !valid_sha256(&file.sha256) || !is_safe_relative_path(&file.path) {
            return Err(MagicSearchError::InvalidModelPack(
                "CaptureOS's compiled SigLIP descriptor contained an invalid artifact".into(),
            ));
        }
    }
    Ok(descriptor)
}

fn validate_approved_pack_admission(
    manifest_bytes: &[u8],
    manifest: &SiglipPackManifest,
) -> Result<(), MagicSearchError> {
    let descriptor = approved_siglip_pack_descriptor()?;
    validate_manifest_against_descriptor(&descriptor, manifest_bytes, manifest)
}

fn validate_manifest_against_descriptor(
    descriptor: &ApprovedSiglipPackDescriptor,
    manifest_bytes: &[u8],
    manifest: &SiglipPackManifest,
) -> Result<(), MagicSearchError> {
    if sha256_bytes(manifest_bytes) != descriptor.manifest_sha256
        || manifest.pack_id != descriptor.pack_id
        || manifest.model_id != descriptor.model_id
        || manifest.model_family != descriptor.model_family
        || manifest.runtime != descriptor.runtime
        || manifest.model_version != descriptor.model_version
        || manifest.source_url != descriptor.source_url
        || manifest.source_revision != descriptor.source_revision
        || manifest.source_weights_sha256 != descriptor.source_weights_sha256
        || manifest.license != descriptor.license
        || manifest.license_url != descriptor.license_url
        || manifest.preprocessing_version != descriptor.preprocessing_version
        || manifest.supported_capabilities != descriptor.supported_capabilities
        || manifest.image_model != descriptor.files.image_model
        || manifest.text_model != descriptor.files.text_model
        || manifest.tokenizer != descriptor.files.tokenizer
        || manifest.license_file != descriptor.files.license_file
        || manifest.notice_file != descriptor.files.notice_file
        || manifest.conversion_provenance_file != descriptor.files.conversion_provenance_file
        || manifest.reference_vectors.image.rgb24 != descriptor.files.reference_rgb24
    {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack did not exactly match CaptureOS's compiled SigLIP descriptor".into(),
        ));
    }
    Ok(())
}

fn validate_reference_vector_contract(
    reference: &ReferenceVectors,
    dimensions: usize,
) -> Result<(), MagicSearchError> {
    if reference.format_version != 2
        || reference.image.width == 0
        || reference.image.height == 0
        || reference.image.width > MAX_REFERENCE_IMAGE_EDGE
        || reference.image.height > MAX_REFERENCE_IMAGE_EDGE
        || u64::from(reference.image.width) * u64::from(reference.image.height)
            > MAX_REFERENCE_IMAGE_PIXELS
        || (reference.image.width == SIGLIP_INPUT_SIZE
            && reference.image.height == SIGLIP_INPUT_SIZE)
        || reference.text_query.trim().is_empty()
        || reference.text_query.chars().count() > 512
    {
        return Err(MagicSearchError::InvalidModelPack(
            "manifest has an unsupported static reference-vector contract".into(),
        ));
    }
    validate_expected_reference_embedding(
        &reference.expected_image_embedding,
        dimensions,
        "image",
    )?;
    validate_expected_reference_embedding(&reference.expected_text_embedding, dimensions, "text")
}

fn valid_siglip_token_sequence(ids: &[i64]) -> bool {
    ids.len() == SIGLIP_TEXT_SEQUENCE_LENGTH
        && ids.iter().all(|id| *id >= 0)
        && ids.last() == Some(&SIGLIP_EOS_AND_PAD_TOKEN_ID)
}

fn validate_expected_reference_embedding(
    values: &[f32],
    dimensions: usize,
    label: &str,
) -> Result<(), MagicSearchError> {
    if values.len() != dimensions || values.iter().any(|value| !value.is_finite()) {
        return Err(MagicSearchError::InvalidModelPack(format!(
            "reference {label} embedding did not match the approved dimensions"
        )));
    }
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > REFERENCE_VECTOR_MAX_ABSOLUTE_ERROR {
        return Err(MagicSearchError::InvalidModelPack(format!(
            "reference {label} embedding was not normalized"
        )));
    }
    Ok(())
}

fn load_reference_image(
    reference: &ReferenceImage,
    path: &Path,
) -> Result<AnalysisImage, MagicSearchError> {
    let expected_bytes = u64::from(reference.width)
        .checked_mul(u64::from(reference.height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| {
            MagicSearchError::InvalidModelPack(
                "static reference image dimensions overflowed the RGB24 contract".into(),
            )
        })?;
    if fs::metadata(path)?.len() != expected_bytes {
        return Err(MagicSearchError::InvalidModelPack(
            "static reference image did not have the approved RGB24 byte length".into(),
        ));
    }
    if reference.rgb24.bytes != expected_bytes {
        return Err(MagicSearchError::InvalidModelPack(
            "static reference image byte count did not match the approved pack manifest".into(),
        ));
    }
    let rgb = fs::read(path)?;
    AnalysisImage::new(reference.width, reference.height, rgb).map_err(|error| {
        MagicSearchError::InvalidModelPack(format!(
            "static reference image did not match its RGB24 contract: {error}"
        ))
    })
}

fn validate_reference_outputs(
    reference: &ReferenceVectors,
    dimensions: usize,
    image_embedding: &[f32],
    text_embedding: &[f32],
) -> Result<(), MagicSearchError> {
    validate_reference_vector_contract(reference, dimensions)?;
    validate_reference_output(
        "image",
        &reference.expected_image_embedding,
        image_embedding,
        dimensions,
    )?;
    validate_reference_output(
        "text",
        &reference.expected_text_embedding,
        text_embedding,
        dimensions,
    )
}

fn validate_reference_output(
    label: &str,
    expected: &[f32],
    actual: &[f32],
    dimensions: usize,
) -> Result<(), MagicSearchError> {
    if actual.len() != dimensions || actual.iter().any(|value| !value.is_finite()) {
        return Err(MagicSearchError::InvalidModelPack(format!(
            "reference {label} inference did not produce the approved dimensions"
        )));
    }
    let maximum_error = expected
        .iter()
        .zip(actual)
        .map(|(expected, actual)| (expected - actual).abs())
        .fold(0.0_f32, f32::max);
    if maximum_error > REFERENCE_VECTOR_MAX_ABSOLUTE_ERROR {
        return Err(MagicSearchError::InvalidModelPack(format!(
            "reference {label} inference did not match the approved local pack"
        )));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// The static pack is data-only and has a closed file set. Reject hidden files, symlinks, and
/// unlisted directories rather than leaving material beside the reviewed ONNX/tokenizer payloads
/// for a future loader to accidentally trust.
fn validate_pack_root_contents(
    root: &Path,
    manifest: &SiglipPackManifest,
) -> Result<(), MagicSearchError> {
    let mut allowed_files = BTreeSet::from(["captureos-semantic-model.json".to_owned()]);
    for file in [
        &manifest.image_model,
        &manifest.text_model,
        &manifest.tokenizer,
        &manifest.license_file,
        &manifest.notice_file,
        &manifest.conversion_provenance_file,
        &manifest.reference_vectors.image.rgb24,
    ] {
        allowed_files.insert(file.path.clone());
    }
    if allowed_files.len() != 8 {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack artifacts must use distinct fixed paths".into(),
        ));
    }

    let allowed_directories = allowed_files
        .iter()
        .flat_map(|path| {
            let mut parents = Vec::new();
            let mut current = Path::new(path).parent();
            while let Some(parent) = current {
                if parent.as_os_str().is_empty() {
                    break;
                }
                parents.push(relative_path_string(parent));
                current = parent.parent();
            }
            parents
        })
        .collect::<BTreeSet<_>>();
    let mut actual_files = BTreeSet::new();
    collect_pack_entries(root, Path::new(""), &allowed_directories, &mut actual_files)?;
    if actual_files != allowed_files {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack contained unlisted, missing, or non-regular artifacts".into(),
        ));
    }
    Ok(())
}

fn collect_pack_entries(
    root: &Path,
    relative: &Path,
    allowed_directories: &BTreeSet<String>,
    actual_files: &mut BTreeSet<String>,
) -> Result<(), MagicSearchError> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let child = relative.join(entry.file_name());
        let child_string = relative_path_string(&child);
        if !is_safe_relative_path(&child_string) {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack contained an unsafe artifact path".into(),
            ));
        }
        if file_type.is_symlink() {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack must not contain symbolic links".into(),
            ));
        }
        if file_type.is_dir() {
            if !allowed_directories.contains(&child_string) {
                return Err(MagicSearchError::InvalidModelPack(
                    "model pack contained an unlisted directory".into(),
                ));
            }
            collect_pack_entries(root, &child, allowed_directories, actual_files)?;
        } else if file_type.is_file() {
            actual_files.insert(child_string);
        } else {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack contained a non-regular artifact".into(),
            ));
        }
    }
    Ok(())
}

fn relative_path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn safe_pack_file(root: &Path, relative: &str) -> Result<PathBuf, MagicSearchError> {
    if !is_safe_relative_path(relative) {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack path was not a safe relative path".into(),
        ));
    }
    let candidate = root.join(relative).canonicalize().map_err(|error| {
        MagicSearchError::InvalidModelPack(format!("model pack file is unavailable: {error}"))
    })?;
    if !candidate.is_file() || !candidate.starts_with(root) {
        return Err(MagicSearchError::InvalidModelPack(
            "model pack file escaped its managed root".into(),
        ));
    }
    Ok(candidate)
}

fn verified_pack_file(root: &Path, file: &ModelPackFile) -> Result<PathBuf, MagicSearchError> {
    let path = safe_pack_file(root, &file.path)?;
    if fs::metadata(&path)?.len() != file.bytes {
        return Err(MagicSearchError::InvalidModelPack(format!(
            "model pack file length did not match the approved manifest: {}",
            file.path
        )));
    }
    if sha256_file(&path)? != file.sha256.to_ascii_lowercase() {
        return Err(MagicSearchError::ChecksumMismatch {
            path: file.path.clone(),
        });
    }
    Ok(path)
}

fn sha256_file(path: &Path) -> Result<String, MagicSearchError> {
    let mut reader = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn load_runnable_model(path: &Path) -> Result<RunnableModel, MagicSearchError> {
    tract_onnx::onnx()
        .model_for_path(path)
        // The fixed SigLIP graphs are already statically exported and validated. Tract's
        // exhaustive optimize/constant-fold pass turns a practical local load into an
        // unbounded multi-minute startup on the full encoder weights. A typed-only plan leaves
        // SigLIP's matrix multiplies on Tract's generic scalar EinSum evaluator, so perform its
        // bounded code-generation pass without the exhaustive declutter/constant-fold pipeline.
        .and_then(|model| model.into_typed())
        .and_then(|mut model| {
            model.optimize()?;
            Ok(model)
        })
        .and_then(|model| model.into_runnable())
        .map_err(|error| {
            MagicSearchError::Provider(format!("could not load static ONNX graph: {error}"))
        })
}

fn siglip_image_input(image: &AnalysisImage, target_size: u32) -> Vec<f32> {
    let target = target_size as usize;
    let pixels = target * target;
    let mut channels = vec![0.0; pixels * 3];
    let rgb = pillow_bicubic_resize_rgb(image, target_size, target_size);
    for y in 0..target {
        for x in 0..target {
            let destination = y * target + x;
            for channel in 0..3 {
                // Google SigLIP's processor: RGB / 255, mean=.5, std=.5.
                channels[channel * pixels + destination] =
                    (f32::from(rgb[(destination * 3) + channel]) / 255.0 - 0.5) / 0.5;
            }
        }
    }
    channels
}

/// Pillow-compatible RGB bicubic resize used by Hugging Face's SigLIP image processor
/// (`resample: 3`). Pillow uses a separable Catmull-Rom cubic filter and rounds each horizontal
/// RGB pass back to bytes before applying the vertical pass. Keeping that intermediate rounding
/// matters: a single floating-point two-dimensional pass produces different pixels.
fn pillow_bicubic_resize_rgb(
    image: &AnalysisImage,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    if image.width == target_width && image.height == target_height {
        return image.rgb().to_vec();
    }

    let horizontal = pillow_resample_contributions(image.width, target_width);
    let vertical = pillow_resample_contributions(image.height, target_height);
    let source_width = image.width as usize;
    let source_height = image.height as usize;
    let target_width = target_width as usize;
    let target_height = target_height as usize;

    let mut intermediate = vec![0_u8; source_height * target_width * 3];
    for source_y in 0..source_height {
        for (target_x, contribution) in horizontal.iter().enumerate() {
            for channel in 0..3 {
                let value = contribution
                    .weights
                    .iter()
                    .enumerate()
                    .map(|(offset, weight)| {
                        f64::from(
                            image.rgb()[((source_y * source_width) + contribution.start + offset)
                                * 3
                                + channel],
                        ) * weight
                    })
                    .sum();
                intermediate[((source_y * target_width) + target_x) * 3 + channel] =
                    round_clamp_u8(value);
            }
        }
    }

    let mut resized = vec![0_u8; target_width * target_height * 3];
    for (target_y, contribution) in vertical.iter().enumerate() {
        for target_x in 0..target_width {
            for channel in 0..3 {
                let value = contribution
                    .weights
                    .iter()
                    .enumerate()
                    .map(|(offset, weight)| {
                        f64::from(
                            intermediate[(((contribution.start + offset) * target_width)
                                + target_x)
                                * 3
                                + channel],
                        ) * weight
                    })
                    .sum();
                resized[((target_y * target_width) + target_x) * 3 + channel] =
                    round_clamp_u8(value);
            }
        }
    }
    resized
}

#[derive(Debug, Clone)]
struct PillowResampleContribution {
    start: usize,
    weights: Vec<f64>,
}

fn pillow_resample_contributions(
    input_size: u32,
    output_size: u32,
) -> Vec<PillowResampleContribution> {
    debug_assert!(input_size > 0 && output_size > 0);
    let scale = f64::from(input_size) / f64::from(output_size);
    let filter_scale = scale.max(1.0);
    let support = 2.0 * filter_scale;
    (0..output_size)
        .map(|target| {
            let center = (f64::from(target) + 0.5) * scale;
            let start = (center - support + 0.5)
                .floor()
                .clamp(0.0, f64::from(input_size)) as usize;
            let end = (center + support + 0.5)
                .floor()
                .clamp(0.0, f64::from(input_size)) as usize;
            let mut weights = (start..end)
                .map(|source| {
                    pillow_cubic_filter((f64::from(source as u32) - center + 0.5) / filter_scale)
                        / filter_scale
                })
                .collect::<Vec<_>>();
            let total = weights.iter().sum::<f64>();
            debug_assert!(total.is_finite() && total.abs() > f64::EPSILON);
            for weight in &mut weights {
                *weight /= total;
            }
            PillowResampleContribution { start, weights }
        })
        .collect()
}

fn pillow_cubic_filter(value: f64) -> f64 {
    let value = value.abs();
    if value < 1.0 {
        ((1.5 * value - 2.5) * value * value) + 1.0
    } else if value < 2.0 {
        ((-0.5 * value + 2.5) * value - 4.0) * value + 2.0
    } else {
        0.0
    }
}

fn round_clamp_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// M6.1 accepts only a single pooled output with the exact `[1, 768]` contract. Do not silently
/// pool token output here: accepting a graph with a changed output contract would make local
/// semantic evidence non-comparable with the reviewed model pack.
fn select_pooled_output_embedding(
    outputs: &TVec<tract_onnx::prelude::TValue>,
    output_index: usize,
    dimensions: usize,
    label: &str,
) -> Result<Vec<f32>, MagicSearchError> {
    let output = outputs.get(output_index).ok_or_else(|| {
        MagicSearchError::Provider(format!(
            "ONNX {label} graph did not return output index {output_index}"
        ))
    })?;
    if output.shape() != [1, dimensions] {
        return Err(MagicSearchError::Provider(format!(
            "ONNX {label} graph returned {:?}; expected pooled [1, {dimensions}] output",
            output.shape()
        )));
    }
    let values = output
        .as_slice::<f32>()
        .map_err(|error| MagicSearchError::Provider(format!("ONNX output was not f32: {error}")))?;
    if values.len() != dimensions {
        return Err(MagicSearchError::Provider(format!(
            "ONNX {label} graph returned {} values; expected {dimensions}",
            values.len()
        )));
    }
    Ok(values.to_vec())
}

pub fn normalize_embedding(mut values: Vec<f32>) -> Result<Vec<f32>, MagicSearchError> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(MagicSearchError::InvalidEmbedding(
            "embedding must contain finite values".into(),
        ));
    }
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(MagicSearchError::InvalidEmbedding(
            "embedding norm must be non-zero".into(),
        ));
    }
    for value in &mut values {
        *value /= norm;
    }
    Ok(values)
}

/// Static Hugging Face `tokenizer.json` adapter for the reviewed SigLIP pack. `tokenizers` parses
/// the serialized normalizer, metaspace pre-tokenizer, Unigram model, and EOS post-processor;
/// it does not execute tokenizer code or load Python/SentencePiece plugins. The manifest's fixed
/// token-ID self-tests then prove the installed JSON is the reviewed tokenizer revision.
#[derive(Debug, Clone)]
struct StaticSiglipTokenizer {
    tokenizer: Tokenizer,
}

impl StaticSiglipTokenizer {
    fn from_json(bytes: &[u8]) -> Result<Self, MagicSearchError> {
        let tokenizer = Tokenizer::from_bytes(bytes).map_err(|error| {
            MagicSearchError::InvalidModelPack(format!(
                "static Hugging Face tokenizer JSON could not be loaded: {error}"
            ))
        })?;
        if tokenizer.token_to_id("</s>") != Some(SIGLIP_EOS_AND_PAD_TOKEN_ID as u32) {
            return Err(MagicSearchError::InvalidModelPack(
                "SigLIP tokenizer did not define </s> as token ID 1".into(),
            ));
        }
        if tokenizer.token_to_id("<unk>") != Some(2) {
            return Err(MagicSearchError::InvalidModelPack(
                "SigLIP tokenizer did not define <unk> as token ID 2".into(),
            ));
        }
        Ok(Self { tokenizer })
    }

    fn encode(&self, query: &str) -> Result<Vec<i64>, MagicSearchError> {
        if query.trim().is_empty() {
            return Err(MagicSearchError::InvalidQuery(
                "search text may not be empty".into(),
            ));
        }
        if query.chars().count() > 512 {
            return Err(MagicSearchError::InvalidQuery(
                "search text exceeds the local tokenizer safety limit".into(),
            ));
        }
        let encoding = self.tokenizer.encode(query, true).map_err(|error| {
            MagicSearchError::InvalidModelPack(format!(
                "static Hugging Face tokenizer could not encode a query: {error}"
            ))
        })?;
        finish_siglip_token_ids(encoding.get_ids())
    }
}

/// The upstream SigLIP tokenizer appends EOS through its static template post-processor. This
/// preserves it while truncating, then pads the fixed model input with the same ID (`</s>` = 1).
/// There is deliberately no BOS token or attention-mask input in the approved M6.1 contract.
fn finish_siglip_token_ids(ids: &[u32]) -> Result<Vec<i64>, MagicSearchError> {
    if ids.last().copied() != Some(SIGLIP_EOS_AND_PAD_TOKEN_ID as u32) {
        return Err(MagicSearchError::InvalidModelPack(
            "SigLIP tokenizer did not append the required EOS token".into(),
        ));
    }
    let mut fixed = ids
        .iter()
        .copied()
        .take(SIGLIP_TEXT_SEQUENCE_LENGTH.saturating_sub(1))
        .map(i64::from)
        .collect::<Vec<_>>();
    fixed.push(SIGLIP_EOS_AND_PAD_TOKEN_ID);
    fixed.resize(SIGLIP_TEXT_SEQUENCE_LENGTH, SIGLIP_EOS_AND_PAD_TOKEN_ID);
    Ok(fixed)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredFilters {
    pub face_count: Option<u64>,
    pub rating_exact: Option<u8>,
    pub rating_minimum: Option<u8>,
    pub decision: Option<String>,
    pub require_sharp: bool,
    pub require_blurry: bool,
    pub require_technical_issue: bool,
    pub camera_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlan {
    pub normalized_query: String,
    pub semantic_query: Option<String>,
    pub filters: StructuredFilters,
    pub chips: Vec<String>,
    pub identity_search_blocked: bool,
}

/// A deliberately modest, deterministic query planner. It only recognizes explicit workflow
/// evidence; remaining ordinary language is sent to the local image/text provider unchanged.
pub fn plan_query(raw: &str) -> Result<QueryPlan, MagicSearchError> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(MagicSearchError::InvalidQuery(
            "search text may not be empty".into(),
        ));
    }
    if normalized.chars().count() > 512 {
        return Err(MagicSearchError::InvalidQuery(
            "search text exceeds the local safety limit".into(),
        ));
    }
    let lower = normalized.to_lowercase();
    let identity_search_blocked = is_identity_search_request(&lower);
    if identity_search_blocked {
        return Ok(QueryPlan {
            normalized_query: normalized,
            semantic_query: None,
            filters: StructuredFilters::default(),
            chips: vec!["Identity search unavailable".into()],
            identity_search_blocked,
        });
    }
    let mut filters = StructuredFilters::default();
    let mut chips = Vec::new();
    let mut residual = format!(" {lower} ");
    for (phrases, count) in [
        (
            &[" with 2 faces ", " 2 faces ", " two faces "] as &[&str],
            2_u64,
        ),
        (
            &[" with 1 face ", " 1 face ", " one face "] as &[&str],
            1_u64,
        ),
    ] {
        if phrases.iter().any(|phrase| residual.contains(phrase)) {
            filters.face_count = Some(count);
            chips.push(format!("Faces: {count}"));
            for phrase in phrases {
                residual = residual.replace(phrase, " ");
            }
        }
    }
    // “Two people” is a normal semantic phrase, but it also maps to existing anonymous face
    // count evidence. Keep the words in the semantic residual so this remains a hybrid query;
    // it does not identify, match, or cluster any person.
    if filters.face_count.is_none()
        && [" two people ", " 2 people "]
            .iter()
            .any(|phrase| residual.contains(phrase))
    {
        filters.face_count = Some(2);
        chips.push("Faces: 2".into());
    }
    if let Some(rating) = exact_star_rating(&residual) {
        filters.rating_exact = Some(rating);
        chips.push(format!("Rating: {rating}★"));
        for phrase in [format!(" {rating} star "), format!(" {rating} stars ")] {
            residual = residual.replace(&phrase, " ");
        }
    } else if let Some(rating) = minimum_star_rating(&residual) {
        filters.rating_minimum = Some(rating);
        chips.push(format!("Rating: {rating}★+"));
        for phrase in [format!(" {rating}+ star "), format!(" {rating}+ stars ")] {
            residual = residual.replace(&phrase, " ");
        }
    }
    for (terms, decision, label) in [
        (&[" kept ", " keep "] as &[&str], "keep", "Decision: Keep"),
        (
            &[" rejected ", " reject "] as &[&str],
            "reject",
            "Decision: Reject",
        ),
        (&[" review "] as &[&str], "review", "Decision: Review"),
    ] {
        if terms.iter().any(|term| residual.contains(term)) {
            filters.decision = Some(decision.into());
            chips.push(label.into());
            for term in terms {
                residual = residual.replace(term, " ");
            }
            break;
        }
    }
    if residual.contains(" sharp ") {
        filters.require_sharp = true;
        chips.push("Sharpness: sharp".into());
        residual = residual.replace(" sharp ", " ");
    }
    if residual.contains(" blurry ") || residual.contains(" blur ") {
        filters.require_blurry = true;
        chips.push("Blur: review".into());
        residual = residual.replace(" blurry ", " ").replace(" blur ", " ");
    }
    for phrase in [
        " technical issue ",
        " technical issues ",
        " quality issue ",
        " quality issues ",
    ] {
        if residual.contains(phrase) {
            filters.require_technical_issue = true;
            chips.push("Technical quality: issue".into());
            residual = residual.replace(phrase, " ");
            break;
        }
    }
    if let Some(camera) = extract_camera_model(&residual) {
        filters.camera_model = Some(camera.clone());
        chips.push(format!("Camera: {camera}"));
        residual = residual.replace(&format!(" camera {camera} "), " ");
    }
    for filler in [
        " photos ", " photo ", " images ", " image ", " with ", " and ",
    ] {
        residual = residual.replace(filler, " ");
    }
    let semantic = residual.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(QueryPlan {
        normalized_query: normalized,
        semantic_query: (!semantic.is_empty()).then_some(semantic),
        filters,
        chips,
        identity_search_blocked: false,
    })
}

/// Refuse only explicit identity-recognition phrasing. A generic retrieval verb such as
/// "find yellow boat" remains a normal semantic query; blocking it would turn a safety guard
/// into a hidden language limitation. This is an intent guard, not person recognition itself.
fn is_identity_search_request(lower: &str) -> bool {
    let lower = lower.trim();
    lower.starts_with("who is ")
        || lower.starts_with("who's ")
        || lower.starts_with("name this person")
        || lower.starts_with("identify this person")
        || lower.starts_with("identify the person")
        || lower.starts_with("find this person")
        || lower.starts_with("find the person")
        || lower.contains("person named ")
}

fn exact_star_rating(value: &str) -> Option<u8> {
    (1_u8..=5).find(|rating| {
        value.contains(&format!(" {rating} star ")) || value.contains(&format!(" {rating} stars "))
    })
}

fn minimum_star_rating(value: &str) -> Option<u8> {
    (1_u8..=5).find(|rating| {
        value.contains(&format!(" {rating}+ star "))
            || value.contains(&format!(" {rating}+ stars "))
    })
}

fn extract_camera_model(value: &str) -> Option<String> {
    let marker = " camera ";
    let start = value.find(marker)? + marker.len();
    let remaining = &value[start..];
    let stop = [
        " with ",
        " sharp ",
        " blurry ",
        " blur ",
        " kept ",
        " rejected ",
        " review ",
    ]
    .iter()
    .filter_map(|marker| remaining.find(marker))
    .min()
    .unwrap_or(remaining.len());
    let model = remaining[..stop].trim();
    (!model.is_empty()).then(|| model.to_uppercase())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredVector {
    pub asset_id: String,
    pub score: f32,
}

pub fn rank_normalized_vectors(
    query: &[f32],
    candidates: impl IntoIterator<Item = (String, Vec<f32>)>,
    limit: usize,
) -> Result<Vec<ScoredVector>, MagicSearchError> {
    let query = normalize_embedding(query.to_vec())?;
    let mut scored = candidates
        .into_iter()
        .filter_map(|(asset_id, vector)| {
            if vector.len() != query.len() {
                return None;
            }
            let vector = normalize_embedding(vector).ok()?;
            let score: f32 = query
                .iter()
                .zip(vector)
                .map(|(left, right)| left * right)
                .sum();
            score
                .is_finite()
                .then_some(ScoredVector { asset_id, score })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.asset_id.cmp(&right.asset_id))
    });
    scored.truncate(limit);
    Ok(scored)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexPayload {
    format_version: u32,
    project_id: String,
    model_cache_key: String,
    dimensions: usize,
    vector_count: usize,
    buckets: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEnvelope {
    payload: IndexPayload,
    checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBuildResult {
    pub relative_path: String,
    pub checksum: String,
    pub vector_count: usize,
    pub dimensions: usize,
    pub approximate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexCandidates {
    /// Exact search is bounded to a small catalog/project.
    AllStoredVectors,
    /// Approximate search must only load these candidate vectors from persistence.
    AssetIds(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct PersistentVectorIndex {
    payload: IndexPayload,
    /// Runtime-only deterministic sign planes. They are intentionally not serialized so the
    /// on-disk v1 payload stays compatible, but a loaded index must not re-hash 64 × dimensions
    /// plane seeds for every query.
    projections: Vec<Vec<f32>>,
}

impl PersistentVectorIndex {
    pub fn build_and_store(
        root: &Path,
        project_id: &str,
        model_cache_key: &str,
        dimensions: usize,
        vectors: impl IntoIterator<Item = (String, Vec<f32>)>,
    ) -> Result<IndexBuildResult, MagicSearchError> {
        if dimensions == 0 {
            return Err(MagicSearchError::InvalidEmbedding(
                "vector index dimensions must be non-zero".into(),
            ));
        }
        // These deterministic sign planes are shared by every vector in this rebuild. Computing
        // them once avoids re-hashing table/bit/dimension seeds for every asset, which would turn
        // a 50k-vector rebuild into hundreds of millions of hash operations.
        let projections = lsh_projections(dimensions);
        let mut buckets = BTreeMap::<String, Vec<String>>::new();
        let mut seen = BTreeSet::new();
        for (asset_id, vector) in vectors {
            if asset_id.is_empty() || !seen.insert(asset_id.clone()) {
                return Err(MagicSearchError::InvalidEmbedding(
                    "vector index received an invalid or duplicate asset ID".into(),
                ));
            }
            let vector = normalize_embedding(vector)?;
            if vector.len() != dimensions {
                return Err(MagicSearchError::InvalidEmbedding(
                    "vector dimensions did not match the semantic model".into(),
                ));
            }
            for table in 0..INDEX_TABLES {
                buckets
                    .entry(bucket_key(table, &vector, &projections))
                    .or_default()
                    .push(asset_id.clone());
            }
        }
        for members in buckets.values_mut() {
            members.sort();
            members.dedup();
        }
        let payload = IndexPayload {
            format_version: INDEX_FORMAT_VERSION,
            project_id: project_id.into(),
            model_cache_key: model_cache_key.into(),
            dimensions,
            vector_count: seen.len(),
            buckets,
        };
        let checksum = payload_checksum(&payload)?;
        let envelope = IndexEnvelope {
            payload: payload.clone(),
            checksum: checksum.clone(),
        };
        fs::create_dir_all(root)?;
        let canonical_root = root.canonicalize()?;
        let relative_path = index_relative_path(project_id, model_cache_key);
        let path = safe_index_path(&canonical_root, &relative_path)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, serde_json::to_vec(&envelope)?)?;
        fs::rename(&temporary, &path)?;
        Ok(IndexBuildResult {
            relative_path,
            checksum,
            vector_count: payload.vector_count,
            dimensions,
            approximate: payload.vector_count > EXACT_SEARCH_ASSET_LIMIT,
        })
    }

    pub fn load(
        root: &Path,
        relative_path: &str,
        expected_project_id: &str,
        expected_model_cache_key: &str,
        expected_dimensions: usize,
    ) -> Result<Option<Self>, MagicSearchError> {
        if !root.is_dir() {
            return Ok(None);
        }
        let canonical_root = root.canonicalize()?;
        let path = safe_index_path(&canonical_root, relative_path)?;
        if !path.exists() {
            return Ok(None);
        }
        let envelope: IndexEnvelope =
            serde_json::from_slice(&fs::read(path)?).map_err(|error| {
                MagicSearchError::CorruptIndex(format!("index JSON could not be read: {error}"))
            })?;
        if payload_checksum(&envelope.payload)? != envelope.checksum
            || envelope.payload.format_version != INDEX_FORMAT_VERSION
            || envelope.payload.project_id != expected_project_id
            || envelope.payload.model_cache_key != expected_model_cache_key
            || envelope.payload.dimensions != expected_dimensions
        {
            return Err(MagicSearchError::CorruptIndex(
                "index metadata/checksum did not match the active semantic model".into(),
            ));
        }
        Ok(Some(Self {
            projections: lsh_projections(envelope.payload.dimensions),
            payload: envelope.payload,
        }))
    }

    pub fn candidates(&self, query: &[f32]) -> Result<IndexCandidates, MagicSearchError> {
        let query = normalize_embedding(query.to_vec())?;
        if query.len() != self.payload.dimensions {
            return Err(MagicSearchError::InvalidEmbedding(
                "query vector dimensions did not match the local index".into(),
            ));
        }
        if self.payload.vector_count <= EXACT_SEARCH_ASSET_LIMIT {
            return Ok(IndexCandidates::AllStoredVectors);
        }
        let mut asset_ids = BTreeSet::new();
        for table in 0..INDEX_TABLES {
            let signature = bucket_signature(table, &query, &self.projections);
            for candidate_signature in signature_with_one_bit_neighbors(signature) {
                let key = format!("{table}:{candidate_signature:04x}");
                if let Some(members) = self.payload.buckets.get(&key) {
                    asset_ids.extend(members.iter().cloned());
                    if asset_ids.len() >= MAX_APPROXIMATE_CANDIDATES {
                        break;
                    }
                }
            }
        }
        let mut ids = asset_ids.into_iter().collect::<Vec<_>>();
        ids.truncate(MAX_APPROXIMATE_CANDIDATES);
        Ok(IndexCandidates::AssetIds(ids))
    }

    pub fn vector_count(&self) -> usize {
        self.payload.vector_count
    }
}

fn index_relative_path(project_id: &str, model_cache_key: &str) -> String {
    let digest = blake3::hash(format!("m6-index|{project_id}|{model_cache_key}").as_bytes());
    format!("{}.m6lsh.json", digest.to_hex())
}

fn safe_index_path(root: &Path, relative: &str) -> Result<PathBuf, MagicSearchError> {
    if !is_safe_relative_path(relative) {
        return Err(MagicSearchError::CorruptIndex(
            "index path was not a safe relative path".into(),
        ));
    }
    let candidate = root.join(relative);
    if candidate.parent() != Some(root) {
        return Err(MagicSearchError::CorruptIndex(
            "index path escaped its managed root".into(),
        ));
    }
    Ok(candidate)
}

fn payload_checksum(payload: &IndexPayload) -> Result<String, MagicSearchError> {
    Ok(blake3::hash(&serde_json::to_vec(payload)?)
        .to_hex()
        .to_string())
}

fn bucket_key(table: usize, vector: &[f32], projections: &[Vec<f32>]) -> String {
    format!(
        "{table}:{:04x}",
        bucket_signature(table, vector, projections)
    )
}

fn bucket_signature(table: usize, vector: &[f32], projections: &[Vec<f32>]) -> u16 {
    let mut signature = 0_u16;
    for bit in 0..INDEX_BITS_PER_TABLE {
        let projection = &projections[table * INDEX_BITS_PER_TABLE + bit];
        let sum = vector
            .iter()
            .zip(projection)
            .map(|(value, coefficient)| value * coefficient)
            .sum::<f32>();
        if sum >= 0.0 {
            signature |= 1 << bit;
        }
    }
    signature
}

fn lsh_projections(dimensions: usize) -> Vec<Vec<f32>> {
    (0..INDEX_TABLES * INDEX_BITS_PER_TABLE)
        .map(|plane| {
            (0..dimensions)
                .map(|dimension| {
                    let table = plane / INDEX_BITS_PER_TABLE;
                    let bit = plane % INDEX_BITS_PER_TABLE;
                    let seed = format!("captureos-m6-lsh-v1|{table}|{bit}|{dimension}");
                    if blake3::hash(seed.as_bytes()).as_bytes()[0] & 1 == 0 {
                        -1.0
                    } else {
                        1.0
                    }
                })
                .collect()
        })
        .collect()
}

fn signature_with_one_bit_neighbors(signature: u16) -> Vec<u16> {
    let mut values = Vec::with_capacity(INDEX_BITS_PER_TABLE + 1);
    values.push(signature);
    values.extend((0..INDEX_BITS_PER_TABLE).map(|bit| signature ^ (1 << bit)));
    values
}

#[cfg(feature = "test-support")]
/// Deterministic test fixture only. It is feature-gated so it cannot become a product fallback
/// or generate photographer-facing "semantic" results when no approved pack is installed.
#[derive(Debug, Clone)]
pub struct FixtureSemanticProvider {
    identity: SemanticProviderIdentity,
    image_embedding: Vec<f32>,
    text_embeddings: std::collections::HashMap<String, Vec<f32>>,
}

#[cfg(feature = "test-support")]
impl FixtureSemanticProvider {
    pub fn new(
        dimensions: usize,
        image_embedding: Vec<f32>,
        text_embeddings: std::collections::HashMap<String, Vec<f32>>,
    ) -> Self {
        Self {
            identity: SemanticProviderIdentity {
                model_id: "fixture-semantic-model".into(),
                model_family: "fixture".into(),
                provider: "fixture-test-only".into(),
                model_version: "v1".into(),
                embedding_version: "fixture".into(),
                preprocessing_version: "fixture".into(),
                metric: "normalized-dot".into(),
                dimensions,
            },
            image_embedding,
            text_embeddings,
        }
    }
}

#[cfg(feature = "test-support")]
impl SemanticEmbeddingProvider for FixtureSemanticProvider {
    fn identity(&self) -> &SemanticProviderIdentity {
        &self.identity
    }

    fn embed_image(&self, _image: &AnalysisImage) -> Result<Vec<f32>, MagicSearchError> {
        normalize_embedding(self.image_embedding.clone())
    }

    fn embed_text(&self, query: &str) -> Result<Vec<f32>, MagicSearchError> {
        self.text_embeddings
            .get(query)
            .cloned()
            .ok_or_else(|| MagicSearchError::InvalidQuery("fixture query is not registered".into()))
            .and_then(normalize_embedding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_pack_file(path: &str) -> ModelPackFile {
        ModelPackFile {
            path: path.into(),
            sha256: "a".repeat(64),
            bytes: 1,
        }
    }

    fn unit_embedding(dimensions: usize, hot_dimension: usize) -> Vec<f32> {
        let mut values = vec![0.0; dimensions];
        values[hot_dimension] = 1.0;
        values
    }

    fn test_reference_vectors(dimensions: usize) -> ReferenceVectors {
        ReferenceVectors {
            format_version: 2,
            image: ReferenceImage {
                // Non-224 source dimensions force the provider admission fixture through
                // Pillow-compatible bicubic resize before image inference.
                width: 3,
                height: 2,
                rgb24: ModelPackFile {
                    path: "reference.rgb24".into(),
                    sha256: "a".repeat(64),
                    bytes: 18,
                },
            },
            text_query: "a static local reference".into(),
            expected_image_embedding: unit_embedding(dimensions, 0),
            expected_text_embedding: unit_embedding(dimensions, 1.min(dimensions - 1)),
        }
    }

    fn test_manifest() -> SiglipPackManifest {
        SiglipPackManifest {
            format_version: 2,
            pack_id: SUPPORTED_SIGLIP_PACK_ID.into(),
            model_id: SUPPORTED_SIGLIP_MODEL_ID.into(),
            model_family: "siglip".into(),
            runtime: "tract-onnx".into(),
            model_version: "test-revision".into(),
            source_url: SUPPORTED_SIGLIP_SOURCE_URL.into(),
            source_revision: "immutable-test-revision".into(),
            source_weights_sha256: "b".repeat(64),
            license: "Apache-2.0".into(),
            license_url: "https://example.invalid/license".into(),
            image_model: test_pack_file("image.onnx"),
            text_model: test_pack_file("text.onnx"),
            tokenizer: test_pack_file("tokenizer.json"),
            license_file: test_pack_file("LICENSE"),
            notice_file: test_pack_file("NOTICE"),
            conversion_provenance_file: test_pack_file("conversion-provenance.json"),
            input_size: SIGLIP_INPUT_SIZE,
            embedding_dimension: SIGLIP_EMBEDDING_DIMENSION,
            preprocessing_version: SIGLIP_PREPROCESSING_VERSION.into(),
            supported_capabilities: SUPPORTED_SIGLIP_CAPABILITIES
                .iter()
                .map(|capability| (*capability).into())
                .collect(),
            text_sequence_length: SIGLIP_TEXT_SEQUENCE_LENGTH,
            token_ids: TokenIds {
                eos: SIGLIP_EOS_AND_PAD_TOKEN_ID,
                pad: SIGLIP_EOS_AND_PAD_TOKEN_ID,
            },
            text_inputs: vec!["input_ids".into()],
            image_output_index: 0,
            text_output_index: 0,
            text_pooling: "pooled".into(),
            tokenizer_self_tests: vec![TokenizerSelfTest {
                text: "static reference".into(),
                expected_ids: vec![SIGLIP_EOS_AND_PAD_TOKEN_ID; SIGLIP_TEXT_SEQUENCE_LENGTH],
            }],
            reference_vectors: test_reference_vectors(SIGLIP_EMBEDDING_DIMENSION),
        }
    }

    fn test_approved_descriptor(
        manifest: &SiglipPackManifest,
        manifest_bytes: &[u8],
    ) -> ApprovedSiglipPackDescriptor {
        ApprovedSiglipPackDescriptor {
            format_version: 1,
            pack_id: manifest.pack_id.clone(),
            model_id: manifest.model_id.clone(),
            model_family: manifest.model_family.clone(),
            runtime: manifest.runtime.clone(),
            model_version: manifest.model_version.clone(),
            source_url: manifest.source_url.clone(),
            source_revision: manifest.source_revision.clone(),
            source_weights_sha256: manifest.source_weights_sha256.clone(),
            license: manifest.license.clone(),
            license_url: manifest.license_url.clone(),
            preprocessing_version: manifest.preprocessing_version.clone(),
            supported_capabilities: manifest.supported_capabilities.clone(),
            manifest_sha256: sha256_bytes(manifest_bytes),
            files: ApprovedSiglipPackFiles {
                image_model: manifest.image_model.clone(),
                text_model: manifest.text_model.clone(),
                tokenizer: manifest.tokenizer.clone(),
                license_file: manifest.license_file.clone(),
                notice_file: manifest.notice_file.clone(),
                conversion_provenance_file: manifest.conversion_provenance_file.clone(),
                reference_rgb24: manifest.reference_vectors.image.rgb24.clone(),
            },
        }
    }

    fn write_stamp_test_pack(root: &Path) -> SiglipPackManifest {
        let manifest = test_manifest();
        fs::write(root.join("captureos-semantic-model.json"), b"test manifest").unwrap();
        for file in [
            &manifest.image_model,
            &manifest.text_model,
            &manifest.tokenizer,
            &manifest.license_file,
            &manifest.notice_file,
            &manifest.conversion_provenance_file,
            &manifest.reference_vectors.image.rgb24,
        ] {
            let path = root.join(&file.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, vec![0_u8; file.bytes as usize]).unwrap();
        }
        manifest
    }

    fn stamp_test_pack(root: &Path, manifest: &SiglipPackManifest) -> SiglipPackStamp {
        closed_siglip_pack_stamp(
            &root.canonicalize().unwrap(),
            manifest,
            "test-manifest-digest".into(),
        )
        .unwrap()
    }

    #[test]
    fn semantic_cache_key_and_index_path_include_embedding_dimensions() {
        let base = SemanticProviderIdentity {
            model_id: "local-model".into(),
            model_family: "siglip".into(),
            provider: "tract-onnx".into(),
            model_version: "v1".into(),
            embedding_version: "embedding-v1".into(),
            preprocessing_version: "preprocess-v1".into(),
            metric: "normalized-dot-v1".into(),
            dimensions: 768,
        };
        let mut incompatible_dimension = base.clone();
        incompatible_dimension.dimensions = 1024;

        assert_ne!(base.cache_key(), incompatible_dimension.cache_key());
        assert_ne!(
            index_relative_path("project-a", &base.cache_key()),
            index_relative_path("project-a", &incompatible_dimension.cache_key())
        );
    }

    #[test]
    fn provider_cache_reports_missing_pack_without_opening_a_model() {
        let directory = tempdir().unwrap();
        let cache = SiglipProviderCache::new(directory.path());
        assert!(cache.acquire().unwrap().is_none());
        assert!(siglip_pack_stamp(directory.path()).unwrap().is_none());
    }

    #[test]
    fn provider_cache_reuses_only_an_equal_closed_pack_stamp() {
        let directory = tempdir().unwrap();
        let manifest = write_stamp_test_pack(directory.path());
        let first = stamp_test_pack(directory.path(), &manifest);
        let second = stamp_test_pack(directory.path(), &manifest);
        assert_eq!(first, second);

        let cached = Some(CachedProvider {
            stamp: first,
            provider: Arc::new(7_u8),
        });
        let reused = cached_provider_if_current(&cached, &second).unwrap();
        assert!(Arc::ptr_eq(&cached.as_ref().unwrap().provider, &reused));
    }

    #[test]
    fn provider_cache_stamp_invalidates_reuse_when_an_artifact_changes() {
        let directory = tempdir().unwrap();
        let manifest = write_stamp_test_pack(directory.path());
        let first = stamp_test_pack(directory.path(), &manifest);
        fs::write(
            directory.path().join(&manifest.image_model.path),
            [1_u8, 2_u8],
        )
        .unwrap();
        let changed = stamp_test_pack(directory.path(), &manifest);
        assert_ne!(first, changed);

        let cached = Some(CachedProvider {
            stamp: first,
            provider: Arc::new(7_u8),
        });
        assert!(cached_provider_if_current(&cached, &changed).is_none());
    }

    #[test]
    fn provider_cache_stamp_rejects_an_extra_pack_artifact() {
        let directory = tempdir().unwrap();
        let manifest = write_stamp_test_pack(directory.path());
        fs::write(directory.path().join("unreviewed.bin"), [0_u8]).unwrap();
        assert!(matches!(
            closed_siglip_pack_stamp(
                &directory.path().canonicalize().unwrap(),
                &manifest,
                "test-manifest-digest".into(),
            ),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn provider_cache_stamp_rejects_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let manifest = write_stamp_test_pack(directory.path());
        symlink(
            directory.path().join(&manifest.image_model.path),
            directory.path().join("unreviewed-link"),
        )
        .unwrap();
        assert!(matches!(
            closed_siglip_pack_stamp(
                &directory.path().canonicalize().unwrap(),
                &manifest,
                "test-manifest-digest".into(),
            ),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn planner_extracts_hybrid_filters_without_hard_coding_visual_concepts() {
        let plan = plan_query("5 star sharp photos with 1 face near water").unwrap();
        assert_eq!(plan.filters.rating_exact, Some(5));
        assert_eq!(plan.filters.face_count, Some(1));
        assert!(plan.filters.require_sharp);
        assert_eq!(plan.semantic_query.as_deref(), Some("near water"));
        assert_eq!(
            plan.chips,
            vec!["Faces: 1", "Rating: 5★", "Sharpness: sharp"]
        );
    }

    #[test]
    fn planner_keeps_two_people_as_semantic_text_while_using_anonymous_face_count() {
        let plan = plan_query("two people on a yellow boat").unwrap();
        assert_eq!(plan.filters.face_count, Some(2));
        assert_eq!(plan.chips, vec!["Faces: 2"]);
        assert_eq!(
            plan.semantic_query.as_deref(),
            Some("two people on a yellow boat")
        );
    }

    #[test]
    fn planner_blocks_explicit_identity_request_without_touching_face_search() {
        let plan = plan_query("find person named John").unwrap();
        assert!(plan.identity_search_blocked);
        assert!(plan.semantic_query.is_none());
    }

    #[test]
    fn planner_keeps_generic_find_phrase_as_a_semantic_query() {
        let plan = plan_query("find yellow boat").unwrap();
        assert!(!plan.identity_search_blocked);
        assert_eq!(plan.semantic_query.as_deref(), Some("find yellow boat"));
    }

    #[test]
    fn planner_extracts_technical_issue_as_deterministic_evidence() {
        let plan = plan_query("technical issue photos").unwrap();
        assert!(plan.filters.require_technical_issue);
        assert!(plan.semantic_query.is_none());
        assert_eq!(plan.chips, vec!["Technical quality: issue"]);
    }

    #[test]
    fn static_reference_outputs_require_matching_image_and_text_vectors() {
        let reference = test_reference_vectors(3);
        validate_reference_outputs(&reference, 3, &[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0]).unwrap();
        assert!(matches!(
            validate_reference_outputs(&reference, 3, &[0.0, 0.0, 1.0], &[0.0, 1.0, 0.0],),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn manifest_requires_a_bounded_normalized_reference_vector_contract() {
        let mut manifest = test_manifest();
        manifest.reference_vectors.expected_text_embedding = vec![1.0; 2];
        assert!(matches!(
            validate_manifest(&manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn controlled_manifest_rejects_unknown_top_level_and_artifact_fields() {
        let mut top_level = serde_json::to_value(test_manifest()).unwrap();
        top_level
            .as_object_mut()
            .unwrap()
            .insert("unreviewedField".into(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<SiglipPackManifest>(top_level).is_err());

        let mut artifact = serde_json::to_value(test_manifest()).unwrap();
        artifact["imageModel"]
            .as_object_mut()
            .unwrap()
            .insert("unreviewedField".into(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<SiglipPackManifest>(artifact).is_err());
    }

    #[test]
    fn compiled_descriptor_pins_manifest_source_and_artifact_identity() {
        let manifest = test_manifest();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let descriptor = test_approved_descriptor(&manifest, &manifest_bytes);
        validate_manifest_against_descriptor(&descriptor, &manifest_bytes, &manifest).unwrap();

        let mut changed_source = manifest.clone();
        changed_source.source_revision = "other-revision".into();
        let changed_bytes = serde_json::to_vec(&changed_source).unwrap();
        let mut descriptor_with_changed_digest = descriptor.clone();
        descriptor_with_changed_digest.manifest_sha256 = sha256_bytes(&changed_bytes);
        assert!(matches!(
            validate_manifest_against_descriptor(
                &descriptor_with_changed_digest,
                &changed_bytes,
                &changed_source
            ),
            Err(MagicSearchError::InvalidModelPack(_))
        ));

        let mut changed_artifact = manifest.clone();
        changed_artifact.tokenizer.sha256 = "b".repeat(64);
        let changed_bytes = serde_json::to_vec(&changed_artifact).unwrap();
        let mut descriptor_with_changed_digest = descriptor;
        descriptor_with_changed_digest.manifest_sha256 = sha256_bytes(&changed_bytes);
        assert!(matches!(
            validate_manifest_against_descriptor(
                &descriptor_with_changed_digest,
                &changed_bytes,
                &changed_artifact
            ),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn compiled_product_descriptor_is_a_valid_closed_contract() {
        let descriptor = approved_siglip_pack_descriptor().unwrap();
        assert_eq!(descriptor.pack_id, SUPPORTED_SIGLIP_PACK_ID);
        assert_eq!(descriptor.files.reference_rgb24.path, "reference.rgb24");
    }

    #[test]
    #[ignore = "requires an explicitly installed, checksum-verified local SigLIP pack"]
    fn approved_installed_pack_loads_and_embeds_with_tract() {
        let model_root = std::env::var_os("CAPTUREOS_SIGLIP_TEST_MODEL_ROOT")
            .map(PathBuf::from)
            .expect("set CAPTUREOS_SIGLIP_TEST_MODEL_ROOT to the controlled model root");
        let provider = discover_siglip_provider(&model_root)
            .unwrap()
            .expect("the controlled model pack must be installed");
        assert_eq!(provider.identity().dimensions, SIGLIP_EMBEDDING_DIMENSION);
        let embedding = provider.embed_text("woman in red").unwrap();
        assert_eq!(embedding.len(), SIGLIP_EMBEDDING_DIMENSION);
        assert!(embedding.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn controlled_pack_root_rejects_unlisted_artifacts() {
        let directory = tempdir().unwrap();
        let manifest = test_manifest();
        fs::write(directory.path().join("captureos-semantic-model.json"), "{}").unwrap();
        for artifact in [
            &manifest.image_model,
            &manifest.text_model,
            &manifest.tokenizer,
            &manifest.license_file,
            &manifest.notice_file,
            &manifest.conversion_provenance_file,
            &manifest.reference_vectors.image.rgb24,
        ] {
            fs::write(directory.path().join(&artifact.path), []).unwrap();
        }
        validate_pack_root_contents(directory.path(), &manifest).unwrap();
        fs::write(directory.path().join("unreviewed.bin"), []).unwrap();
        assert!(matches!(
            validate_pack_root_contents(directory.path(), &manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn manifest_rejects_unapproved_text_graph_shapes_and_token_ids() {
        let mut manifest = test_manifest();
        manifest.text_inputs = vec!["input_ids".into(), "attention_mask".into()];
        assert!(matches!(
            validate_manifest(&manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));

        let mut manifest = test_manifest();
        manifest.text_pooling = "mean_tokens".into();
        assert!(matches!(
            validate_manifest(&manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));

        let mut manifest = test_manifest();
        manifest.text_output_index = 1;
        assert!(matches!(
            validate_manifest(&manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));

        let mut manifest = test_manifest();
        manifest.token_ids.pad = 0;
        assert!(matches!(
            validate_manifest(&manifest),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn siglip_text_input_has_no_bos_and_uses_eos_as_the_fixed_length_pad() {
        let ids = finish_siglip_token_ids(&[401, 402, 1]).unwrap();
        assert_eq!(ids.len(), SIGLIP_TEXT_SEQUENCE_LENGTH);
        assert_eq!(&ids[..3], &[401, 402, 1]);
        assert!(ids[3..].iter().all(|id| *id == SIGLIP_EOS_AND_PAD_TOKEN_ID));

        let mut long = (10_u32..80).collect::<Vec<_>>();
        long.push(1);
        let ids = finish_siglip_token_ids(&long).unwrap();
        assert_eq!(ids.len(), SIGLIP_TEXT_SEQUENCE_LENGTH);
        assert_eq!(ids[0], 10);
        assert_eq!(
            ids[SIGLIP_TEXT_SEQUENCE_LENGTH - 1],
            SIGLIP_EOS_AND_PAD_TOKEN_ID
        );
        assert!(!ids.contains(&0), "M6.1 never emits a BOS or <pad> ID 0");
    }

    #[test]
    fn pillow_bicubic_resize_matches_a_checked_rgb_fixture() {
        // Generated with Pillow 12.3.0:
        // Image.frombytes("RGB", (3, 2), source).resize((5, 4), Image.Resampling.BICUBIC)
        let source = AnalysisImage::new(
            3,
            2,
            vec![
                0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170,
            ],
        )
        .unwrap();
        let actual = pillow_bicubic_resize_rgb(&source, 5, 4);
        let expected = vec![
            0, 0, 10, 1, 11, 21, 22, 32, 42, 43, 53, 63, 54, 64, 74, 18, 27, 37, 28, 38, 48, 49,
            59, 69, 70, 80, 90, 81, 91, 101, 70, 79, 89, 80, 90, 100, 101, 111, 121, 122, 132, 142,
            133, 143, 153, 96, 106, 116, 107, 117, 127, 128, 138, 148, 149, 159, 169, 160, 170,
            180,
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn static_reference_image_requires_the_exact_rgb24_payload() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("reference.rgb24");
        fs::write(&path, [0_u8; 3]).unwrap();
        let reference = ReferenceImage {
            width: 3,
            height: 2,
            rgb24: ModelPackFile {
                path: "reference.rgb24".into(),
                sha256: "a".repeat(64),
                bytes: 18,
            },
        };
        assert!(matches!(
            load_reference_image(&reference, &path),
            Err(MagicSearchError::InvalidModelPack(_))
        ));
    }

    #[test]
    fn normalized_rank_prefers_related_vector_over_unrelated_vector() {
        let ranked = rank_normalized_vectors(
            &[1.0, 0.0, 0.0],
            [
                ("related".into(), vec![0.9, 0.1, 0.0]),
                ("unrelated".into(), vec![0.0, 1.0, 0.0]),
            ],
            10,
        )
        .unwrap();
        assert_eq!(ranked[0].asset_id, "related");
        assert_eq!(ranked[1].asset_id, "unrelated");
    }

    #[test]
    fn index_rebuild_is_derived_and_large_catalog_search_uses_bounded_candidates() {
        let directory = tempdir().unwrap();
        let vectors = (0..50_000)
            .map(|index| {
                let value = index as f32 / 50_000.0;
                (
                    format!("asset-{index:05}"),
                    vec![1.0, value, value * value, 0.25],
                )
            })
            .collect::<Vec<_>>();
        let built = PersistentVectorIndex::build_and_store(
            directory.path(),
            "project-a",
            "model-v1",
            4,
            vectors,
        )
        .unwrap();
        let index = PersistentVectorIndex::load(
            directory.path(),
            &built.relative_path,
            "project-a",
            "model-v1",
            4,
        )
        .unwrap()
        .unwrap();
        assert_eq!(index.vector_count(), 50_000);
        let serialized = fs::read_to_string(directory.path().join(&built.relative_path)).unwrap();
        assert!(
            !serialized.contains("projections"),
            "runtime query planes must not change the v1 on-disk payload"
        );
        let cached_projections = index.projections.clone();
        let IndexCandidates::AssetIds(candidates) =
            index.candidates(&[1.0, 0.9, 0.8, 0.2]).unwrap()
        else {
            panic!("large index must not request an exact full scan");
        };
        assert!(candidates.len() < 50_000);
        assert!(candidates.len() <= MAX_APPROXIMATE_CANDIDATES);
        let _ = index.candidates(&[1.0, 0.8, 0.6, 0.1]).unwrap();
        assert_eq!(index.projections, cached_projections);
    }

    #[test]
    fn corrupted_index_is_rejected_for_safe_rebuild() {
        let directory = tempdir().unwrap();
        let built = PersistentVectorIndex::build_and_store(
            directory.path(),
            "project-a",
            "model-v1",
            2,
            [("asset-a".into(), vec![1.0, 0.0])],
        )
        .unwrap();
        fs::write(
            directory.path().join(&built.relative_path),
            b"not index JSON",
        )
        .unwrap();
        assert!(matches!(
            PersistentVectorIndex::load(
                directory.path(),
                &built.relative_path,
                "project-a",
                "model-v1",
                2
            ),
            Err(MagicSearchError::CorruptIndex(_))
        ));
    }
}
