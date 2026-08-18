"""Optional Matplotlib preview rendering for normalized DWF drawings.

The parser model remains backend-neutral.  This module converts normalized
paper-space entities into Matplotlib artists without mutating or flattening
the source drawing.
"""

from __future__ import annotations

import math
from collections.abc import Mapping, Sequence
from io import BytesIO
from pathlib import Path
from typing import Any, TypeAlias, cast

from .document import (
    Bounds2D,
    Brush,
    ClipPath,
    Drawing,
    Entity,
    EntityQuery,
    GradientBrush,
    Image,
    PathFigure,
    PathSegment,
    Point2D,
    Sheet,
    SolidBrush,
)
from .svg import PaletteColor, _image_rgba_pixels

Rgb: TypeAlias = tuple[int, int, int]
RgbaFloat: TypeAlias = tuple[float, float, float, float]
LineStyle: TypeAlias = str | tuple[float, tuple[float, ...]]

_DASH_PATTERNS: dict[str, tuple[float, ...]] = {
    "dashed": (6.0, 3.0),
    "dotted": (1.0, 2.0),
    "dash_dot": (6.0, 2.0, 1.0, 2.0),
    "short_dash": (3.0, 2.0),
    "medium_dash": (6.0, 3.0),
    "long_dash": (10.0, 4.0),
    "long_dash_dot": (10.0, 3.0, 1.0, 3.0),
    "long_dash_dot_dot": (10.0, 3.0, 1.0, 3.0, 1.0, 3.0),
}


def plot(
    target: Drawing | Sheet | EntityQuery,
    *,
    sheet: int | str = 0,
    ax: Any | None = None,
    margin: float = 0.0,
    background: str | Rgb | None = None,
    monochrome: bool = False,
    include_invisible: bool = False,
    include_markup: bool = False,
    show_text: bool = True,
    show_axes: bool = False,
    curve_segments: int = 96,
    linewidth_scale: float = 1.0,
    palette: Mapping[int, PaletteColor] | None = None,
    unresolved_color: str | Rgb = "#000000",
) -> tuple[Any, Any]:
    """Render *target* and return a Matplotlib ``(figure, axes)`` pair.

    Matplotlib is imported lazily. Install the ``plot`` extra before calling
    this function::

        python -m pip install "ezdwf[plot]"

    Geometry remains in normalized, bottom-left-origin paper coordinates.
    Elliptical arcs are sampled only for this preview. Solid paints, opacity,
    line styles, text, raster entities, and the first clip in a clip chain are
    supported. Complex XPS gradient, image, and visual brushes use a stable
    representative-color preview; the SVG renderer remains the fidelity
    reference for those brush and compositing features.
    """

    if margin < 0.0 or not math.isfinite(margin):
        raise ValueError("margin must be a finite non-negative value")
    if curve_segments < 8:
        raise ValueError("curve_segments must be at least 8")
    if linewidth_scale <= 0.0 or not math.isfinite(linewidth_scale):
        raise ValueError("linewidth_scale must be a finite positive value")

    selected_sheet, entities = _resolve_target(target, sheet, include_markup)
    min_x, min_y, max_x, max_y = _render_bounds(selected_sheet, entities)
    min_x -= margin
    min_y -= margin
    max_x += margin
    max_y += margin
    if max_x <= min_x:
        min_x -= 0.5
        max_x += 0.5
    if max_y <= min_y:
        min_y -= 0.5
        max_y += 0.5

    matplotlib = _load_matplotlib()
    pyplot = matplotlib["pyplot"]
    colors = matplotlib["colors"]
    if ax is None:
        figure, ax = pyplot.subplots()
    else:
        figure = ax.figure

    background_rgba = _background_rgba(selected_sheet, background, colors=colors)
    if background_rgba is None:
        figure.patch.set_alpha(0.0)
        ax.patch.set_alpha(0.0)
        foreground = (0.0, 0.0, 0.0, 1.0)
    else:
        figure.patch.set_facecolor(background_rgba)
        ax.set_facecolor(background_rgba)
        foreground = (*_contrasting_color(background_rgba[:3]), 1.0)

    renderer = _Renderer(
        ax=ax,
        bounds=(min_x, min_y, max_x, max_y),
        monochrome=monochrome,
        foreground=foreground,
        curve_segments=curve_segments,
        linewidth_scale=linewidth_scale,
        palette=palette,
        unresolved_color=unresolved_color,
        matplotlib=matplotlib,
    )
    renderer.render(
        tuple(
            entity
            for entity in entities
            if (include_invisible or entity.style.visible)
            and (show_text or entity.kind != "TEXT")
        )
    )

    ax.set_xlim(min_x, max_x)
    ax.set_ylim(min_y, max_y)
    ax.set_aspect("equal", adjustable="box")
    ax.margins(0.0)
    _style_axes(ax, foreground, selected_sheet, show_axes)
    return figure, ax


