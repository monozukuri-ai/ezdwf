#!/usr/bin/env python3
"""Download and verify the external DWF/XPS samples recorded in the manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any
from urllib.request import Request, urlopen

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = REPOSITORY_ROOT / "samples" / "manifest.json"
CHUNK_SIZE = 1024 * 1024
DOWNLOAD_TIMEOUT_SECONDS = 60


def digest(path: Path) -> tuple[int, str]:
    hasher = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(CHUNK_SIZE):
            size += len(chunk)
            hasher.update(chunk)
    return size, hasher.hexdigest()


def matches(path: Path, sample: dict[str, Any]) -> bool:
    if not path.is_file():
        return False
    size, sha256 = digest(path)
    return size == sample["size_bytes"] and sha256 == sample["sha256"]


def download(sample: dict[str, Any], destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    request = Request(
        sample["download_url"],
        headers={"User-Agent": "ezdwf-sample-fetcher/1"},
    )
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".part",
            delete=False,
        ) as output:
            temporary_path = Path(output.name)
            with urlopen(request, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
                while chunk := response.read(CHUNK_SIZE):
                    output.write(chunk)

        if not matches(temporary_path, sample):
            actual_size, actual_sha256 = digest(temporary_path)
            raise RuntimeError(
                f"verification failed for {sample['id']}: "
                f"size={actual_size}, sha256={actual_sha256}"
            )
        os.replace(temporary_path, destination)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "sample_ids",
        nargs="*",
        help="sample IDs to process (default: every manifest entry)",
    )
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="do not download; fail if a sample is missing or invalid",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    samples = manifest["samples"]
    by_id = {sample["id"]: sample for sample in samples}

    requested_ids = args.sample_ids or list(by_id)
    unknown = sorted(set(requested_ids) - set(by_id))
    if unknown:
        print(f"unknown sample ID(s): {', '.join(unknown)}", file=sys.stderr)
        return 2

    failed = False
    for sample_id in requested_ids:
        sample = by_id[sample_id]
        destination = REPOSITORY_ROOT / sample["path"]
        if matches(destination, sample):
            print(f"verified {sample_id}: {destination.relative_to(REPOSITORY_ROOT)}")
            continue

        if args.verify_only:
            print(f"missing or invalid {sample_id}: {destination}", file=sys.stderr)
            failed = True
            continue

        print(f"downloading {sample_id} from {sample['download_url']}")
        try:
            download(sample, destination)
        except (OSError, RuntimeError, ValueError) as error:
            print(f"failed {sample_id}: {error}", file=sys.stderr)
            failed = True
        else:
            print(f"verified {sample_id}: {destination.relative_to(REPOSITORY_ROOT)}")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
