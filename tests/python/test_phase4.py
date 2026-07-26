from __future__ import annotations

import base64
import json
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path

import pytest

import ezdwf

ADVANCED = rb"""(DWF V00.55)
(ColorMap 2 1,2,3,255 4,5,6,255) C 1
(GourLine 2 0,0 255,0,0,255 10,0 0,0,255,255)
(Gouraud 1 0,0 255,0,0,255 10,0 0,255,0,255 0,10 0,0,255,255)
(Texture 3 0,0 4,0 0,4)
(Contour 1 3 0,0 4,0 4,4)
(Image 'RGB' 7 1,1 0,0 2,2 (3 FF0000))
(Embedded_Font 1 2 0 14 Aabced Regular 6 Aabced (2 AABB))
(BlockRef 'full' 10 20)
(EndOfDWF)"""


def _compressed_wrapper(payload: bytes) -> bytes:
    return b"{\0\0\0\0\x11\0" + zlib.compress(payload) + b"}"


def test_advanced_raw_models_are_typed() -> None:
    stream = ezdwf.decode_w2d(ADVANCED)

    assert stream.source_format == "legacy_dwf"
    assert stream.color_maps == (((1, 2, 3, 255), (4, 5, 6, 255)),)
    assert [entity.kind for entity in stream.entities] == [
        "gouraud_polyline",
        "gouraud_polytriangle",
        "textured_polytriangle",
        "contour_set",
        "image",
    ]
    assert stream.entities[0].colored_points[1].color == (0, 0, 255, 255)
    assert len(stream.entities[1].colored_points) == 3
    assert len(stream.entities[3].contours[0]) == 3
    assert stream.entities[4].image is not None
    assert stream.entities[4].image.data == b"\xff\x00\x00"
    assert stream.embedded_fonts[0].typeface_name == "Aabced Regular"
    assert stream.embedded_fonts[0].logfont_name == "Aabced"
    assert stream.embedded_fonts[0].data == b"\xaa\xbb"
    assert stream.block_refs[0].format == "full"


def test_read_supports_legacy_and_renders_raw_rgb_image() -> None:
    drawing = ezdwf.read(ADVANCED)

    assert drawing.is_legacy
    assert drawing.package is None
    assert drawing.legacy_stream is drawing.raw
    sheet = drawing.modelspace()
    assert sheet.name == "Model"
    assert len(sheet.entities) == 5
    assert sheet.query("IMAGE").first is not None

    svg = ezdwf.render_svg(sheet.query("IMAGE"), background="none")
    root = ET.fromstring(svg)
    image = root.find("{http://www.w3.org/2000/svg}g/{http://www.w3.org/2000/svg}image")
    assert image is not None
    uri = image.attrib["href"]
    assert uri.startswith("data:image/png;base64,")
    assert base64.b64decode(uri.split(",", 1)[1]).startswith(b"\x89PNG\r\n\x1a\n")


def test_internal_compression_limits_are_enforced() -> None:
    data = b"(W2D V06.00)" + _compressed_wrapper(b"(Comment " + b"x" * 100 + b")")
    with pytest.raises(ezdwf.DwfLimitError, match="expanded W2D data"):
        ezdwf.decode_w2d(
            data,
            limits=ezdwf.ParseLimits(max_w2d_decompressed_size=64),
        )

    nested = _compressed_wrapper(_compressed_wrapper(b"(Line 0,0 1,1)"))
    with pytest.raises(ezdwf.DwfLimitError, match="nesting depth"):
        ezdwf.decode_w2d(
            b"(W2D V06.00)" + nested,
            limits=ezdwf.ParseLimits(max_w2d_compression_depth=1),
        )


def test_zero_sized_raw_raster_uses_a_safe_placeholder() -> None:
    drawing = ezdwf.read(b"(DWF V00.55)(Image 'RGB' 1 0,1 0,0 2,2 (0))(EndOfDWF)")

    svg = ezdwf.render_svg(drawing.modelspace(), background="none")
    root = ET.fromstring(svg)
    placeholder = root.find(
        "{http://www.w3.org/2000/svg}g/{http://www.w3.org/2000/svg}rect"
    )

    assert placeholder is not None
    assert placeholder.attrib["data-preview"] == "unsupported-raster-placeholder"


def test_bitonal_raster_preview_uses_most_significant_bit_first() -> None:
    drawing = ezdwf.read(
        b"(DWF V00.55)"
        b"(Image 'bitonal' 1 2,1 0,0 2,1 "
        b"(ColorMap 2 255,0,0,255 0,0,255,255) (1 80))"
        b"(EndOfDWF)"
    )

    svg = ezdwf.render_svg(drawing.modelspace(), background="none")
    root = ET.fromstring(svg)
    image = root.find("{http://www.w3.org/2000/svg}g/{http://www.w3.org/2000/svg}image")
    assert image is not None
    png = base64.b64decode(image.attrib["href"].split(",", 1)[1])
    position = 8
    idat = bytearray()
    while position < len(png):
        size = int.from_bytes(png[position : position + 4], "big")
        name = png[position + 4 : position + 8]
        payload = png[position + 8 : position + 8 + size]
        if name == b"IDAT":
            idat.extend(payload)
        position += 12 + size

    assert zlib.decompress(idat) == (b"\x00\x00\x00\xff\xff\xff\x00\x00\xff")


def test_cli_inspects_legacy_stream_and_serializes_binary_data(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "legacy.dwf"
    path.write_bytes(ADVANCED)

    assert ezdwf.main(["inspect", str(path)]) == 0
    output = capsys.readouterr().out
    assert "format: legacy_dwf 00.55" in output
    assert "entities: 5" in output
    assert "embedded fonts: 1" in output

    assert ezdwf.main(["inspect", str(path), "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["source_format"] == "legacy_dwf"
    assert payload["embedded_fonts"][0]["data"] == {
        "encoding": "hex",
        "size": 2,
        "data": "aabb",
    }
