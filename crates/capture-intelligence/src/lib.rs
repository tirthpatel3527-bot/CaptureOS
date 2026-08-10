//! Local, explainable Capture Intelligence providers.
//!
//! This crate deliberately contains deterministic image evidence and provider boundaries rather
//! than a hosted model client. It never opens a source path supplied by a UI caller and never
//! writes beside original media. `capture-core` gives it only a validated, CaptureOS-managed
//! analysis preview or a test image.

use blake3::Hasher;
use chrono::Utc;
use media_model::{
    AnalysisStatus, BlurEvidenceLevel, EyeState, RecommendationLabel, SimilarityGroupId,
    SimilarityGroupKind, TechnicalQualityBand,
};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[cfg(target_os = "macos")]
use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    ptr,
};
use tempfile::tempdir;
use thiserror::Error;
use tract_onnx::prelude::{
    Framework, InferenceModelExt, TVec, Tensor, TypedModel, TypedRunnableModel,
};
use uuid::Uuid;

pub const DETERMINISTIC_PROVIDER: &str = "captureos-deterministic-image";
pub const DETERMINISTIC_VERSION: &str = "m4.det.v1";
pub const RECOMMENDATION_PROVIDER: &str = "captureos-technical-recommendation";
pub const RECOMMENDATION_VERSION: &str = "m4.rules.v1";
/// Bumped for M4.1 because the resolver can now create and select a dedicated, 2048px
/// CaptureOS-managed analysis input. Existing M4 evidence is retained as history and is safely
/// recomputed once against the corrected local-input contract.
pub const ANALYSIS_SETTINGS_VERSION: &str = "m4.1.analysis-input-resolver.v1";
/// Face processing has its own settings lifecycle because a host-platform provider can change
/// without changing deterministic visual/technical evidence.
pub const FACE_ANALYSIS_SETTINGS_VERSION: &str = "m4.face-analysis.v2";
/// Stable cache identity for CaptureOS's local rectangle-first provider chain. The resolved
/// native provider is recorded separately for diagnostics, so a failed Apple Vision attempt
/// cannot make a later local fallback look unavailable.
pub const LOCAL_FACE_DETECTION_PROVIDER: &str = "captureos-local-face-detection";
pub const LOCAL_FACE_DETECTION_VERSION: &str = "m4.face-detection-chain.v3";
pub const APPLE_VISION_FACE_PROVIDER: &str = "apple-vision-face-rectangles";
pub const APPLE_VISION_ADAPTER_VERSION: &str = "m4.apple-vision-adapter.v3";
pub const ULTRAFACE_FACE_PROVIDER: &str = "ultraface-rfb-320";
pub const ULTRAFACE_FACE_PROVIDER_VERSION: &str =
    "version-RFB-320.onnx;sha256=34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017";
pub const UNAVAILABLE_FACE_PROVIDER: &str = "none";
pub const UNAVAILABLE_FACE_PROVIDER_VERSION: &str = "no-approved-local-provider.v1";
const FACE_PROVIDER_CACHE_IDENTITY_VERSION: &str = "m4.face-provider-cache.v1";
const FACE_ANALYSIS_INPUT_IDENTITY_VERSION: &str = "m4.face-analysis-input.v1";
const ULTRAFACE_MODEL_BYTES: &[u8] = include_bytes!("../models/ultraface-rfb-320.onnx");
const ULTRAFACE_INPUT_WIDTH: usize = 320;
const ULTRAFACE_INPUT_HEIGHT: usize = 240;
const ULTRAFACE_FACE_CONFIDENCE_THRESHOLD: f32 = 0.7;
const ULTRAFACE_NMS_IOU_THRESHOLD: f64 = 0.3;
/// A rectangle smaller than 0.2% of the analysis image is below the reliable operating range of
/// this fixed 320×240 detector and otherwise produced a high-confidence background false positive
/// in the real couple-photo acceptance probe.
const ULTRAFACE_MIN_FACE_RELATIVE_AREA: f64 = 0.002;
#[cfg(any(target_os = "macos", test))]
const MACOS_VISION_REQUEST_REVISION_UNAVAILABLE: &str = "unavailable";
#[cfg(any(target_os = "macos", test))]
const MACOS_PRODUCT_VERSION_UNAVAILABLE: &str = "unavailable";
pub const MAX_BUCKET_MEMBERS: usize = 96;
/// Exhaustive comparisons are bounded to small projects to recover valid related-frame pairs
/// whose four exact pHash bands do not overlap. Larger projects retain bounded LSH/time recall.
pub const SMALL_PROJECT_EXHAUSTIVE_CANDIDATE_LIMIT: usize = 256;
pub const NEAR_DUPLICATE_MAX_PHASH_DISTANCE: u32 = 10;
pub const NEAR_DUPLICATE_MIN_VISUAL_SIMILARITY: f64 = 0.86;
pub const SIMILAR_MAX_PHASH_DISTANCE: u32 = 18;
pub const SIMILAR_MIN_VISUAL_SIMILARITY: f64 = 0.78;
const PLATFORM_PROVIDER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Error)]
pub enum IntelligenceError {
    #[error("analysis preview is unavailable: {0}")]
    NeedsOriginal(String),
    #[error("analysis preview is unsupported: {0}")]
    Unsupported(String),
    #[error("analysis preview is corrupt: {0}")]
    Corrupt(String),
    #[error("local provider timed out")]
    Timeout,
    #[error("local provider failed: {0}")]
    Provider(String),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid image data: {0}")]
    InvalidImage(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl IntelligenceError {
    pub fn status(&self) -> AnalysisStatus {
        match self {
            Self::NeedsOriginal(_) => AnalysisStatus::NeedsOriginal,
            Self::Unsupported(_) => AnalysisStatus::Unsupported,
            Self::Corrupt(_) | Self::InvalidImage(_) => AnalysisStatus::Corrupt,
            Self::Timeout | Self::Provider(_) | Self::Io(_) | Self::Serialization(_) => {
                AnalysisStatus::Failed
            }
        }
    }
}

/// RGB raster owned by CaptureOS. The constructor validates dimensions so later algorithms can
/// safely index pixels without trusting decoded metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisImage {
    pub width: u32,
    pub height: u32,
    rgb: Vec<u8>,
}

impl AnalysisImage {
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Result<Self, IntelligenceError> {
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|value| value.checked_mul(usize::try_from(height).ok()?))
            .and_then(|value| value.checked_mul(3))
            .ok_or_else(|| IntelligenceError::InvalidImage("image dimensions overflow".into()))?;
        if width == 0 || height == 0 || rgb.len() != pixels {
            return Err(IntelligenceError::InvalidImage(
                "RGB byte count does not match image dimensions".into(),
            ));
        }
        Ok(Self { width, height, rgb })
    }

    pub fn solid(width: u32, height: u32, color: [u8; 3]) -> Self {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..width.saturating_mul(height) {
            rgb.extend_from_slice(&color);
        }
        Self { width, height, rgb }
    }

    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let offset = ((y * self.width + x) * 3) as usize;
        [self.rgb[offset], self.rgb[offset + 1], self.rgb[offset + 2]]
    }

    pub fn from_ppm(bytes: &[u8]) -> Result<Self, IntelligenceError> {
        let mut cursor = 0;
        let magic = ppm_token(bytes, &mut cursor)?;
        let width = ppm_token(bytes, &mut cursor)?
            .parse::<u32>()
            .map_err(|_| IntelligenceError::InvalidImage("invalid PPM width".into()))?;
        let height = ppm_token(bytes, &mut cursor)?
            .parse::<u32>()
            .map_err(|_| IntelligenceError::InvalidImage("invalid PPM height".into()))?;
        let max = ppm_token(bytes, &mut cursor)?
            .parse::<u32>()
            .map_err(|_| IntelligenceError::InvalidImage("invalid PPM maximum".into()))?;
        if max == 0 || max > 255 {
            return Err(IntelligenceError::Unsupported(
                "only 8-bit PPM analysis previews are supported".into(),
            ));
        }
        match magic.as_str() {
            "P6" => {
                if cursor >= bytes.len() || !bytes[cursor].is_ascii_whitespace() {
                    return Err(IntelligenceError::InvalidImage(
                        "binary PPM header is missing its delimiter".into(),
                    ));
                }
                cursor += 1;
                let expected = usize::try_from(width)
                    .ok()
                    .and_then(|value| value.checked_mul(usize::try_from(height).ok()?))
                    .and_then(|value| value.checked_mul(3))
                    .ok_or_else(|| {
                        IntelligenceError::InvalidImage("PPM dimensions overflow".into())
                    })?;
                if bytes.len().saturating_sub(cursor) != expected {
                    return Err(IntelligenceError::InvalidImage(
                        "binary PPM pixel count does not match its header".into(),
                    ));
                }
                Self::new(width, height, bytes[cursor..].to_vec())
            }
            "P3" => {
                let expected = usize::try_from(width)
                    .ok()
                    .and_then(|value| value.checked_mul(usize::try_from(height).ok()?))
                    .and_then(|value| value.checked_mul(3))
                    .ok_or_else(|| {
                        IntelligenceError::InvalidImage("PPM dimensions overflow".into())
                    })?;
                let mut rgb = Vec::with_capacity(expected);
                for _ in 0..expected {
                    let value = ppm_token(bytes, &mut cursor)?.parse::<u32>().map_err(|_| {
                        IntelligenceError::InvalidImage("invalid PPM channel".into())
                    })?;
                    if value > max {
                        return Err(IntelligenceError::InvalidImage(
                            "PPM channel exceeds maximum".into(),
                        ));
                    }
                    rgb.push(((value * 255) / max) as u8);
                }
                Self::new(width, height, rgb)
            }
            _ => Err(IntelligenceError::Unsupported(
                "analysis decoder expects PPM P6 or P3".into(),
            )),
        }
    }

    /// Decode an uncompressed Windows BMP raster. macOS `sips` reliably writes this small,
    /// local interchange format even on systems where it cannot write PPM. The parser accepts
    /// only bounded 24/32-bit RGB layouts and validates every offset before reading pixels.
    pub fn from_bmp(bytes: &[u8]) -> Result<Self, IntelligenceError> {
        if bytes.len() < 54 || &bytes[..2] != b"BM" {
            return Err(IntelligenceError::InvalidImage(
                "BMP header is missing or invalid".into(),
            ));
        }
        let pixel_offset = read_u32_le(bytes, 10)? as usize;
        let dib_size = read_u32_le(bytes, 14)?;
        if dib_size < 40 || bytes.len() < 14 + dib_size as usize {
            return Err(IntelligenceError::Unsupported(
                "BMP uses an unsupported DIB header".into(),
            ));
        }
        let width = read_i32_le(bytes, 18)?;
        let signed_height = read_i32_le(bytes, 22)?;
        let planes = read_u16_le(bytes, 26)?;
        let bits_per_pixel = read_u16_le(bytes, 28)?;
        let compression = read_u32_le(bytes, 30)?;
        if width <= 0 || signed_height == 0 || signed_height == i32::MIN || planes != 1 {
            return Err(IntelligenceError::InvalidImage(
                "BMP dimensions or planes are invalid".into(),
            ));
        }
        if !matches!(bits_per_pixel, 24 | 32) || compression != 0 {
            return Err(IntelligenceError::Unsupported(
                "analysis decoder supports only uncompressed 24/32-bit BMP".into(),
            ));
        }
        let width = u32::try_from(width).map_err(|_| {
            IntelligenceError::InvalidImage("BMP width cannot be represented".into())
        })?;
        let height = signed_height.unsigned_abs();
        let columns = usize::try_from(width)
            .map_err(|_| IntelligenceError::InvalidImage("BMP width overflows".into()))?;
        let bytes_per_pixel = usize::from(bits_per_pixel / 8);
        let row_unpadded = columns
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| IntelligenceError::InvalidImage("BMP row overflows".into()))?;
        let row_stride = row_unpadded
            .checked_add(3)
            .map(|value| value & !3)
            .ok_or_else(|| IntelligenceError::InvalidImage("BMP row stride overflows".into()))?;
        let rows = usize::try_from(height)
            .map_err(|_| IntelligenceError::InvalidImage("BMP height overflows".into()))?;
        let source_length = row_stride
            .checked_mul(rows)
            .ok_or_else(|| IntelligenceError::InvalidImage("BMP pixels overflow".into()))?;
        let source_end = pixel_offset
            .checked_add(source_length)
            .ok_or_else(|| IntelligenceError::InvalidImage("BMP pixel offset overflows".into()))?;
        if pixel_offset < 14 + dib_size as usize || source_end > bytes.len() {
            return Err(IntelligenceError::InvalidImage(
                "BMP pixels do not fit its declared bounds".into(),
            ));
        }
        let destination_length = columns
            .checked_mul(rows)
            .and_then(|value| value.checked_mul(3))
            .ok_or_else(|| IntelligenceError::InvalidImage("BMP RGB pixels overflow".into()))?;
        let mut rgb = vec![0_u8; destination_length];
        let top_down = signed_height < 0;
        for output_y in 0..rows {
            let source_y = if top_down {
                output_y
            } else {
                rows - 1 - output_y
            };
            let source_row = pixel_offset + source_y * row_stride;
            for x in 0..columns {
                let source = source_row + x * bytes_per_pixel;
                let destination = (output_y * columns + x) * 3;
                // BMP stores channel order as B, G, R (and optionally alpha).
                rgb[destination] = bytes[source + 2];
                rgb[destination + 1] = bytes[source + 1];
                rgb[destination + 2] = bytes[source];
            }
        }
        Self::new(width, height, rgb)
    }

    pub fn ppm_bytes(&self) -> Vec<u8> {
        let mut value = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        value.extend_from_slice(&self.rgb);
        value
    }

    fn luminance(&self, x: u32, y: u32) -> f64 {
        let [r, g, b] = self.pixel(x, y);
        (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0
    }

    fn sampled_luminance(&self, x: usize, y: usize, width: usize, height: usize) -> f64 {
        let source_x = ((x as f64 + 0.5) * self.width as f64 / width as f64)
            .floor()
            .clamp(0.0, f64::from(self.width.saturating_sub(1))) as u32;
        let source_y = ((y as f64 + 0.5) * self.height as f64 / height as f64)
            .floor()
            .clamp(0.0, f64::from(self.height.saturating_sub(1))) as u32;
        self.luminance(source_x, source_y)
    }

    fn sampled_rgb(&self, x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
        let source_x = ((x as f64 + 0.5) * self.width as f64 / width as f64)
            .floor()
            .clamp(0.0, f64::from(self.width.saturating_sub(1))) as u32;
        let source_y = ((y as f64 + 0.5) * self.height as f64 / height as f64)
            .floor()
            .clamp(0.0, f64::from(self.height.saturating_sub(1))) as u32;
        self.pixel(source_x, source_y)
    }

    pub fn crop(&self, x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        if !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
            || width <= 0.0
            || height <= 0.0
            || x + width > 1.0 + f64::EPSILON
            || y + height > 1.0 + f64::EPSILON
        {
            return None;
        }
        let left = (x * f64::from(self.width)).floor() as u32;
        let top = (y * f64::from(self.height)).floor() as u32;
        let right = ((x + width) * f64::from(self.width)).ceil() as u32;
        let bottom = ((y + height) * f64::from(self.height)).ceil() as u32;
        let right = right.min(self.width);
        let bottom = bottom.min(self.height);
        if right <= left || bottom <= top {
            return None;
        }
        let mut rgb = Vec::with_capacity(((right - left) * (bottom - top) * 3) as usize);
        for row in top..bottom {
            for column in left..right {
                rgb.extend_from_slice(&self.pixel(column, row));
            }
        }
        Self::new(right - left, bottom - top, rgb).ok()
    }
}

