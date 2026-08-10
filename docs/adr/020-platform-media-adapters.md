# ADR 020: Platform media adapters and video poster strategy

## Decision

Media preparation is isolated in `media-visual`. macOS photo thumbnails use `sips`; video posters use Quick Look (`qlmanage`) through structured process arguments. The app records unsupported status when a provider is absent or declines a format. Every platform process has a local deadline and concurrently drained stdout/stderr, so a malformed source or provider failure cannot block later assets.

## Consequences

No shell command is constructed from a media path. FFmpeg/ffprobe are not bundled or required in the current build, so there is no hidden codec download or paid service. A future `FFmpegThumbnailProvider` may be installed explicitly behind the same boundary. Browser video playback is attempted only when the local source is available and the platform codec supports it; otherwise the poster and metadata remain visible.
