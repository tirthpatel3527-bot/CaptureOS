//! Local, read-only metadata and preview preparation adapters.
//!
//! This crate deliberately keeps platform media tooling at an adapter boundary. It never
//! writes alongside a source file: every generated artifact is placed under the caller's
//! CaptureOS-managed cache root.

use chrono::{DateTime, Utc};
use media_model::MediaType;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const GENERATOR_VERSION: &str = "m3.1";
/// Generator identity for a high-resolution, CaptureOS-owned input that may be created on
/// demand for local analysis. It is deliberately separate from the browsing-preview generator
/// so a future analysis-input revision does not invalidate the grid cache.
pub const ANALYSIS_PREVIEW_GENERATOR_VERSION: &str = "m4.1-analysis-preview.v1";
pub const ANALYSIS_PREVIEW_LONG_EDGE: u32 = 2048;
/// A platform decoder is an adapter, not part of CaptureOS's job scheduler.  Keep every
/// invocation bounded so a malformed local file can never hold the preparation queue open.
pub const PROVIDER_TIMEOUT: Duration = Duration::from_secs(20);
const METADATA_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum VisualError {
    #[error("source is unavailable: {0}")]
    Offline(String),
    #[error("cache path is unsafe")]
    UnsafeCachePath,
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSize {
    Small,
    Medium,
    Preview,
    Analysis,
}

impl PreviewSize {
    pub fn pixels(self) -> u32 {
        match self {
            Self::Small => 256,
            Self::Medium => 768,
            Self::Preview => 1600,
            Self::Analysis => ANALYSIS_PREVIEW_LONG_EDGE,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Preview => "preview",
            Self::Analysis => "analysis",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    #[default]
    Pending,
    Ready,
    Unsupported,
    Offline,
    Corrupt,
    Failed,
    Timeout,
    Cancelled,
    Stale,
}

impl ArtifactStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Unsupported => "unsupported",
            Self::Offline => "offline",
            Self::Corrupt => "corrupt",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedMetadata {
    pub mime_type: Option<String>,
    pub byte_size: Option<u64>,
    pub file_created_at: Option<String>,
    pub file_modified_at: Option<String>,
    pub captured_at_raw: Option<String>,
    pub captured_at_local: Option<String>,
    pub capture_timezone: Option<String>,
    pub capture_time_source: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub focal_length_equivalent_mm: Option<f64>,
    pub aperture: Option<f64>,
    pub shutter_speed: Option<String>,
    pub iso: Option<u32>,
    pub exposure_compensation: Option<String>,
    pub flash: Option<String>,
    pub white_balance: Option<String>,
    pub color_space: Option<String>,
    pub gps_present: Option<bool>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub codec: Option<String>,
    pub pixel_format: Option<String>,
    pub bitrate: Option<u64>,
    pub audio_streams: Option<u32>,
    pub video_streams: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub raw: Value,
    pub status: ArtifactStatus,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPreview {
    pub artifact_type: String,
    pub size: PreviewSize,
    pub cache_relative_path: String,
    pub provider: String,
    pub source_fingerprint: String,
    pub status: ArtifactStatus,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub format: String,
    pub metadata: bool,
    pub thumbnail: bool,
    pub viewer: bool,
    pub note: String,
}

pub trait MetadataExtractor {
    fn extract(&self, source: &Path, media_type: &MediaType) -> ExtractedMetadata;
}

pub trait ThumbnailProvider {
    fn generate(
        &self,
        source: &Path,
        media_type: &MediaType,
        destination: &Path,
        size: PreviewSize,
    ) -> Result<ProviderResult, VisualError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResult {
    pub provider: String,
    pub status: ArtifactStatus,
    pub failure_reason: Option<String>,
}

/// The default adapter set uses platform tools only through `Command` argument arrays.
/// No paths are interpolated into a shell string.
pub struct LocalVisualAdapters;

impl MetadataExtractor for LocalVisualAdapters {
    fn extract(&self, source: &Path, media_type: &MediaType) -> ExtractedMetadata {
        extract_metadata(source, media_type)
    }
}

impl ThumbnailProvider for LocalVisualAdapters {
    fn generate(
        &self,
        source: &Path,
        media_type: &MediaType,
        destination: &Path,
        size: PreviewSize,
    ) -> Result<ProviderResult, VisualError> {
        match media_type {
            MediaType::Jpeg | MediaType::Png | MediaType::Tiff | MediaType::Heif => {
                generate_photo_thumbnail(source, destination, size)
            }
            MediaType::Video => generate_video_poster(source, destination, size),
            MediaType::RawPhoto => Ok(ProviderResult {
                provider: "raw-preview-boundary".into(),
                status: ArtifactStatus::Unsupported,
                failure_reason: Some(
                    "No embedded-RAW preview decoder is bundled in this build".into(),
                ),
            }),
            _ => Ok(ProviderResult {
                provider: "none".into(),
                status: ArtifactStatus::Unsupported,
                failure_reason: Some("This media type has no image thumbnail provider".into()),
            }),
        }
    }
}

pub fn capabilities() -> Vec<Capability> {
    vec![
        Capability {
            format: "JPEG / PNG / TIFF".into(),
            metadata: cfg!(target_os = "macos"),
            thumbnail: cfg!(target_os = "macos"),
            viewer: true,
            note: "macOS SIPS adapter when available".into(),
        },
        Capability {
            format: "HEIC / HEIF".into(),
            metadata: cfg!(target_os = "macos"),
            thumbnail: cfg!(target_os = "macos"),
            viewer: true,
            note: "platform decoder capability varies".into(),
        },
        Capability {
            format: "MOV / MP4".into(),
            metadata: cfg!(target_os = "macos"),
            thumbnail: cfg!(target_os = "macos"),
            viewer: true,
            note: "Quick Look poster; browser codec support varies".into(),
        },
        Capability {
            format: "WAV".into(),
            metadata: true,
            thumbnail: false,
            viewer: true,
            note: "native RIFF metadata; audio card".into(),
        },
        Capability {
            format: "ARW / CR2 / CR3 / NEF / RAF / ORF / RW2 / DNG".into(),
            metadata: false,
            thumbnail: false,
            viewer: false,
            note: "RawPreviewProvider boundary only; unsupported files show an honest placeholder"
                .into(),
        },
    ]
}

pub fn extract_metadata(source: &Path, media_type: &MediaType) -> ExtractedMetadata {
    let file_metadata = match fs::metadata(source) {
        Ok(value) => value,
        Err(error) => {
            return ExtractedMetadata {
                status: if error.kind() == std::io::ErrorKind::NotFound {
                    ArtifactStatus::Offline
                } else {
                    ArtifactStatus::Failed
                },
                failure_reason: Some(error.to_string()),
                ..Default::default()
            }
        }
    };
    let mut metadata = ExtractedMetadata {
        mime_type: mime_type(source).map(str::to_owned),
        byte_size: Some(file_metadata.len()),
        file_created_at: file_metadata.created().ok().map(system_time_string),
        file_modified_at: file_metadata.modified().ok().map(system_time_string),
        raw: json!({}),
        status: ArtifactStatus::Ready,
        ..Default::default()
    };

    if let Some(reason) = source_integrity_error(source, media_type, file_metadata.len()) {
        metadata.status = ArtifactStatus::Corrupt;
        metadata.failure_reason = Some(reason);
        return metadata;
    }

    if matches!(media_type, MediaType::Audio) && is_wav(source) {
        apply_wav_metadata(source, &mut metadata);
    }
    if metadata.status != ArtifactStatus::Ready {
        return metadata;
    }
    if cfg!(target_os = "macos") {
        let spotlight = macos_metadata(source);
        apply_platform_metadata(&mut metadata, &spotlight);
        if matches!(
            media_type,
            MediaType::Jpeg | MediaType::Png | MediaType::Tiff | MediaType::Heif
        ) {
            let sips = sips_properties(source);
            apply_sips_metadata(&mut metadata, &sips);
            merge_raw(&mut metadata.raw, "sips", sips);
        }
        merge_raw(&mut metadata.raw, "spotlight", spotlight);
    }
    metadata
}

pub fn prepare_previews(
    adapters: &impl ThumbnailProvider,
    cache_root: &Path,
    asset_id: &str,
    file_instance_id: &str,
    source: &Path,
    media_type: &MediaType,
    source_fingerprint: &str,
) -> Result<Vec<GeneratedPreview>, VisualError> {
    // Audio cards intentionally have no bitmap artifact. Metadata is its terminal preparation
    // result; asking image providers to process audio would make the queue lie about progress.
    if matches!(
        media_type,
        MediaType::Audio | MediaType::Sidecar | MediaType::Unknown
    ) {
        return Ok(Vec::new());
    }
    if !source.is_file() {
        return Ok(PreviewSize::all()
            .into_iter()
            .map(|size| GeneratedPreview {
                artifact_type: preview_type(media_type).into(),
                size,
                cache_relative_path: cache_relative_path(
                    asset_id,
                    file_instance_id,
                    source_fingerprint,
                    media_type,
                    size,
                ),
                provider: "none".into(),
                source_fingerprint: source_fingerprint.into(),
                status: ArtifactStatus::Offline,
                failure_reason: Some("Original file is offline or no longer available".into()),
            })
            .collect());
    }
    let source_len = match fs::metadata(source) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return Ok(preview_results_with_status(
                asset_id,
                file_instance_id,
                source_fingerprint,
                media_type,
                "local-filesystem",
                ArtifactStatus::Failed,
                Some(error.to_string()),
            ))
        }
    };
    if let Some(reason) = source_integrity_error(source, media_type, source_len) {
        return Ok(preview_results_with_status(
            asset_id,
            file_instance_id,
            source_fingerprint,
            media_type,
            "local-validation",
            ArtifactStatus::Corrupt,
            Some(reason),
        ));
    }
    PreviewSize::all()
        .into_iter()
        .map(|size| {
            let relative = cache_relative_path(
                asset_id,
                file_instance_id,
                source_fingerprint,
                media_type,
                size,
            );
            let destination = match cache_path(cache_root, &relative) {
                Ok(destination) => destination,
                Err(error) => {
                    return Ok(GeneratedPreview {
                        artifact_type: preview_type(media_type).into(),
                        size,
                        cache_relative_path: relative,
                        provider: "cache".into(),
                        source_fingerprint: source_fingerprint.into(),
                        status: ArtifactStatus::Failed,
                        failure_reason: Some(error.to_string()),
                    })
                }
            };
            if destination.is_file() {
                return Ok(GeneratedPreview {
                    artifact_type: preview_type(media_type).into(),
                    size,
                    cache_relative_path: relative,
                    provider: "cache".into(),
                    source_fingerprint: source_fingerprint.into(),
                    status: ArtifactStatus::Ready,
                    failure_reason: None,
                });
            }
            let result = match adapters.generate(source, media_type, &destination, size) {
                Ok(result) => result,
                Err(error) => ProviderResult {
                    provider: "local-provider".into(),
                    status: ArtifactStatus::Failed,
                    failure_reason: Some(error.to_string()),
                },
            };
            Ok(GeneratedPreview {
                artifact_type: preview_type(media_type).into(),
                size,
                cache_relative_path: relative,
                provider: result.provider,
                source_fingerprint: source_fingerprint.into(),
                status: result.status,
                failure_reason: result.failure_reason,
            })
        })
        .collect()
}

/// Produces one high-resolution, analysis-only cache artifact from an available source. This is
/// intentionally separate from `prepare_previews`: visual browsing does not eagerly create an
/// extra rendition, while Capture Intelligence can obtain one automatically when needed.
pub fn prepare_analysis_preview(
    adapters: &impl ThumbnailProvider,
    cache_root: &Path,
    asset_id: &str,
    file_instance_id: &str,
    source: &Path,
    media_type: &MediaType,
    source_fingerprint: &str,
) -> Result<GeneratedPreview, VisualError> {
    let relative =
        analysis_cache_relative_path(asset_id, file_instance_id, source_fingerprint, media_type);
    let unavailable = |status: ArtifactStatus, provider: &str, reason: String| GeneratedPreview {
        artifact_type: "analysis_preview".into(),
        size: PreviewSize::Analysis,
        cache_relative_path: relative.clone(),
        provider: provider.into(),
        source_fingerprint: source_fingerprint.into(),
        status,
        failure_reason: Some(reason),
    };
    if !source.is_file() {
        return Ok(unavailable(
            ArtifactStatus::Offline,
            "none",
            "Original file is offline or no longer available".into(),
        ));
    }
    let source_len = match fs::metadata(source) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return Ok(unavailable(
                ArtifactStatus::Failed,
                "local-filesystem",
                error.to_string(),
            ))
        }
    };
    if let Some(reason) = source_integrity_error(source, media_type, source_len) {
        return Ok(unavailable(
            ArtifactStatus::Corrupt,
            "local-validation",
            reason,
        ));
    }
    let destination = match cache_path(cache_root, &relative) {
        Ok(destination) => destination,
        Err(error) => {
            return Ok(unavailable(
                ArtifactStatus::Failed,
                "cache",
                error.to_string(),
            ))
        }
    };
    if destination.is_file() {
        return Ok(GeneratedPreview {
            artifact_type: "analysis_preview".into(),
            size: PreviewSize::Analysis,
            cache_relative_path: relative,
            provider: "cache".into(),
            source_fingerprint: source_fingerprint.into(),
            status: ArtifactStatus::Ready,
            failure_reason: None,
        });
    }
    let result = match adapters.generate(source, media_type, &destination, PreviewSize::Analysis) {
        Ok(result) => result,
        Err(error) => ProviderResult {
            provider: "local-provider".into(),
            status: ArtifactStatus::Failed,
            failure_reason: Some(error.to_string()),
        },
    };
    Ok(GeneratedPreview {
        artifact_type: "analysis_preview".into(),
        size: PreviewSize::Analysis,
        cache_relative_path: relative,
        provider: result.provider,
        source_fingerprint: source_fingerprint.into(),
        status: result.status,
        failure_reason: result.failure_reason,
    })
}

fn preview_results_with_status(
    asset_id: &str,
    file_instance_id: &str,
    source_fingerprint: &str,
    media_type: &MediaType,
    provider: &str,
    status: ArtifactStatus,
    failure_reason: Option<String>,
) -> Vec<GeneratedPreview> {
    PreviewSize::all()
        .into_iter()
        .map(|size| GeneratedPreview {
            artifact_type: preview_type(media_type).into(),
            size,
            cache_relative_path: cache_relative_path(
                asset_id,
                file_instance_id,
                source_fingerprint,
                media_type,
                size,
            ),
            provider: provider.into(),
            source_fingerprint: source_fingerprint.into(),
            status: status.clone(),
            failure_reason: failure_reason.clone(),
        })
        .collect()
}

impl PreviewSize {
    fn all() -> [Self; 3] {
        [Self::Small, Self::Medium, Self::Preview]
    }
}

pub fn cache_path(cache_root: &Path, relative: &str) -> Result<PathBuf, VisualError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(VisualError::UnsafeCachePath);
    }
    Ok(cache_root.join(relative_path))
}

