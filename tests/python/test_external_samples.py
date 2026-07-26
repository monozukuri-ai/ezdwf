from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

import ezdwf

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = REPOSITORY_ROOT / "samples" / "manifest.json"
SAMPLES = [
    sample
    for sample in json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))["samples"]
    if "parser_expectation" in sample
]


@pytest.mark.parametrize("sample", SAMPLES, ids=lambda sample: sample["id"])
def test_external_sample_parser_expectation(sample: dict[str, object]) -> None:
    path = REPOSITORY_ROOT / str(sample["path"])
    if not path.is_file():
        pytest.skip(f"external sample was not downloaded: {sample['id']}")

    data = path.read_bytes()
    assert len(data) == sample["size_bytes"]
    assert hashlib.sha256(data).hexdigest() == sample["sha256"]

    expectation = sample["parser_expectation"]
    assert isinstance(expectation, dict)
    format_info = ezdwf.detect_format(data)
    assert format_info.kind == expectation["kind"]
    assert format_info.version == expectation["version"]

    if expectation["status"] == "recognized-unsupported":
        with pytest.raises(
            ezdwf.UnsupportedDwfError,
            match=str(expectation["error_contains"]),
        ):
            ezdwf.read(data)
        return

    assert expectation["status"] == "supported"
    drawing = ezdwf.read(data)
    stats = drawing.stats()
    assert stats["sheet_count"] == expectation["sheet_count"]
    assert stats["entity_count"] == expectation["entity_count"]
    assert len(drawing.diagnostics) == expectation["diagnostics"]


def test_design_review_sample_resolves_packaged_fonts() -> None:
    sample = next(
        sample for sample in SAMPLES if sample["id"] == "cadforum-design-review-drawing"
    )
    path = REPOSITORY_ROOT / sample["path"]
    if not path.is_file():
        pytest.skip("Design Review DWFx sample was not downloaded")

    package = ezdwf.inspect_dwfx(path, resolve_glyph_outlines=True)
    glyph_runs = [
        entity.glyphs
        for page in package.pages
        for entity in page.entities
        if entity.glyphs is not None
    ]
    assert len(glyph_runs) == 3
    assert all(glyphs.font_part is not None for glyphs in glyph_runs)
    assert all(glyphs.outline is not None for glyphs in glyph_runs)
