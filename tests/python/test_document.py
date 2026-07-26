from __future__ import annotations

import json
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest
from conftest import make_dwf

import ezdwf

SNAPSHOTS = Path(__file__).parent / "snapshots"


def test_read_builds_normalized_sheet_and_keeps_raw_links(dwf_bytes: bytes) -> None:
    drawing = ezdwf.read(dwf_bytes)

    assert drawing.stats() == {
        "sheet_count": 1,
        "entity_count": 3,
        "visible_count": 3,
        "by_type": {"LINE": 1, "POLYGON": 1, "TEXT": 1},
        "by_layer": {"walls": 3},
    }
    sheet = drawing.modelspace()
    assert drawing.sheet(0) is sheet
    assert drawing.sheet("Fixture Sheet") is sheet
    assert sheet.units == "mm"
    assert sheet.paper_bounds == (0.0, 0.0, 297.0, 210.0)
    assert sheet.content_bounds == (2.0, 3.0, 12.0, 13.0)
    assert sheet.layers == ("walls",)

    polygon = sheet.entities[0]
    assert polygon.dxftype() == "POLYGON"
    assert polygon.points == (
        ezdwf.Point2D(2.0, 3.0),
        ezdwf.Point2D(12.0, 3.0),
        ezdwf.Point2D(12.0, 13.0),
    )
    assert polygon.style.layer == "walls"
    assert polygon.style.color == (12, 34, 56, 255)
    assert polygon.style.nominal_stroke_width == 20.0
    assert polygon.raw is drawing.raw.sheets[0].entities[0]
    assert polygon.source == polygon.raw.source


def test_entity_query_is_chainable_and_validates_selectors(dwf_bytes: bytes) -> None:
    sheet = ezdwf.read(dwf_bytes).modelspace()

    assert len(sheet.query("LINE TEXT")) == 2
    assert len(sheet.query('POLYGON[layer=="walls", visible==true]')) == 1
    assert len(sheet.query("*[layer_number==4]")) == 3
    assert len(sheet.query(layer=4, color_index=None, visible=True)) == 3
    assert len(sheet.entities[:2]) == 2
    assert sheet.entities[:2].query("LINE").first is sheet.entities[1]
    assert sheet.query("CIRCLE").first is None

    with pytest.raises(ValueError, match="invalid query condition"):
        sheet.query("LINE[layer~=4]")
    with pytest.raises(ValueError, match="unsupported query field"):
        sheet.query("LINE[weight==4]")
    with pytest.raises(KeyError, match="not found"):
        ezdwf.read(dwf_bytes).sheet("missing")


def test_entity_and_style_snapshot_is_stable(dwf_bytes: bytes) -> None:
    polygon = ezdwf.read(dwf_bytes).modelspace().entities[0]

    assert polygon.snapshot() == {
        "type": "POLYGON",
        "points": ((2.0, 3.0), (12.0, 3.0), (12.0, 13.0)),
        "center": None,
        "x_axis": None,
        "y_axis": None,
        "start_angle_degrees": None,
        "end_angle_degrees": None,
        "closed": True,
        "text": None,
        "bounds": None,
        "style": {
            "layer": "walls",
            "layer_number": 4,
            "color": (12, 34, 56, 255),
            "color_index": None,
            "line_pattern": None,
            "line_weight_logical": 20,
            "nominal_stroke_width": 20.0,
            "fill": True,
            "fill_pattern": None,
            "font_name": None,
            "font_canonical_name": None,
            "font_bold": None,
            "font_italic": None,
            "font_underlined": None,
            "font_height": None,
            "font_rotation_degrees": None,
            "visible": True,
            "viewport": None,
        },
        "source": {
            "resource": "sheet\\main.w2d",
            "offset": polygon.source.offset,
            "length": polygon.source.length,
            "opcode": "P",
        },
    }
    # The complete sheet snapshot must remain JSON serializable.
    json.dumps(ezdwf.read(dwf_bytes).modelspace().snapshot())


def test_normalizes_curves_and_bezier() -> None:
    w2d = b"""(W2D V06.00)
    (Circle 10,20 5)
    (Circle 10,20 5 0,16384)
    (Ellipse 20,30 8,4 0,65536 16384)
    (Bezier 1 0,0 1,2 3,2 4,0)
    (EndOfDWF)"""
    entities = ezdwf.read(make_dwf(w2d=w2d)).modelspace().entities

    assert tuple(entity.kind for entity in entities) == (
        "CIRCLE",
        "ARC",
        "ELLIPSE",
        "POLYBEZIER",
    )
    circle = entities[0]
    assert circle.center == ezdwf.Point2D(12.0, 23.0)
    assert circle.x_axis == ezdwf.Point2D(5.0, 0.0)
    assert circle.y_axis == ezdwf.Point2D(0.0, 5.0)
    assert circle.closed
    arc = entities[1]
    assert arc.start_angle_degrees == 0.0
    assert arc.end_angle_degrees == 90.0
    assert not arc.closed
    ellipse = entities[2]
    assert ellipse.x_axis is not None
    assert ellipse.y_axis is not None
    assert (ellipse.x_axis.x, ellipse.x_axis.y) == pytest.approx((0.0, 8.0))
    assert (ellipse.y_axis.x, ellipse.y_axis.y) == pytest.approx((-4.0, 0.0))
    assert entities[3].points[-1] == ezdwf.Point2D(6.0, 3.0)


def test_svg_matches_reference_image_snapshot(dwf_bytes: bytes, tmp_path: Path) -> None:
    sheet = ezdwf.read(dwf_bytes).modelspace()
    actual = ezdwf.render_svg(sheet, precision=3)
    expected = (SNAPSHOTS / "fixture.svg").read_text(encoding="utf-8")

    assert actual == expected
    root = ET.fromstring(actual)
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    assert len(root.findall(".//svg:polygon", namespace)) == 1
    assert len(root.findall(".//svg:line", namespace)) == 1
    assert len(root.findall(".//svg:text", namespace)) == 1

    output = tmp_path / "fixture.svg"
    assert sheet.save_svg(output, precision=3) == output
    assert output.read_text(encoding="utf-8") == expected


def test_svg_filters_visibility_text_and_validates_options() -> None:
    w2d = b"""(W2D V06.00)
    v (Line 0,0 5,5)
    V (Text 1,2 'shown')
    (EndOfDWF)"""
    sheet = ezdwf.read(make_dwf(w2d=w2d)).modelspace()

    default = sheet.render_svg()
    assert 'data-type="LINE"' not in default
    assert 'data-type="TEXT"' in default
    assert 'data-type="LINE"' in sheet.render_svg(include_invisible=True)
    assert "<text" not in sheet.render_svg(show_text=False)
    with pytest.raises(ValueError, match="at least 8"):
        sheet.render_svg(curve_segments=7)
    with pytest.raises(ValueError, match="non-negative"):
        sheet.render_svg(margin=-1)


def test_readfile_requires_a_path(dwf_path: Path) -> None:
    assert ezdwf.readfile(dwf_path).source_name == str(dwf_path)
    with pytest.raises(TypeError, match="filesystem path"):
        ezdwf.readfile(b"not a path")  # type: ignore[arg-type]