pub fn cache_relative_path(
    asset_id: &str,
    file_instance_id: &str,
    source_fingerprint: &str,
    media_type: &MediaType,
    size: PreviewSize,
) -> String {
    let suffix = match media_type {
        MediaType::Video => "poster",
        _ => "thumbnail",
    };
    let extension = if matches!(media_type, MediaType::Video) {
        "png"
    } else {
        "jpg"
    };
    format!(
        "{}/{}/{}-{}/{}-{}.{}",
        safe_component(GENERATOR_VERSION),
        safe_component(asset_id),
        safe_component(file_instance_id),
        blake3::hash(source_fingerprint.as_bytes()).to_hex(),
        suffix,
        size.as_str(),
        extension
    )
}

pub fn analysis_cache_relative_path(
    asset_id: &str,
    file_instance_id: &str,
    source_fingerprint: &str,
    media_type: &MediaType,
) -> String {
    let extension = if matches!(media_type, MediaType::Video) {
        "png"
    } else {
        "jpg"
    };
    format!(
        "{}/{}/{}-{}/analysis-{}.{}",
        safe_component(ANALYSIS_PREVIEW_GENERATOR_VERSION),
        safe_component(asset_id),
        safe_component(file_instance_id),
        blake3::hash(source_fingerprint.as_bytes()).to_hex(),
        PreviewSize::Analysis.as_str(),
        extension
    )
}

