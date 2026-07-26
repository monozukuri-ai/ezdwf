from __future__ import annotations

from dataclasses import FrozenInstanceError
from io import BytesIO
from zipfile import ZIP_STORED, ZipFile

import pytest
from conftest import DEFAULT_W2D, DWF_HEADER, make_dwf, make_dwfx, zip_bytes

import ezdwf


def test_detects_dwf_families_from_bytes(dwf_bytes: bytes) -> None:
    legacy = ezdwf.detect_format(b"(DWF V00.55)legacy")
    package = ezdwf.detect_format(dwf_bytes)
    dwfx = ezdwf.detect_format(make_dwfx())

    assert (legacy.kind, legacy.version, legacy.header_size) == (
        "legacy_dwf",
        "00.55",
        0,
    )
    assert (package.kind, package.version, package.header_size) == (
        "dwf_package",
        "06.00",
        12,
    )
    assert (dwfx.kind, dwfx.version, dwfx.header_size) == ("dwfx", None, 0)


def test_rejects_an_unrelated_zip() -> None:
    unrelated = zip_bytes({"readme.txt": b"not a DWF"})
    with pytest.raises(ezdwf.InvalidDwfError, match="unrecognized DWF signature"):
        ezdwf.detect_format(unrelated)


def test_inspects_package_sheet_and_w2d_entities(dwf_bytes: bytes) -> None:
    package = ezdwf.inspect_package(dwf_bytes)

    assert len(package.entries) == 3
    assert package.sheet_count == 1
    assert package.entity_count == 3
    assert package.diagnostics == ()
    sheet = package.sheets[0]
    assert sheet.title == "Fixture Sheet"
    assert sheet.resources[0].normalized_href == "sheet/descriptor.xml"
    assert len(sheet.w2d_streams) == 1
    stream = sheet.w2d_streams[0]
    assert stream.version == "06.00"
    assert stream.complete and stream.end_of_dwf_seen
    assert stream.transform == (
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        2.0,
        3.0,
        0.0,
        1.0,
    )
    assert stream.units is not None and stream.units.name == "mm"
    assert stream.layers[0] == ezdwf.W2dLayer(number=4, name="walls")
    assert tuple(entity.kind for entity in stream.entities) == (
        "polygon",
        "line",
        "text",
    )
    assert stream.logical_bounds == (0, 0, 10, 10)
    polygon = stream.entities[0]
    assert polygon.rendition.fill
    assert polygon.rendition.color == (12, 34, 56, 255)
    assert polygon.rendition.layer == ezdwf.W2dLayer(number=4, name="walls")
    assert polygon.source.opcode == "P"
    assert polygon.source.offset > 12
    assert stream.entities[1].rendition.fill is False
    text = stream.entities[2]
    assert text.text == "fixture"
    assert text.bounds is not None and len(text.bounds) == 4

    with pytest.raises(FrozenInstanceError):
        stream.complete = False  # type: ignore[misc]


def test_decodes_standalone_w2d() -> None:
    stream = ezdwf.decode_w2d(DEFAULT_W2D, resource_name="standalone.w2d")
    assert stream.href == "standalone.w2d"
    assert tuple(entity.kind for entity in stream.entities) == (
        "polygon",
        "line",
        "text",
    )


def test_file_archive_xml_and_w2d_limits_are_typed(dwf_bytes: bytes) -> None:
    with pytest.raises(ezdwf.DwfLimitError, match="input size"):
        ezdwf.inspect_package(
            dwf_bytes,
            limits=ezdwf.ParseLimits(max_file_size=len(dwf_bytes) - 1),
        )
    with pytest.raises(ezdwf.DwfLimitError, match="entry count"):
        ezdwf.inspect_package(
            dwf_bytes,
            limits=ezdwf.ParseLimits(max_archive_entries=2),
        )
    with pytest.raises(ezdwf.DwfLimitError, match="declares 3 points"):
        ezdwf.inspect_package(
            dwf_bytes,
            limits=ezdwf.ParseLimits(max_w2d_points_per_entity=2),
        )
    with pytest.raises(ezdwf.DwfLimitError, match="nesting depth"):
        ezdwf.inspect_package(
            dwf_bytes,
            limits=ezdwf.ParseLimits(max_xml_depth=1),
        )


