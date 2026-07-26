from __future__ import annotations

import json
from pathlib import Path

import ezdwf


def test_cli_text_summary(dwf_path: Path, capsys) -> None:  # type: ignore[no-untyped-def]
    assert ezdwf.main(["inspect", str(dwf_path)]) == 0
    output = capsys.readouterr().out
    assert "format: dwf_package 06.00" in output
    assert "sheets: 1" in output
    assert "entities: 3" in output


def test_cli_json_is_structured(dwf_path: Path, capsys) -> None:  # type: ignore[no-untyped-def]
    assert ezdwf.main(["inspect", str(dwf_path), "--json"]) == 0
    output = json.loads(capsys.readouterr().out)
    assert (
        output["manifest"]["sections"][0]["w2d_streams"][0]["entities"][0]["kind"]
        == "polygon"
    )


def test_cli_reports_invalid_input(tmp_path: Path, capsys) -> None:  # type: ignore[no-untyped-def]
    path = tmp_path / "invalid.dwf"
    path.write_bytes(b"not a DWF")
    assert ezdwf.main(["inspect", str(path)]) == 1
    assert "unrecognized DWF signature" in capsys.readouterr().err


def test_cli_renders_selected_sheet_to_svg(
    dwf_path: Path, tmp_path: Path, capsys
) -> None:  # type: ignore[no-untyped-def]
    output = tmp_path / "sheet.svg"

    assert (
        ezdwf.main(
            [
                "render",
                str(dwf_path),
                str(output),
                "--sheet",
                "Fixture Sheet",
                "--monochrome",
            ]
        )
        == 0
    )
    assert capsys.readouterr().out.strip() == f"wrote: {output}"
    text = output.read_text(encoding="utf-8")
    assert text.startswith('<?xml version="1.0" encoding="UTF-8"?>')
    assert 'stroke="#000000"' in text
