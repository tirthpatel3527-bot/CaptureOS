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
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use thiserror::Error;
use tract_onnx::prelude::{Framework, InferenceModelExt, TVec, Tensor, TypedModel, TypedRunnableModel};
use unicode_normalization::UnicodeNormalization;

pub const SUPPORTED_SIGLIP_MODEL_ID: &str = "google-siglip-base-patch16-224";
pub const SUPPORTED_SIGLIP_PACK_DIRECTORY: &str = "google-siglip-base-patch16-224";
pub const SEMANTIC_EMBEDDING_VERSION: &str = "m6.semantic-embedding.v1";
pub const SIGLIP_PREPROCESSING_VERSION: &str = "siglip-rgb224-bilinear-scale05.v1";
pub const SIGLIP_NORMALIZED_DOT_METRIC: &str = "siglip-normalized-dot.v1";
pub const SIGLIP_INPUT_SIZE: u32 = 224;
pub const SIGLIP_EMBEDDING_DIMENSION: usize = 768;
pub const SIGLIP_TEXT_SEQUENCE_LENGTH: usize = 64;

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
            "{}|{}|{}|{}|{}|{}",
            self.model_id,
            self.provider,
            self.model_version,
            self.embedding_version,
            self.preprocessing_version,
            self.metric
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
            message: "Semantic search model unavailable. Metadata and technical filters remain available.".into(),
            identity: None,
        },
        Err(error) => LocalModelCapability {
            installed: false,
            message: format!("Semantic search model unavailable: {error}"),
            identity: None,
        },
    }
}