fn ppm_token(bytes: &[u8], cursor: &mut usize) -> Result<String, IntelligenceError> {
    while *cursor < bytes.len() {
        if bytes[*cursor].is_ascii_whitespace() {
            *cursor += 1;
        } else if bytes[*cursor] == b'#' {
            while *cursor < bytes.len() && bytes[*cursor] != b'\n' {
                *cursor += 1;
            }
        } else {
            break;
        }
    }
    let start = *cursor;
    while *cursor < bytes.len() && !bytes[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
    if start == *cursor {
        return Err(IntelligenceError::InvalidImage(
            "PPM header ended unexpectedly".into(),
        ));
    }
    std::str::from_utf8(&bytes[start..*cursor])
        .map(str::to_owned)
        .map_err(|_| IntelligenceError::InvalidImage("PPM token is not UTF-8".into()))
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, IntelligenceError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| IntelligenceError::InvalidImage("BMP header ended unexpectedly".into()))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, IntelligenceError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| IntelligenceError::InvalidImage("BMP header ended unexpectedly".into()))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Result<i32, IntelligenceError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| IntelligenceError::InvalidImage("BMP header ended unexpectedly".into()))?;
    Ok(i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Decoder boundary. Production only uses a CaptureOS-owned cache rendition; unit tests can use
/// the PPM decoder directly without relying on a platform image framework.
pub trait AnalysisImageDecoder {
    fn decode(&self, path: &Path) -> Result<AnalysisImage, IntelligenceError>;
}

pub struct PpmDecoder;

impl AnalysisImageDecoder for PpmDecoder {
    fn decode(&self, path: &Path) -> Result<AnalysisImage, IntelligenceError> {
        if !path.is_file() {
            return Err(IntelligenceError::NeedsOriginal(
                "analysis preview is not available".into(),
            ));
        }
        AnalysisImage::from_ppm(&fs::read(path)?)
    }
}

/// macOS's bundled SIPS decoder is used only to decode an already-generated local preview into
/// an ephemeral BMP raster. It is an adapter boundary, not a cloud or a bundled model.
pub struct LocalPreviewDecoder;

impl AnalysisImageDecoder for LocalPreviewDecoder {
    fn decode(&self, path: &Path) -> Result<AnalysisImage, IntelligenceError> {
        if !path.is_file() {
            return Err(IntelligenceError::NeedsOriginal(
                "a suitable cached analysis preview is not available".into(),
            ));
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ppm"))
        {
            return PpmDecoder.decode(path);
        }
        decode_with_platform_preview(path)
    }
}

#[cfg(target_os = "macos")]
fn decode_with_platform_preview(path: &Path) -> Result<AnalysisImage, IntelligenceError> {
    let directory = tempdir()?;
    let output = directory.path().join("analysis-preview.bmp");
    let result = bounded_command(
        Command::new("/usr/bin/sips")
            .args(["-s", "format", "bmp", "--out"])
            .arg(&output)
            .arg(path),
    )?;
    if !result.success {
        return Err(IntelligenceError::Unsupported(format!(
            "macOS SIPS could not decode the cached preview: {}",
            result.stderr
        )));
    }
    AnalysisImage::from_bmp(&fs::read(output)?)
}

#[cfg(not(target_os = "macos"))]
fn decode_with_platform_preview(_path: &Path) -> Result<AnalysisImage, IntelligenceError> {
    Err(IntelligenceError::Unsupported(
        "this build has no local decoder for the cached analysis preview".into(),
    ))
}

struct CommandResult {
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

fn bounded_command(command: &mut Command) -> Result<CommandResult, IntelligenceError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut stream) = child.stdout.take() {
                stream.read_to_end(&mut stdout)?;
            }
            if let Some(mut stream) = child.stderr.take() {
                stream.read_to_end(&mut stderr)?;
            }
            return Ok(CommandResult {
                success: child.try_wait()?.is_some_and(|status| status.success()),
                stdout,
                stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
            });
        }
        if started.elapsed() >= PLATFORM_PROVIDER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(IntelligenceError::Timeout);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A provider boundary for deterministic visual descriptors. The current baseline is intentionally
/// replaceable: its compact embedding is useful for local grouping, not a claim of semantic AI.
pub trait ImageAnalyzer {
    fn provider(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn fingerprint(&self, image: &AnalysisImage) -> FingerprintEvidence;
    fn technical_evidence(&self, image: &AnalysisImage) -> TechnicalEvidence;
}

pub trait SimilarityProvider {
    fn provider(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn similarity(
        &self,
        left: &FingerprintEvidence,
        right: &FingerprintEvidence,
    ) -> SimilarityEvidence;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FingerprintEvidence {
    pub perceptual_hash: String,
    pub difference_hash: String,
    pub color_signature: Vec<u8>,
    /// Quantized, mean-centred 8 × 8 luminance descriptor. It is stored as an SQLite BLOB by
    /// persistence and can be swapped for a true embedding provider in a future milestone.
    pub embedding: Vec<i8>,
    pub bucket_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarityEvidence {
    pub perceptual_distance: u32,
    pub visual_similarity: f64,
    pub color_similarity: f64,
    pub embedding_similarity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TechnicalEvidence {
    pub global_sharpness: f64,
    pub sharpness_band: String,
    pub laplacian_variance: f64,
    pub edge_strength: f64,
    pub local_high_frequency: f64,
    pub directional_blur_ratio: f64,
    pub blur_level: BlurEvidenceLevel,
    pub mean_luminance: f64,
    pub median_luminance: f64,
    pub highlight_clipping_percent: f64,
    pub shadow_clipping_percent: f64,
    pub channel_clipping_percent: f64,
    pub technical_quality_score: f64,
    pub technical_quality_band: TechnicalQualityBand,
    pub confidence: f64,
}

pub struct DeterministicImageAnalyzer;

impl ImageAnalyzer for DeterministicImageAnalyzer {
    fn provider(&self) -> &'static str {
        DETERMINISTIC_PROVIDER
    }

    fn version(&self) -> &'static str {
        DETERMINISTIC_VERSION
    }

    fn fingerprint(&self, image: &AnalysisImage) -> FingerprintEvidence {
        fingerprint_image(image)
    }

    fn technical_evidence(&self, image: &AnalysisImage) -> TechnicalEvidence {
        technical_evidence(image)
    }
}

impl SimilarityProvider for DeterministicImageAnalyzer {
    fn provider(&self) -> &'static str {
        DETERMINISTIC_PROVIDER
    }

    fn version(&self) -> &'static str {
        DETERMINISTIC_VERSION
    }

    fn similarity(
        &self,
        left: &FingerprintEvidence,
        right: &FingerprintEvidence,
    ) -> SimilarityEvidence {
        similarity_evidence(left, right)
    }
}

pub fn fingerprint_image(image: &AnalysisImage) -> FingerprintEvidence {
    let perceptual_hash = perceptual_hash(image);
    let difference_hash = difference_hash(image);
    let color_signature = color_signature(image);
    let embedding = luminance_embedding(image);
    let mut bucket_keys = Vec::with_capacity(4);
    let hash = u64::from_str_radix(&perceptual_hash, 16).unwrap_or_default();
    for band in 0..4 {
        let segment = ((hash >> (band * 16)) & 0xffff) as u16;
        bucket_keys.push(format!("phash:{band}:{segment:04x}"));
    }
    FingerprintEvidence {
        perceptual_hash,
        difference_hash,
        color_signature,
        embedding,
        bucket_keys,
    }
}

fn perceptual_hash(image: &AnalysisImage) -> String {
    // pHash baseline: sample to 32 × 32, apply the low-frequency 8 × 8 DCT, and compare to the
    // median coefficient. It is stable under small resizes/re-encodes but is not cryptographic.
    let side = 32usize;
    let mut samples = vec![0.0; side * side];
    for y in 0..side {
        for x in 0..side {
            samples[y * side + x] = image.sampled_luminance(x, y, side, side);
        }
    }
    let mut coefficients = [0.0_f64; 64];
    for v in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0;
            for y in 0..side {
                for x in 0..side {
                    let horizontal = (std::f64::consts::PI * (2.0 * x as f64 + 1.0) * u as f64
                        / (2.0 * side as f64))
                        .cos();
                    let vertical = (std::f64::consts::PI * (2.0 * y as f64 + 1.0) * v as f64
                        / (2.0 * side as f64))
                        .cos();
                    sum += samples[y * side + x] * horizontal * vertical;
                }
            }
            coefficients[v * 8 + u] = sum;
        }
    }
    let mut values = coefficients[1..].to_vec();
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let mut hash = 0_u64;
    for (index, coefficient) in coefficients.iter().enumerate() {
        if index != 0 && *coefficient >= median {
            hash |= 1_u64 << index;
        }
    }
    format!("{hash:016x}")
}

fn difference_hash(image: &AnalysisImage) -> String {
    let width = 9usize;
    let height = 8usize;
    let mut hash = 0_u64;
    let mut bit = 0;
    for y in 0..height {
        for x in 0..(width - 1) {
            if image.sampled_luminance(x, y, width, height)
                >= image.sampled_luminance(x + 1, y, width, height)
            {
                hash |= 1_u64 << bit;
            }
            bit += 1;
        }
    }
    format!("{hash:016x}")
}

fn color_signature(image: &AnalysisImage) -> Vec<u8> {
    let mut histogram = [0_u32; 64];
    let sample_side = 64usize;
    for y in 0..sample_side {
        for x in 0..sample_side {
            let [r, g, b] = image.sampled_rgb(x, y, sample_side, sample_side);
            let bucket =
                ((usize::from(r) / 64) << 4) | ((usize::from(g) / 64) << 2) | (usize::from(b) / 64);
            histogram[bucket] += 1;
        }
    }
    let total = (sample_side * sample_side) as u32;
    histogram
        .into_iter()
        .map(|count| ((count * 255) / total) as u8)
        .collect()
}