pub fn clear_cache(cache_root: &Path) -> Result<(), VisualError> {
    // The caller owns this root. Do not accept a source path or a relative deletion target.
    if cache_root.as_os_str().is_empty() || !cache_root.is_absolute() {
        return Err(VisualError::UnsafeCachePath);
    }
    if cache_root.exists() {
        fs::remove_dir_all(cache_root)?;
    }
    fs::create_dir_all(cache_root)?;
    Ok(())
}

fn preview_type(media_type: &MediaType) -> &'static str {
    if matches!(media_type, MediaType::Video) {
        "poster"
    } else {
        "thumbnail"
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(120)
        .collect::<String>()
}

fn generate_photo_thumbnail(
    source: &Path,
    destination: &Path,
    size: PreviewSize,
) -> Result<ProviderResult, VisualError> {
    if !cfg!(target_os = "macos") {
        return Ok(ProviderResult {
            provider: "platform-photo".into(),
            status: ArtifactStatus::Unsupported,
            failure_reason: Some("No bundled photo thumbnail adapter on this platform".into()),
        });
    }
    if let Some(parent) = destination.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return Ok(failed_provider_result("macos-sips", error));
        }
    }
    let mut command = Command::new("/usr/bin/sips");
    command
        .arg("-Z")
        .arg(size.pixels().to_string())
        .args(["-s", "format", "jpeg", "--out"])
        .arg(destination)
        .arg(source);
    process_result(
        run_command_with_timeout(&mut command, PROVIDER_TIMEOUT),
        "macos-sips",
    )
}

