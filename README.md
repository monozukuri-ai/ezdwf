# ezdwf

`ezdwf` is an experimental, read-only 2D DWF parser with a pure Rust core and
an ergonomic Python API. Phase 0 through Phase 7 are implemented: format
detection, bounded DWF 6 package inspection, manifest/ePlot metadata, a
stateful W2D geometry decoder, a DWFx OPC/XPS backend, and a
normalized/queryable paper-space model.

The parser does not use DXF as an intermediate representation. Unsupported or
ambiguous source semantics are reported explicitly instead of being silently
invented or discarded.

## Current support

- Detect legacy DWF, DWF 6 packages, and DWFx by content signature.
- Traverse DWFx content types and internal OPC relationships without fetching
  external targets.
- Decode ordered XPS `FixedDocumentSequence` / `FixedDocument` / `FixedPage`
  parts, including Canvas transforms, Path geometry, Glyphs, solid brushes,
  image/linear-gradient/radial-gradient brushes, opacity masks, scoped inline
  resources, and package-local remote resource dictionaries.
- Parse abbreviated and explicit XPS line, cubic/quadratic Bezier, and
  elliptical-arc path segments.
- Preserve nested Canvas/Path/Glyphs clip chains and segment-level
  `IsStroked`/`IsSmoothJoin` plus figure-level `IsFilled` semantics.
- Preserve nested opacity-mask chains, apply `PathGeometry.Transform` without
  scaling stroke thickness, and normalize brush transforms into paper space.
- Execute inline and static-resource `VisualBrush` trees and preserve Canvas as
  isolated compositing groups, so Canvas opacity, clip, and mask apply once to
  the combined child result.
- Read PNG/JPEG/TIFF pixel density for DIP-accurate `ImageBrush.Viewbox` crops.
- Deobfuscate package-local ODTTF resources internally and materialize
  positioned `Glyphs.Indices` runs as OpenType outlines for normalized output.
- Resolve remote `ResourceDictionary` parts with inner-scope shadowing,
  declaration-base image URIs, and required-resource relationship diagnostics.
- Decode bounded UTF-8 and UTF-16LE/BE package XML, including real-world XPS
  fixed payloads.
- Safely inspect DWF 6 ZIP entries without extracting them to disk.
- Parse DWF 6 manifests and ePlot 1.2 page/resource descriptors.
- Decode W2D 6 line, polyline/polygon, circle/arc, ellipse, PolyBezier,
  polytriangle, contour, Gouraud, texture, image, and text records in ASCII
  and common binary forms.
- Expand W2D zlib streams (including the standard preset dictionary) and
  legacy LZ streams with size/depth limits and compressed source mapping.
- Preserve ColorMap, embedded-font bytes, and deprecated 00.55 BlockRef data.
- Read legacy WHIP/DWF 00.42 and 00.55 through the same high-level API.
- Expose markup separately as `Sheet.markup_entities` and optionally render it.
- Snapshot layer, RGBA/indexed color, line/fill, font, visibility, viewport,
  units, package resource transform, raw opcode, and byte range per entity.
- Apply the ePlot resource transform in Rust and expose exact paper-space
  points/curve axes while retaining links to every raw W2D entity.
- Query entities by type, layer, indexed color, visibility, and viewport.
- Produce deterministic dependency-free SVG reference renders.
- Enforce archive, XML, W2D record/point/string/nesting/decompression, and XPS
  visual/path-segment limits.

DWFx linear/radial gradients, VisualBrush trees, image-brush
viewport/transform/tile/DPI-viewbox placement, Canvas group compositing, and
opacity masks have an SVG preview. DWFx Glyphs preserve font URI, Indices, and
style metadata; package-local OpenType/ODTTF fonts are converted to positioned
outlines, with an explicit Unicode text fallback and diagnostic when a font
cannot be decoded. Color-profile brushes, JPEG XR intrinsic DPI, `ContextColor`,
and scRGB-specific interpolation remain preview boundaries. Group3/Group4 W2D
raster payloads are retained but use an SVG placeholder because no fax codec is
bundled; PNG/JPEG and raw bitonal/RGB/RGBA/indexed images can be rendered. W2D
Embedded Font records remain retained bytes and are not converted to glyph
outlines. Unknown
length-delimited records are skipped with diagnostics, and unknown single-byte
opcodes fail closed.

## Python API

```python
import ezdwf

drawing = ezdwf.read("drawing.dwf")
sheet = drawing.sheet(0)

for entity in sheet.query('LINE POLYLINE[layer=="Walls", visible==true]'):
    print(entity.dxftype(), entity.layer, entity.points, entity.source.offset)

sheet.save_svg("sheet.svg", curve_segments=96, include_markup=True)
```

`Drawing.raw` exposes a `PackageInfo`, `DwfxPackageInfo`, or legacy `W2dStream`.
`Entity.raw` points to the exact `W2dEntity` or `XpsEntity` used to build the
normalized entity. An extracted W2D resource can also be decoded directly with
`ezdwf.decode_w2d(...)`; DWFx packages can be inspected without normalization
with `ezdwf.inspect_dwfx(...)`.

`ezdwf.inspect_dwfx()` defaults to structure-only inspection and therefore does
not expand packaged fonts. Pass `resolve_glyph_outlines=True` when raw outline
geometry is required. `ezdwf.read()` enables outline resolution because the
normalized model and SVG renderer consume that geometry.

Coordinates in high-level `Entity` values are ePlot paper coordinates and use
the sheet's declared paper units. DWFx sheets use DIP (1/96 inch), converted
from XPS's top-left/Y-down space to the common bottom-left/Y-up convention.
Coordinates in `W2dEntity` and `XpsEntity` preserve their respective source
representations. The W2D units matrix and ePlot resource transform remain
available separately. See
[`docs/object-model.ja.md`](docs/object-model.ja.md) for the coordinate and
rendering contract, [`docs/phase5-dwfx.ja.md`](docs/phase5-dwfx.ja.md) for the
initial DWFx backend,
[`docs/phase6-dwfx-fidelity.ja.md`](docs/phase6-dwfx-fidelity.ja.md) for the
clip/interoperability work, and
[`docs/phase7-xps-resources-brushes.ja.md`](docs/phase7-xps-resources-brushes.ja.md)
for the current resource and brush fidelity boundary.

## CLI

```console
ezdwf inspect drawing.dwf
ezdwf inspect drawing.dwf --json
ezdwf inspect drawing.dwf --entries
ezdwf render drawing.dwf sheet.svg --sheet 0
ezdwf render drawing.dwf sheet.svg --include-markup
```

## Development

```console
uv sync --extra dev
uv run maturin develop --release
uv run pytest -q
uv run ruff format --check src tests
uv run ruff check src tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --locked
cargo check --manifest-path fuzz/Cargo.toml --bins --locked
```

Fetch and verify the external Autodesk DWF and ECMA XPS integration samples,
then inspect them:

```console
python scripts/fetch_samples.py
uv run ezdwf inspect samples/external/blocks_and_tables.dwf --json
uv run ezdwf inspect samples/external/ECMA-388.xps --json
```

Third-party reference files under `samples/external/` are downloaded locally
and are not included in Git or Python distributions. See
[`samples/README.md`](samples/README.md) for provenance and verification.
