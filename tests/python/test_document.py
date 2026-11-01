from __future__ import annotations

import json
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest
from conftest import make_dwf, make_dwfx

import ezdwf
from ezdwf import _core
from ezdwf.document import _drawing_from_mapping
from ezdwf.raw import DEFAULT_LIMITS

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


@pytest.mark.parametrize(
    "data",
    (
        make_dwf(),
        b"(DWF V00.55)(Line 0,0 5,5)(EndOfDWF)",
        make_dwfx(),
        b"(W2D V06.01)(Line 0,0 5,5)(EndOfDWF)",
    ),
    ids=("package", "legacy", "dwfx", "w2d_stream"),
)
def test_streaming_read_matches_eager_mapping(data: bytes) -> None:
    eager_mapping = _core.read_drawing_bytes(data, *DEFAULT_LIMITS.as_args())
    eager = _drawing_from_mapping(eager_mapping, source_name=None)
    streamed = ezdwf.read(data)

    assert streamed.package == eager.package
    assert streamed.legacy_stream == eager.legacy_stream
    assert streamed.dwfx_package == eager.dwfx_package
    assert tuple(sheet.snapshot() for sheet in streamed.sheets) == tuple(
        sheet.snapshot() for sheet in eager.sheets
    )


def test_reads_bare_w2d_stream_as_a_single_stream_drawing() -> None:
    # 本番の実ファイル: DWFパッケージから取り出した (W2D V06.01) が .dwf として届き、
    # "unrecognized DWF signature: 28 57 32 44 …" で読めなかった
    data = b"(W2D V06.01)(Line 0,0 5,5)(Circle 10,20 5)(EndOfDWF)"
    info = ezdwf.detect_format(data)
    assert (info.kind, info.version, info.header_size) == ("w2d_stream", "06.01", 0)
    assert info.is_w2d_stream and info.is_single_stream
    assert not info.is_legacy and not info.is_package

    drawing = ezdwf.read(data)
    assert drawing.package is None and drawing.dwfx_package is None
    stream = drawing.legacy_stream
    assert stream is not None
    assert stream.source_format == "w2d"
    assert stream.mime == "application/x-w2d"
    assert [entity.kind for entity in drawing.modelspace().entities] == [
        "LINE",
        "CIRCLE",
    ]


def _sheet_with_plot(transform: str, *lines: str) -> bytes:
    """A DWF 6 package on 1189 x 841 mm paper with the given W2D lines."""
    descriptor = f"""<?xml version="1.0" encoding="UTF-8"?>
<ePlot:Page xmlns:ePlot="DWF-ePlot:1.2" version="1.2" plotOrder="1" name="Sheet">
  <ePlot:Paper show="true" units="mm" width="1189" height="841" clip="0 0 1189 841"/>
  <ePlot:Resources>
    <ePlot:GraphicResource role="2d streaming graphics" mime="application/x-w2d" href="sheet\\main.w2d" transform="{transform}"/>
  </ePlot:Resources>
</ePlot:Page>"""
    w2d = "(W2D V06.00)\n" + "\n".join(lines) + "\n(EndOfDWF)\n"
    return make_dwf(
        w2d=w2d.encode("ascii"),
        extra_entries={"sheet\\descriptor.xml": descriptor.encode("utf-8")},
    )


def test_sheet_outside_its_paper_is_centred_on_it() -> None:
    # 実物の AutoCAD の plot: X は i32::MAX の手前、transform は用紙の余白だけを持つ
    data = _sheet_with_plot(
        "0.0635 0 0 0 0 0.0635 0 0 0 0 1 0 72.086768 0 0 1",
        "(Line 2147464949,1135 2147483647,12082)",
    )

    sheet = ezdwf.read(data).sheets[0]

    width = 18698 * 0.0635
    assert sheet.content_bounds == pytest.approx(
        ((1189 - width) / 2, 1135 * 0.0635, (1189 + width) / 2, 12082 * 0.0635)
    )
    assert sheet.content_shift[0] == pytest.approx(
        (1189 - width) / 2 - (2147464949 * 0.0635 + 72.086768)
    )
    assert sheet.content_shift[1] == 0.0