fn generate_video_poster(
    source: &Path,
    destination: &Path,
    size: PreviewSize,
) -> Result<ProviderResult, VisualError> {
    if !cfg!(target_os = "macos") {
        return Ok(ProviderResult {
            provider: "video-poster".into(),
            status: ArtifactStatus::Unsupported,
            failure_reason: Some("No bundled video poster adapter on this platform".into()),
        });
    }
    let output_dir = destination.with_extension("quicklook");
    if output_dir.exists() {
        if let Err(error) = fs::remove_dir_all(&output_dir) {
            return Ok(failed_provider_result("macos-quicklook", error));
        }
    }
    if let Err(error) = fs::create_dir_all(&output_dir) {
        return Ok(failed_provider_result("macos-quicklook", error));
    }
    let mut command = Command::new("/usr/bin/qlmanage");
    command
        .args(["-t", "-s"])
        .arg(size.pixels().to_string())
        .arg("-o")
        .arg(&output_dir)
        .arg(source);
    let result = process_result(
        run_command_with_timeout(&mut command, PROVIDER_TIMEOUT),
        "macos-quicklook",
    )?;
    if result.status != ArtifactStatus::Ready {
        let _ = fs::remove_dir_all(&output_dir);
        return Ok(result);
    }
    let generated = match fs::read_dir(&output_dir) {
        Ok(entries) => entries,
        Err(error) => {
            let _ = fs::remove_dir_all(&output_dir);
            return Ok(failed_provider_result("macos-quicklook", error));
        }
    }
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
    .find(|path| {
        path.extension() == Some(OsStr::new("png")) || path.extension() == Some(OsStr::new("jpg"))
    });
    let Some(generated) = generated else {
        return Ok(ProviderResult {
            provider: "macos-quicklook".into(),
            status: ArtifactStatus::Unsupported,
            failure_reason: Some("Quick Look did not return a poster image".into()),
        });
    };
    if let Some(parent) = destination.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            let _ = fs::remove_dir_all(&output_dir);
            return Ok(failed_provider_result("macos-quicklook", error));
        }
    }
    if let Err(error) = fs::rename(generated, destination) {
        let _ = fs::remove_dir_all(&output_dir);
        return Ok(failed_provider_result("macos-quicklook", error));
    }
    let _ = fs::remove_dir_all(output_dir);
    Ok(ProviderResult {
        provider: "macos-quicklook".into(),
        status: ArtifactStatus::Ready,
        failure_reason: None,
    })
}

