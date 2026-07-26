from __future__ import annotations

import hashlib
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path
from zipfile import ZipFile

import pytest
from conftest import make_dwfx

import ezdwf

SAMPLE = (
    Path(__file__).resolve().parents[2]
    / "samples"
    / "external"
    / "blocks_and_tables.dwf"
)
EXPECTED_SHA256 = "f74eb5c4a5d1cd6f0f3dd6e7e95e9475ff63680981d5a0ce390e8fd283ef771b"
ECMA_XPS_SAMPLE = SAMPLE.with_name("ECMA-388.xps")
ECMA_XPS_SHA256 = "579b553f499800713bdbbc3a82be6065db8611a050b585c011a29ebd8533c9ad"


@pytest.mark.skipif(not SAMPLE.is_file(), reason="official sample was not downloaded")
def test_official_blocks_and_tables_snapshot() -> None:
    data = SAMPLE.read_bytes()
    assert hashlib.sha256(data).hexdigest() == EXPECTED_SHA256
    package = ezdwf.inspect_package(data)

    assert len(package.entries) == 18
    assert len(package.manifest.sections) == 3
    assert package.sheet_count == 2
    assert tuple(sheet.title for sheet in package.sheets) == (
        "Blocks and Tables - Imperial",
        "Blocks and Tables - Metric",
    )
    assert package.entity_count == 9_770
    assert package.diagnostics == ()

    imperial, metric = package.sheets
    expected = (
        Counter(line=2_063, polyline=2_190, circle=422, text=190, polytriangle=8),
        Counter(
            line=2_082, polyline=2_171, circle=454, ellipse=6, text=176, polytriangle=8
        ),
    )
    expected_bounds = ((1, 2, 40_862, 27_663), (0, 0, 38_579, 26_107))
    for sheet, expected_counts, bounds in zip(
        (imperial, metric), expected, expected_bounds, strict=True
    ):
        main, markup = sheet.w2d_streams
        assert main.complete and main.end_of_dwf_seen
        assert markup.complete and markup.end_of_dwf_seen
        assert markup.compressed_blocks == 1
        assert markup.decompressed_size > markup.source_size
        assert Counter(entity.kind for entity in main.entities) == expected_counts
        assert main.logical_bounds == bounds
        assert main.transform is not None and len(main.transform) == 16
        assert len(main.layers) >= 14
        assert len(main.viewports) == 3


@pytest.mark.skipif(not SAMPLE.is_file(), reason="official sample was not downloaded")
def test_official_normalized_model_and_svg_subset() -> None:
    drawing = ezdwf.read(SAMPLE)

    assert drawing.stats()["entity_count"] == 9_770
    assert drawing.stats()["visible_count"] == 9_456
    imperial, metric = drawing.sheets
    assert len(imperial.markup_entities) == 52
    assert len(metric.markup_entities) == 46
    assert imperial.markup_entities.query("*[markup==true]")
    assert imperial.paper_bounds == pytest.approx((0.0, 0.0, 36.0, 24.0))
    assert imperial.content_bounds == pytest.approx(
        (0.22893372, 0.426617447, 34.279767053, 23.47745078)
    )
    assert metric.paper_bounds == pytest.approx((0.0, 0.0, 841.0, 594.0))
    assert metric.content_bounds == pytest.approx(
        (5.793749809, 17.793750763, 822.382583143, 570.39191743)
    )
    assert Counter(entity.kind for entity in imperial) == Counter(
        {
            "LINE": 2_063,
            "POLYLINE": 2_190,
            "CIRCLE": 172,
            "ARC": 250,
            "TEXT": 190,
            "POLYTRIANGLE": 8,
        }
    )
    assert len(imperial.query('LINE[layer=="Text", visible==true]')) > 0

    # Exercise the deterministic renderer on real binary W2D while keeping the
    # committed snapshot small and license-independent.
    svg = ezdwf.render_svg(imperial.query("LINE")[:20], background="none", precision=4)
    root = ET.fromstring(svg)
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    assert len(root.findall(".//svg:line", namespace)) == 20


@pytest.mark.skipif(
    not ECMA_XPS_SAMPLE.is_file(), reason="ECMA OpenXPS sample was not downloaded"
)
def test_ecma_openxps_interoperability_snapshot() -> None:
    data = ECMA_XPS_SAMPLE.read_bytes()
    assert hashlib.sha256(data).hexdigest() == ECMA_XPS_SHA256

    package = ezdwf.inspect_dwfx(data)
    assert package.sheet_count == 494
    assert package.entity_count == 202_852
    assert package.diagnostics == ()
    assert (
        sum(
            bool(entity.clip_chain)
            for page in package.pages
            for entity in page.entities
        )
        == 38_143
    )
    assert (
        max(
            len(entity.clip_chain) for page in package.pages for entity in page.entities
        )
        == 2
    )
    assert (
        sum(
            brush is not None and brush.kind == "image"
            for page in package.pages
            for entity in page.entities
            for brush in (entity.style.fill, entity.style.stroke)
        )
        == 275
    )


@pytest.mark.skipif(
    not ECMA_XPS_SAMPLE.is_file(), reason="ECMA OpenXPS sample was not downloaded"
)
def test_packaged_odttf_is_materialized_as_glyph_outlines() -> None:
    name = "Resources/29340BCC-2505-0220-1653-905980034711.odttf"
    with ZipFile(ECMA_XPS_SAMPLE) as archive:
        font = archive.read(name)
    packaged_part = f"Documents/1/{name}"
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="100" Height="40">
      <Glyphs FontUri="../Resources/29340BCC-2505-0220-1653-905980034711.odttf"
        FontRenderingEmSize="20" OriginX="5" OriginY="25"
        UnicodeString="OpenXPS" Fill="#123456"/>
    </FixedPage>"""
    data = make_dwfx(fixed_page=page, extra_entries={packaged_part: font})

    lightweight = ezdwf.inspect_dwfx(data)
    assert lightweight.pages[0].entities[0].glyphs.outline is None
    resolved = ezdwf.inspect_dwfx(data, resolve_glyph_outlines=True)
    glyphs = resolved.pages[0].entities[0].glyphs
    assert glyphs.font_obfuscated
    assert glyphs.font_part == packaged_part
    assert glyphs.outline is not None and len(glyphs.outline.figures) > 20

    with pytest.raises(ezdwf.DwfLimitError, match="path segments"):
        ezdwf.inspect_dwfx(
            data,
            limits=ezdwf.ParseLimits(max_xps_path_segments=1),
            resolve_glyph_outlines=True,
        )

    entity = ezdwf.read(data).modelspace().entities[0]
    assert entity.glyph_outline is not None and len(entity.glyph_outline) > 20
    root = ET.fromstring(ezdwf.render_svg(ezdwf.read(data), background="none"))
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    outline = root.find(
        ".//svg:path[@data-xps-glyph-outline='packaged-font']", namespace
    )
    assert outline is not None and outline.attrib["fill"] == "#123456"
    assert root.find(".//svg:text", namespace) is None
