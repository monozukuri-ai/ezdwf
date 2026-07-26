# ezdwf

`ezdwf` is an experimental, read-only Python library for parsing 2D Autodesk
DWF and DWFx files. Its parser is written in Rust and exposes drawings through
an ezdxf-style Python API.

The library reads source formats directly without converting them to DXF. It
provides both a normalized paper-space model for application code and access to
the underlying DWF, W2D, OPC, and XPS records when lower-level data is needed.

## Installation

Python 3.10 or later is required. From a source checkout, run:

```console
python -m pip install .
```

Building from source also requires a Rust toolchain.

## Quick start

```python
import ezdwf

drawing = ezdwf.read("drawing.dwf")
sheet = drawing.modelspace()

print(drawing.stats())

for entity in sheet.query('LINE POLYLINE[layer=="Walls", visible==true]'):
    print(entity.dxftype(), entity.layer, entity.points)

sheet.save_svg("drawing.svg")
```

Multi-sheet drawings can be accessed by index, page name, or title:

```python
first_sheet = drawing.sheet(0)
named_sheet = drawing.sheet("Floor Plan")
```

## Command line

```console
ezdwf inspect drawing.dwf
ezdwf inspect drawing.dwf --json
ezdwf render drawing.dwf drawing.svg --sheet 0
```

Run `ezdwf --help` or `ezdwf <command> --help` for all options.

## Supported input

- Legacy 2D DWF/WHIP streams
- DWF 6 packages containing ePlot/W2D resources
- DWFx packages containing OPC/OpenXPS fixed pages

Common 2D geometry, layers, colors, line and fill styles, text, raster images,
clips, opacity masks, gradients, Canvas groups, and VisualBrush resources are
represented in the normalized model. DWFx package fonts can be decoded into
positioned glyph outlines. Drawings can be queried by entity type and style and
rendered to deterministic SVG without additional runtime dependencies.

For package inspection without normalization, use the lower-level API:

```python
package = ezdwf.inspect_package("drawing.dwf")
dwfx = ezdwf.inspect_dwfx("drawing.dwfx")

# Glyph outlines are opt-in for structure-only DWFx inspection.
dwfx_with_outlines = ezdwf.inspect_dwfx(
    "drawing.dwfx",
    resolve_glyph_outlines=True,
)
```

## Coordinates

High-level entities use the sheet's paper units in a bottom-left, Y-up
coordinate system. DWFx pages use device-independent pixels (DIP), where
96 DIP equals one inch. Raw objects retain their source coordinate systems and
transforms.

See [the object model documentation](docs/object-model.ja.md) for details.

## Limitations

- The API is read-only and is still pre-alpha.
- Support is focused on 2D ePlot content; 3D DWF content is outside the current
  scope.
- SVG output is a reference preview. Some color-profile brushes, JPEG XR image
  details, and format-specific embedded fonts cannot yet be rendered exactly.
- Unsupported or ambiguous source semantics are reported through diagnostics
  instead of being silently approximated.

## License

This project is licensed under the [MIT License](LICENSE).
