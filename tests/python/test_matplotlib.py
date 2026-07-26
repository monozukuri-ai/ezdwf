from __future__ import annotations

from dataclasses import replace
from pathlib import Path

import pytest
from conftest import make_dwf, make_dwfx

matplotlib = pytest.importorskip("matplotlib")
matplotlib.use("Agg")
from matplotlib import pyplot as plt
from matplotlib.collections import LineCollection, PolyCollection
from matplotlib.patches import PathPatch

import ezdwf


def test_plot_renders_normalized_entities_and_paper_bounds(dwf_bytes: bytes) -> None:
    drawing = ezdwf.read(dwf_bytes)

    figure, axes = ezdwf.plot(drawing)

    assert figure is axes.figure
    assert sum(isinstance(item, LineCollection) for item in axes.collections) == 1
    assert sum(isinstance(item, PolyCollection) for item in axes.collections) == 1
    assert len(axes.texts) == 1
    assert axes.get_xlim() == pytest.approx((0.0, 297.0))
    assert axes.get_ylim() == pytest.approx((0.0, 210.0))
    assert not axes.axison
    plt.close(figure)


def test_sheet_plot_reuses_axes_and_exposes_paper_units(dwf_bytes: bytes) -> None:
    sheet = ezdwf.read(dwf_bytes).modelspace()
    figure, supplied_axes = plt.subplots()

    result_figure, result_axes = sheet.plot(ax=supplied_axes, show_axes=True)

    assert result_figure is figure
    assert result_axes is supplied_axes
    assert result_axes.get_xlabel() == "x [mm]"
    assert result_axes.get_ylabel() == "y [mm]"
    assert result_axes.axison
    plt.close(figure)


def test_plot_samples_curves_paths_and_raw_image() -> None:
    drawing = ezdwf.read(
        make_dwf(
            w2d=b"""(W2D V06.00)
            (Circle 10,20 5)
            (Bezier 1 0,0 1,2 3,2 4,0)
            (Image 'RGB' 7 1,1 0,0 2,2 (3 FF0000))
            (EndOfDWF)"""
        )
    )

    figure, axes = ezdwf.plot(drawing, curve_segments=24)

    assert any(isinstance(item, PolyCollection) for item in axes.collections)
    assert any(isinstance(item, PathPatch) for item in axes.patches)
    assert len(axes.images) == 1
    plt.close(figure)


def test_plot_batches_repeated_styles_without_per_path_properties() -> None:
    drawing = ezdwf.read(
        make_dwf(
            w2d=b"""(W2D V06.00)
            (Color 0,184,46,255)(LineWeight 20)
            (Line 0,0 5,5)(Line 1,0 6,5)
            (EndOfDWF)"""
        )
    )

    figure, axes = ezdwf.plot(drawing)

    lines = [item for item in axes.collections if isinstance(item, LineCollection)]
    assert len(lines) == 1
    assert len(lines[0].get_segments()) == 2
    assert len(lines[0].get_colors()) == 1
    assert len(lines[0].get_linewidths()) == 1
    plt.close(figure)


def test_plot_renders_dwfx_paths_text_and_first_clip() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="100" Height="50">
      <Path Data="M 0,0 L 40,0 L 40,40 L 0,40 Z"
            Fill="#4080C0" Stroke="#102030"
            Clip="M 5,5 L 30,5 L 30,30 L 5,30 Z"/>
      <Glyphs FontUri="../Resources/font.odttf" FontRenderingEmSize="8"
              OriginX="50" OriginY="20" UnicodeString="DWFx" Fill="#000000"/>
    </FixedPage>"""
    drawing = ezdwf.read(make_dwfx(fixed_page=page))

    figure, axes = ezdwf.plot(drawing)

    assert len(axes.patches) == 2
    assert all(patch.get_clip_path() is not None for patch in axes.patches)
    assert len(axes.texts) == 1
    plt.close(figure)


def test_plot_filters_markup_visibility_and_text() -> None:
    drawing = ezdwf.read(
        make_dwf(
            w2d=b"""(W2D V06.00)
            v (Line 0,0 5,5)
            V (Text 1,2 'shown')
            (EndOfDWF)"""
        )
    )

    figure, axes = ezdwf.plot(drawing, show_text=False)
    assert len(axes.collections) == 0
    assert len(axes.texts) == 0
    plt.close(figure)

    figure, axes = ezdwf.plot(
        drawing, include_invisible=True, show_text=False, monochrome=True
    )
    lines = [item for item in axes.collections if isinstance(item, LineCollection)]
    assert len(lines) == 1
    assert tuple(lines[0].get_colors()[0][:3]) == pytest.approx((0.08, 0.08, 0.08))
    plt.close(figure)


def test_plot_accepts_integer_palette_colors(dwf_bytes: bytes) -> None:
    source = ezdwf.read(dwf_bytes).modelspace().query("LINE").first
    assert source is not None
    entity = replace(
        source,
        style=replace(
            source.style,
            color=None,
            color_index=7,
            stroke_color=None,
        ),
    )

    figure, axes = ezdwf.plot(
        ezdwf.EntityQuery((entity,)),
        palette={7: (255, 0, 0, 128)},
        unresolved_color=(12, 34, 56),
    )

    lines = [item for item in axes.collections if isinstance(item, LineCollection)]
    assert len(lines) == 1
    assert tuple(lines[0].get_colors()[0]) == pytest.approx((1.0, 0.0, 0.0, 128 / 255))
    plt.close(figure)


def test_save_plot_writes_png_and_closes_figure(
    dwf_bytes: bytes, tmp_path: Path
) -> None:
    drawing = ezdwf.read(dwf_bytes)
    output = tmp_path / "preview.png"
    open_figures = set(plt.get_fignums())

    result = drawing.save_plot(output, dpi=96)

    assert result == output
    assert output.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    assert set(plt.get_fignums()) == open_figures


def test_plot_validates_options(dwf_bytes: bytes, tmp_path: Path) -> None:
    drawing = ezdwf.read(dwf_bytes)
    with pytest.raises(ValueError, match="at least 8"):
        ezdwf.plot(drawing, curve_segments=7)
    with pytest.raises(ValueError, match="non-negative"):
        ezdwf.plot(drawing, margin=-1)
    with pytest.raises(ValueError, match="positive"):
        ezdwf.plot(drawing, linewidth_scale=0)
    with pytest.raises(ValueError, match="greater than zero"):
        ezdwf.save_plot(drawing, tmp_path / "unused.png", dpi=0)
    with pytest.raises(TypeError, match="Drawing, Sheet, or EntityQuery"):
        ezdwf.plot(object())  # type: ignore[arg-type]


def test_cli_plot_writes_png(dwf_path: Path, tmp_path: Path, capsys) -> None:  # type: ignore[no-untyped-def]
    output = tmp_path / "cli-preview.png"

    assert (
        ezdwf.main(
            [
                "plot",
                str(dwf_path),
                str(output),
                "--sheet",
                "Fixture Sheet",
                "--dpi",
                "96",
                "--show-axes",
            ]
        )
        == 0
    )
    assert capsys.readouterr().out.strip() == f"wrote: {output}"
    assert output.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
