"""Command-line interface for DWF and DWFx inspection/rendering."""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Sequence
from dataclasses import asdict

from . import (
    ArchiveEntry,
    DwfError,
    __version__,
    detect_format,
    inspect_dwfx,
    inspect_package,
    read,
    save_plot,
    save_svg,
)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ezdwf",
        description="Inspect DWF, DWF 6, and DWFx with the native Rust parser",
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {__version__}"
    )
    subcommands = parser.add_subparsers(dest="command")
    inspect = subcommands.add_parser(
        "inspect", help="Inspect package and ePlot metadata"
    )
    inspect.add_argument("input", help="Path to a DWF file")
    inspect.add_argument("--json", action="store_true", help="Emit structured JSON")
    inspect.add_argument(
        "--entries",
        action="store_true",
        help="Include the normalized ZIP entry inventory in text output",
    )
    render = subcommands.add_parser(
        "render", help="Render an ePlot sheet to deterministic SVG"
    )
    render.add_argument("input", help="Path to a DWF file")
    render.add_argument("output", help="Output SVG path")
    render.add_argument(
        "--sheet",
        default="0",
        help="Zero-based sheet index or exact page/title (default: 0)",
    )
    render.add_argument(
        "--margin",
        type=float,
        default=0.0,
        help="Extra margin in paper units (default: 0)",
    )
    render.add_argument(
        "--curve-segments",
        type=int,
        default=96,
        help="Segments per full ellipse preview (default: 96)",
    )
    render.add_argument(
        "--monochrome", action="store_true", help="Render all entities in black"
    )
    render.add_argument(
        "--include-invisible",
        action="store_true",
        help="Include entities whose W2D visibility state is off",
    )
    render.add_argument(
        "--include-markup",
        action="store_true",
        help="Include entities from markup-role W2D resources",
    )
    render.add_argument(
        "--hide-text", action="store_true", help="Do not emit SVG text elements"
    )
    plot = subcommands.add_parser("plot", help="Render an ePlot sheet with Matplotlib")
    plot.add_argument("input", help="Path to a DWF or DWFx file")
    plot.add_argument("output", help="Output image path (for example PNG or PDF)")
    plot.add_argument(
        "--sheet",
        default="0",
        help="Zero-based sheet index or exact page/title (default: 0)",
    )
    plot.add_argument(
        "--dpi", type=int, default=150, help="Raster output DPI (default: 150)"
    )
    plot.add_argument(
        "--margin",
        type=float,
        default=0.0,
        help="Extra margin in paper units (default: 0)",
    )
    plot.add_argument(
        "--curve-segments",
        type=int,
        default=96,
        help="Segments per full ellipse preview (default: 96)",
    )
    plot.add_argument(
        "--monochrome", action="store_true", help="Render using one contrast color"
    )
    plot.add_argument(
        "--include-invisible",
        action="store_true",
        help="Include entities whose W2D visibility state is off",
    )
    plot.add_argument(
        "--include-markup",
        action="store_true",
        help="Include entities from markup-role W2D resources",
    )
    plot.add_argument(
        "--hide-text", action="store_true", help="Do not render text entities"
    )
    plot.add_argument(
        "--show-axes", action="store_true", help="Show coordinate axes and paper units"
    )
    plot.add_argument(
        "--transparent", action="store_true", help="Save with a transparent background"
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _build_parser()
    args = parser.parse_args(list(argv) if argv is not None else None)
    if args.command is None:
        parser.print_help()
        return 0

    if args.command == "render":
        sheet_key: int | str = int(args.sheet) if args.sheet.isdecimal() else args.sheet
        try:
            drawing = read(args.input)
            save_svg(
                drawing,
                args.output,
                sheet=sheet_key,
                margin=args.margin,
                curve_segments=args.curve_segments,
                monochrome=args.monochrome,
                include_invisible=args.include_invisible,
                include_markup=args.include_markup,
                show_text=not args.hide_text,
            )
        except (
            OSError,
            DwfError,
            IndexError,
            KeyError,
            TypeError,
            ValueError,
        ) as error:
            print(f"ezdwf: {error}", file=sys.stderr)
            return 1
        print(f"wrote: {args.output}")
        return 0

    if args.command == "plot":
        sheet_key = int(args.sheet) if args.sheet.isdecimal() else args.sheet
        try:
            drawing = read(args.input)
            save_plot(
                drawing,
                args.output,
                sheet=sheet_key,
                dpi=args.dpi,
                margin=args.margin,
                curve_segments=args.curve_segments,
                monochrome=args.monochrome,
                include_invisible=args.include_invisible,
                include_markup=args.include_markup,
                show_text=not args.hide_text,
                show_axes=args.show_axes,
                transparent=args.transparent,
            )
        except (
            ImportError,
            OSError,
            DwfError,
            IndexError,
            KeyError,
            TypeError,
            ValueError,
        ) as error:
            print(f"ezdwf: {error}", file=sys.stderr)
            return 1
        print(f"wrote: {args.output}")
        return 0

    try:
        format_info = detect_format(args.input)
        if format_info.is_legacy:
            drawing = read(args.input)
            stream = drawing.legacy_stream
            assert stream is not None
            if args.json:
                print(
                    json.dumps(
                        asdict(stream),
                        ensure_ascii=False,
                        indent=2,
                        default=_json_default,
                    )
                )
                return 0
            print(f"format: legacy_dwf {format_info.version or 'n/a'}")
            print(f"entities: {len(stream.entities)}")
            print(f"compressed blocks: {stream.compressed_blocks}")
            print(f"embedded fonts: {len(stream.embedded_fonts)}")
            print(f"block refs: {len(stream.block_refs)}")
            print(f"diagnostics: {len(stream.diagnostics)}")
            return 0
        package = (
            inspect_dwfx(args.input)
            if format_info.is_dwfx
            else inspect_package(args.input)
        )
    except (OSError, DwfError) as error:
        print(f"ezdwf: {error}", file=sys.stderr)
        return 1

    if args.json:
        print(
            json.dumps(
                asdict(package), ensure_ascii=False, indent=2, default=_json_default
            )
        )
        return 0

    if format_info.is_dwfx:
        print("format: dwfx")
        print(f"document sequence: {package.document_sequence}")
        print(f"entries: {len(package.entries)}")
        print(f"documents: {len(package.documents)}")
        print(f"sheets: {package.sheet_count}")
        print(f"entities: {package.entity_count}")
        print(f"diagnostics: {len(package.diagnostics)}")
        for index, page in enumerate(package.pages, start=1):
            print(f"  {index}: {page.name} ({page.width} x {page.height} dip)")
        if args.entries:
            _print_entries(package.entries)
        return 0

    version = package.format.version or "n/a"
    print(f"format: {package.format.kind} {version}")
    print(f"manifest: {package.manifest.version}")
    print(f"entries: {len(package.entries)}")
    print(f"sections: {len(package.manifest.sections)}")
    print(f"sheets: {package.sheet_count}")
    print(f"entities: {package.entity_count}")
    print(f"diagnostics: {len(package.diagnostics)}")
    for index, section in enumerate(package.sheets, start=1):
        page = section.page
        assert page is not None
        paper = page.paper
        paper_label = "unknown paper"
        if paper is not None:
            paper_label = f"{paper.width} x {paper.height} {paper.units or ''}".rstrip()
        print(f"  {index}: {page.name} ({paper_label})")
    if args.entries:
        _print_entries(package.entries)
    return 0


def _print_entries(entries: Sequence[ArchiveEntry]) -> None:
    for entry in entries:
        print(
            f"  {entry.normalized_name} "
            f"({entry.uncompressed_size} bytes, {entry.compression_method})"
        )


def _json_default(value: object) -> object:
    if isinstance(value, bytes):
        return {"encoding": "hex", "size": len(value), "data": value.hex()}
    raise TypeError(f"cannot encode {type(value).__name__} as JSON")


if __name__ == "__main__":
    raise SystemExit(main())