fn process_result(
    output: Result<CommandOutcome, std::io::Error>,
    provider: &str,
) -> Result<ProviderResult, VisualError> {
    match output {
        Ok(CommandOutcome::Completed(output)) if output.status.success() => Ok(ProviderResult {
            provider: provider.into(),
            status: ArtifactStatus::Ready,
            failure_reason: None,
        }),
        Ok(CommandOutcome::Completed(output)) => Ok(ProviderResult {
            provider: provider.into(),
            status: ArtifactStatus::Failed,
            failure_reason: Some(command_failure_reason(&output.stderr, output.status)),
        }),
        Ok(CommandOutcome::TimedOut) => Ok(ProviderResult {
            provider: provider.into(),
            status: ArtifactStatus::Timeout,
            failure_reason: Some(format!(
                "Local provider exceeded the {} second timeout and was stopped",
                PROVIDER_TIMEOUT.as_secs()
            )),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProviderResult {
            provider: provider.into(),
            status: ArtifactStatus::Unsupported,
            failure_reason: Some("The local platform provider is unavailable".into()),
        }),
        Err(error) => Ok(failed_provider_result(provider, error)),
    }
}

fn failed_provider_result(provider: &str, error: std::io::Error) -> ProviderResult {
    ProviderResult {
        provider: provider.into(),
        status: ArtifactStatus::Failed,
        failure_reason: Some(error.to_string()),
    }
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum CommandOutcome {
    Completed(CommandOutput),
    TimedOut,
}

/// Runs a local adapter process without a shell. Its output pipes are drained concurrently so a
/// noisy provider cannot block itself, and a deadline always terminates the child process.
fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<CommandOutcome, std::io::Error> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    Ok(match status {
        Some(status) => CommandOutcome::Completed(CommandOutput {
            status,
            stdout,
            stderr,
        }),
        None => CommandOutcome::TimedOut,
    })
}

fn command_failure_reason(stderr: &[u8], status: ExitStatus) -> String {
    let reason = String::from_utf8_lossy(stderr).trim().to_owned();
    if reason.is_empty() {
        format!("Local provider exited unsuccessfully ({status})")
    } else {
        reason
    }
}

fn sips_properties(source: &Path) -> BTreeMap<String, String> {
    command_key_values(
        Command::new("/usr/bin/sips")
            .args([
                "-g",
                "pixelWidth",
                "-g",
                "pixelHeight",
                "-g",
                "orientation",
                "-g",
                "format",
                "-g",
                "profile",
            ])
            .arg(source),
    )
}

fn macos_metadata(source: &Path) -> BTreeMap<String, String> {
    command_key_values(
        Command::new("/usr/bin/mdls")
            .args([
                "-name",
                "kMDItemAcquisitionModel",
                "-name",
                "kMDItemFSCreationDate",
                "-name",
                "kMDItemPixelWidth",
                "-name",
                "kMDItemPixelHeight",
                "-name",
                "kMDItemDurationSeconds",
                "-name",
                "kMDItemAudioSampleRate",
                "-name",
                "kMDItemAudioChannelCount",
                "-name",
                "kMDItemCodecs",
                "-name",
                "kMDItemGPSLatitude",
                "-name",
                "kMDItemGPSLongitude",
            ])
            .arg(source),
    )
}

fn command_key_values(command: &mut Command) -> BTreeMap<String, String> {
    let Ok(CommandOutcome::Completed(output)) = run_command_with_timeout(command, METADATA_TIMEOUT)
    else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(':').or_else(|| line.split_once(" = ")))
        .map(|(key, value)| {
            (
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            )
        })
        .filter(|(_, value)| value != "(null)" && !value.is_empty())
        .collect()
}

fn apply_sips_metadata(metadata: &mut ExtractedMetadata, values: &BTreeMap<String, String>) {
    metadata.width = value_u32(values, "pixelWidth").or(metadata.width);
    metadata.height = value_u32(values, "pixelHeight").or(metadata.height);
    metadata.orientation = values.get("orientation").cloned();
    metadata.color_space = values.get("profile").cloned();
}