def save_plot(
    target: Drawing | Sheet | EntityQuery,
    output: str | Path,
    *,
    dpi: int = 150,
    transparent: bool = False,
    **plot_options: Any,
) -> Path:
    """Render *target* to a Matplotlib-supported image and return its path."""

    if dpi <= 0:
        raise ValueError("dpi must be greater than zero")
    figure, _ = plot(target, **plot_options)
    try:
        path = Path(output)
        figure.savefig(
            path,
            dpi=dpi,
            facecolor=figure.get_facecolor(),
            transparent=transparent,
            bbox_inches="tight",
            pad_inches=0.0,
        )
        return path
    finally:
        _load_matplotlib()["pyplot"].close(figure)


class _Renderer:
    def __init__(
        self,
        *,
        ax: Any,
        bounds: Bounds2D,
        monochrome: bool,
        foreground: RgbaFloat,
        curve_segments: int,
        linewidth_scale: float,
        palette: Mapping[int, PaletteColor] | None,
        unresolved_color: str | Rgb,
        matplotlib: dict[str, Any],
    ) -> None:
        self.ax = ax
        self.bounds = bounds
        self.monochrome = monochrome
        self.foreground = foreground
        self.curve_segments = curve_segments
        self.linewidth_scale = linewidth_scale
        self.palette = palette
        self.unresolved_color = unresolved_color
        self.colors = matplotlib["colors"]
        self.image_read = matplotlib["image_read"]
        self.np = matplotlib["numpy"]
        self.font_manager = matplotlib["font_manager"]
        self.FontProperties = matplotlib["FontProperties"]
        self.LineCollection = matplotlib["LineCollection"]
        self.PolyCollection = matplotlib["PolyCollection"]
        self.Path = matplotlib["Path"]
        self.PathPatch = matplotlib["PathPatch"]

        width = max(bounds[2] - bounds[0], 1e-12)
        height = max(bounds[3] - bounds[1], 1e-12)
        figure_width, figure_height = ax.figure.get_size_inches()
        axes_box = ax.get_position()
        self.points_per_unit = max(
            min(
                figure_width * axes_box.width * 72.0 / width,
                figure_height * axes_box.height * 72.0 / height,
            ),
            1e-9,
        )
        self.fallback_width = max(width, height) / 1_000.0

        self._batch_kind: str | None = None
        self._batch_style: tuple[object, ...] | None = None
        self._batch_zorder = 1.0
        self._line_segments: list[tuple[tuple[float, float], ...]] = []
        self._line_colors: list[RgbaFloat] = []
        self._line_widths: list[float] = []
        self._line_styles: list[LineStyle] = []
        self._polygons: list[tuple[tuple[float, float], ...]] = []
        self._face_colors: list[RgbaFloat] = []
        self._edge_colors: list[RgbaFloat] = []
        self._polygon_widths: list[float] = []
        self._polygon_styles: list[LineStyle] = []
        self._zorder = 1.0
        self._font_families: dict[str, str] = {}

    def render(self, entities: Sequence[Entity]) -> None:
        for entity in entities:
            self._render_entity(entity)
            self._zorder += 1.0
        self._flush()

    def _render_entity(self, entity: Entity) -> None:
        kind = entity.kind
        if kind == "LINE" and len(entity.points) == 2:
            self._line(entity.points, entity)
            return
        if kind == "POLYLINE":
            self._line(entity.points, entity)
            return
        if kind == "POLYMARKER":
            self._polymarker(entity)
            return
        if kind == "POLYGON":
            self._polygon(entity.points, entity)
            return
        if kind == "POLYBEZIER" and len(entity.points) >= 4:
            self._polybezier(entity)
            return
        if kind == "PATH" and entity.path:
            self._path_entity(entity)
            return
        if kind in {"CIRCLE", "ARC", "ELLIPSE"}:
            points = _sample_ellipse(entity, self.curve_segments)
            if entity.closed:
                self._polygon(points, entity)
            else:
                self._line(points, entity)
            return
        if kind in {"POLYTRIANGLE", "TEXTURED_POLYTRIANGLE"}:
            for index in range(max(0, len(entity.points) - 2)):
                self._polygon(entity.points[index : index + 3], entity)
            return
        if kind == "CONTOUR_SET" and entity.contours:
            self._contour_set(entity)
            return
        if kind == "GOURAUD_POLYLINE":
            for index in range(max(0, len(entity.colored_points) - 1)):
                first, second = entity.colored_points[index : index + 2]
                self._line(
                    (first.point, second.point),
                    entity,
                    color=_average_rgba((first.color, second.color)),
                )
            return
        if kind == "GOURAUD_POLYTRIANGLE":
            for index in range(max(0, len(entity.colored_points) - 2)):
                triangle = entity.colored_points[index : index + 3]
                self._polygon(
                    tuple(item.point for item in triangle),
                    entity,
                    facecolor=_average_rgba(tuple(item.color for item in triangle)),
                )
            return
        if kind == "IMAGE" and entity.image is not None:
            self._image(entity)
            return
        if kind == "TEXT" and entity.points and entity.text is not None:
            self._text(entity)

    def _line(
        self,
        points: Sequence[Point2D],
        entity: Entity,
        *,
        color: RgbaFloat | None = None,
    ) -> None:
        if len(points) < 2:
            return
        stroke = color or self._paint(entity, fill=False)
        if stroke is None:
            return
        coordinates = tuple((point.x, point.y) for point in points)
        if self._requires_individual_artist(entity):
            path = self.Path(
                coordinates,
                [self.Path.MOVETO, *([self.Path.LINETO] * (len(coordinates) - 1))],
            )
            self._path_patch(
                path,
                entity,
                facecolor=(0.0, 0.0, 0.0, 0.0),
                edgecolor=stroke,
            )
            return
        linewidth = self._line_width(entity)
        line_style = self._line_style(entity, linewidth)
        self._prepare_batch("line", (stroke, linewidth, line_style))
        self._line_segments.append(coordinates)
        self._line_colors.append(stroke)
        self._line_widths.append(linewidth)
        self._line_styles.append(line_style)

    def _polymarker(self, entity: Entity) -> None:
        stroke = self._paint(entity, fill=False)
        if stroke is None or not entity.points:
            return
        linewidth = self._line_width(entity)
        self.ax.scatter(
            [point.x for point in entity.points],
            [point.y for point in entity.points],
            s=max(linewidth * 1.5, 1.0) ** 2,
            color=stroke,
            marker="o",
            linewidths=0,
            zorder=3,
        )

    def _polygon(
        self,
        points: Sequence[Point2D],
        entity: Entity,
        *,
        facecolor: RgbaFloat | None = None,
    ) -> None:
        if len(points) < 3:
            return
        face = facecolor or self._paint(entity, fill=True)
        edge = self._paint(entity, fill=False)
        transparent = (0.0, 0.0, 0.0, 0.0)
        face = face or transparent
        edge = edge or transparent
        coordinates = tuple((point.x, point.y) for point in points)
        if self._requires_individual_artist(entity):
            path = self.Path(
                (*coordinates, coordinates[0]),
                [
                    self.Path.MOVETO,
                    *([self.Path.LINETO] * (len(coordinates) - 1)),
                    self.Path.CLOSEPOLY,
                ],
            )
            self._path_patch(path, entity, facecolor=face, edgecolor=edge)
            return
        linewidth = self._line_width(entity) if edge[3] > 0.0 else 0.0
        line_style = self._line_style(entity, linewidth)
        self._prepare_batch("polygon", (face, edge, linewidth, line_style))
        self._polygons.append(coordinates)
        self._face_colors.append(face)
        self._edge_colors.append(edge)
        self._polygon_widths.append(linewidth)
        self._polygon_styles.append(line_style)

    def _polybezier(self, entity: Entity) -> None:
        vertices: list[tuple[float, float]] = [(entity.points[0].x, entity.points[0].y)]
        codes = [self.Path.MOVETO]
        for offset in range(1, len(entity.points) - 2, 3):
            vertices.extend(
                (point.x, point.y) for point in entity.points[offset : offset + 3]
            )
            codes.extend((self.Path.CURVE4,) * 3)
        self._path_patch(
            self.Path(vertices, codes),
            entity,
            facecolor=(0.0, 0.0, 0.0, 0.0),
            edgecolor=self._paint(entity, fill=False),
        )

    def _path_entity(self, entity: Entity) -> None:
        face = self._paint(entity, fill=True)
        edge = self._paint(entity, fill=False)
        fill_path = _matplotlib_path(
            entity.path,
            self.Path,
            curve_segments=self.curve_segments,
            mode="fill",
        )
        stroke_path = _matplotlib_path(
            entity.path,
            self.Path,
            curve_segments=self.curve_segments,
            mode="stroke",
        )
        if fill_path is not None and face is not None:
            self._path_patch(
                fill_path,
                entity,
                facecolor=face,
                edgecolor=(0.0, 0.0, 0.0, 0.0),
            )
        if stroke_path is not None and edge is not None:
            self._path_patch(
                stroke_path,
                entity,
                facecolor=(0.0, 0.0, 0.0, 0.0),
                edgecolor=edge,
            )

    def _contour_set(self, entity: Entity) -> None:
        figures = tuple(
            PathFigure(
                start=contour[0],
                segments=tuple(
                    PathSegment(kind="line", end=point) for point in contour[1:]
                ),
                closed=True,
                filled=True,
            )
            for contour in entity.contours
            if contour
        )
        path = _matplotlib_path(
            figures,
            self.Path,
            curve_segments=self.curve_segments,
            mode="fill",
        )
        if path is not None:
            self._path_patch(
                path,
                entity,
                facecolor=self._paint(entity, fill=True),
                edgecolor=self._paint(entity, fill=False),
            )

    def _text(self, entity: Entity) -> None:
        self._flush()
        fill = self._paint(entity, fill=True) or self._paint(entity, fill=False)
        if fill is None:
            return
        if entity.glyph_outline:
            path = _matplotlib_path(
                entity.glyph_outline,
                self.Path,
                curve_segments=self.curve_segments,
                mode="fill",
            )
            if path is not None:
                self._path_patch(
                    path,
                    entity,
                    facecolor=fill,
                    edgecolor=(
                        fill if entity.style.font_bold else (0.0, 0.0, 0.0, 0.0)
                    ),
                    linewidth=(
                        max(
                            (entity.style.font_height or 0.0)
                            * self.points_per_unit
                            * 0.02,
                            0.0,
                        )
                        if entity.style.font_bold
                        else 0.0
                    ),
                )
                return

        height = entity.style.font_height or self.fallback_width * 12.0
        font_size = max(height * self.points_per_unit, 1.0)
        family = self._font_family(
            entity.style.font_name or entity.style.font_canonical_name
        )
        text = self.ax.text(
            entity.points[0].x,
            entity.points[0].y,
            entity.text,
            color=fill,
            fontsize=font_size,
            fontfamily=family,
            fontweight="bold" if entity.style.font_bold else "normal",
            fontstyle="italic" if entity.style.font_italic else "normal",
            rotation=entity.style.font_rotation_degrees or 0.0,
            rotation_mode="anchor",
            horizontalalignment="left",
            verticalalignment="baseline",
            zorder=self._zorder,
        )
        self._apply_clip(text, entity)

    def _font_family(self, requested: str | None) -> str:
        if not requested:
            return "sans-serif"
        cached = self._font_families.get(requested)
        if cached is not None:
            return cached
        properties = self.FontProperties(family=[requested])
        try:
            self.font_manager.findfont(properties, fallback_to_default=False)
        except ValueError:
            resolved = "sans-serif"
        else:
            resolved = requested
        self._font_families[requested] = resolved
        return resolved

    def _image(self, entity: Entity) -> None:
        self._flush()
        image = cast(Image, entity.image)
        pixels: Any | None = None
        format_name = image.format.casefold()
        if (format_name == "png" and image.data.startswith(b"\x89PNG\r\n\x1a\n")) or (
            format_name in {"jpeg", "jpg"} and image.data.startswith(b"\xff\xd8")
        ):
            pixels = self.image_read(BytesIO(image.data), format=format_name)
        else:
            rgba = _image_rgba_pixels(image)
            if rgba is not None:
                pixels = self.np.frombuffer(rgba, dtype=self.np.uint8).reshape(
                    image.rows, image.columns, 4
                )

        left = min(image.min.x, image.max.x)
        right = max(image.min.x, image.max.x)
        bottom = min(image.min.y, image.max.y)
        top = max(image.min.y, image.max.y)
        if pixels is not None:
            artist = self.ax.imshow(
                pixels,
                extent=(left, right, bottom, top),
                origin="upper",
                interpolation="nearest",
                alpha=self._entity_opacity(entity),
                zorder=self._zorder,
            )
            self._apply_clip(artist, entity)
            return

        vertices = (
            (left, bottom),
            (right, bottom),
            (right, top),
            (left, top),
            (left, bottom),
        )
        path = self.Path(
            vertices,
            [
                self.Path.MOVETO,
                self.Path.LINETO,
                self.Path.LINETO,
                self.Path.LINETO,
                self.Path.CLOSEPOLY,
            ],
        )
        self._path_patch(
            path,
            entity,
            facecolor=(0.0, 0.0, 0.0, 0.0),
            edgecolor=self._paint(entity, fill=False),
        )

    def _path_patch(
        self,
        path: Any,
        entity: Entity,
        *,
        facecolor: RgbaFloat | None,
        edgecolor: RgbaFloat | None,
        linewidth: float | None = None,
    ) -> None:
        self._flush()
        transparent = (0.0, 0.0, 0.0, 0.0)
        edge = edgecolor or transparent
        patch = self.PathPatch(
            path,
            facecolor=facecolor or transparent,
            edgecolor=edge,
            linewidth=(
                self._line_width(entity)
                if linewidth is None and edge[3] > 0.0
                else linewidth or 0.0
            ),
            linestyle=self._line_style(entity, self._line_width(entity)),
            capstyle="round",
            joinstyle="round",
            zorder=self._zorder,
        )
        self._apply_clip(patch, entity)
        self.ax.add_patch(patch)

    def _apply_clip(self, artist: Any, entity: Entity) -> None:
        clip = _first_clip(entity)
        if clip is None:
            return
        path = _matplotlib_path(
            clip.figures,
            self.Path,
            curve_segments=self.curve_segments,
            mode="fill",
        )
        if path is not None:
            artist.set_clip_path(path, self.ax.transData)

    def _paint(self, entity: Entity, *, fill: bool) -> RgbaFloat | None:
        style = entity.style
        brush = style.fill_brush if fill else style.stroke_brush
        specific = style.fill_color if fill else style.stroke_color
        is_xps = entity.resource_role == "xps fixed page"
        if fill and not style.fill:
            return None
        if (
            fill
            and is_xps
            and specific is None
            and brush is None
            and style.fill_image is None
        ):
            return None
        if not fill and is_xps and specific is None and brush is None:
            return None

        opacity = self._entity_opacity(entity)
        if self.monochrome:
            return (*self.foreground[:3], opacity)

        brush_color = self._brush_color(brush)
        if brush_color is not None:
            return (*brush_color[:3], brush_color[3] * opacity)
        if specific is not None:
            return _rgba_float(specific, opacity=opacity)
        if style.color is not None:
            return _rgba_float(style.color, opacity=opacity)
        if style.color_index is not None and self.palette is not None:
            value = self.palette.get(style.color_index, self.unresolved_color)
        else:
            value = self.unresolved_color
        red, green, blue, alpha = _matplotlib_rgba(value, colors=self.colors)
        return (red, green, blue, alpha * opacity)

    def _brush_color(self, brush: Brush | None) -> RgbaFloat | None:
        if isinstance(brush, SolidBrush):
            return _rgba_float(brush.color, opacity=brush.opacity)
        if isinstance(brush, GradientBrush):
            colors = [
                _rgba_float(stop.color)
                for stop in brush.gradient_stops
                if stop.color is not None
            ]
            if colors:
                count = len(colors)
                average = cast(
                    RgbaFloat,
                    tuple(
                        sum(color[channel] for color in colors) / count
                        for channel in range(4)
                    ),
                )
                return (
                    average[0],
                    average[1],
                    average[2],
                    average[3] * brush.opacity,
                )
        return None

    def _entity_opacity(self, entity: Entity) -> float:
        opacity = entity.style.opacity
        for group in entity.compositing_groups:
            opacity *= group.opacity
            if isinstance(group.opacity_mask, SolidBrush):
                opacity *= group.opacity_mask.opacity
                opacity *= group.opacity_mask.color[3] / 255.0
        for mask in entity.opacity_masks:
            if isinstance(mask, SolidBrush):
                opacity *= mask.opacity
                opacity *= mask.color[3] / 255.0
        return min(max(opacity, 0.0), 1.0)

    def _line_width(self, entity: Entity) -> float:
        width = entity.style.nominal_stroke_width
        if width is None or width <= 0.0 or not math.isfinite(width):
            width = self.fallback_width
        # Sub-pixel strokes disappear entirely in raster backends after a large
        # paper sheet is fitted into a normal figure. Keep a hairline floor
        # while preserving larger physical line weights proportionally.
        return max(width * self.points_per_unit * self.linewidth_scale, 0.5)

    def _line_style(self, entity: Entity, linewidth: float) -> LineStyle:
        style = entity.style
        if style.stroke_dash_array:
            dash = tuple(
                max(value * self.points_per_unit * self.linewidth_scale, 0.01)
                for value in style.stroke_dash_array
            )
            offset = (
                style.stroke_dash_offset * self.points_per_unit * self.linewidth_scale
            )
            return (offset, dash)
        pattern = _DASH_PATTERNS.get((style.line_pattern or "").casefold())
        if pattern:
            return (0.0, tuple(max(value * linewidth, 0.01) for value in pattern))
        return "solid"

    @staticmethod
    def _requires_individual_artist(entity: Entity) -> bool:
        return bool(
            entity.clips
            or entity.local_clips
            or entity.opacity_masks
            or entity.compositing_groups
        )

    def _prepare_batch(self, kind: str, style: tuple[object, ...]) -> None:
        if self._batch_kind != kind or self._batch_style != style:
            self._flush()
            self._batch_kind = kind
            self._batch_style = style
            self._batch_zorder = self._zorder

    def _flush(self) -> None:
        if self._batch_kind == "line" and self._line_segments:
            collection = self.LineCollection(
                self._line_segments,
                colors=[self._line_colors[0]],
                linewidths=[self._line_widths[0]],
                linestyles=[self._line_styles[0]],
                capstyle="round",
                joinstyle="round",
                zorder=self._batch_zorder,
            )
            self.ax.add_collection(collection, autolim=False)
        elif self._batch_kind == "polygon" and self._polygons:
            collection = self.PolyCollection(
                self._polygons,
                closed=True,
                facecolors=[self._face_colors[0]],
                edgecolors=[self._edge_colors[0]],
                linewidths=[self._polygon_widths[0]],
                linestyles=[self._polygon_styles[0]],
                antialiaseds=True,
                zorder=self._batch_zorder,
            )
            self.ax.add_collection(collection, autolim=False)

        self._batch_kind = None
        self._batch_style = None
        self._line_segments.clear()
        self._line_colors.clear()
        self._line_widths.clear()
        self._line_styles.clear()
        self._polygons.clear()
        self._face_colors.clear()
        self._edge_colors.clear()
        self._polygon_widths.clear()
        self._polygon_styles.clear()