fn luminance_embedding(image: &AnalysisImage) -> Vec<i8> {
    let side = 8usize;
    let mut values = Vec::with_capacity(side * side);
    for y in 0..side {
        for x in 0..side {
            values.push(image.sampled_luminance(x, y, side, side));
        }
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values
        .into_iter()
        .map(|value| ((value - mean) * 254.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

pub fn similarity_evidence(
    left: &FingerprintEvidence,
    right: &FingerprintEvidence,
) -> SimilarityEvidence {
    let perceptual_distance =
        hamming_distance_hex(&left.perceptual_hash, &right.perceptual_hash).unwrap_or(64);
    let hash_similarity = 1.0 - f64::from(perceptual_distance) / 64.0;
    let color_similarity = 1.0 - color_distance(&left.color_signature, &right.color_signature);
    let embedding_similarity = cosine_similarity(&left.embedding, &right.embedding);
    // pHash should remain the dominant evidence; colour and low-frequency descriptor avoid
    // grouping unrelated photos that happen to share a hash bucket.
    let visual_similarity =
        (hash_similarity * 0.60 + color_similarity * 0.15 + embedding_similarity * 0.25)
            .clamp(0.0, 1.0);
    SimilarityEvidence {
        perceptual_distance,
        visual_similarity,
        color_similarity,
        embedding_similarity,
    }
}

pub fn hamming_distance_hex(left: &str, right: &str) -> Option<u32> {
    let left = u64::from_str_radix(left, 16).ok()?;
    let right = u64::from_str_radix(right, 16).ok()?;
    Some((left ^ right).count_ones())
}

fn color_distance(left: &[u8], right: &[u8]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 1.0;
    }
    let sum = left
        .iter()
        .zip(right)
        .map(|(a, b)| (i16::from(*a) - i16::from(*b)).unsigned_abs() as f64)
        .sum::<f64>();
    (sum / (left.len() as f64 * 255.0)).clamp(0.0, 1.0)
}

fn cosine_similarity(left: &[i8], right: &[i8]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let (dot, left_norm, right_norm) =
        left.iter()
            .zip(right)
            .fold((0.0, 0.0, 0.0), |(dot, left_norm, right_norm), (a, b)| {
                let a = f64::from(*a);
                let b = f64::from(*b);
                (dot + a * b, left_norm + a * a, right_norm + b * b)
            });
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return 0.5;
    }
    ((dot / (left_norm.sqrt() * right_norm.sqrt()) + 1.0) / 2.0).clamp(0.0, 1.0)
}

pub fn technical_evidence(image: &AnalysisImage) -> TechnicalEvidence {
    // Restrict deterministic metrics to a bounded working raster. This prevents a 60 MP source
    // from making the first baseline consume a disproportionate amount of memory or CPU.
    let (width, height) = working_dimensions(image.width, image.height, 512);
    let mut luminance = Vec::with_capacity(width * height);
    let mut highlights = 0_u64;
    let mut shadows = 0_u64;
    let mut clipped_channels = 0_u64;
    for y in 0..height {
        for x in 0..width {
            let value = image.sampled_luminance(x, y, width, height);
            let [r, g, b] = image.sampled_rgb(x, y, width, height);
            if value >= 0.98 {
                highlights += 1;
            }
            if value <= 0.02 {
                shadows += 1;
            }
            clipped_channels += [r, g, b]
                .into_iter()
                .filter(|value| *value <= 2 || *value >= 253)
                .count() as u64;
            luminance.push(value);
        }
    }
    let count = luminance.len().max(1) as f64;
    let mean_luminance = luminance.iter().sum::<f64>() / count;
    let mut sorted = luminance.clone();
    sorted.sort_by(f64::total_cmp);
    let median_luminance = sorted[sorted.len() / 2];
    let (laplacian_variance, edge_strength, local_high_frequency, directional_ratio) =
        edge_metrics(&luminance, width, height);
    let global_sharpness =
        (laplacian_variance.sqrt() * 2.5 + edge_strength * 0.45 + local_high_frequency * 1.25)
            .clamp(0.0, 100.0);
    let sharpness_band = if global_sharpness >= 62.0 {
        "excellent"
    } else if global_sharpness >= 38.0 {
        "good"
    } else if global_sharpness >= 20.0 {
        "review"
    } else {
        "low_detail"
    }
    .to_owned();
    let blur_level = if global_sharpness >= 42.0 {
        BlurEvidenceLevel::Low
    } else if directional_ratio < 0.32 && global_sharpness < 20.0 {
        BlurEvidenceLevel::High
    } else if directional_ratio < 0.52 && global_sharpness < 34.0 {
        BlurEvidenceLevel::Moderate
    } else {
        // A low-detail frame can be an intentional shallow-focus or low-light image. The first
        // baseline reports uncertainty rather than declaring a motion-blur cause.
        BlurEvidenceLevel::Uncertain
    };
    let highlight_clipping_percent = highlights as f64 * 100.0 / count;
    let shadow_clipping_percent = shadows as f64 * 100.0 / count;
    let channel_clipping_percent = clipped_channels as f64 * 100.0 / (count * 3.0);
    let exposure_penalty = severe_clipping_penalty(
        highlight_clipping_percent,
        shadow_clipping_percent,
        channel_clipping_percent,
    );
    let blur_penalty = match blur_level {
        BlurEvidenceLevel::High => 28.0,
        BlurEvidenceLevel::Moderate => 13.0,
        BlurEvidenceLevel::Uncertain if global_sharpness < 18.0 => 8.0,
        _ => 0.0,
    };
    let technical_quality_score =
        (global_sharpness * 0.72 + 28.0 - exposure_penalty - blur_penalty).clamp(0.0, 100.0);
    let technical_quality_band = if technical_quality_score >= 76.0
        && highlight_clipping_percent < 5.0
        && shadow_clipping_percent < 12.0
    {
        TechnicalQualityBand::Strong
    } else if technical_quality_score >= 54.0 {
        TechnicalQualityBand::Good
    } else if technical_quality_score >= 30.0 {
        TechnicalQualityBand::Review
    } else {
        TechnicalQualityBand::TechnicalIssue
    };
    let confidence = if width >= 128 && height >= 128 {
        0.82
    } else {
        0.55
    };
    TechnicalEvidence {
        global_sharpness,
        sharpness_band,
        laplacian_variance,
        edge_strength,
        local_high_frequency,
        directional_blur_ratio: directional_ratio,
        blur_level,
        mean_luminance,
        median_luminance,
        highlight_clipping_percent,
        shadow_clipping_percent,
        channel_clipping_percent,
        technical_quality_score,
        technical_quality_band,
        confidence,
    }
}

fn working_dimensions(width: u32, height: u32, maximum_edge: usize) -> (usize, usize) {
    let width = width.max(1) as f64;
    let height = height.max(1) as f64;
    let scale = (maximum_edge as f64 / width.max(height)).min(1.0);
    (
        (width * scale).round().max(1.0) as usize,
        (height * scale).round().max(1.0) as usize,
    )
}

fn edge_metrics(values: &[f64], width: usize, height: usize) -> (f64, f64, f64, f64) {
    if width < 3 || height < 3 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    let mut laplacians = Vec::with_capacity((width - 2) * (height - 2));
    let mut edge_sum = 0.0;
    let mut local_frequency = 0.0;
    let mut horizontal_energy = 0.0;
    let mut vertical_energy = 0.0;
    for y in 1..(height - 1) {
        for x in 1..(width - 1) {
            let center = values[y * width + x];
            let left = values[y * width + x - 1];
            let right = values[y * width + x + 1];
            let above = values[(y - 1) * width + x];
            let below = values[(y + 1) * width + x];
            let laplacian = 4.0 * center - left - right - above - below;
            laplacians.push(laplacian);
            local_frequency += laplacian.abs();
            let gradient_x = right - left;
            let gradient_y = below - above;
            horizontal_energy += gradient_x.abs();
            vertical_energy += gradient_y.abs();
            edge_sum += (gradient_x * gradient_x + gradient_y * gradient_y).sqrt();
        }
    }
    let count = laplacians.len().max(1) as f64;
    let mean = laplacians.iter().sum::<f64>() / count;
    let variance = laplacians
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / count;
    let directional_ratio = horizontal_energy.min(vertical_energy)
        / horizontal_energy.max(vertical_energy).max(f64::EPSILON);
    (
        variance * 10_000.0,
        edge_sum * 100.0 / count,
        local_frequency * 100.0 / count,
        directional_ratio,
    )
}

fn severe_clipping_penalty(highlights: f64, shadows: f64, channels: f64) -> f64 {
    let highlight_penalty = (highlights - 3.0).max(0.0) * 1.7;
    let shadow_penalty = (shadows - 8.0).max(0.0) * 0.7;
    let channel_penalty = (channels - 5.0).max(0.0) * 0.45;
    (highlight_penalty + shadow_penalty + channel_penalty).min(52.0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceObservation {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub detection_confidence: f64,
    pub pose: Option<String>,
    pub eye_state: EyeState,
    pub eye_confidence: Option<f64>,
    pub face_sharpness: Option<f64>,
}

/// Stable provenance for a face provider. `provider_version` is deliberately separate from the
/// deterministic image analyzer version: platform face capability changes must not be hidden by
/// a still-valid technical/similarity artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceProviderIdentity {
    pub provider: String,
    pub provider_version: String,
    /// Opaque, deterministic identity intended for face-artifact cache/staleness keys. It is
    /// derived from provider/version rather than a customer path, image, or machine name.
    pub cache_identity: String,
}

impl FaceProviderIdentity {
    pub fn new(provider: impl Into<String>, provider_version: impl Into<String>) -> Self {
        let provider = provider.into();
        let provider_version = provider_version.into();
        let cache_identity = face_provider_cache_identity(&provider, &provider_version);
        Self {
            provider,
            provider_version,
            cache_identity,
        }
    }
}

/// Returns a provider-only cache identity. Core/persistence can keep this alongside face
/// evidence so a provider or host-capability upgrade invalidates face results independently of
/// the deterministic descriptor/technical artifact.
pub fn face_provider_cache_identity(provider: &str, provider_version: &str) -> String {
    let mut hasher = Hasher::new();
    for part in [
        FACE_PROVIDER_CACHE_IDENTITY_VERSION,
        provider,
        provider_version,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    format!("face-provider-v1:{}", hasher.finalize().to_hex())
}

/// Returns a face-artifact input identity. It combines the image preview input with the face
/// provider identity and face settings only; changing a deterministic analyzer implementation
/// does not accidentally make this face evidence current or stale.
pub fn face_analysis_input_fingerprint(
    input_fingerprint: &str,
    identity: &FaceProviderIdentity,
) -> String {
    analysis_cache_key(
        input_fingerprint,
        &identity.cache_identity,
        FACE_ANALYSIS_INPUT_IDENTITY_VERSION,
        FACE_ANALYSIS_SETTINGS_VERSION,
    )
}

/// Identity for the cacheable local face-detection chain. It is deliberately independent of a
/// particular native provider: Apple Vision can fail on one host and the static local fallback
/// can still produce a valid rectangle without invalidating unrelated technical evidence.
pub fn platform_face_provider_identity() -> FaceProviderIdentity {
    FaceProviderIdentity::new(LOCAL_FACE_DETECTION_PROVIDER, LOCAL_FACE_DETECTION_VERSION)
}

pub fn unavailable_face_provider_identity() -> FaceProviderIdentity {
    FaceProviderIdentity::new(UNAVAILABLE_FACE_PROVIDER, UNAVAILABLE_FACE_PROVIDER_VERSION)
}

#[cfg(target_os = "macos")]
fn macos_face_provider_identity() -> FaceProviderIdentity {
    let product_version = macos_product_version();
    FaceProviderIdentity::new(
        APPLE_VISION_FACE_PROVIDER,
        macos_face_provider_version(&product_version, std::env::consts::ARCH),
    )
}

#[cfg(any(target_os = "macos", test))]
fn macos_face_provider_version(product_version: &str, architecture: &str) -> String {
    format!(
        "{APPLE_VISION_ADAPTER_VERSION};platform=macos;arch={architecture};os={product_version};vision-request-revision={MACOS_VISION_REQUEST_REVISION_UNAVAILABLE}",
    )
}

#[cfg(target_os = "macos")]
fn macos_product_version() -> String {
    static PRODUCT_VERSION: OnceLock<String> = OnceLock::new();
    PRODUCT_VERSION
        .get_or_init(|| {
            let output = bounded_command(Command::new("/usr/bin/sw_vers").arg("-productVersion"));
            let Ok(output) = output else {
                return MACOS_PRODUCT_VERSION_UNAVAILABLE.into();
            };
            if !output.success {
                return MACOS_PRODUCT_VERSION_UNAVAILABLE.into();
            }
            normalize_macos_product_version(&String::from_utf8_lossy(&output.stdout))
                .unwrap_or_else(|| MACOS_PRODUCT_VERSION_UNAVAILABLE.into())
        })
        .clone()
}

/// `sw_vers` is a fixed OS executable, but its output is still treated as untrusted text before
/// it becomes persisted provenance. Keep only a short dotted numeric version.
#[cfg(any(target_os = "macos", test))]
fn normalize_macos_product_version(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 32 || value.split('.').count() > 4 {
        return None;
    }
    value
        .split('.')
        .all(|part| {
            !part.is_empty() && part.len() <= 4 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
        .then(|| value.to_owned())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceProviderResult {
    /// Stable CaptureOS chain identity used for cache validity.
    pub provider: String,
    pub provider_version: String,
    /// The provider that produced the final usable face rectangles. This never names a person
    /// and is diagnostic provenance only.
    pub resolved_provider: String,
    pub resolved_provider_version: String,
    pub status: AnalysisStatus,
    pub faces: Vec<FaceObservation>,
    /// A failed earlier provider attempt, retained for Developer Details only. It must not
    /// change `status` when `faces` came from a later successful fallback.
    pub provider_attempt_error: Option<String>,
    /// Rectangle detection is independently useful. Landmark and eye analysis are optional and
    /// cannot erase an otherwise ready face result.
    pub landmark_status: AnalysisStatus,
    pub landmark_error_message: Option<String>,
    pub error_message: Option<String>,
}

impl FaceProviderResult {
    pub fn identity(&self) -> FaceProviderIdentity {
        FaceProviderIdentity::new(self.provider.clone(), self.provider_version.clone())
    }

    pub fn analysis_input_fingerprint(&self, input_fingerprint: &str) -> String {
        face_analysis_input_fingerprint(input_fingerprint, &self.identity())
    }
}

pub trait FaceDetector {
    fn detect(&self, preview_path: &Path, image: &AnalysisImage) -> FaceProviderResult;
}

/// Honest fallback used where the local platform capability is not installed. It deliberately
/// makes no claim about face count or eyes.
pub struct UnavailableFaceDetector;

impl FaceDetector for UnavailableFaceDetector {
    fn detect(&self, _preview_path: &Path, _image: &AnalysisImage) -> FaceProviderResult {
        let identity = unavailable_face_provider_identity();
        FaceProviderResult {
            provider: identity.provider,
            provider_version: identity.provider_version,
            resolved_provider: UNAVAILABLE_FACE_PROVIDER.into(),
            resolved_provider_version: UNAVAILABLE_FACE_PROVIDER_VERSION.into(),
            status: AnalysisStatus::NotApplicable,
            faces: Vec::new(),
            provider_attempt_error: None,
            landmark_status: AnalysisStatus::NotApplicable,
            landmark_error_message: Some(
                "No local landmark provider is enabled; eye state is not analyzable".into(),
            ),
            error_message: Some(
                "Face analysis unavailable: no approved local face provider is installed".into(),
            ),
        }
    }
}

pub struct PlatformFaceDetector;

impl FaceDetector for PlatformFaceDetector {
    fn detect(&self, preview_path: &Path, image: &AnalysisImage) -> FaceProviderResult {
        platform_face_detection(preview_path, image)
    }
}

#[cfg(target_os = "macos")]
fn platform_face_detection(preview_path: &Path, image: &AnalysisImage) -> FaceProviderResult {
    // Vision's service-backed request is serialized because it can use an XPC-backed native
    // session. A valid Vision response wins, including a genuine zero-face photograph.
    let vision_attempt = (|| -> Result<Vec<FaceObservation>, IntelligenceError> {
        let _vision_guard = macos_vision_request_lock().lock().map_err(|_| {
            IntelligenceError::Provider(
                "macOS Vision request lock was unexpectedly poisoned".into(),
            )
        })?;
        let raw = native_vision_rectangles_with_transient_retry(preview_path)?;
        if raw.status != "ready" {
            return Err(IntelligenceError::Provider(raw.error.unwrap_or_else(
                || "Apple Vision returned a non-ready rectangle response".into(),
            )));
        }
        Ok(raw
            .faces
            .into_iter()
            .filter_map(vision_face_observation)
            .collect())
    })();
    match vision_attempt {
        Ok(faces) => face_detection_ready(macos_face_provider_identity(), faces, None),
        Err(vision_error) => local_ultraface_result(image, Some(vision_error.to_string())),
    }
}

#[cfg(target_os = "macos")]
fn macos_vision_request_lock() -> &'static Mutex<()> {
    static REQUEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    REQUEST_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn captureos_macos_detect_face_rectangles(
        utf8_path: *const c_char,
        out_error: *mut *mut c_char,
    ) -> *mut c_char;
    fn captureos_macos_free_string(value: *mut c_char);
}

#[cfg(target_os = "macos")]
fn native_vision_rectangles_with_transient_retry(
    preview_path: &Path,
) -> Result<VisionResultEnvelope, IntelligenceError> {
    for attempt in 0..2 {
        match native_vision_rectangles(preview_path) {
            Ok(raw) => return Ok(raw),
            Err(error) if attempt == 0 && vision_error_text_is_transient(&error.to_string()) => {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("a bounded Vision retry always returns or propagates an error")
}

#[cfg(target_os = "macos")]
fn native_vision_rectangles(
    preview_path: &Path,
) -> Result<VisionResultEnvelope, IntelligenceError> {
    let path = preview_path.to_str().ok_or_else(|| {
        IntelligenceError::Provider("input_error: analysis preview path is not valid UTF-8".into())
    })?;
    let path = CString::new(path).map_err(|_| {
        IntelligenceError::Provider("input_error: analysis preview path contains a NUL byte".into())
    })?;
    let mut native_error = ptr::null_mut();
    // The bridge receives a single owned UTF-8 path, performs Vision in an autorelease pool, and
    // returns an owned JSON string. No UI-thread object or source-media path crosses this FFI.
    let response =
        unsafe { captureos_macos_detect_face_rectangles(path.as_ptr(), &mut native_error) };
    let response_text = unsafe { take_native_string(response) };
    let error_text = unsafe { take_native_string(native_error) };
    let response_text = response_text.ok_or_else(|| {
        IntelligenceError::Provider(format!(
            "provider_error: {}",
            error_text.unwrap_or_else(|| "native Vision returned no response".into())
        ))
    })?;
    serde_json::from_str(&response_text).map_err(IntelligenceError::Serialization)
}

#[cfg(target_os = "macos")]
unsafe fn take_native_string(value: *mut c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { captureos_macos_free_string(value) };
    Some(text)
}

#[cfg(target_os = "macos")]
fn vision_error_text_is_transient(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("connection invalid")
        || message.contains("xpc")
        || (message.contains("unexpected condition") && message.contains("unspecified error"))
}

#[cfg(not(target_os = "macos"))]
fn platform_face_detection(_preview_path: &Path, image: &AnalysisImage) -> FaceProviderResult {
    local_ultraface_result(image, None)
}

fn face_detection_ready(
    resolved_identity: FaceProviderIdentity,
    faces: Vec<FaceObservation>,
    provider_attempt_error: Option<String>,
) -> FaceProviderResult {
    let identity = platform_face_provider_identity();
    FaceProviderResult {
        provider: identity.provider,
        provider_version: identity.provider_version,
        resolved_provider: resolved_identity.provider,
        resolved_provider_version: resolved_identity.provider_version,
        status: AnalysisStatus::Ready,
        faces,
        provider_attempt_error,
        landmark_status: AnalysisStatus::NotApplicable,
        landmark_error_message: Some(
            "Face rectangles are ready. No landmark provider is enabled, so eye state is not analyzable."
                .into(),
        ),
        error_message: None,
    }
}

fn local_ultraface_result(
    image: &AnalysisImage,
    provider_attempt_error: Option<String>,
) -> FaceProviderResult {
    match ultraface_detect(image) {
        Ok(faces) => face_detection_ready(
            FaceProviderIdentity::new(ULTRAFACE_FACE_PROVIDER, ULTRAFACE_FACE_PROVIDER_VERSION),
            faces,
            provider_attempt_error,
        ),
        Err(error) => face_detection_failed(
            FaceProviderIdentity::new(ULTRAFACE_FACE_PROVIDER, ULTRAFACE_FACE_PROVIDER_VERSION),
            error.status(),
            error.to_string(),
            provider_attempt_error,
        ),
    }
}

fn face_detection_failed(
    resolved_identity: FaceProviderIdentity,
    status: AnalysisStatus,
    error_message: String,
    provider_attempt_error: Option<String>,
) -> FaceProviderResult {
    let identity = platform_face_provider_identity();
    FaceProviderResult {
        provider: identity.provider,
        provider_version: identity.provider_version,
        resolved_provider: resolved_identity.provider,
        resolved_provider_version: resolved_identity.provider_version,
        status,
        faces: Vec::new(),
        provider_attempt_error,
        landmark_status: AnalysisStatus::NotApplicable,
        landmark_error_message: Some(
            "No landmark provider is enabled, so eye state is not analyzable.".into(),
        ),
        error_message: Some(error_message),
    }
}

type UltraFaceRunnableModel = TypedRunnableModel<TypedModel>;

fn ultraface_model() -> Result<&'static Mutex<UltraFaceRunnableModel>, IntelligenceError> {
    static MODEL: OnceLock<Result<Mutex<UltraFaceRunnableModel>, String>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            tract_onnx::onnx()
                .model_for_read(&mut &ULTRAFACE_MODEL_BYTES[..])
                .and_then(|model| model.into_optimized())
                .and_then(|model| model.into_runnable())
                .map(Mutex::new)
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| {
            IntelligenceError::Provider(format!("UltraFace model initialization failed: {error}"))
        })
}

fn ultraface_detect(image: &AnalysisImage) -> Result<Vec<FaceObservation>, IntelligenceError> {
    let input = Tensor::from_shape(
        &[1, 3, ULTRAFACE_INPUT_HEIGHT, ULTRAFACE_INPUT_WIDTH],
        &ultraface_input(image),
    )
    .map_err(|error| {
        IntelligenceError::Provider(format!("UltraFace input creation failed: {error}"))
    })?;
    let model = ultraface_model()?;
    let outputs = model
        .lock()
        .map_err(|_| {
            IntelligenceError::Provider("UltraFace inference lock was unexpectedly poisoned".into())
        })?
        .run(TVec::from_vec(vec![input.into()]))
        .map_err(|error| {
            IntelligenceError::Provider(format!("UltraFace inference failed: {error}"))
        })?;
    if outputs.len() != 2 {
        return Err(IntelligenceError::Provider(format!(
            "UltraFace returned {} outputs; expected scores and boxes",
            outputs.len()
        )));
    }
    let scores = outputs[0].as_slice::<f32>().map_err(|error| {
        IntelligenceError::Provider(format!("UltraFace scores were invalid: {error}"))
    })?;
    let boxes = outputs[1].as_slice::<f32>().map_err(|error| {
        IntelligenceError::Provider(format!("UltraFace boxes were invalid: {error}"))
    })?;
    ultraface_faces_from_outputs(scores, boxes)
}

fn ultraface_faces_from_outputs(
    scores: &[f32],
    boxes: &[f32],
) -> Result<Vec<FaceObservation>, IntelligenceError> {
    if !scores.len().is_multiple_of(2) || boxes.len() != scores.len() / 2 * 4 {
        return Err(IntelligenceError::Provider(format!(
            "UltraFace output dimensions were unexpected: {} score values and {} box values",
            scores.len(),
            boxes.len()
        )));
    }
    let mut candidates = Vec::new();
    for index in 0..scores.len() / 2 {
        let confidence = scores[index * 2 + 1];
        let box_start = index * 4;
        let [left, top, right, bottom] = [
            boxes[box_start],
            boxes[box_start + 1],
            boxes[box_start + 2],
            boxes[box_start + 3],
        ];
        if !confidence.is_finite()
            || confidence < ULTRAFACE_FACE_CONFIDENCE_THRESHOLD
            || [left, top, right, bottom]
                .iter()
                .any(|value| !value.is_finite())
        {
            continue;
        }
        let left = f64::from(left).clamp(0.0, 1.0);
        let top = f64::from(top).clamp(0.0, 1.0);
        let right = f64::from(right).clamp(0.0, 1.0);
        let bottom = f64::from(bottom).clamp(0.0, 1.0);
        if right <= left
            || bottom <= top
            || (right - left) * (bottom - top) < ULTRAFACE_MIN_FACE_RELATIVE_AREA
        {
            continue;
        }
        candidates.push(FaceCandidate {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
            confidence: f64::from(confidence).clamp(0.0, 1.0),
        });
    }
    candidates.sort_by(|left, right| right.confidence.total_cmp(&left.confidence));
    let mut accepted = Vec::new();
    for candidate in candidates {
        if accepted
            .iter()
            .all(|existing| face_iou(existing, &candidate) <= ULTRAFACE_NMS_IOU_THRESHOLD)
        {
            accepted.push(candidate);
        }
    }
    Ok(accepted
        .into_iter()
        .map(|face| FaceObservation {
            x: face.x,
            y: face.y,
            width: face.width,
            height: face.height,
            detection_confidence: face.confidence,
            pose: None,
            eye_state: EyeState::NotAnalyzable,
            eye_confidence: None,
            face_sharpness: None,
        })
        .collect())
}

#[derive(Debug, Clone, Copy)]
struct FaceCandidate {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    confidence: f64,
}

fn face_iou(left: &FaceCandidate, right: &FaceCandidate) -> f64 {
    let x_overlap = (left.x + left.width).min(right.x + right.width) - left.x.max(right.x);
    let y_overlap = (left.y + left.height).min(right.y + right.height) - left.y.max(right.y);
    let intersection = x_overlap.max(0.0) * y_overlap.max(0.0);
    let union = left.width * left.height + right.width * right.height - intersection;
    if union > 0.0 {
        intersection / union
    } else {
        0.0
    }
}

fn ultraface_input(image: &AnalysisImage) -> Vec<f32> {
    let pixels = ULTRAFACE_INPUT_WIDTH * ULTRAFACE_INPUT_HEIGHT;
    let mut channels = vec![0.0; pixels * 3];
    for y in 0..ULTRAFACE_INPUT_HEIGHT {
        for x in 0..ULTRAFACE_INPUT_WIDTH {
            let source_x =
                (x as f64 + 0.5) * f64::from(image.width) / ULTRAFACE_INPUT_WIDTH as f64 - 0.5;
            let source_y =
                (y as f64 + 0.5) * f64::from(image.height) / ULTRAFACE_INPUT_HEIGHT as f64 - 0.5;
            let rgb = bilinear_rgb(image, source_x, source_y);
            let destination = y * ULTRAFACE_INPUT_WIDTH + x;
            for channel in 0..3 {
                channels[channel * pixels + destination] =
                    (f32::from(rgb[channel]) - 127.0) / 128.0;
            }
        }
    }
    channels
}

fn bilinear_rgb(image: &AnalysisImage, x: f64, y: f64) -> [u8; 3] {
    let x0 = x
        .floor()
        .clamp(0.0, f64::from(image.width.saturating_sub(1))) as u32;
    let y0 = y
        .floor()
        .clamp(0.0, f64::from(image.height.saturating_sub(1))) as u32;
    let x1 = (x0 + 1).min(image.width.saturating_sub(1));
    let y1 = (y0 + 1).min(image.height.saturating_sub(1));
    let x_weight = (x - x.floor()).clamp(0.0, 1.0);
    let y_weight = (y - y.floor()).clamp(0.0, 1.0);
    let top_left = image.pixel(x0, y0);
    let top_right = image.pixel(x1, y0);
    let bottom_left = image.pixel(x0, y1);
    let bottom_right = image.pixel(x1, y1);
    std::array::from_fn(|channel| {
        let top = f64::from(top_left[channel]) * (1.0 - x_weight)
            + f64::from(top_right[channel]) * x_weight;
        let bottom = f64::from(bottom_left[channel]) * (1.0 - x_weight)
            + f64::from(bottom_right[channel]) * x_weight;
        (top * (1.0 - y_weight) + bottom * y_weight)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisionResultEnvelope {
    status: String,
    faces: Vec<VisionFace>,
    error: Option<String>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisionFace {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    detection_confidence: f64,
    pose: Option<String>,
    eye_state: String,
    eye_confidence: Option<f64>,
}

#[cfg(target_os = "macos")]
fn vision_face_observation(face: VisionFace) -> Option<FaceObservation> {
    let ranges = [
        face.x,
        face.y,
        face.width,
        face.height,
        face.detection_confidence,
    ];
    if ranges.iter().any(|value| !value.is_finite())
        || face.x < 0.0
        || face.y < 0.0
        || face.width <= 0.0
        || face.height <= 0.0
        || face.x + face.width > 1.0 + f64::EPSILON
        || face.y + face.height > 1.0 + f64::EPSILON
    {
        return None;
    }
    let eye_state = match face.eye_state.as_str() {
        "open" => EyeState::Open,
        "closed" => EyeState::Closed,
        "uncertain" => EyeState::Uncertain,
        _ => EyeState::NotAnalyzable,
    };
    Some(FaceObservation {
        x: face.x,
        y: face.y,
        width: face.width,
        height: face.height,
        detection_confidence: face.detection_confidence.clamp(0.0, 1.0),
        pose: face.pose,
        eye_state,
        eye_confidence: face.eye_confidence.map(|value| value.clamp(0.0, 1.0)),
        face_sharpness: None,
    })
}

/// A completed per-image analysis result before it is persisted. It deliberately retains status
/// and errors per provider so an unavailable face adapter cannot invalidate real deterministic
/// exposure or sharpness evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageAnalysisResult {
    pub status: AnalysisStatus,
    pub fingerprint: Option<FingerprintEvidence>,
    pub technical: Option<TechnicalEvidence>,
    pub faces: FaceProviderResult,
    pub error_message: Option<String>,
}

pub fn analyze_preview(
    preview_path: &Path,
    decoder: &impl AnalysisImageDecoder,
    image_analyzer: &impl ImageAnalyzer,
    face_detector: &impl FaceDetector,
) -> ImageAnalysisResult {
    let image = match decoder.decode(preview_path) {
        Ok(image) => image,
        Err(error) => {
            let identity = unavailable_face_provider_identity();
            return ImageAnalysisResult {
                status: error.status(),
                fingerprint: None,
                technical: None,
                faces: FaceProviderResult {
                    provider: identity.provider,
                    provider_version: identity.provider_version,
                    resolved_provider: UNAVAILABLE_FACE_PROVIDER.into(),
                    resolved_provider_version: UNAVAILABLE_FACE_PROVIDER_VERSION.into(),
                    status: AnalysisStatus::NotApplicable,
                    faces: Vec::new(),
                    provider_attempt_error: None,
                    landmark_status: AnalysisStatus::NotApplicable,
                    landmark_error_message: Some(
                        "No decoded analysis image is available for landmarks or eye state".into(),
                    ),
                    error_message: Some("No decoded analysis image is available".into()),
                },
                error_message: Some(error.to_string()),
            };
        }
    };
    let fingerprint = image_analyzer.fingerprint(&image);
    let technical = image_analyzer.technical_evidence(&image);
    let mut faces = face_detector.detect(preview_path, &image);
    for face in &mut faces.faces {
        face.face_sharpness = face_sharpness(&image, face);
    }
    ImageAnalysisResult {
        status: AnalysisStatus::Ready,
        fingerprint: Some(fingerprint),
        technical: Some(technical),
        faces,
        error_message: None,
    }
}

pub fn face_sharpness(image: &AnalysisImage, face: &FaceObservation) -> Option<f64> {
    image
        .crop(face.x, face.y, face.width, face.height)
        .map(|crop| technical_evidence(&crop).global_sharpness)
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupingInput {
    pub project_id: String,
    pub asset_id: String,
    /// This must be a full cryptographic content hash. A bounded fast fingerprint is not exact
    /// duplicate proof and must be supplied as `None` here.
    pub verified_content_hash: Option<String>,
    pub captured_at_unix_seconds: Option<i64>,
    pub camera_model: Option<String>,
    pub fingerprint: FingerprintEvidence,
    pub technical_quality_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupedMember {
    pub asset_id: String,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub is_representative: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltSimilarityGroup {
    pub id: SimilarityGroupId,
    pub kind: SimilarityGroupKind,
    pub representative_asset_id: String,
    pub grouping_method: String,
    pub grouping_version: String,
    pub similarity_confidence: f64,
    pub time_proximity_seconds: Option<u64>,
    pub visual_similarity: Option<f64>,
    pub members: Vec<GroupedMember>,
}

/// Local diagnostic evidence for developer logs. It contains no image pixels, source paths, or
/// identity claims: only deterministic candidate-generation and grouping measurements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityDiagnostics {
    pub input_count: usize,
    pub embedding_dimension: Option<usize>,
    pub candidate_strategy: String,
    pub lsh_candidate_pairs: usize,
    pub time_candidate_pairs: usize,
    pub exhaustive_fallback_used: bool,
    pub candidate_pair_count: usize,
    pub pairs_with_time_metadata: usize,
    pub accepted_by_same_camera: usize,
    pub accepted_by_time_proximity: usize,
    pub accepted_edge_count: usize,
    pub perceptual_distance_min: Option<u32>,
    pub perceptual_distance_max: Option<u32>,
    pub perceptual_distance_average: Option<f64>,
    pub visual_similarity_min: Option<f64>,
    pub visual_similarity_max: Option<f64>,
    pub visual_similarity_average: Option<f64>,
    pub near_duplicate_max_phash_distance: u32,
    pub near_duplicate_min_visual_similarity: f64,
    pub similar_max_phash_distance: u32,
    pub similar_min_visual_similarity: f64,
    pub group_count: usize,
    pub groups: Vec<SimilarityGroupDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityGroupDiagnostic {
    pub id: String,
    pub kind: String,
    pub member_asset_ids: Vec<String>,
    pub visual_similarity: Option<f64>,
    pub time_proximity_seconds: Option<u64>,
}

/// Return the deterministic ordering used whenever one asset belongs to more than one related
/// set. Exact identity is stronger evidence than visual similarity; a burst is useful context,
/// but never supersedes duplicate evidence.
///
/// Lower values have higher priority. Keep this ordering in one place so persisted group output
/// and advisory recommendation selection cannot disagree.
pub fn similarity_group_priority(kind: SimilarityGroupKind) -> u8 {
    match kind {
        SimilarityGroupKind::ExactDuplicateSet => 0,
        SimilarityGroupKind::NearDuplicateSet => 1,
        SimilarityGroupKind::Burst => 2,
        SimilarityGroupKind::SimilarSet => 3,
    }
}

fn compare_primary_group_order(
    left: &BuiltSimilarityGroup,
    right: &BuiltSimilarityGroup,
) -> std::cmp::Ordering {
    similarity_group_priority(left.kind)
        .cmp(&similarity_group_priority(right.kind))
        // IDs are derived from stable group membership, so this makes equally ranked groups
        // deterministic without injecting discovery or database insertion order.
        .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
}

/// Resolve the one group that should drive an asset's primary related-set presentation and its
/// advisory recommendation. Other memberships remain persisted and inspectable.
pub fn primary_group_for_asset<'a>(
    groups: &'a [BuiltSimilarityGroup],
    asset_id: &str,
) -> Option<(&'a BuiltSimilarityGroup, &'a GroupedMember)> {
    groups
        .iter()
        .filter_map(|group| {
            group
                .members
                .iter()
                .find(|member| member.asset_id == asset_id)
                .map(|member| (group, member))
        })
        .min_by(|(left, _), (right, _)| compare_primary_group_order(left, right))
}

/// Build group memberships using LSH-like pHash buckets plus bounded time/camera candidates.
/// Small projects additionally receive a bounded exhaustive pass so candidate recall does not
/// depend on exact 16-bit pHash-band matches. Larger projects never run catalog-wide all pairs.
pub fn build_similarity_groups(inputs: &[GroupingInput]) -> Vec<BuiltSimilarityGroup> {
    build_similarity_groups_with_diagnostics(inputs).0
}

/// Builds the same durable groups plus developer-log diagnostics for candidate recall and
/// threshold decisions. This is deliberately local, deterministic, and pixel-free.
pub fn build_similarity_groups_with_diagnostics(
    inputs: &[GroupingInput],
) -> (Vec<BuiltSimilarityGroup>, SimilarityDiagnostics) {
    let mut groups = exact_duplicate_groups(inputs);
    let (visual_groups, mut diagnostics) = visual_similarity_groups_with_diagnostics(inputs);
    groups.extend(visual_groups);
    groups.extend(burst_groups(inputs));
    groups.sort_by(compare_primary_group_order);
    diagnostics.group_count = groups.len();
    diagnostics.groups = groups
        .iter()
        .map(|group| SimilarityGroupDiagnostic {
            id: group.id.to_string(),
            kind: group.kind.as_str().into(),
            member_asset_ids: group
                .members
                .iter()
                .map(|member| member.asset_id.clone())
                .collect(),
            visual_similarity: group.visual_similarity,
            time_proximity_seconds: group.time_proximity_seconds,
        })
        .collect();
    (groups, diagnostics)
}

fn exact_duplicate_groups(inputs: &[GroupingInput]) -> Vec<BuiltSimilarityGroup> {
    let mut by_hash: BTreeMap<&str, Vec<&GroupingInput>> = BTreeMap::new();
    for input in inputs {
        if let Some(hash) = input
            .verified_content_hash
            .as_deref()
            .filter(|hash| !hash.is_empty())
        {
            by_hash.entry(hash).or_default().push(input);
        }
    }
    by_hash
        .into_values()
        .filter(|members| members.len() >= 2)
        .map(|members| {
            built_group(
                SimilarityGroupKind::ExactDuplicateSet,
                "verified-content-hash",
                members,
                1.0,
                None,
                Some(1.0),
            )
        })
        .collect()
}

fn visual_similarity_groups_with_diagnostics(
    inputs: &[GroupingInput],
) -> (Vec<BuiltSimilarityGroup>, SimilarityDiagnostics) {
    let mut lsh_buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut time_buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        for bucket in &input.fingerprint.bucket_keys {
            lsh_buckets.entry(bucket.clone()).or_default().push(index);
        }
        // Same-camera temporal windows provide candidate recall when a small crop changes pHash
        // bands. They are only candidates; visual evidence still gates every connection.
        if let (Some(camera), Some(captured_at)) = (
            input
                .camera_model
                .as_deref()
                .filter(|value| !value.is_empty()),
            input.captured_at_unix_seconds,
        ) {
            time_buckets
                .entry(format!("time:{camera}:{}", captured_at.div_euclid(12)))
                .or_default()
                .push(index);
        }
    }
    let lsh_candidates = bounded_bucket_pairs(lsh_buckets);
    let time_candidates = bounded_bucket_pairs(time_buckets);
    let exhaustive_fallback_used = inputs.len() <= SMALL_PROJECT_EXHAUSTIVE_CANDIDATE_LIMIT;
    let mut candidates = lsh_candidates.clone();
    candidates.extend(time_candidates.iter().copied());
    if exhaustive_fallback_used {
        for left in 0..inputs.len() {
            for right in (left + 1)..inputs.len() {
                candidates.insert((left, right));
            }
        }
    }
    let mut union = UnionFind::new(inputs.len());
    let mut edges: HashMap<(usize, usize), SimilarityEvidence> = HashMap::new();
    let mut perceptual_distances = Vec::with_capacity(candidates.len());
    let mut visual_similarities = Vec::with_capacity(candidates.len());
    let mut pairs_with_time_metadata = 0;
    let mut accepted_by_same_camera = 0;
    let mut accepted_by_time_proximity = 0;
    for (left, right) in candidates.iter().copied() {
        let a = &inputs[left];
        let b = &inputs[right];
        if a.project_id != b.project_id {
            continue;
        }
        let evidence = similarity_evidence(&a.fingerprint, &b.fingerprint);
        let time_gap = time_gap_seconds(a.captured_at_unix_seconds, b.captured_at_unix_seconds);
        if time_gap.is_some() {
            pairs_with_time_metadata += 1;
        }
        let same_camera = a.camera_model.is_some()
            && a.camera_model == b.camera_model
            && a.camera_model
                .as_deref()
                .is_some_and(|value| !value.is_empty());
        perceptual_distances.push(evidence.perceptual_distance);
        visual_similarities.push(evidence.visual_similarity);
        let near_duplicate = evidence.perceptual_distance <= NEAR_DUPLICATE_MAX_PHASH_DISTANCE
            && evidence.visual_similarity >= NEAR_DUPLICATE_MIN_VISUAL_SIMILARITY;
        let similar = evidence.perceptual_distance <= SIMILAR_MAX_PHASH_DISTANCE
            && evidence.visual_similarity >= SIMILAR_MIN_VISUAL_SIMILARITY
            && (same_camera || time_gap.is_some_and(|gap| gap <= 120));
        if near_duplicate || similar {
            if similar {
                if same_camera {
                    accepted_by_same_camera += 1;
                } else if time_gap.is_some_and(|gap| gap <= 120) {
                    accepted_by_time_proximity += 1;
                }
            }
            union.union(left, right);
            edges.insert((left, right), evidence);
        }
    }
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..inputs.len() {
        components.entry(union.find(index)).or_default().push(index);
    }
    let groups = components
        .into_values()
        .filter(|component| component.len() >= 2)
        .map(|component| {
            let members = component
                .iter()
                .map(|index| &inputs[*index])
                .collect::<Vec<_>>();
            let mut evidence = Vec::new();
            for (left_position, left) in component.iter().enumerate() {
                for right in component.iter().skip(left_position + 1) {
                    if let Some(value) = edges.get(&(*left.min(right), *left.max(right))) {
                        evidence.push(value.clone());
                    }
                }
            }
            let average_similarity = average(evidence.iter().map(|value| value.visual_similarity));
            let average_distance = average(
                evidence
                    .iter()
                    .map(|value| value.perceptual_distance as f64),
            );
            let kind = if average_distance <= 10.0 && average_similarity >= 0.86 {
                SimilarityGroupKind::NearDuplicateSet
            } else {
                SimilarityGroupKind::SimilarSet
            };
            let time = group_time_proximity(&members);
            built_group(
                kind,
                "phash-lsh+color+embedding",
                members,
                average_similarity,
                time,
                Some(average_similarity),
            )
        })
        .collect::<Vec<_>>();
    let diagnostics = SimilarityDiagnostics {
        input_count: inputs.len(),
        embedding_dimension: inputs
            .first()
            .map(|input| input.fingerprint.embedding.len()),
        candidate_strategy: if exhaustive_fallback_used {
            format!(
                "phash-lsh(4x16-bit)+camera-time+bounded-exhaustive<= {SMALL_PROJECT_EXHAUSTIVE_CANDIDATE_LIMIT}"
            )
        } else {
            "phash-lsh(4x16-bit)+camera-time".into()
        },
        lsh_candidate_pairs: lsh_candidates.len(),
        time_candidate_pairs: time_candidates.len(),
        exhaustive_fallback_used,
        candidate_pair_count: candidates.len(),
        pairs_with_time_metadata,
        accepted_by_same_camera,
        accepted_by_time_proximity,
        accepted_edge_count: edges.len(),
        perceptual_distance_min: perceptual_distances.iter().copied().min(),
        perceptual_distance_max: perceptual_distances.iter().copied().max(),
        perceptual_distance_average: (!perceptual_distances.is_empty()).then(|| {
            perceptual_distances
                .iter()
                .map(|value| f64::from(*value))
                .sum::<f64>()
                / perceptual_distances.len() as f64
        }),
        visual_similarity_min: visual_similarities.iter().copied().reduce(f64::min),
        visual_similarity_max: visual_similarities.iter().copied().reduce(f64::max),
        visual_similarity_average: (!visual_similarities.is_empty())
            .then(|| average(visual_similarities.iter().copied())),
        near_duplicate_max_phash_distance: NEAR_DUPLICATE_MAX_PHASH_DISTANCE,
        near_duplicate_min_visual_similarity: NEAR_DUPLICATE_MIN_VISUAL_SIMILARITY,
        similar_max_phash_distance: SIMILAR_MAX_PHASH_DISTANCE,
        similar_min_visual_similarity: SIMILAR_MIN_VISUAL_SIMILARITY,
        group_count: 0,
        groups: Vec::new(),
    };
    (groups, diagnostics)
}

fn bounded_bucket_pairs(buckets: BTreeMap<String, Vec<usize>>) -> BTreeSet<(usize, usize)> {
    let mut pairs = BTreeSet::new();
    for bucket in buckets.into_values() {
        if bucket.len() > MAX_BUCKET_MEMBERS {
            continue;
        }
        for (position, left) in bucket.iter().enumerate() {
            for right in bucket.iter().skip(position + 1) {
                pairs.insert((*left.min(right), *left.max(right)));
            }
        }
    }
    pairs
}

fn burst_groups(inputs: &[GroupingInput]) -> Vec<BuiltSimilarityGroup> {
    let mut buckets: BTreeMap<(String, i64), Vec<&GroupingInput>> = BTreeMap::new();
    for input in inputs {
        if let (Some(camera), Some(captured_at)) = (
            input
                .camera_model
                .as_deref()
                .filter(|value| !value.is_empty()),
            input.captured_at_unix_seconds,
        ) {
            buckets
                .entry((camera.to_owned(), captured_at.div_euclid(10)))
                .or_default()
                .push(input);
        }
    }
    buckets
        .into_values()
        .filter(|members| members.len() >= 3 && members.len() <= MAX_BUCKET_MEMBERS)
        .filter_map(|members| {
            let mut similarities = Vec::new();
            for (position, left) in members.iter().enumerate() {
                for right in members.iter().skip(position + 1) {
                    similarities.push(
                        similarity_evidence(&left.fingerprint, &right.fingerprint)
                            .visual_similarity,
                    );
                }
            }
            let visual = average(similarities.into_iter());
            let time = group_time_proximity(&members);
            (visual >= 0.78 && time.is_some_and(|seconds| seconds <= 12)).then(|| {
                built_group(
                    SimilarityGroupKind::Burst,
                    "camera+time+visual-evidence",
                    members,
                    visual,
                    time,
                    Some(visual),
                )
            })
        })
        .collect()
}

fn built_group(
    kind: SimilarityGroupKind,
    method: &str,
    mut inputs: Vec<&GroupingInput>,
    confidence: f64,
    time_proximity_seconds: Option<u64>,
    visual_similarity: Option<f64>,
) -> BuiltSimilarityGroup {
    inputs.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
    let representative_asset_id = inputs
        .iter()
        .max_by(|left, right| {
            left.technical_quality_score
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&right.technical_quality_score.unwrap_or(f64::NEG_INFINITY))
                .then_with(|| right.asset_id.cmp(&left.asset_id))
        })
        .expect("groups only contain members")
        .asset_id
        .clone();
    let stable_key = format!(
        "{}|{}|{}|{}",
        inputs[0].project_id,
        kind.as_str(),
        DETERMINISTIC_VERSION,
        inputs
            .iter()
            .map(|input| input.asset_id.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let hash = blake3::hash(stable_key.as_bytes());
    let uuid_bytes: [u8; 16] = hash.as_bytes()[..16]
        .try_into()
        .expect("hash prefix is 16 bytes");
    let id = SimilarityGroupId::from_uuid(Uuid::from_bytes(uuid_bytes));
    let members = inputs
        .into_iter()
        .map(|input| GroupedMember {
            asset_id: input.asset_id.clone(),
            similarity_confidence: if input.asset_id == representative_asset_id {
                1.0
            } else {
                confidence
            },
            time_proximity_seconds,
            is_representative: input.asset_id == representative_asset_id,
        })
        .collect();
    BuiltSimilarityGroup {
        id,
        kind,
        representative_asset_id,
        grouping_method: method.into(),
        grouping_version: DETERMINISTIC_VERSION.into(),
        similarity_confidence: confidence.clamp(0.0, 1.0),
        time_proximity_seconds,
        visual_similarity,
        members,
    }
}

fn group_time_proximity(inputs: &[&GroupingInput]) -> Option<u64> {
    let timestamps = inputs
        .iter()
        .filter_map(|input| input.captured_at_unix_seconds)
        .collect::<Vec<_>>();
    if timestamps.len() != inputs.len() {
        return None;
    }
    let min = timestamps.iter().min()?;
    let max = timestamps.iter().max()?;
    Some(max.saturating_sub(*min) as u64)
}

fn time_gap_seconds(left: Option<i64>, right: Option<i64>) -> Option<u64> {
    Some(left?.saturating_sub(right?).unsigned_abs())
}

fn average(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = values.fold((0.0, 0_u64), |(sum, count), value| (sum + value, count + 1));
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

#[derive(Debug)]
struct UnionFind {
    parents: Vec<usize>,
    ranks: Vec<u8>,
}

impl UnionFind {
    fn new(length: usize) -> Self {
        Self {
            parents: (0..length).collect(),
            ranks: vec![0; length],
        }
    }

    fn find(&mut self, value: usize) -> usize {
        if self.parents[value] != value {
            let root = self.find(self.parents[value]);
            self.parents[value] = root;
        }
        self.parents[value]
    }

    fn union(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left == right {
            return;
        }
        if self.ranks[left] < self.ranks[right] {
            self.parents[left] = right;
        } else if self.ranks[left] > self.ranks[right] {
            self.parents[right] = left;
        } else {
            self.parents[right] = left;
            self.ranks[left] += 1;
        }
    }
}

pub trait RecommendationProvider {
    fn provider(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn recommend(&self, input: &RecommendationInput) -> RecommendationEvidence;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecommendationInput {
    pub technical: TechnicalEvidence,
    pub face_count: usize,
    pub open_eyes: usize,
    pub possible_closed_eyes: usize,
    pub group_kind: Option<SimilarityGroupKind>,
    pub is_group_representative: bool,
    pub group_rank: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecommendationEvidence {
    pub label: RecommendationLabel,
    pub confidence: f64,
    pub reasons: Vec<String>,
}

pub struct TechnicalRecommendationEngine;

impl RecommendationProvider for TechnicalRecommendationEngine {
    fn provider(&self) -> &'static str {
        RECOMMENDATION_PROVIDER
    }

    fn version(&self) -> &'static str {
        RECOMMENDATION_VERSION
    }

    fn recommend(&self, input: &RecommendationInput) -> RecommendationEvidence {
        recommend(input)
    }
}

pub fn recommend(input: &RecommendationInput) -> RecommendationEvidence {
    let mut reasons = Vec::new();
    if input.technical.blur_level == BlurEvidenceLevel::High {
        reasons.push("Strong directional low-detail evidence suggests motion blur".into());
    }
    if input.technical.highlight_clipping_percent >= 5.0 {
        reasons.push(format!(
            "Highlights clipped: {:.1}%",
            input.technical.highlight_clipping_percent
        ));
    }
    if input.technical.shadow_clipping_percent >= 14.0 {
        reasons.push(format!(
            "Shadows clipped: {:.1}%",
            input.technical.shadow_clipping_percent
        ));
    }
    if input.possible_closed_eyes > 0 {
        reasons.push(format!(
            "{} analyzable face(s) have possibly closed eyes",
            input.possible_closed_eyes
        ));
    }
    let label = if input.technical.technical_quality_band == TechnicalQualityBand::TechnicalIssue {
        RecommendationLabel::TechnicalIssue
    } else if matches!(
        input.group_kind,
        Some(SimilarityGroupKind::ExactDuplicateSet | SimilarityGroupKind::NearDuplicateSet)
    ) && !input.is_group_representative
    {
        reasons.push("Another member has stronger relative technical evidence".into());
        RecommendationLabel::ProbableDuplicate
    } else if input.possible_closed_eyes > 0 {
        RecommendationLabel::Review
    } else if input.is_group_representative
        && matches!(
            input.technical.technical_quality_band,
            TechnicalQualityBand::Strong | TechnicalQualityBand::Good
        )
    {
        reasons.push(format!(
            "Sharpness evidence: {}",
            input.technical.sharpness_band
        ));
        if input.face_count > 0 {
            reasons.push(format!(
                "{} face(s) detected; {} analyzable face(s) open-eyed",
                input.face_count, input.open_eyes
            ));
        }
        RecommendationLabel::StrongCandidate
    } else if matches!(
        input.technical.technical_quality_band,
        TechnicalQualityBand::Strong | TechnicalQualityBand::Good
    ) {
        reasons.push("Comparable technical evidence within this related set".into());
        RecommendationLabel::StrongAlternative
    } else {
        if reasons.is_empty() {
            reasons.push(
                "Technical evidence is incomplete or mixed; photographer review is appropriate"
                    .into(),
            );
        }
        RecommendationLabel::Review
    };
    let confidence = (input.technical.confidence
        * if input.group_kind.is_some() {
            0.92
        } else {
            0.78
        }
        * if input.possible_closed_eyes > 0 {
            0.88
        } else {
            1.0
        })
    .clamp(0.25, 0.92);
    RecommendationEvidence {
        label,
        confidence,
        reasons,
    }
}

/// Build a compact content identity for analysis caching. The caller supplies a source or preview
/// fingerprint; provider and settings versions make algorithm changes invalidate safely.
pub fn analysis_cache_key(
    input_fingerprint: &str,
    provider: &str,
    provider_version: &str,
    settings_version: &str,
) -> String {
    let mut hasher = Hasher::new();
    for part in [
        input_fingerprint,
        provider,
        provider_version,
        settings_version,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

pub fn generated_at_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticFaceDetector {
        result: FaceProviderResult,
    }

    impl FaceDetector for StaticFaceDetector {
        fn detect(&self, _preview_path: &Path, _image: &AnalysisImage) -> FaceProviderResult {
            self.result.clone()
        }
    }

    fn checkerboard(width: u32, height: u32) -> AnalysisImage {
        let mut image = AnalysisImage::solid(width, height, [0, 0, 0]);
        for y in 0..height {
            for x in 0..width {
                let value = if (((x * 8 / width) + (y * 8 / height)) & 1) == 0 {
                    20
                } else {
                    240
                };
                let offset = ((y * width + x) * 3) as usize;
                image.rgb[offset..offset + 3].fill(value);
            }
        }
        image
    }

    fn directional_blur(image: &AnalysisImage, radius: u32) -> AnalysisImage {
        let mut rgb = vec![0; image.rgb.len()];
        for y in 0..image.height {
            for x in 0..image.width {
                let mut channels = [0_u32; 3];
                let mut samples = 0_u32;
                for offset in -(radius as i32)..=(radius as i32) {
                    let source_x = (x as i32 + offset).clamp(0, image.width as i32 - 1) as u32;
                    let pixel = image.pixel(source_x, y);
                    for channel in 0..3 {
                        channels[channel] += u32::from(pixel[channel]);
                    }
                    samples += 1;
                }
                let target = ((y * image.width + x) * 3) as usize;
                for channel in 0..3 {
                    rgb[target + channel] = (channels[channel] / samples) as u8;
                }
            }
        }
        AnalysisImage::new(image.width, image.height, rgb).unwrap()
    }

    #[test]
    fn ppm_round_trip_preserves_pixels() {
        let image = checkerboard(16, 12);
        assert_eq!(AnalysisImage::from_ppm(&image.ppm_bytes()).unwrap(), image);
    }

    #[test]
    fn perceptual_fingerprint_is_stable_for_a_resize() {
        let image = checkerboard(64, 64);
        let resized = checkerboard(128, 128);
        let first = fingerprint_image(&image);
        let second = fingerprint_image(&resized);
        assert!(
            hamming_distance_hex(&first.perceptual_hash, &second.perceptual_hash).unwrap() <= 4
        );
        assert!(similarity_evidence(&first, &second).visual_similarity >= 0.8);
    }

    #[test]
    fn same_filename_is_not_visual_duplicate_evidence() {
        let dark = AnalysisImage::solid(64, 64, [10, 10, 10]);
        let bright = AnalysisImage::solid(64, 64, [245, 245, 245]);
        let evidence = similarity_evidence(&fingerprint_image(&dark), &fingerprint_image(&bright));
        assert!(evidence.visual_similarity < 0.8);
    }

    #[test]
    fn decoded_metadata_variation_does_not_change_visual_fingerprint() {
        let image = checkerboard(32, 32);
        let plain = image.ppm_bytes();
        let mut with_comment = b"P6\n# synthetic metadata-only variation\n32 32\n255\n".to_vec();
        with_comment.extend_from_slice(image.rgb());
        assert_eq!(
            AnalysisImage::from_ppm(&plain).unwrap(),
            AnalysisImage::from_ppm(&with_comment).unwrap()
        );
        assert_eq!(
            fingerprint_image(&AnalysisImage::from_ppm(&plain).unwrap()),
            fingerprint_image(&AnalysisImage::from_ppm(&with_comment).unwrap())
        );
    }

    #[test]
    fn exact_duplicate_groups_require_verified_content_identity() {
        let visual = fingerprint_image(&checkerboard(64, 64));
        let inputs = [
            GroupingInput {
                project_id: "project".into(),
                asset_id: "different-filename-a".into(),
                verified_content_hash: Some("blake3-verified-same-bytes".into()),
                captured_at_unix_seconds: None,
                camera_model: None,
                fingerprint: visual.clone(),
                technical_quality_score: None,
            },
            GroupingInput {
                project_id: "project".into(),
                asset_id: "different-filename-b".into(),
                verified_content_hash: Some("blake3-verified-same-bytes".into()),
                captured_at_unix_seconds: None,
                camera_model: None,
                fingerprint: visual,
                technical_quality_score: None,
            },
        ];
        let groups = build_similarity_groups(&inputs);
        assert!(groups.iter().any(|group| {
            group.kind == SimilarityGroupKind::ExactDuplicateSet && group.members.len() == 2
        }));
    }

    #[test]
    fn exact_duplicate_is_primary_over_overlapping_near_and_burst_groups() {
        let fingerprint = fingerprint_image(&checkerboard(64, 64));
        let input = |asset_id: &str,
                     content_hash: &str,
                     captured_at_unix_seconds,
                     technical_quality_score| GroupingInput {
            project_id: "project".into(),
            asset_id: asset_id.into(),
            verified_content_hash: Some(content_hash.into()),
            captured_at_unix_seconds: Some(captured_at_unix_seconds),
            camera_model: Some("camera".into()),
            fingerprint: fingerprint.clone(),
            technical_quality_score: Some(technical_quality_score),
        };
        // A/B are byte-identical, while C has different bytes but is visually related and part
        // of the same short camera burst. A is therefore intentionally in multiple groups.
        let inputs = vec![
            input("a", "verified-a-and-b", 100, 80.0),
            input("b", "verified-a-and-b", 102, 90.0),
            input("c", "verified-c", 104, 70.0),
        ];

        let groups = build_similarity_groups(&inputs);
        let kinds = groups.iter().map(|group| group.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                SimilarityGroupKind::ExactDuplicateSet,
                SimilarityGroupKind::NearDuplicateSet,
                SimilarityGroupKind::Burst,
            ]
        );
        let (primary, member) = primary_group_for_asset(&groups, "a").unwrap();
        assert_eq!(primary.kind, SimilarityGroupKind::ExactDuplicateSet);
        assert_eq!(member.asset_id, "a");
        assert!(!member.is_representative);

        // The selected group is the one an advisory recommendation consumes: A is a
        // non-representative exact duplicate, not merely a non-representative burst frame.
        assert_eq!(
            recommend(&RecommendationInput {
                technical: technical_evidence(&checkerboard(64, 64)),
                face_count: 0,
                open_eyes: 0,
                possible_closed_eyes: 0,
                group_kind: Some(primary.kind),
                is_group_representative: member.is_representative,
                group_rank: Some(2),
            })
            .label,
            RecommendationLabel::ProbableDuplicate
        );

        // Selection uses the same priority comparator as output ordering, not an incidental
        // vector order such as database insertion order.
        let mut reverse_order = groups.clone();
        reverse_order.reverse();
        assert_eq!(
            primary_group_for_asset(&reverse_order, "a").unwrap().0.kind,
            SimilarityGroupKind::ExactDuplicateSet
        );
    }

    #[test]
    fn sharpness_and_directional_blur_have_expected_ordering() {
        let sharp = checkerboard(128, 128);
        let blurred = directional_blur(&sharp, 6);
        let sharpness = technical_evidence(&sharp);
        let blur = technical_evidence(&blurred);
        assert!(sharpness.global_sharpness > blur.global_sharpness);
        assert!(sharpness.edge_strength > blur.edge_strength);
    }

    #[test]
    fn exposure_reports_measured_clipping() {
        let black = AnalysisImage::solid(80, 80, [0, 0, 0]);
        let white = AnalysisImage::solid(80, 80, [255, 255, 255]);
        let black_evidence = technical_evidence(&black);
        let white_evidence = technical_evidence(&white);
        assert_eq!(black_evidence.shadow_clipping_percent, 100.0);
        assert_eq!(white_evidence.highlight_clipping_percent, 100.0);
    }

    #[test]
    fn successful_deterministic_analysis_stays_ready_when_faces_are_unavailable() {
        let directory = tempdir().unwrap();
        let preview = directory.path().join("preview.ppm");
        fs::write(&preview, checkerboard(64, 64).ppm_bytes()).unwrap();
        let result = analyze_preview(
            &preview,
            &PpmDecoder,
            &DeterministicImageAnalyzer,
            &UnavailableFaceDetector,
        );
        assert_eq!(result.status, AnalysisStatus::Ready);
        assert!(result.fingerprint.is_some());
        assert!(result.technical.is_some());
        assert_eq!(result.faces.status, AnalysisStatus::NotApplicable);
        assert!(result.faces.faces.is_empty());
    }

    #[test]
    fn provider_failure_followed_by_local_fallback_is_ready() {
        let result = face_detection_ready(
            FaceProviderIdentity::new(ULTRAFACE_FACE_PROVIDER, ULTRAFACE_FACE_PROVIDER_VERSION),
            vec![FaceObservation {
                x: 0.2,
                y: 0.2,
                width: 0.3,
                height: 0.3,
                detection_confidence: 0.91,
                pose: None,
                eye_state: EyeState::NotAnalyzable,
                eye_confidence: None,
                face_sharpness: None,
            }],
            Some("Apple Vision failed: host service unavailable".into()),
        );
        assert_eq!(result.status, AnalysisStatus::Ready);
        assert_eq!(result.faces.len(), 1);
        assert!(result.error_message.is_none());
        assert_eq!(result.resolved_provider, ULTRAFACE_FACE_PROVIDER);
        assert_eq!(
            result.provider_attempt_error.as_deref(),
            Some("Apple Vision failed: host service unavailable")
        );
        assert_eq!(result.landmark_status, AnalysisStatus::NotApplicable);
    }

    #[test]
    fn all_face_providers_failed_remains_failed_without_a_false_zero_face_claim() {
        let result = face_detection_failed(
            FaceProviderIdentity::new(ULTRAFACE_FACE_PROVIDER, ULTRAFACE_FACE_PROVIDER_VERSION),
            AnalysisStatus::Failed,
            "UltraFace model initialization failed".into(),
            Some("Apple Vision failed: host service unavailable".into()),
        );
        assert_eq!(result.status, AnalysisStatus::Failed);
        assert!(result.faces.is_empty());
        assert!(result.error_message.is_some());
        assert!(result.provider_attempt_error.is_some());
    }

    #[test]
    fn ultraface_postprocessing_preserves_zero_faces_and_distinct_multiple_faces() {
        let no_faces = ultraface_faces_from_outputs(&[0.99, 0.01], &[0.1, 0.1, 0.2, 0.2]).unwrap();
        assert!(no_faces.is_empty());

        let faces = ultraface_faces_from_outputs(
            &[0.01, 0.95, 0.01, 0.93, 0.01, 0.82, 0.01, 0.99],
            &[
                0.10, 0.10, 0.40, 0.40, // retained
                0.11, 0.11, 0.41, 0.41, // suppressed by NMS
                0.60, 0.20, 0.82, 0.48, // retained as a second face
                0.04, 0.70, 0.06, 0.74, // below the supported minimum face area
            ],
        )
        .unwrap();
        assert_eq!(faces.len(), 2);
        assert!(faces
            .iter()
            .all(|face| face.eye_state == EyeState::NotAnalyzable));
    }

    #[test]
    fn face_boundary_preserves_eye_uncertainty_without_identity_claims() {
        let directory = tempdir().unwrap();
        let preview = directory.path().join("preview.ppm");
        fs::write(&preview, checkerboard(64, 64).ppm_bytes()).unwrap();
        let detector = StaticFaceDetector {
            result: FaceProviderResult {
                provider: "test-local-face-boundary".into(),
                provider_version: "1".into(),
                resolved_provider: "test-local-face-boundary".into(),
                resolved_provider_version: "1".into(),
                status: AnalysisStatus::Ready,
                faces: vec![
                    FaceObservation {
                        x: 0.1,
                        y: 0.1,
                        width: 0.3,
                        height: 0.3,
                        detection_confidence: 0.9,
                        pose: Some("frontal".into()),
                        eye_state: EyeState::Open,
                        eye_confidence: Some(0.85),
                        face_sharpness: None,
                    },
                    FaceObservation {
                        x: 0.55,
                        y: 0.1,
                        width: 0.08,
                        height: 0.08,
                        detection_confidence: 0.6,
                        pose: Some("profile".into()),
                        eye_state: EyeState::NotAnalyzable,
                        eye_confidence: None,
                        face_sharpness: None,
                    },
                ],
                provider_attempt_error: None,
                landmark_status: AnalysisStatus::Ready,
                landmark_error_message: None,
                error_message: None,
            },
        };
        let result = analyze_preview(
            &preview,
            &PpmDecoder,
            &DeterministicImageAnalyzer,
            &detector,
        );
        assert_eq!(result.status, AnalysisStatus::Ready);
        assert_eq!(result.faces.faces.len(), 2);
        assert_eq!(result.faces.faces[0].eye_state, EyeState::Open);
        assert_eq!(result.faces.faces[1].eye_state, EyeState::NotAnalyzable);
        assert!(result
            .faces
            .faces
            .iter()
            .all(|face| face.face_sharpness.is_some()));
    }

    #[test]
    fn corrupt_preview_is_not_promoted_to_ready() {
        let directory = tempdir().unwrap();
        let preview = directory.path().join("corrupt.ppm");
        fs::write(&preview, b"P6\n10 10\n255\nonly-a-few-bytes").unwrap();
        let result = analyze_preview(
            &preview,
            &PpmDecoder,
            &DeterministicImageAnalyzer,
            &UnavailableFaceDetector,
        );
        assert_eq!(result.status, AnalysisStatus::Corrupt);
        assert!(result.fingerprint.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn jpeg_recompression_and_small_crop_remain_near_visual_evidence() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.ppm");
        let recompressed = directory.path().join("recompressed.jpg");
        let cropped = directory.path().join("cropped.jpg");
        let image = checkerboard(512, 512);
        fs::write(&source, image.ppm_bytes()).unwrap();
        assert!(Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "-s", "formatOptions", "60", "--out"])
            .arg(&recompressed)
            .arg(&source)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("/usr/bin/sips")
            .args(["-c", "480", "480", "--out"])
            .arg(&cropped)
            .arg(&recompressed)
            .status()
            .unwrap()
            .success());

        let original = fingerprint_image(&image);
        let compressed = fingerprint_image(&LocalPreviewDecoder.decode(&recompressed).unwrap());
        let cropped = fingerprint_image(&LocalPreviewDecoder.decode(&cropped).unwrap());
        assert!(similarity_evidence(&original, &compressed).visual_similarity >= 0.80);
        assert!(similarity_evidence(&original, &cropped).visual_similarity >= 0.75);
    }

    #[test]
    fn grouping_is_deterministic_and_avoids_unrelated_images() {
        let base = checkerboard(64, 64);
        let related = checkerboard(128, 128);
        let unrelated = AnalysisImage::solid(64, 64, [180, 20, 20]);
        let inputs = vec![
            GroupingInput {
                project_id: "project".into(),
                asset_id: "a".into(),
                verified_content_hash: None,
                captured_at_unix_seconds: Some(10),
                camera_model: Some("camera".into()),
                fingerprint: fingerprint_image(&base),
                technical_quality_score: Some(85.0),
            },
            GroupingInput {
                project_id: "project".into(),
                asset_id: "b".into(),
                verified_content_hash: None,
                captured_at_unix_seconds: Some(14),
                camera_model: Some("camera".into()),
                fingerprint: fingerprint_image(&related),
                technical_quality_score: Some(70.0),
            },
            GroupingInput {
                project_id: "project".into(),
                asset_id: "c".into(),
                verified_content_hash: None,
                captured_at_unix_seconds: Some(14),
                camera_model: Some("camera".into()),
                fingerprint: fingerprint_image(&unrelated),
                technical_quality_score: Some(70.0),
            },
        ];
        let first = build_similarity_groups(&inputs);
        let second = build_similarity_groups(&inputs);
        assert_eq!(first, second);
        assert!(first.iter().any(|group| group
            .members
            .iter()
            .any(|member| member.asset_id == "a")
            && group.members.iter().any(|member| member.asset_id == "b")));
        assert!(!first.iter().any(|group| group
            .members
            .iter()
            .any(|member| member.asset_id == "a")
            && group.members.iter().any(|member| member.asset_id == "c")));
    }

    #[test]
    fn small_project_exhaustive_fallback_recovers_related_pair_with_unique_lsh_buckets() {
        let fingerprint = fingerprint_image(&checkerboard(64, 64));
        let input = |asset_id: &str, bucket: &str| GroupingInput {
            project_id: "project".into(),
            asset_id: asset_id.into(),
            verified_content_hash: None,
            captured_at_unix_seconds: None,
            camera_model: Some("same-camera".into()),
            fingerprint: FingerprintEvidence {
                bucket_keys: vec![bucket.into()],
                ..fingerprint.clone()
            },
            technical_quality_score: Some(80.0),
        };
        let inputs = [
            input("a", "phash:aaaaaaaaaaaaaaaa:0"),
            input("b", "phash:bbbbbbbbbbbbbbbb:1"),
        ];

        let (groups, diagnostics) = build_similarity_groups_with_diagnostics(&inputs);

        assert!(diagnostics.exhaustive_fallback_used);
        assert_eq!(diagnostics.lsh_candidate_pairs, 0);
        assert_eq!(diagnostics.time_candidate_pairs, 0);
        assert_eq!(diagnostics.candidate_pair_count, 1);
        assert_eq!(diagnostics.accepted_by_same_camera, 1);
        assert_eq!(diagnostics.accepted_by_time_proximity, 0);
        assert_eq!(diagnostics.accepted_edge_count, 1);
        assert_eq!(diagnostics.group_count, 1);
        assert_eq!(diagnostics.groups[0].member_asset_ids, vec!["a", "b"]);
        assert!(groups.iter().any(|group| {
            group.kind == SimilarityGroupKind::NearDuplicateSet && group.members.len() == 2
        }));
    }

    #[test]
    fn large_project_skips_exhaustive_candidate_generation() {
        let fingerprint = fingerprint_image(&checkerboard(64, 64));
        let inputs = (0..=SMALL_PROJECT_EXHAUSTIVE_CANDIDATE_LIMIT)
            .map(|index| GroupingInput {
                project_id: "project".into(),
                asset_id: format!("asset-{index}"),
                verified_content_hash: None,
                captured_at_unix_seconds: None,
                camera_model: None,
                fingerprint: fingerprint.clone(),
                technical_quality_score: None,
            })
            .collect::<Vec<_>>();

        let (groups, diagnostics) = build_similarity_groups_with_diagnostics(&inputs);

        assert!(!diagnostics.exhaustive_fallback_used);
        assert_eq!(diagnostics.candidate_pair_count, 0);
        assert!(groups.is_empty());
    }

    #[test]
    fn recommendation_remains_technical_and_explainable() {
        let technical = technical_evidence(&checkerboard(64, 64));
        let recommendation = recommend(&RecommendationInput {
            technical,
            face_count: 0,
            open_eyes: 0,
            possible_closed_eyes: 0,
            group_kind: Some(SimilarityGroupKind::SimilarSet),
            is_group_representative: true,
            group_rank: Some(1),
        });
        assert_eq!(recommendation.label, RecommendationLabel::StrongCandidate);
        assert!(recommendation
            .reasons
            .iter()
            .all(|reason| !reason.contains("beautiful")));
    }

    #[test]
    fn cache_key_changes_for_analyzer_versions() {
        assert_ne!(
            analysis_cache_key("input", "provider", "1", "settings"),
            analysis_cache_key("input", "provider", "2", "settings")
        );
    }

    #[test]
    fn face_provider_identity_is_stable_and_version_sensitive() {
        let first = FaceProviderIdentity::new("local-face", "adapter;os=15.4");
        let same = FaceProviderIdentity::new("local-face", "adapter;os=15.4");
        let changed = FaceProviderIdentity::new("local-face", "adapter;os=15.5");
        assert_eq!(first.cache_identity, same.cache_identity);
        assert_ne!(first.cache_identity, changed.cache_identity);
        assert!(first.cache_identity.starts_with("face-provider-v1:"));
    }

    #[test]
    fn face_artifact_input_is_separate_from_deterministic_analyzer_identity() {
        let first = FaceProviderIdentity::new("local-face", "adapter;os=15.4");
        let changed_provider = FaceProviderIdentity::new("local-face", "adapter;os=15.5");
        let first_input = face_analysis_input_fingerprint("preview-v1", &first);
        assert_eq!(
            first_input,
            face_analysis_input_fingerprint("preview-v1", &first)
        );
        assert_ne!(
            first_input,
            face_analysis_input_fingerprint("preview-v2", &first)
        );
        assert_ne!(
            first_input,
            face_analysis_input_fingerprint("preview-v1", &changed_provider)
        );
        assert_ne!(
            first_input,
            analysis_cache_key(
                "preview-v1",
                DETERMINISTIC_PROVIDER,
                DETERMINISTIC_VERSION,
                ANALYSIS_SETTINGS_VERSION,
            )
        );
    }

    #[test]
    fn unavailable_face_provider_has_an_explicit_stable_identity() {
        let identity = unavailable_face_provider_identity();
        assert_eq!(identity.provider, UNAVAILABLE_FACE_PROVIDER);
        assert_eq!(identity.provider_version, UNAVAILABLE_FACE_PROVIDER_VERSION);
        assert_eq!(
            identity.cache_identity,
            face_provider_cache_identity(
                UNAVAILABLE_FACE_PROVIDER,
                UNAVAILABLE_FACE_PROVIDER_VERSION
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vision_retries_only_explicit_transient_xpc_failures() {
        assert!(vision_error_text_is_transient(
            "Connection Invalid error for service com.apple.hiservices-xpcservice"
        ));
        assert!(vision_error_text_is_transient(
            "encountered an unexpected condition: Unspecified error"
        ));
        assert!(!vision_error_text_is_transient(
            "the image data is malformed"
        ));
    }

    #[test]
    fn macos_provider_version_uses_only_validated_os_provenance() {
        assert_eq!(
            normalize_macos_product_version("15.4.1\n"),
            Some("15.4.1".into())
        );
        assert_eq!(
            normalize_macos_product_version(" 26.0 "),
            Some("26.0".into())
        );
        assert_eq!(normalize_macos_product_version("15.4\nmalicious"), None);
        assert_eq!(normalize_macos_product_version("15.-4"), None);
        assert_eq!(normalize_macos_product_version("15.4.1.2.3"), None);
        let version = macos_face_provider_version("15.4.1", "aarch64");
        assert_eq!(
            version,
            "m4.apple-vision-adapter.v3;platform=macos;arch=aarch64;os=15.4.1;vision-request-revision=unavailable"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn platform_face_identity_records_host_or_explicit_unavailable_capability() {
        let identity = platform_face_provider_identity();
        assert_eq!(identity.provider, LOCAL_FACE_DETECTION_PROVIDER);
        assert_eq!(identity.provider_version, LOCAL_FACE_DETECTION_VERSION);
        assert!(identity.cache_identity.starts_with("face-provider-v1:"));
    }

    /// Optional local acceptance probe. It is deliberately opt-in because CaptureOS does not
    /// commit customer portraits or biometric fixtures to the repository.
    #[cfg(target_os = "macos")]
    #[test]
    fn configured_real_face_preview_produces_expected_rectangles() {
        let Some(path) = std::env::var_os("CAPTUREOS_REAL_FACE_PREVIEW") else {
            return;
        };
        let preview = Path::new(&path);
        let image = LocalPreviewDecoder.decode(preview).unwrap();
        let detector = PlatformFaceDetector;
        let result = detector.detect(preview, &image);
        assert_eq!(
            result.status,
            AnalysisStatus::Ready,
            "{:?}",
            result.error_message
        );
        eprintln!(
            "CaptureOS real face probe: provider={} {} faces={} attempted={:?}",
            result.resolved_provider,
            result.resolved_provider_version,
            result.faces.len(),
            result.provider_attempt_error
        );
        if let Some(expected) = std::env::var_os("CAPTUREOS_REAL_FACE_EXPECTED_COUNT") {
            let expected = expected
                .to_string_lossy()
                .parse::<usize>()
                .expect("CAPTUREOS_REAL_FACE_EXPECTED_COUNT must be an integer");
            assert_eq!(result.faces.len(), expected);
        }
        assert!(!result.faces.is_empty());
        assert!(result.faces.iter().all(|face| {
            face.x >= 0.0
                && face.y >= 0.0
                && face.width > 0.0
                && face.height > 0.0
                && face.x + face.width <= 1.0
                && face.y + face.height <= 1.0
        }));
    }

    /// Optional acceptance probe for the bundled fallback itself. It intentionally uses an
    /// environment-provided local preview rather than committing a portrait fixture.
    #[cfg(target_os = "macos")]
    #[test]
    fn configured_real_face_preview_uses_ultraface_fallback() {
        let Some(path) = std::env::var_os("CAPTUREOS_REAL_FACE_FALLBACK_PREVIEW") else {
            return;
        };
        let image = LocalPreviewDecoder.decode(Path::new(&path)).unwrap();
        let faces = ultraface_detect(&image).unwrap();
        eprintln!(
            "CaptureOS real UltraFace fallback probe: faces={} observations={faces:?}",
            faces.len()
        );
        if let Some(expected) = std::env::var_os("CAPTUREOS_REAL_FACE_FALLBACK_EXPECTED_COUNT") {
            let expected = expected
                .to_string_lossy()
                .parse::<usize>()
                .expect("CAPTUREOS_REAL_FACE_FALLBACK_EXPECTED_COUNT must be an integer");
            assert_eq!(faces.len(), expected);
        }
        assert!(faces.iter().all(|face| {
            face.x >= 0.0
                && face.y >= 0.0
                && face.width > 0.0
                && face.height > 0.0
                && face.x + face.width <= 1.0
                && face.y + face.height <= 1.0
        }));
    }
}
