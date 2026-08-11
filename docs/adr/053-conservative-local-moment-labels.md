# ADR 053: Conservative local Moment-label suggestions

## Decision

Moment label suggestion has a closed candidate-policy boundary. The M6 SigLIP image/text encoder
may supply compatible local ranking vectors, but it is not treated as a captioning,
object-detection, identity, event, or relationship model. The pure Moment engine accepts only
reviewed generic candidates or exact photographer-provided phrases; it has no free-form label
input/output path.

The initial provider scores only a reviewed generic descriptive candidate vocabulary supported by the product boundary: neutral concepts such as portrait(s), group, indoor, outdoor, water, boat, close-up, wide scene, one person, and multiple people. It can form a concise label only when underlying candidate evidence and a conservative score margin support the combination. Photographer-authored project/checklist phrases are additional candidate text; they are not semantic facts inferred by CaptureOS.

The analysis run stores its local model compatibility identity and algorithm version. Each Moment
stores its candidate source, supporting generic concepts/evidence state, compatible-vector count,
and an optional local ranking value; AI suggestion, evidence, and human label are stored
independently. Normal UI shows only the concise winning label or **Untitled Moment**; Developer
Details may show the non-numeric evidence.

## Consequences

Weak, ambiguous, conflicting, unavailable, or incompatible evidence abstains rather than fabricating a label. CaptureOS never infers a bride/groom, identity, family relationship, emotion, ceremony stage, wedding event, creative intent, or missing shot. User-provided terminology may be matched as a candidate but is not invented or auto-confirmed.

A human rename is authoritative for presentation and never overwrites historical AI evidence. This preserves local transparency without introducing a model download, paid API, cloud request, or unreviewed free-form generation path.
