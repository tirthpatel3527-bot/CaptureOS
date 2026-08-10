# Local index fixtures

These tiny, deterministic text payloads use media-like filenames only. They are not real photos, video, or audio. The Milestone 1 indexer deliberately performs extension classification and bounded byte fingerprinting, so real decoders are unnecessary.

`unreadable-simulation` is represented by a documented fixture rather than restrictive filesystem permissions, which are not portable across source checkouts. Tests inject or report filesystem read errors where the host permits it.