def _matplotlib_path(
    figures: Sequence[PathFigure],
    path_type: Any,
    *,
    curve_segments: int,
    mode: str,
) -> Any | None:
    vertices: list[tuple[float, float]] = []
    codes: list[int] = []
    for figure in figures:
        if mode == "fill" and not figure.filled:
            continue
        current = figure.start
        run_open = False
        all_stroked = all(segment.stroked for segment in figure.segments)
        if mode == "fill":
            vertices.append((current.x, current.y))
            codes.append(path_type.MOVETO)
            run_open = True
        for segment in figure.segments:
            if mode == "stroke" and not segment.stroked:
                run_open = False
                current = segment.end
                continue
            if not run_open:
                vertices.append((current.x, current.y))
                codes.append(path_type.MOVETO)
                run_open = True
            _append_segment(
                vertices,
                codes,
                segment,
                path_type,
                curve_segments=curve_segments,
            )
            current = segment.end
        if figure.closed:
            if mode == "fill" or (mode == "stroke" and all_stroked):
                vertices.append((figure.start.x, figure.start.y))
                codes.append(path_type.CLOSEPOLY)
            elif mode == "stroke":
                if not run_open:
                    vertices.append((current.x, current.y))
                    codes.append(path_type.MOVETO)
                vertices.append((figure.start.x, figure.start.y))
                codes.append(path_type.LINETO)
    return path_type(vertices, codes) if vertices else None


