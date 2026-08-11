# ADR 045: Versioned shared image/text embedding space

## Decision

Magic Search stores semantic image embeddings for logical `MediaAsset` records and produces text
query embeddings through the same admitted provider/model/version/preprocessing space. An image
embedding’s durable provenance includes MediaAsset ID, managed-preview input fingerprint, model
ID/version, embedding dimension, preprocessing version, generated timestamp, terminal state, and
error. A MediaAsset with multiple FileInstances receives one current embedding, not one duplicate
per copy.

Image input prefers the existing oriented CaptureOS-managed analysis preview. The source is read
only to produce that contained preview when no valid cache exists; inference never receives a
general original path. Text and image preprocessing are versioned together. Vectors from different
model, model-version, manifest digest, dimension, tokenizer, or preprocessing identities are never
compared as if they were one semantic space. The dimension is included in both the durable
embedding identity and the derived-index cache key. An incompatible or changed record becomes
stale rather than being overwritten or silently reused.

## Consequences

Text queries and Find Similar have one truthful comparison contract. A READY embedding stays
usable while originals are offline, so long as its managed input/provenance remains valid. If no
compatible cache/source exists, `NEEDS_ORIGINAL` is a per-asset terminal state; corrupt,
unsupported, and failed media are similarly isolated and cannot block a project queue.

The design makes model migration explicit. A newer model can build a new active embedding/index
version while older evidence remains inspectable until an explicit cleanup policy is approved.
