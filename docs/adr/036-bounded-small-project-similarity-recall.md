# ADR 036: Bounded small-project similarity recall

## Decision

CaptureOS continues to use four 16-bit pHash LSH-style bands and same-camera 12-second windows as its scalable visual-group candidate index. If a project has 256 or fewer eligible local analysis inputs, it also evaluates its bounded complete candidate set. At that limit the maximum is 32,640 unordered pairs.

This is a candidate-recall repair, not a threshold change. Near-duplicate and related-frame decisions keep their existing pHash, visual-similarity, and camera/time evidence gates. Projects larger than the limit keep the bounded LSH/time strategy and do not receive a catalog-wide complete pass.

The rebuild emits local developer diagnostics for the deterministic descriptor provider/version, descriptor count/dimension, candidate counts and strategy, distributions, threshold values, acceptance source, and created group/member IDs. It does not emit image pixels, source paths, face evidence, or cloud-derived data.

## Consequences

Small real shoots do not silently lose otherwise qualifying related-frame pairs solely because all exact pHash bands differ. The runtime limit remains explicit and bounded, group records remain deterministic and rebuildable, and no semantic embedding, hosted vector service, or new user-facing intelligence feature is introduced.
