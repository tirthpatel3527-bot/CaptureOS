# ADR 029: Explainable technical-quality evidence

## Decision

M4 calculates bounded, deterministic sharpness, blur, and exposure evidence from an analysis preview. It records Laplacian variance, edge strength, local high-frequency content, directional edge imbalance, luminance statistics, clipping percentages, technical score/band, and confidence. Face-region sharpness remains a separate measurement when a face box is available.

The initial technical score is a documented, versioned aggregation of global sharpness, severe clipping penalty, and blur penalty. Recommendations expose their component reasons and are group-relative when group evidence exists. Low detail without strong directional evidence is `UNCERTAIN`, not a confident motion-blur diagnosis.

## Consequences

`STRONG`, `GOOD`, `REVIEW`, and `TECHNICAL_ISSUE` are technical bands, never universal artistic judgments. The M4 constants are transparent provider settings and participate in versioned invalidation; no hidden aesthetic model or automatic culling exists. Future calibration must be benchmarked and versioned rather than rewriting old evidence.