def _append_segment(
    vertices: list[tuple[float, float]],
    codes: list[int],
    segment: PathSegment,
    path_type: Any,
    *,
    curve_segments: int,
) -> None:
    if (
        segment.kind == "cubic_bezier"
        and segment.control1 is not None
        and segment.control2 is not None
    ):
        vertices.extend(
            (point.x, point.y)
            for point in (segment.control1, segment.control2, segment.end)
        )
        codes.extend((path_type.CURVE4,) * 3)
        return
    if segment.kind == "quadratic_bezier" and segment.control is not None:
        vertices.extend((point.x, point.y) for point in (segment.control, segment.end))
        codes.extend((path_type.CURVE3,) * 2)
        return
    if segment.kind == "elliptical_arc":
        sampled = _sample_path_arc(segment, curve_segments)
        vertices.extend((point.x, point.y) for point in sampled[1:])
        codes.extend((path_type.LINETO,) * max(0, len(sampled) - 1))
        return
    vertices.append((segment.end.x, segment.end.y))
    codes.append(path_type.LINETO)


def _sample_ellipse(entity: Entity, curve_segments: int) -> tuple[Point2D, ...]:
    if entity.center is None or entity.x_axis is None or entity.y_axis is None:
        return ()
    start = entity.start_angle_degrees or 0.0
    if entity.closed:
        span = 360.0
    else:
        if entity.end_angle_degrees is None:
            return ()
        span = entity.end_angle_degrees - start
        while span <= 0.0:
            span += 360.0
    count = max(2, math.ceil(abs(span) / 360.0 * curve_segments) + 1)
    return tuple(
        Point2D(
            entity.center.x
            + entity.x_axis.x * math.cos(angle)
            + entity.y_axis.x * math.sin(angle),
            entity.center.y
            + entity.x_axis.y * math.cos(angle)
            + entity.y_axis.y * math.sin(angle),
        )
        for angle in (
            math.radians(start + span * index / (count - 1)) for index in range(count)
        )
    )