fn apply_platform_metadata(metadata: &mut ExtractedMetadata, values: &BTreeMap<String, String>) {
    metadata.camera_model = values.get("kMDItemAcquisitionModel").cloned();
    metadata.width = value_u32(values, "kMDItemPixelWidth").or(metadata.width);
    metadata.height = value_u32(values, "kMDItemPixelHeight").or(metadata.height);
    metadata.duration_ms = values
        .get("kMDItemDurationSeconds")
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| (value * 1000.0).round() as u64)
        .or(metadata.duration_ms);
    metadata.sample_rate = value_u32(values, "kMDItemAudioSampleRate").or(metadata.sample_rate);
    metadata.channels = value_u32(values, "kMDItemAudioChannelCount").or(metadata.channels);
    metadata.codec = values
        .get("kMDItemCodecs")
        .cloned()
        .or(metadata.codec.clone());
    metadata.gps_present = Some(
        values.contains_key("kMDItemGPSLatitude") || values.contains_key("kMDItemGPSLongitude"),
    );
    if let Some(captured) = values.get("kMDItemFSCreationDate") {
        metadata.captured_at_raw = Some(captured.clone());
        metadata.captured_at_local = Some(captured.clone());
        metadata.capture_time_source = Some("platform-file-creation-date".into());
    }
}

fn apply_wav_metadata(source: &Path, metadata: &mut ExtractedMetadata) {
    let Ok(wav) = wav_properties(source) else {
        metadata.status = ArtifactStatus::Corrupt;
        metadata.failure_reason = Some("Invalid WAV container".into());
        return;
    };
    metadata.codec = Some("PCM/WAVE".into());
    metadata.sample_rate = Some(wav.sample_rate);
    metadata.bit_depth = Some(wav.bit_depth);
    metadata.channels = Some(wav.channels);
    metadata.duration_ms = wav.duration_ms;
    merge_raw(
        &mut metadata.raw,
        "wav",
        json!({ "audioFormat": wav.audio_format, "dataBytes": wav.data_bytes }),
    );
}

struct WavProperties {
    audio_format: u16,
    channels: u32,
    sample_rate: u32,
    bit_depth: u32,
    data_bytes: u64,
    duration_ms: Option<u64>,
}

fn wav_properties(source: &Path) -> Result<WavProperties, VisualError> {
    let mut file = fs::File::open(source)?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(VisualError::Offline("not a RIFF/WAVE file".into()));
    }
    let mut format = None;
    let mut data_bytes = None;
    loop {
        let mut chunk_header = [0_u8; 8];
        if file.read_exact(&mut chunk_header).is_err() {
            break;
        }
        let size = u32::from_le_bytes(chunk_header[4..8].try_into().expect("slice length")) as u64;
        if &chunk_header[0..4] == b"fmt " {
            let mut chunk = vec![0_u8; size.min(64) as usize];
            file.read_exact(&mut chunk)?;
            if size > chunk.len() as u64 {
                file.seek(SeekFrom::Current((size - chunk.len() as u64) as i64))?;
            }
            if chunk.len() >= 16 {
                format = Some((
                    u16::from_le_bytes([chunk[0], chunk[1]]),
                    u16::from_le_bytes([chunk[2], chunk[3]]) as u32,
                    u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
                    u32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]),
                    u16::from_le_bytes([chunk[14], chunk[15]]) as u32,
                ));
            }
        } else {
            if &chunk_header[0..4] == b"data" {
                data_bytes = Some(size);
            }
            file.seek(SeekFrom::Current(size as i64))?;
        }
        if size % 2 == 1 {
            file.seek(SeekFrom::Current(1))?;
        }
    }
    let (audio_format, channels, sample_rate, byte_rate, bit_depth) =
        format.ok_or_else(|| VisualError::Offline("WAV fmt chunk missing".into()))?;
    let data_bytes = data_bytes.unwrap_or(0);
    Ok(WavProperties {
        audio_format,
        channels,
        sample_rate,
        bit_depth,
        data_bytes,
        duration_ms: (byte_rate > 0)
            .then(|| data_bytes.saturating_mul(1000) / u64::from(byte_rate)),
    })
}

fn is_wav(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
}

/// Fast, read-only container checks keep obvious text fixtures and truncated inputs away from
/// platform decoders. They are deliberately conservative: formats without a reliable compact
/// signature are left to their provider and still protected by the provider watchdog.
fn source_integrity_error(source: &Path, media_type: &MediaType, byte_size: u64) -> Option<String> {
    let signature_len = match media_type {
        MediaType::Jpeg
        | MediaType::Png
        | MediaType::Tiff
        | MediaType::Heif
        | MediaType::Video
        | MediaType::Audio => 16,
        _ => return None,
    };
    if byte_size < signature_len as u64 {
        return Some(format!(
            "File is only {byte_size} bytes and is too small to contain a valid {} container",
            media_label(media_type)
        ));
    }
    let mut signature = [0_u8; 32];
    let mut file = match fs::File::open(source) {
        Ok(file) => file,
        Err(error) => return Some(format!("Unable to read source file: {error}")),
    };
    let bytes_read = match file.read(&mut signature) {
        Ok(bytes_read) => bytes_read,
        Err(error) => return Some(format!("Unable to read source file: {error}")),
    };
    let signature = &signature[..bytes_read];
    let valid = match media_type {
        MediaType::Jpeg => signature.starts_with(&[0xFF, 0xD8, 0xFF]),
        MediaType::Png => signature.starts_with(b"\x89PNG\r\n\x1a\n"),
        MediaType::Tiff => signature.starts_with(b"II*\0") || signature.starts_with(b"MM\0*"),
        MediaType::Heif | MediaType::Video => signature.get(4..8) == Some(b"ftyp".as_slice()),
        MediaType::Audio => {
            !is_wav(source)
                || (signature.starts_with(b"RIFF")
                    && signature.get(8..12) == Some(b"WAVE".as_slice()))
        }
        _ => true,
    };
    (!valid).then(|| format!("Invalid {} container signature", media_label(media_type)))
}