def test_sheet_touching_its_paper_keeps_the_file_coordinates() -> None:
    data = _sheet_with_plot(
        "0.0635 0 0 0 0 0.0635 0 0 0 0 1 0 0 0 0 1",
        "(Line -1000,100 1000,200)",
    )

    sheet = ezdwf.read(data).sheets[0]

    assert sheet.content_shift is None
    assert sheet.content_bounds == pytest.approx((-63.5, 6.35, 63.5, 12.7))


def test_legacy_stream_at_the_end_of_the_int32_range_starts_at_zero() -> None:
    far = ezdwf.read(
        b"(DWF V00.42)\n(Line 2147332424,-5 2147482475,70)\n(EndOfDWF)\n"
    ).sheets[0]
    near = ezdwf.read(b"(DWF V00.42)\n(Line 136166,-5 731140,70)\n(EndOfDWF)\n").sheets[
        0
    ]

    assert far.content_bounds == (0.0, -5.0, 150051.0, 70.0)
    assert far.content_shift == (-2147332424.0, 0.0)
    assert near.content_bounds == (136166.0, -5.0, 731140.0, 70.0)
    assert near.content_shift is None


def test_keep_raw_false_leaves_out_the_raw_records(dwf_bytes: bytes) -> None:
    kept = ezdwf.read(dwf_bytes)
    lean = ezdwf.read(dwf_bytes, keep_raw=False)

    assert [e.kind for e in lean.sheets[0].entities] == [
        e.kind for e in kept.sheets[0].entities
    ]
    assert lean.sheets[0].content_bounds == kept.sheets[0].content_bounds
    assert lean.diagnostics == kept.diagnostics
    assert all(entity.raw is None for entity in lean.sheets[0].entities)
    assert all(entity.raw is not None for entity in kept.sheets[0].entities)
    assert lean.package.manifest.sections[0].w2d_streams[0].entities == ()


def test_drawing_handle_defers_package_entities(dwf_bytes: bytes) -> None:
    eager = _core.read_drawing_bytes(dwf_bytes, *DEFAULT_LIMITS.as_args())
    handle = _core.read_drawing_handle(dwf_bytes, *DEFAULT_LIMITS.as_args())

    assert handle.kind() == "package"
    assert handle.sheet_count() == 1

    package_shell = handle.package_shell()
    shell_stream = package_shell["manifest"]["sections"][0]["w2d_streams"][0]
    eager_stream = eager["package"]["manifest"]["sections"][0]["w2d_streams"][0]
    assert shell_stream["entities"] is None
    assert handle.stream_entities(0, 0, 0, 1_000_000) == eager_stream["entities"]
    # 範囲取得: 分割しても連結すれば同一、末尾超えは空
    first = handle.stream_entities(0, 0, 0, 1)
    rest = handle.stream_entities(0, 0, 1, 1_000_000)
    assert first + rest == eager_stream["entities"]
    assert handle.stream_entities(0, 0, 10_000_000, 10) == []
    assert handle.sheet(0) == eager["drawing"]["sheets"][0]

    # shell + 範囲取得でシートを組み直しても一括取得と一致する
    shell = dict(handle.sheet_shell(0))
    assert shell["entities"] is None and shell["markup_entities"] is None
    shell["entities"] = handle.sheet_entities(0, 0, 1_000_000)
    shell["markup_entities"] = handle.sheet_markup_entities(0, 0, 1_000_000)
    assert shell == eager["drawing"]["sheets"][0]
    assert handle.sheet_entities(0, 10_000_000, 5) == []

    with pytest.raises(IndexError, match="sheet index 1 out of range"):
        handle.sheet(1)


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