def _sample_path_arc(segment: PathSegment, curve_segments: int) -> tuple[Point2D, ...]:
    if (
        segment.center is None
        or segment.x_axis is None
        or segment.y_axis is None
        or segment.start_angle_degrees is None
        or segment.sweep_angle_degrees is None
    ):
        return (segment.end,)
    count = max(
        2,
        math.ceil(abs(segment.sweep_angle_degrees) / 360.0 * curve_segments) + 1,
    )
    points = [
        Point2D(
            segment.center.x
            + segment.x_axis.x * math.cos(angle)
            + segment.y_axis.x * math.sin(angle),
            segment.center.y
            + segment.x_axis.y * math.cos(angle)
            + segment.y_axis.y * math.sin(angle),
        )
        for angle in (
            math.radians(
                segment.start_angle_degrees
                + segment.sweep_angle_degrees * index / (count - 1)
            )
            for index in range(count)
        )
    ]
    points[-1] = segment.end
    return tuple(points)


def _resolve_target(
    target: Drawing | Sheet | EntityQuery, sheet: int | str, include_markup: bool
) -> tuple[Sheet | None, EntityQuery]:
    if isinstance(target, Drawing):
        selected = target.sheet(sheet)
        return selected, selected.all_entities if include_markup else selected.entities
    if isinstance(target, Sheet):
        return target, target.all_entities if include_markup else target.entities
    if isinstance(target, EntityQuery):
        return None, target
    raise TypeError("plot() requires a Drawing, Sheet, or EntityQuery")


