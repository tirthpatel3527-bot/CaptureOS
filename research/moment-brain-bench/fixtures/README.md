# MomentBrainBench fixtures

`captureos-moment-timeline.synthetic.v1` is generated deterministically in the benchmark binary.
It contains only numeric chronological records: a synthetic ordinal, optional timestamp, structural
segment ID, generated 768-dimensional numeric signature, camera/lens/orientation category, and optional anonymous
face-count category. It contains no photograph, preview, customer metadata, path, identity label,
event name, model weight, tokenizer, or downloaded data.

The fixture deliberately includes four known mechanics cases: small same-signature continuity,
large synthetic visual transition, camera/lens/orientation change with continuing signature, and a
large gap with repeated signature. It also includes ambiguous boundaries, within-segment pauses,
and missing timestamps. A missing timestamp has no ground-truth segment and must stay ungrouped.
Ground truth means the generator's structural partition only. It is not a claim that a real
photograph proves a moment, event, relationship, or required shot.

The checked-in [timeline.v1.json](timeline.v1.json) is a manifest/schema note, not a photo dataset.