/// Discovers the only product-approved M6 pack location. This deliberately does not offer a UI
/// path picker: executable model material must be managed beneath CaptureOS's data directory.
pub fn discover_siglip_provider(
    model_root: &Path,
) -> Result<Option<SiglipOnnxProvider>, MagicSearchError> {
    let pack_root = model_root.join(SUPPORTED_SIGLIP_PACK_DIRECTORY);
    if !pack_root.is_dir() {
        return Ok(None);
    }
    SiglipOnnxProvider::open(&pack_root).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPackFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenIds {
    bos: i64,
    eos: i64,
    pad: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenizerSelfTest {
    text: String,
    expected_ids: Vec<i64>,
}

/// The static pack manifest is intentionally stricter than a generic ONNX manifest. This lets
/// CaptureOS reject unknown model families and unreviewed architecture changes before inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SiglipPackManifest {
    format_version: u32,
    model_id: String,
    model_family: String,
    provider: String,
    model_version: String,
    source_revision: String,
    source_weights_sha256: String,
    license: String,
    license_url: String,
    image_model: ModelPackFile,
    text_model: ModelPackFile,
    tokenizer: ModelPackFile,
    license_file: ModelPackFile,
    input_size: u32,
    embedding_dimension: usize,
    text_sequence_length: usize,
    token_ids: TokenIds,
    /// The exact ONNX graph input order. Only `input_ids` and `attention_mask` are accepted.
    text_inputs: Vec<String>,
    image_output_index: usize,
    text_output_index: usize,
    /// `pooled`, `first_token`, or `mean_tokens` for a text graph that has not emitted a pooled
    /// vector directly.
    text_pooling: String,
    tokenizer_self_test: TokenizerSelfTest,
}

type RunnableModel = TypedRunnableModel<TypedModel>;

/// CPU-only, static ONNX SigLIP adapter. It contains no network implementation and never loads
/// a model selected by raw user text. The runtime is intentionally narrow so a future provider
/// (for example an audited CoreML accelerator) can sit behind the same trait.
pub struct SiglipOnnxProvider {
    identity: SemanticProviderIdentity,
    manifest: SiglipPackManifest,
    image_model: Mutex<RunnableModel>,
    text_model: Mutex<RunnableModel>,
    tokenizer: UnigramTokenizer,
}

impl SiglipOnnxProvider {
    fn open(pack_root: &Path) -> Result<Self, MagicSearchError> {
        let root = pack_root.canonicalize().map_err(|error| {
            MagicSearchError::InvalidModelPack(format!("model pack root is unavailable: {error}"))
        })?;
        if !root.is_dir() {
            return Err(MagicSearchError::InvalidModelPack(
                "model pack root is not a directory".into(),
            ));
        }
        let manifest_path = safe_pack_file(&root, "captureos-semantic-model.json")?;
        let manifest: SiglipPackManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
        validate_manifest(&manifest)?;
        let image_path = verified_pack_file(&root, &manifest.image_model)?;
        let text_path = verified_pack_file(&root, &manifest.text_model)?;
        let tokenizer_path = verified_pack_file(&root, &manifest.tokenizer)?;
        let _license_path = verified_pack_file(&root, &manifest.license_file)?;
        let tokenizer = UnigramTokenizer::from_json(&fs::read(tokenizer_path)?, &manifest.token_ids)?;
        let self_test_ids = tokenizer.encode(&manifest.tokenizer_self_test.text, manifest.text_sequence_length)?;
        if self_test_ids != manifest.tokenizer_self_test.expected_ids {
            return Err(MagicSearchError::InvalidModelPack(
                "tokenizer self-test did not match the approved pack".into(),
            ));
        }
        let image_model = load_runnable_model(&image_path)?;
        let text_model = load_runnable_model(&text_path)?;
        Ok(Self {
            identity: SemanticProviderIdentity {
                model_id: manifest.model_id.clone(),
                model_family: manifest.model_family.clone(),
                provider: manifest.provider.clone(),
                model_version: manifest.model_version.clone(),
                embedding_version: SEMANTIC_EMBEDDING_VERSION.into(),
                preprocessing_version: SIGLIP_PREPROCESSING_VERSION.into(),
                metric: SIGLIP_NORMALIZED_DOT_METRIC.into(),
                dimensions: manifest.embedding_dimension,
            },
            manifest,
            image_model: Mutex::new(image_model),
            text_model: Mutex::new(text_model),
            tokenizer,
        })
    }

    pub fn pack_metadata(&self) -> ValidatedModelPackMetadata {
        ValidatedModelPackMetadata {
            source_revision: self.manifest.source_revision.clone(),
            source_weights_sha256: self.manifest.source_weights_sha256.clone(),
            license: self.manifest.license.clone(),
            license_url: self.manifest.license_url.clone(),
            input_size: self.manifest.input_size,
            embedding_dimension: self.manifest.embedding_dimension,
        }
    }
}

impl SemanticEmbeddingProvider for SiglipOnnxProvider {
    fn identity(&self) -> &SemanticProviderIdentity {
        &self.identity
    }

    fn embed_image(&self, image: &AnalysisImage) -> Result<Vec<f32>, MagicSearchError> {
        let input = Tensor::from_shape(
            &[1, 3, self.manifest.input_size as usize, self.manifest.input_size as usize],
            &siglip_image_input(image, self.manifest.input_size),
        )
        .map_err(|error| MagicSearchError::Provider(format!("image tensor construction failed: {error}")))?;
        let outputs = self
            .image_model
            .lock()
            .map_err(|_| MagicSearchError::Provider("image model lock was poisoned".into()))?
            .run(TVec::from_vec(vec![input.into()]))
            .map_err(|error| MagicSearchError::Provider(format!("image inference failed: {error}")))?;
        normalize_embedding(select_output_embedding(
            &outputs,
            self.manifest.image_output_index,
            self.identity.dimensions,
            "pooled",
        )?)
    }

    fn embed_text(&self, query: &str) -> Result<Vec<f32>, MagicSearchError> {
        let token_ids = self
            .tokenizer
            .encode(query, self.manifest.text_sequence_length)?;
        let attention_mask = token_ids
            .iter()
            .map(|token| i64::from(*token != self.manifest.token_ids.pad))
            .collect::<Vec<_>>();
        let ids = Tensor::from_shape(&[1, self.manifest.text_sequence_length], &token_ids)
            .map_err(|error| MagicSearchError::Provider(format!("text token tensor failed: {error}")))?;
        let mask = Tensor::from_shape(&[1, self.manifest.text_sequence_length], &attention_mask)
            .map_err(|error| MagicSearchError::Provider(format!("text mask tensor failed: {error}")))?;
        let inputs = self
            .manifest
            .text_inputs
            .iter()
            .map(|name| match name.as_str() {
                "input_ids" => Ok(ids.clone().into()),
                "attention_mask" => Ok(mask.clone().into()),
                _ => Err(MagicSearchError::InvalidModelPack(format!(
                    "unsupported text input {name}"
                ))),
            })
            .collect::<Result<TVec<_>, _>>()?;
        let outputs = self
            .text_model
            .lock()
            .map_err(|_| MagicSearchError::Provider("text model lock was poisoned".into()))?
            .run(inputs)
            .map_err(|error| MagicSearchError::Provider(format!("text inference failed: {error}")))?;
        normalize_embedding(select_output_embedding(
            &outputs,
            self.manifest.text_output_index,
            self.identity.dimensions,
            &self.manifest.text_pooling,
        )?)
    }
}

fn validate_manifest(manifest: &SiglipPackManifest) -> Result<(), MagicSearchError> {
    if manifest.format_version != 1
        || manifest.model_id != SUPPORTED_SIGLIP_MODEL_ID
        || manifest.model_family != "siglip"
        || manifest.provider != "tract-onnx"
        || manifest.license != "Apache-2.0"
        || manifest.input_size != SIGLIP_INPUT_SIZE
        || manifest.embedding_dimension != SIGLIP_EMBEDDING_DIMENSION
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
    if manifest.text_inputs.is_empty()
        || manifest.text_inputs.len() > 2
        || manifest
            .text_inputs
            .iter()
            .any(|input| !matches!(input.as_str(), "input_ids" | "attention_mask"))
        || !matches!(manifest.text_pooling.as_str(), "pooled" | "first_token" | "mean_tokens")
        || manifest.tokenizer_self_test.expected_ids.len() != manifest.text_sequence_length
    {
        return Err(MagicSearchError::InvalidModelPack(
            "manifest has an unsupported ONNX input or tokenizer contract".into(),
        ));
    }
    for file in [
        &manifest.image_model,
        &manifest.text_model,
        &manifest.tokenizer,
        &manifest.license_file,
    ] {
        if !valid_sha256(&file.sha256) || !is_safe_relative_path(&file.path) {
            return Err(MagicSearchError::InvalidModelPack(
                "manifest contains an unsafe file location or checksum".into(),
            ));
        }
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value).components().all(|component| {
            matches!(component, Component::Normal(_))
        })
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

fn load_runnable_model(path: &Path) -> Result<RunnableModel, MagicSearchError> {
    tract_onnx::onnx()
        .model_for_path(path)
        .and_then(|model| model.into_optimized())
        .and_then(|model| model.into_runnable())
        .map_err(|error| MagicSearchError::Provider(format!("could not load static ONNX graph: {error}")))
}

fn siglip_image_input(image: &AnalysisImage, target_size: u32) -> Vec<f32> {
    let target = target_size as usize;
    let pixels = target * target;
    let mut channels = vec![0.0; pixels * 3];
    for y in 0..target {
        for x in 0..target {
            let source_x = (x as f64 + 0.5) * f64::from(image.width) / target as f64 - 0.5;
            let source_y = (y as f64 + 0.5) * f64::from(image.height) / target as f64 - 0.5;
            let rgb = bilinear_rgb(image, source_x, source_y);
            let destination = y * target + x;
            for channel in 0..3 {
                // Google SigLIP's processor: RGB / 255, mean=.5, std=.5.
                channels[channel * pixels + destination] =
                    (f32::from(rgb[channel]) / 255.0 - 0.5) / 0.5;
            }
        }
    }
    channels
}

fn bilinear_rgb(image: &AnalysisImage, x: f64, y: f64) -> [u8; 3] {
    let x0 = x.floor().clamp(0.0, f64::from(image.width.saturating_sub(1))) as u32;
    let y0 = y.floor().clamp(0.0, f64::from(image.height.saturating_sub(1))) as u32;
    let x1 = (x0 + 1).min(image.width.saturating_sub(1));
    let y1 = (y0 + 1).min(image.height.saturating_sub(1));
    let wx = (x - x.floor()).clamp(0.0, 1.0);
    let wy = (y - y.floor()).clamp(0.0, 1.0);
    let top_left = image.pixel(x0, y0);
    let top_right = image.pixel(x1, y0);
    let bottom_left = image.pixel(x0, y1);
    let bottom_right = image.pixel(x1, y1);
    std::array::from_fn(|channel| {
        let top = f64::from(top_left[channel]) * (1.0 - wx) + f64::from(top_right[channel]) * wx;
        let bottom =
            f64::from(bottom_left[channel]) * (1.0 - wx) + f64::from(bottom_right[channel]) * wx;
        (top * (1.0 - wy) + bottom * wy).round().clamp(0.0, 255.0) as u8
    })
}

fn select_output_embedding(
    outputs: &TVec<tract_onnx::prelude::TValue>,
    output_index: usize,
    dimensions: usize,
    pooling: &str,
) -> Result<Vec<f32>, MagicSearchError> {
    let output = outputs.get(output_index).ok_or_else(|| {
        MagicSearchError::Provider(format!("ONNX graph did not return output index {output_index}"))
    })?;
    let values = output
        .as_slice::<f32>()
        .map_err(|error| MagicSearchError::Provider(format!("ONNX output was not f32: {error}")))?;
    if values.len() == dimensions {
        return Ok(values.to_vec());
    }
    if values.len() < dimensions || values.len() % dimensions != 0 {
        return Err(MagicSearchError::Provider(format!(
            "ONNX output has {} values; expected a multiple of {dimensions}",
            values.len()
        )));
    }
    let tokens = values.len() / dimensions;
    match pooling {
        "first_token" => Ok(values[..dimensions].to_vec()),
        "mean_tokens" => Ok((0..dimensions)
            .map(|dimension| {
                values
                    .chunks_exact(dimensions)
                    .map(|token| token[dimension])
                    .sum::<f32>()
                    / tokens as f32
            })
            .collect()),
        "pooled" => Err(MagicSearchError::Provider(
            "pack declared a pooled text output but graph returned token vectors".into(),
        )),
        _ => Err(MagicSearchError::InvalidModelPack(
            "unknown text pooling mode".into(),
        )),
    }
}

pub fn normalize_embedding(mut values: Vec<f32>) -> Result<Vec<f32>, MagicSearchError> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(MagicSearchError::InvalidEmbedding(
            "embedding must contain finite values".into(),
        ));
    }
    let norm = values
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
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

/// Minimal, safe Unigram tokenizer for the static pack format. The pack has a mandatory token-ID
/// self-test, so a configuration that differs from the reviewed upstream tokenizer is rejected
/// before semantic output could be persisted. This intentionally accepts only JSON tokenizer data
/// and never executes tokenizer code.
#[derive(Debug, Clone)]
struct UnigramTokenizer {
    pieces: BTreeMap<char, Vec<UnigramPiece>>,
    unknown_id: i64,
    ids: TokenIds,
}

#[derive(Debug, Clone)]
struct UnigramPiece {
    characters: Vec<char>,
    score: f64,
    id: i64,
}

impl UnigramTokenizer {
    fn from_json(bytes: &[u8], ids: &TokenIds) -> Result<Self, MagicSearchError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let model = value.get("model").and_then(serde_json::Value::as_object).ok_or_else(|| {
            MagicSearchError::InvalidModelPack("tokenizer JSON has no model object".into())
        })?;
        if model.get("type").and_then(serde_json::Value::as_str) != Some("Unigram") {
            return Err(MagicSearchError::InvalidModelPack(
                "SigLIP pack requires a Unigram tokenizer JSON".into(),
            ));
        }
        let unknown_id = model
            .get("unk_id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| MagicSearchError::InvalidModelPack("tokenizer has no unk_id".into()))?;
        let vocabulary = model
            .get("vocab")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| MagicSearchError::InvalidModelPack("tokenizer has no vocab".into()))?;
        let mut pieces = BTreeMap::<char, Vec<UnigramPiece>>::new();
        for (id, entry) in vocabulary.iter().enumerate() {
            let pair = entry.as_array().ok_or_else(|| {
                MagicSearchError::InvalidModelPack("tokenizer vocab entry is not a pair".into())
            })?;
            let token = pair.first().and_then(serde_json::Value::as_str).ok_or_else(|| {
                MagicSearchError::InvalidModelPack("tokenizer vocab token is invalid".into())
            })?;
            let score = pair.get(1).and_then(serde_json::Value::as_f64).ok_or_else(|| {
                MagicSearchError::InvalidModelPack("tokenizer vocab score is invalid".into())
            })?;
            let characters = token.chars().collect::<Vec<_>>();
            if let Some(first) = characters.first().copied() {
                pieces.entry(first).or_default().push(UnigramPiece {
                    characters,
                    score,
                    id: id as i64,
                });
            }
        }
        if pieces.is_empty() {
            return Err(MagicSearchError::InvalidModelPack(
                "tokenizer vocabulary was empty".into(),
            ));
        }
        for candidates in pieces.values_mut() {
            candidates.sort_by_key(|piece| Reverse(piece.characters.len()));
        }
        Ok(Self {
            pieces,
            unknown_id,
            ids: ids.clone(),
        })
    }

    fn encode(&self, query: &str, max_length: usize) -> Result<Vec<i64>, MagicSearchError> {
        if query.trim().is_empty() {
            return Err(MagicSearchError::InvalidQuery(
                "search text may not be empty".into(),
            ));
        }
        if query.chars().count() > 512 || max_length < 3 {
            return Err(MagicSearchError::InvalidQuery(
                "search text exceeds the local tokenizer safety limit".into(),
            ));
        }
        let normalized = normalize_siglip_text(query);
        let chars = normalized.chars().collect::<Vec<_>>();
        let mut scores = vec![f64::NEG_INFINITY; chars.len() + 1];
        let mut previous = vec![None; chars.len() + 1];
        scores[0] = 0.0;
        for start in 0..chars.len() {
            if !scores[start].is_finite() {
                continue;
            }
            let mut matched = false;
            if let Some(candidates) = self.pieces.get(&chars[start]) {
                for piece in candidates {
                    let end = start + piece.characters.len();
                    if end <= chars.len()
                        && chars[start..end] == piece.characters[..]
                        && scores[start] + piece.score > scores[end]
                    {
                        scores[end] = scores[start] + piece.score;
                        previous[end] = Some((start, piece.id));
                        matched = true;
                    }
                }
            }
            if !matched && scores[start] - 20.0 > scores[start + 1] {
                scores[start + 1] = scores[start] - 20.0;
                previous[start + 1] = Some((start, self.unknown_id));
            }
        }
        let mut pieces = Vec::new();
        let mut cursor = chars.len();
        while cursor > 0 {
            let Some((start, id)) = previous[cursor] else {
                return Err(MagicSearchError::InvalidModelPack(
                    "tokenizer could not segment its input".into(),
                ));
            };
            pieces.push(id);
            cursor = start;
        }
        pieces.reverse();
        let mut ids = Vec::with_capacity(max_length);
        ids.push(self.ids.bos);
        ids.extend(pieces.into_iter().take(max_length - 2));
        ids.push(self.ids.eos);
        ids.resize(max_length, self.ids.pad);
        Ok(ids)
    }
}