def _render_bounds(sheet: Sheet | None, entities: EntityQuery) -> Bounds2D:
    bounds = sheet.paper_bounds if sheet is not None else None
    if bounds is None:
        bounds = entities.bbox()
    return bounds or (0.0, 0.0, 1.0, 1.0)


def _background_rgba(
    sheet: Sheet | None,
    value: str | Rgb | None,
    *,
    colors: Any,
) -> RgbaFloat | None:
    if value is None:
        source: str | tuple[float, float, float] = tuple(
            component / 255.0
            for component in (
                (sheet.background_color if sheet else None) or (255, 255, 255)
            )
        )
    elif isinstance(value, str) and value.casefold() in {"none", "transparent"}:
        return None
    elif isinstance(value, str):
        source = value
    else:
        source = tuple(component / 255.0 for component in _validate_rgb(value))
    return cast(RgbaFloat, colors.to_rgba(source))


def _first_clip(entity: Entity) -> ClipPath | None:
    if entity.local_clips:
        return entity.local_clips[0]
    if entity.clips:
        return entity.clips[0]
    for group in entity.compositing_groups:
        if group.clip is not None:
            return group.clip
    return None


def _average_rgba(colors: Sequence[tuple[int, int, int, int]]) -> RgbaFloat:
    count = max(len(colors), 1)
    return cast(
        RgbaFloat,
        tuple(
            sum(color[channel] for color in colors) / (255.0 * count)
            for channel in range(4)
        ),
    )


