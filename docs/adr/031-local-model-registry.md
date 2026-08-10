# ADR 031: Explicit local-model registry and license gate

## Decision

Migration 008 introduces a `LocalModelRecord`/`local_model_registry` contract with model ID, provider, version, controlled relative path, checksum, capability, status, license/source metadata, size, hardware requirements, and registration time. M4.3 adds one reviewed static local face-rectangle asset, documented in ADR 037, but no downloader or model manager.

A future model is eligible only after explicit user installation, checksum/path validation, license and redistribution review, supported-runtime review, and provenance integration. It must not auto-download, execute model-provided code, or contact a remote service.

## Consequences

The deterministic baseline can run at zero cloud/API cost while local providers remain auditable. The optional Apple Vision host capability is documented but not entered as a CaptureOS-distributed model. A registry row does not itself authorize a model to run.
