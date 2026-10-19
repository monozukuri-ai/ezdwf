from __future__ import annotations

from dataclasses import FrozenInstanceError
from io import BytesIO
from zipfile import ZIP_STORED, ZipFile, ZipInfo

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


def test_w2d_entity_limit_is_soft_and_reported() -> None:
    three_lines = b"(W2D V06.00)(Line 0,0 1,1)(Line 2,2 3,3)(Line 4,4 5,5)"
    limits = ezdwf.ParseLimits(max_w2d_entities=2)
    stream = ezdwf.decode_w2d(three_lines, limits=limits)
    assert len(stream.entities) == 2
    assert stream.complete is False
    truncated = [
        d for d in stream.diagnostics if d.code == "W2D_ENTITY_LIMIT_TRUNCATED"
    ]
    assert len(truncated) == 1
    assert truncated[0].severity == "warning"
    assert truncated[0].offset == 40
    assert truncated[0].details["entities_kept"] == "2"
    assert truncated[0].details["limit"] == "2"
    assert truncated[0].details["opcode"] == "(Line"

    # Entities drawn with one rendition share the same frozen objects on the
    # Python side as well (raw rendition and normalized style).
    assert stream.entities[0].rendition is stream.entities[1].rendition

    drawing = ezdwf.read(three_lines, limits=limits)
    assert drawing.stats()["entity_count"] == 2
    assert [d.code for d in drawing.diagnostics] == ["W2D_ENTITY_LIMIT_TRUNCATED"]
    first, second = drawing.sheets[0].entities
    assert first.style is second.style
    assert first.raw.rendition is second.raw.rendition

    assert len(ezdwf.decode_w2d(three_lines).entities) == 3
    assert ezdwf.ParseLimits().max_w2d_entities == 500_000
    assert len(ezdwf.ParseLimits().as_args()) == 17


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
        backslash_name = ZipInfo("sheet/main.w2d")
        # ZipInfo.__init__ converts the host separator to '/' on Windows.
        # Assigning afterwards preserves the historical backslash in ZIP bytes.
        backslash_name.filename = "sheet\\main.w2d"
        backslash_name.compress_type = ZIP_STORED
        archive.writestr(backslash_name, b"x")
        archive.writestr("sheet/main.w2d", b"x")
    with pytest.raises(ezdwf.InvalidDwfError, match="normalize to the same path"):
        ezdwf.inspect_package(output.getvalue())


def test_rejects_exact_duplicate_zip_names() -> None:
    output = BytesIO()
    output.write(DWF_HEADER)
    with ZipFile(output, "a", compression=ZIP_STORED) as archive:
        archive.writestr("manifest.xml", b"x")
        archive.writestr("sheet/main.w2d", b"first")
        with pytest.warns(UserWarning, match="Duplicate name"):
            archive.writestr("sheet/main.w2d", b"second")
    with pytest.raises(ezdwf.InvalidDwfError, match="normalize to the same path"):
        ezdwf.inspect_package(output.getvalue())


def test_inspector_rejects_recognized_but_unsupported_family() -> None:
    with pytest.raises(ezdwf.UnsupportedDwfError, match="legacy DWF"):
        ezdwf.inspect_package(b"(DWF V00.55)legacy")


def test_unknown_single_byte_w2d_opcode_fails_closed() -> None:
    data = make_dwf(w2d=b"(W2D V06.00)\x01payload")
    with pytest.raises(ezdwf.UnsupportedDwfError, match="byte offset 12"):
        ezdwf.inspect_package(data)


def test_unknown_opcode_after_geometry_keeps_a_partial_sheet() -> None:
    line = b"\x0c" + b"".join(
        value.to_bytes(2, "little", signed=True) for value in (1, 2, 3, 4)
    )
    data = make_dwf(w2d=b"(W2D V06.00)" + line + b"\x01payload that is never decoded")

    drawing = ezdwf.read(data)

    assert drawing.stats()["entity_count"] == 1
    truncated = [d for d in drawing.diagnostics if d.code == "W2D_STREAM_TRUNCATED"]
    assert len(truncated) == 1
    assert truncated[0].severity == "warning"
    assert truncated[0].offset == 12 + len(line)
    assert truncated[0].details["opcode"] == "0x01"
    assert truncated[0].details["entities_kept"] == "1"


def test_legacy_v050_binary_text_is_decoded() -> None:
    text = (
        b"\x18"
        + (10).to_bytes(4, "little", signed=True)
        + (20).to_bytes(4, "little", signed=True)
        + b"'A1'\x01\x01"
        + b"".join(
            v.to_bytes(4, "little", signed=True) for v in (0, -3, 40, 0, 0, 5, -40, 0)
        )
    )
    line = b"\x0c" + b"".join(
        v.to_bytes(2, "little", signed=True) for v in (1, 1, 2, 2)
    )

    drawing = ezdwf.read(b"(DWF V00.50)" + text + line + b"(EndOfDWF)")

    assert drawing.is_legacy
    assert drawing.stats()["entity_count"] == 2
    assert not drawing.diagnostics


def test_invalid_internal_compression_fails_closed() -> None:
    w2d = b"(W2D V06.00){\x00\x00\x00\x00\x11\x00compressed"
    with pytest.raises(ezdwf.InvalidDwfError, match="invalid zlib compressed data"):
        ezdwf.inspect_package(make_dwf(w2d=w2d))
