#!/usr/bin/env python3
"""Explicit, narrowly scoped removal for the optional M6.1 local SigLIP pack.

This never touches the CaptureOS catalog, semantic embeddings, semantic indexes, previews,
Capture Intelligence artifacts, human decisions, source media, or any sibling model pack. It only
accepts the fixed `google-siglip-base-patch16-224` child below an operator-selected model root.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import sys
import uuid
from pathlib import Path


PACK_DIRECTORY = "google-siglip-base-patch16-224"
PACK_ID = "captureos.semantic.siglip-base-p16-224.v1"
MANIFEST_NAME = "captureos-semantic-model.json"


class RemovalError(RuntimeError):
    pass


def validate_removal_target(model_root: Path) -> tuple[Path, Path]:
    root = model_root.expanduser().resolve(strict=True)
    if not root.is_dir() or root.is_symlink():
        raise RemovalError("model root must be an existing non-symlink directory")
    target = root / PACK_DIRECTORY
    try:
        target_stat = target.lstat()
    except FileNotFoundError as error:
        raise RemovalError("the fixed local SigLIP pack is not installed") from error
    if stat.S_ISLNK(target_stat.st_mode) or not stat.S_ISDIR(target_stat.st_mode):
        raise RemovalError("the fixed local SigLIP pack is not a normal directory")
    resolved = target.resolve(strict=True)
    if resolved.parent != root:
        raise RemovalError("the fixed local SigLIP pack escaped the CaptureOS model root")
    manifest = resolved / MANIFEST_NAME
    if manifest.is_symlink() or not manifest.is_file():
        raise RemovalError("the fixed local SigLIP pack has no normal manifest file")
    try:
        payload = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RemovalError(f"cannot read the fixed local SigLIP manifest: {error}") from error
    if payload.get("packId") != PACK_ID:
        raise RemovalError("the target did not identify itself as the controlled CaptureOS SigLIP pack")
    for path in resolved.rglob("*"):
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise RemovalError(f"refusing to remove a pack containing a symlink: {path.name}")
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
            raise RemovalError(f"refusing to remove a pack containing a special file: {path.name}")
    return root, resolved


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-root", required=True, type=Path)
    parser.add_argument(
        "--confirm-remove-local-model",
        action="store_true",
        help="required for removal; without it the command performs only a dry-run validation",
    )
    arguments = parser.parse_args()
    try:
        root, target = validate_removal_target(arguments.model_root)
        print(f"validated_target={target}")
        print("preserved=projects,catalog,semantic_embeddings,semantic_indexes,previews,capture_intelligence,human_decisions,source_media")
        if not arguments.confirm_remove_local_model:
            print("DRY_RUN: pass --confirm-remove-local-model to remove only this fixed model pack")
            return 0
        staging = root / f".{PACK_DIRECTORY}.removing-{uuid.uuid4().hex}"
        if staging.exists() or staging.is_symlink():
            raise RemovalError("controlled removal staging location unexpectedly exists")
        os.replace(target, staging)
        shutil.rmtree(staging)
        print("REMOVED: the local model pack was removed; retained embeddings require reinstalling the same compatible pack before semantic search or Find Similar can run.")
        return 0
    except RemovalError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