fn media_label(media_type: &MediaType) -> &'static str {
    match media_type {
        MediaType::Jpeg => "JPEG",
        MediaType::Png => "PNG",
        MediaType::Tiff => "TIFF",
        MediaType::Heif => "HEIF",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        _ => "media",
    }
}
fn mime_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "tif" | "tiff" => Some("image/tiff"),
        "heic" | "heif" => Some("image/heif"),
        "mov" => Some("video/quicktime"),
        "mp4" => Some("video/mp4"),
        "wav" => Some("audio/wav"),
        "mp3" => Some("audio/mpeg"),
        _ => None,
    }
}
fn value_u32(values: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    values
        .get(key)?
        .trim_matches(|character: char| !character.is_ascii_digit())
        .parse()
        .ok()
}
fn system_time_string(value: std::time::SystemTime) -> String {
    DateTime::<Utc>::from(value).to_rfc3339()
}
fn merge_raw(destination: &mut Value, key: &str, values: impl Serialize) {
    if !destination.is_object() {
        *destination = json!({});
    }
    if let Some(object) = destination.as_object_mut() {
        object.insert(
            key.into(),
            serde_json::to_value(values).unwrap_or(Value::Null),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    use tempfile::tempdir;

    #[test]
    fn parses_real_wav_metadata_without_touching_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("tone.wav");
        let original = minimal_wav();
        fs::write(&source, &original).unwrap();
        let metadata = extract_metadata(&source, &MediaType::Audio);
        assert_eq!(metadata.channels, Some(2));
        assert_eq!(metadata.sample_rate, Some(48_000));
        assert_eq!(metadata.bit_depth, Some(16));
        assert_eq!(metadata.duration_ms, Some(100));
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn cache_paths_are_contained_and_clear_never_touches_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("original.jpg");
        fs::write(&source, b"original-media").unwrap();
        let cache = directory.path().join("captureos-preview-cache");
        let target = cache_path(&cache, "v1/asset/file/thumb-small.jpg").unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, b"cache").unwrap();
        clear_cache(&cache).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original-media");
        assert!(cache.is_dir());
        assert!(cache_path(&cache, "../original.jpg").is_err());
    }

    #[test]
    fn cache_identity_changes_when_source_evidence_changes() {
        let first = cache_relative_path(
            "asset",
            "instance",
            "fast-fingerprint-a",
            &MediaType::Jpeg,
            PreviewSize::Small,
        );
        let second = cache_relative_path(
            "asset",
            "instance",
            "fast-fingerprint-b",
            &MediaType::Jpeg,
            PreviewSize::Small,
        );
        assert_ne!(first, second);
        assert_ne!(
            analysis_cache_relative_path(
                "asset",
                "instance",
                "fast-fingerprint-a",
                &MediaType::Jpeg,
            ),
            analysis_cache_relative_path(
                "asset",
                "instance",
                "fast-fingerprint-b",
                &MediaType::Jpeg,
            )
        );
    }

    #[test]
    fn analysis_preview_is_dedicated_read_only_and_reused() {
        struct RecordingProvider(Arc<AtomicUsize>);

        impl ThumbnailProvider for RecordingProvider {
            fn generate(
                &self,
                _source: &Path,
                _media_type: &MediaType,
                destination: &Path,
                size: PreviewSize,
            ) -> Result<ProviderResult, VisualError> {
                assert_eq!(size, PreviewSize::Analysis);
                self.0.fetch_add(1, Ordering::SeqCst);
                fs::create_dir_all(destination.parent().expect("cache parent"))?;
                fs::write(destination, b"analysis-preview")?;
                Ok(ProviderResult {
                    provider: "recording-provider".into(),
                    status: ArtifactStatus::Ready,
                    failure_reason: None,
                })
            }
        }

        let directory = tempdir().unwrap();
        let source = directory.path().join("source.raw");
        let cache = directory.path().join("preview-cache");
        let original = b"read-only raw fixture";
        fs::write(&source, original).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = RecordingProvider(calls.clone());

        let first = prepare_analysis_preview(
            &provider,
            &cache,
            "asset",
            "instance",
            &source,
            &MediaType::RawPhoto,
            "source-v1",
        )
        .unwrap();
        assert_eq!(first.artifact_type, "analysis_preview");
        assert_eq!(first.size, PreviewSize::Analysis);
        assert_eq!(first.status, ArtifactStatus::Ready);
        assert!(cache.join(&first.cache_relative_path).is_file());
        assert_eq!(fs::read(&source).unwrap(), original);

        let second = prepare_analysis_preview(
            &provider,
            &cache,
            "asset",
            "instance",
            &source,
            &MediaType::RawPhoto,
            "source-v1",
        )
        .unwrap();
        assert_eq!(second.status, ArtifactStatus::Ready);
        assert_eq!(second.provider, "cache");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn analysis_preview_keeps_unsupported_and_corrupt_outcomes_distinct() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("preview-cache");
        let raw = directory.path().join("unsupported.raw");
        fs::write(&raw, b"raw fixture that has no bundled decoder").unwrap();
        let unsupported = prepare_analysis_preview(
            &LocalVisualAdapters,
            &cache,
            "raw-asset",
            "raw-instance",
            &raw,
            &MediaType::RawPhoto,
            "raw-source-v1",
        )
        .unwrap();
        assert_eq!(unsupported.status, ArtifactStatus::Unsupported);

        let corrupt = directory.path().join("corrupt.jpg");
        fs::write(&corrupt, b"not a JPEG container").unwrap();
        let corrupt = prepare_analysis_preview(
            &LocalVisualAdapters,
            &cache,
            "corrupt-asset",
            "corrupt-instance",
            &corrupt,
            &MediaType::Jpeg,
            "corrupt-source-v1",
        )
        .unwrap();
        assert_eq!(corrupt.status, ArtifactStatus::Corrupt);
    }

    #[test]
    fn invalid_media_like_fixtures_are_corrupt_without_calling_a_provider() {
        let directory = tempdir().unwrap();
        for (name, media_type) in [
            ("IMG_0001.JPG", MediaType::Jpeg),
            ("C0001.MOV", MediaType::Video),
            ("IMG_0003.heic", MediaType::Heif),
        ] {
            let source = directory.path().join(name);
            fs::write(&source, b"synthetic fixture payload").unwrap();
            let metadata = extract_metadata(&source, &media_type);
            assert_eq!(metadata.status, ArtifactStatus::Corrupt);
            let artifacts = prepare_previews(
                &LocalVisualAdapters,
                &directory.path().join("cache"),
                "asset",
                name,
                &source,
                &media_type,
                "fingerprint",
            )
            .unwrap();
            assert_eq!(artifacts.len(), 3);
            assert!(artifacts
                .iter()
                .all(|artifact| artifact.status == ArtifactStatus::Corrupt));
            assert_eq!(
                prepare_analysis_preview(
                    &LocalVisualAdapters,
                    &directory.path().join("analysis-cache"),
                    "asset",
                    name,
                    &source,
                    &media_type,
                    "fingerprint",
                )
                .unwrap()
                .status,
                ArtifactStatus::Corrupt
            );
        }
    }

    #[test]
    fn audio_is_terminal_metadata_without_bitmap_artifacts() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("REC0001.WAV");
        fs::write(&source, minimal_wav()).unwrap();
        assert_eq!(
            extract_metadata(&source, &MediaType::Audio).status,
            ArtifactStatus::Ready
        );
        assert!(prepare_previews(
            &LocalVisualAdapters,
            &directory.path().join("cache"),
            "asset",
            "instance",
            &source,
            &MediaType::Audio,
            "fingerprint",
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn provider_watchdog_terminates_a_slow_child_process() {
        let mut command = Command::new("/bin/sleep");
        command.arg("1");
        let started = Instant::now();
        let outcome = run_command_with_timeout(&mut command, Duration::from_millis(5)).unwrap();
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generates_an_oriented_platform_thumbnail_from_a_real_jpeg() {
        let directory = tempdir().unwrap();
        let ppm = directory.path().join("fixture.ppm");
        let jpeg = directory.path().join("fixture.jpg");
        let output = directory.path().join("cache/thumb.jpg");
        let mut ppm_file = fs::File::create(&ppm).unwrap();
        ppm_file.write_all(b"P3\n4 2\n255\n255 0 0  0 255 0  0 0 255  255 255 255\n255 255 0  0 255 255  255 0 255  0 0 0\n").unwrap();
        let converted = Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "--out"])
            .arg(&jpeg)
            .arg(&ppm)
            .status()
            .unwrap();
        assert!(converted.success());
        let before = fs::read(&jpeg).unwrap();
        let result = generate_photo_thumbnail(&jpeg, &output, PreviewSize::Small).unwrap();
        assert_eq!(result.status, ArtifactStatus::Ready);
        assert!(output.is_file());
        assert_eq!(fs::read(&jpeg).unwrap(), before);
    }

    fn minimal_wav() -> Vec<u8> {
        let data_bytes = 19_200_u32; // 100ms, 48kHz, stereo, 16-bit
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&192_000_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_bytes.to_le_bytes());
        bytes.resize(bytes.len() + data_bytes as usize, 0);
        bytes
    }
}