def _rgba_float(color: tuple[int, int, int, int], *, opacity: float = 1.0) -> RgbaFloat:
    if len(color) != 4 or any(not 0 <= value <= 255 for value in color):
        raise ValueError("RGBA color must contain four channels between 0 and 255")
    return (
        color[0] / 255.0,
        color[1] / 255.0,
        color[2] / 255.0,
        color[3] / 255.0 * opacity,
    )


def _matplotlib_rgba(color: str | Sequence[int], *, colors: Any) -> RgbaFloat:
    if isinstance(color, str):
        return cast(RgbaFloat, colors.to_rgba(color))
    channels = tuple(int(value) for value in color)
    if len(channels) not in {3, 4} or any(not 0 <= value <= 255 for value in channels):
        raise ValueError(
            "palette colors must contain three or four channels between 0 and 255"
        )
    alpha = channels[3] / 255.0 if len(channels) == 4 else 1.0
    return (channels[0] / 255.0, channels[1] / 255.0, channels[2] / 255.0, alpha)


def _validate_rgb(color: Sequence[int]) -> Rgb:
    channels = tuple(int(value) for value in color)
    if len(channels) != 3 or any(not 0 <= value <= 255 for value in channels):
        raise ValueError("color must contain three integer channels between 0 and 255")
    return cast(Rgb, channels)