fn normalize_siglip_text(query: &str) -> String {
    let mut normalized = String::from("▁");
    let mut emitted_space = true;
    for character in query.nfkc().flat_map(char::to_lowercase) {
        if character.is_whitespace() {
            if !emitted_space {
                normalized.push('▁');
                emitted_space = true;
            }
        } else {
            normalized.push(character);
            emitted_space = false;
        }
    }
    normalized.trim_end_matches('▁').to_owned()
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
        return Err(MagicSearchError::InvalidQuery("search text may not be empty".into()));
    }
    if normalized.chars().count() > 512 {
        return Err(MagicSearchError::InvalidQuery(
            "search text exceeds the local safety limit".into(),
        ));
    }
    let lower = normalized.to_lowercase();
    let identity_search_blocked = ["who is ", "person named ", "find ", "name this person"]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
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
        (&[" with 2 faces ", " 2 faces ", " two faces "] as &[&str], 2_u64),
        (&[" with 1 face ", " 1 face ", " one face "] as &[&str], 1_u64),
    ] {
        if phrases.iter().any(|phrase| residual.contains(phrase)) {
            filters.face_count = Some(count);
            chips.push(format!("Faces: {count}"));
            for phrase in phrases {
                residual = residual.replace(phrase, " ");
            }
        }
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
        (&[" rejected ", " reject "] as &[&str], "reject", "Decision: Reject"),
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
    if let Some(camera) = extract_camera_model(&residual) {
        filters.camera_model = Some(camera.clone());
        chips.push(format!("Camera: {camera}"));
        residual = residual.replace(&format!(" camera {camera} "), " ");
    }
    for filler in [" photos ", " photo ", " images ", " image ", " with ", " and "] {
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

fn exact_star_rating(value: &str) -> Option<u8> {
    (1_u8..=5).find(|rating| {
        value.contains(&format!(" {rating} star ")) || value.contains(&format!(" {rating} stars "))
    })
}

fn minimum_star_rating(value: &str) -> Option<u8> {
    (1_u8..=5).find(|rating| {
        value.contains(&format!(" {rating}+ star ")) || value.contains(&format!(" {rating}+ stars "))
    })
}

fn extract_camera_model(value: &str) -> Option<String> {
    let marker = " camera ";
    let start = value.find(marker)? + marker.len();
    let remaining = &value[start..];
    let stop = [" with ", " sharp ", " blurry ", " blur ", " kept ", " rejected ", " review "]
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
            let score: f32 = query.iter().zip(vector).map(|(left, right)| left * right).sum();
            score.is_finite().then_some(ScoredVector { asset_id, score })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| left.asset_id.cmp(&right.asset_id)));
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
        let envelope: IndexEnvelope = serde_json::from_slice(&fs::read(path)?).map_err(|error| {
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
        let projections = lsh_projections(self.payload.dimensions);
        for table in 0..INDEX_TABLES {
            let signature = bucket_signature(table, &query, &projections);
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
    Ok(blake3::hash(&serde_json::to_vec(payload)?).to_hex().to_string())
}

fn bucket_key(table: usize, vector: &[f32], projections: &[Vec<f32>]) -> String {
    format!("{table}:{:04x}", bucket_signature(table, vector, projections))
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
    pub fn new(dimensions: usize, image_embedding: Vec<f32>, text_embeddings: std::collections::HashMap<String, Vec<f32>>) -> Self {
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

    #[test]
    fn planner_extracts_hybrid_filters_without_hard_coding_visual_concepts() {
        let plan = plan_query("5 star sharp photos with 1 face near water").unwrap();
        assert_eq!(plan.filters.rating_exact, Some(5));
        assert_eq!(plan.filters.face_count, Some(1));
        assert!(plan.filters.require_sharp);
        assert_eq!(plan.semantic_query.as_deref(), Some("near water"));
        assert_eq!(plan.chips, vec!["Faces: 1", "Rating: 5★", "Sharpness: sharp"]);
    }

    #[test]
    fn planner_blocks_identity_request_without_touching_face_search() {
        let plan = plan_query("find John").unwrap();
        assert!(plan.identity_search_blocked);
        assert!(plan.semantic_query.is_none());
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
                (format!("asset-{index:05}"), vec![1.0, value, value * value, 0.25])
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
        let IndexCandidates::AssetIds(candidates) = index.candidates(&[1.0, 0.9, 0.8, 0.2]).unwrap() else {
            panic!("large index must not request an exact full scan");
        };
        assert!(candidates.len() < 50_000);
        assert!(candidates.len() <= MAX_APPROXIMATE_CANDIDATES);
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
        fs::write(directory.path().join(&built.relative_path), b"not index JSON").unwrap();
        assert!(matches!(
            PersistentVectorIndex::load(directory.path(), &built.relative_path, "project-a", "model-v1", 2),
            Err(MagicSearchError::CorruptIndex(_))
        ));
    }
}