def test_rejects_suspicious_compression_and_xml_doctype() -> None:
    compressed = make_dwf(extra_entries={"sheet/high-ratio.bin": b"0" * 100_000})
    with pytest.raises(ezdwf.DwfLimitError, match="compression ratio"):
        ezdwf.inspect_package(
            compressed,
            limits=ezdwf.ParseLimits(max_compression_ratio=20),
        )

    with_doctype = zip_bytes(
        {
            "manifest.xml": (
                b"<!DOCTYPE Manifest [<!ENTITY xxe SYSTEM 'file:///etc/passwd'>]>"
                b"<Manifest version='6.0'>&xxe;</Manifest>"
            )
        },
        prefix=DWF_HEADER,
    )
    with pytest.raises(ezdwf.InvalidDwfError, match="DOCTYPE"):
        ezdwf.inspect_package(with_doctype)


def test_w2d_total_points_nesting_and_truncation_limits() -> None:
    two_lines = b"(W2D V06.00)(Line 0,0 1,1)(Line 2,2 3,3)"
    with pytest.raises(ezdwf.DwfLimitError, match="aggregate points"):
        ezdwf.decode_w2d(
            two_lines,
            limits=ezdwf.ParseLimits(max_w2d_total_points=3),
        )

    nested = b"(W2D V06.00)(Future ((((value)))))"
    with pytest.raises(ezdwf.DwfLimitError, match="nesting depth"):
        ezdwf.decode_w2d(
            nested,
            limits=ezdwf.ParseLimits(max_w2d_nesting_depth=3),
        )

    with pytest.raises(ezdwf.InvalidDwfError, match="truncated 8-byte operand"):
        ezdwf.decode_w2d(b"(W2D V06.00)\x0c\x00")


def test_parse_limits_reject_bool_negative_and_wrong_type() -> None:
    with pytest.raises(TypeError):
        ezdwf.ParseLimits(max_file_size=True)  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        ezdwf.ParseLimits(max_w2d_records=-1)
    with pytest.raises(TypeError):
        ezdwf.ParseLimits(max_xml_depth=1.5)  # type: ignore[arg-type]


def test_rejects_path_traversal_and_normalized_duplicates() -> None:
    unsafe = zip_bytes({"../manifest.xml": b"x"}, prefix=DWF_HEADER)
    with pytest.raises(ezdwf.InvalidDwfError, match="unsafe ZIP entry"):
        ezdwf.inspect_package(unsafe)

    output = BytesIO()
    output.write(DWF_HEADER)
    with ZipFile(output, "a", compression=ZIP_STORED) as archive:
        archive.writestr("manifest.xml", b"x")
        archive.writestr("sheet\\main.w2d", b"x")
        archive.writestr("sheet/main.w2d", b"x")
    with pytest.raises(ezdwf.InvalidDwfError, match="normalize to the same path"):
        ezdwf.inspect_package(output.getvalue())


def test_inspector_rejects_recognized_but_unsupported_family() -> None:
    with pytest.raises(ezdwf.UnsupportedDwfError, match="legacy DWF"):
        ezdwf.inspect_package(b"(DWF V00.55)legacy")


def test_unknown_single_byte_w2d_opcode_fails_closed() -> None:
    data = make_dwf(w2d=b"(W2D V06.00)\x01payload")
    with pytest.raises(ezdwf.UnsupportedDwfError, match="byte offset 12"):
        ezdwf.inspect_package(data)


def test_invalid_internal_compression_fails_closed() -> None:
    w2d = b"(W2D V06.00){\x00\x00\x00\x00\x11\x00compressed"
    with pytest.raises(ezdwf.InvalidDwfError, match="invalid zlib compressed data"):
        ezdwf.inspect_package(make_dwf(w2d=w2d))