def _contrasting_color(background: Sequence[float]) -> tuple[float, float, float]:
    luminance = 0.2126 * background[0] + 0.7152 * background[1] + 0.0722 * background[2]
    return (0.08, 0.08, 0.08) if luminance > 0.5 else (0.92, 0.92, 0.92)


def _style_axes(
    ax: Any,
    foreground: RgbaFloat,
    sheet: Sheet | None,
    show_axes: bool,
) -> None:
    if not show_axes:
        ax.set_axis_off()
        return
    suffix = f" [{sheet.units}]" if sheet is not None and sheet.units else ""
    ax.set_xlabel(f"x{suffix}", color=foreground)
    ax.set_ylabel(f"y{suffix}", color=foreground)
    ax.tick_params(colors=foreground)
    for spine in ax.spines.values():
        spine.set_color(foreground)


def _load_matplotlib() -> dict[str, Any]:
    try:
        import numpy
        from matplotlib import colors, font_manager, pyplot
        from matplotlib.collections import LineCollection, PolyCollection
        from matplotlib.font_manager import FontProperties
        from matplotlib.image import imread
        from matplotlib.patches import PathPatch
        from matplotlib.path import Path as MplPath
    except ImportError as error:  # pragma: no cover - depends on optional package
        raise ImportError(
            "plotting requires Matplotlib; install it with "
            "`python -m pip install 'ezdwf[plot]'`"
        ) from error
    return {
        "LineCollection": LineCollection,
        "FontProperties": FontProperties,
        "Path": MplPath,
        "PathPatch": PathPatch,
        "PolyCollection": PolyCollection,
        "colors": colors,
        "font_manager": font_manager,
        "image_read": imread,
        "numpy": numpy,
        "pyplot": pyplot,
    }


__all__ = ["plot", "save_plot"]
