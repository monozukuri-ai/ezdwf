"""``Image.to_png``: raw rows and Group 4 records become PNG; containers pass through."""

from __future__ import annotations

import io
import struct
import zlib

import pytest

import ezdwf
from ezdwf.document import Image, Point2D

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def _image(
    format_name: str,
    columns: int,
    rows: int,
    data: bytes,
    color_map: tuple[tuple[int, int, int, int], ...] = (),
) -> Image:
    return Image(
        format=format_name,
        identifier=7,
        columns=columns,
        rows=rows,
        min=Point2D(0.0, 0.0),
        max=Point2D(float(columns), float(rows)),
        color_map=color_map,
        data=data,
    )


def _chunks(png: bytes) -> dict[str, bytes]:
    assert png.startswith(PNG_SIGNATURE)
    chunks: dict[str, bytes] = {}
    offset = 8
    while offset < len(png):
        (length,) = struct.unpack(">I", png[offset : offset + 4])
        kind = png[offset + 4 : offset + 8].decode("ascii")
        chunks[kind] = png[offset + 8 : offset + 8 + length]
        offset += 12 + length
    return chunks


def test_png_data_is_returned_unchanged() -> None:
    payload = PNG_SIGNATURE + b"not really a png"
    assert _image("png", 3, 3, payload).to_png() == payload


def test_rgb_rows_become_a_truecolor_png() -> None:
    png = _image("rgb", 2, 1, bytes([1, 2, 3, 4, 5, 6])).to_png()
    chunks = _chunks(png)
    assert struct.unpack(">IIBB", chunks["IHDR"][:10]) == (2, 1, 8, 2)
    assert zlib.decompress(chunks["IDAT"]) == bytes([0, 1, 2, 3, 4, 5, 6])


def test_bitonal_rows_use_the_color_map_with_alpha() -> None:
    image = _image(
        "bitonal_mapped",
        10,
        1,
        bytes([0b1010_0000, 0b0100_0000]),
        color_map=((255, 255, 255, 0), (20, 30, 40, 255)),
    )
    chunks = _chunks(image.to_png())
    assert struct.unpack(">IIBB", chunks["IHDR"][:10]) == (10, 1, 1, 3)
    assert chunks["PLTE"] == bytes([255, 255, 255, 20, 30, 40])
    assert chunks["tRNS"] == bytes([0, 255])
    assert zlib.decompress(chunks["IDAT"]) == bytes([0, 0b1010_0000, 0b0100_0000])


def test_group4_tiff_from_pillow_decodes_by_raw_bit_through_the_color_map() -> None:
    pillow = pytest.importorskip("PIL.Image")
    features = pytest.importorskip("PIL.features")
    if not features.check("libtiff"):
        pytest.skip("Pillow without libtiff cannot write Group 4 TIFF")
    # Pillow stores a mode "1" image as BlackIsZero (PhotometricInterpretation 1):
    # the white background is bit 1, the dark rectangle bit 0.
    source = pillow.new("1", (40, 12), 1)
    for x in range(5, 25):
        for y in range(3, 9):
            source.putpixel((x, y), 0)
    buffer = io.BytesIO()
    source.save(buffer, format="TIFF", compression="group4")
    tiff = buffer.getvalue()
    assert tiff.startswith((b"II*\x00", b"MM\x00*"))
    with pillow.open(io.BytesIO(tiff)) as written:
        assert written.tag_v2[262] == 1

    # The color map is indexed by the raw bit, as Autodesk's files require, so
    # the photometric tag is not consulted: bit 0 (the rectangle) takes entry 0.
    image = _image(
        "group4x_mapped",
        40,
        12,
        tiff,
        color_map=((255, 255, 255, 0), (0, 0, 0, 255)),
    )
    with pillow.open(io.BytesIO(image.to_png())) as decoded:
        assert decoded.size == (40, 12)
        rgba = decoded.convert("RGBA")
    assert rgba.getpixel((10, 5)) == (255, 255, 255, 0)
    assert rgba.getpixel((0, 0)) == (0, 0, 0, 255)
    assert rgba.getpixel((39, 11)) == (0, 0, 0, 255)

    flipped = _image(
        "group4x_mapped",
        40,
        12,
        tiff,
        color_map=((0, 0, 0, 255), (255, 255, 255, 255)),
    )
    with pillow.open(io.BytesIO(flipped.to_png())) as decoded:
        rgba = decoded.convert("RGBA")
    assert rgba.getpixel((10, 5)) == (0, 0, 0, 255)
    assert rgba.getpixel((0, 0)) == (255, 255, 255, 255)


def test_jpeg_and_undecodable_data_raise_the_dwf_errors() -> None:
    with pytest.raises(ezdwf.UnsupportedDwfError):
        _image("jpeg", 2, 2, b"\xff\xd8\xff\xe0").to_png()
    with pytest.raises(ezdwf.InvalidDwfError):
        _image("png", 2, 2, b"\x00\x01\x02").to_png()
    with pytest.raises(ezdwf.InvalidDwfError):
        _image("group4", 8, 8, b"").to_png()
