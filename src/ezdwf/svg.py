"""Deterministic, dependency-free SVG reference rendering."""

from __future__ import annotations

import base64
import binascii
import html
import math
import os
import struct
import zlib
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import TypeAlias, cast

from .document import (
    Bounds2D,
    Brush,
    ClipPath,
    CompositingGroup,
    Drawing,
    Entity,
    EntityQuery,
    GradientBrush,
    Image,
    ImageBrush,
    PathFigure,
    PathSegment,
    Point2D,
    Sheet,
    SolidBrush,
    UnsupportedBrush,
    VisualBrush,
)

Rgb: TypeAlias = tuple[int, int, int]
Rgba: TypeAlias = tuple[int, int, int, int]
PaletteColor: TypeAlias = Rgb | Rgba

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


def render_svg(
    target: Drawing | Sheet | EntityQuery,
    *,
    sheet: int | str = 0,
    margin: float = 0.0,
    background: str | Rgb | None = None,
    monochrome: bool = False,
    include_invisible: bool = False,
    include_markup: bool = False,
    show_text: bool = True,
    curve_segments: int = 96,
    precision: int = 6,
    palette: Mapping[int, PaletteColor] | None = None,
    unresolved_color: str | Rgb = "#000000",
) -> str:
    """Render normalized paper-space entities as a stable SVG document.

    Ellipses and arcs are sampled only in this preview layer. Their exact
    transformed axes and source W2D operands remain intact on each entity.
    Indexed colors without a decoded source palette use ``unresolved_color``;
    callers can supply an explicit ``palette`` mapping when available.
    """

    if margin < 0.0 or not math.isfinite(margin):
        raise ValueError("margin must be a finite non-negative value")
    if curve_segments < 8:
        raise ValueError("curve_segments must be at least 8")
    if not 0 <= precision <= 15:
        raise ValueError("precision must be between 0 and 15")

    selected_sheet, entities = _resolve_target(target, sheet, include_markup)
    bounds = _render_bounds(selected_sheet, entities)
    min_x, min_y, max_x, max_y = bounds
    min_x -= margin
    min_y -= margin
    max_x += margin
    max_y += margin
    width = max_x - min_x
    height = max_y - min_y
    if width <= 0.0:
        min_x -= 0.5
        max_x += 0.5
        width = 1.0
    if height <= 0.0:
        min_y -= 0.5
        max_y += 0.5
        height = 1.0

    formatter = _Formatter(precision)
    background_value = _background_color(selected_sheet, background)
    fallback_width = max(width, height) / 1_000.0
    if fallback_width <= 0.0:
        fallback_width = 0.001

    root_attributes = {
        "xmlns": "http://www.w3.org/2000/svg",
        "version": "1.1",
        "viewBox": f"0 0 {formatter.number(width)} {formatter.number(height)}",
        "width": formatter.number(width),
        "height": formatter.number(height),
        "preserveAspectRatio": "xMidYMid meet",
        "data-coordinate-space": "eplot-paper",
    }
    if selected_sheet is not None:
        root_attributes["data-sheet"] = selected_sheet.name
        if selected_sheet.units:
            root_attributes["data-units"] = selected_sheet.units

    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        f"<svg{_attributes(root_attributes)}>",
    ]
    if background_value is not None:
        lines.append(
            "  <rect"
            + _attributes(
                {
                    "id": "paper",
                    "x": "0",
                    "y": "0",
                    "width": formatter.number(width),
                    "height": formatter.number(height),
                    "fill": background_value,
                }
            )
            + "/>"
        )
    lines.append('  <g id="entities">')
    lines.extend(
        _render_entities(
            entities,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            curve_segments=curve_segments,
            fallback_width=fallback_width,
            monochrome=monochrome,
            palette=palette,
            unresolved_color=unresolved_color,
            include_invisible=include_invisible,
            show_text=show_text,
        )
    )
    lines.extend(("  </g>", "</svg>"))
    return "\n".join(lines) + "\n"


def save_svg(
    target: Drawing | Sheet | EntityQuery,
    output: str | os.PathLike[str],
    **options: object,
) -> Path:
    """Render *target* to UTF-8 SVG and return the output path."""

    path = Path(output)
    path.write_text(render_svg(target, **options), encoding="utf-8")
    return path


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
    raise TypeError("render_svg() requires a Drawing, Sheet, or EntityQuery")


def _render_bounds(sheet: Sheet | None, entities: EntityQuery) -> Bounds2D:
    bounds = sheet.paper_bounds if sheet is not None else None
    if bounds is None:
        bounds = entities.bbox()
    return bounds or (0.0, 0.0, 1.0, 1.0)


def _background_color(sheet: Sheet | None, value: str | Rgb | None) -> str | None:
    if value is None:
        color = sheet.background_color if sheet is not None else (255, 255, 255)
        return _rgb_hex(color or (255, 255, 255))
    if isinstance(value, str):
        return None if value.casefold() in {"none", "transparent"} else value
    return _rgb_hex(_validate_channels(value, expected=3))


def _render_entities(
    entities: Sequence[Entity],
    *,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
    fallback_width: float,
    monochrome: bool,
    palette: Mapping[int, PaletteColor] | None,
    unresolved_color: str | Rgb,
    include_invisible: bool,
    show_text: bool,
    id_prefix: str = "",
    mask: bool = False,
) -> list[str]:
    rendered = [
        (index, entity)
        for index, entity in enumerate(entities)
        if (include_invisible or entity.style.visible)
        and (show_text or entity.kind != "TEXT")
    ]
    group_bounds: dict[str, Bounds2D] = {}
    for _, entity in rendered:
        bounds = _painted_entity_bounds(entity)
        if bounds is None:
            continue
        for group in entity.compositing_groups:
            key = f"{id_prefix}{group.id}"
            group_bounds[key] = _merge_bounds(group_bounds.get(key), bounds)

    output: list[str] = []
    active: list[CompositingGroup] = []
    for index, entity in rendered:
        groups = list(entity.compositing_groups)
        common = 0
        while (
            common < len(active)
            and common < len(groups)
            and active[common].id == groups[common].id
        ):
            common += 1
        for _ in range(len(active) - common):
            output.append("    " + "  " * (len(active) - 1) + "</g>")
            active.pop()
        for group in groups[common:]:
            key = f"{id_prefix}{group.id}"
            indent = "    " + "  " * len(active)
            definitions = _compositing_group_definitions(
                group,
                key=key,
                bounds=group_bounds.get(key),
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
                curve_segments=curve_segments,
            )
            output.extend(indent + line.lstrip() for line in definitions)
            output.append(
                indent
                + "<g"
                + _attributes(
                    {
                        "id": f"canvas-{key}",
                        "data-xps-canvas": group.name,
                        "opacity": formatter.number(group.opacity)
                        if group.opacity < 1.0
                        else None,
                        "clip-path": f"url(#clip-canvas-{key}-0)"
                        if group.clip is not None
                        else None,
                        "mask": f"url(#canvas-mask-{key})"
                        if group.opacity_mask is not None
                        else None,
                        "style": "isolation:isolate",
                    }
                )
                + ">"
            )
            active.append(group)
        content = _render_entity(
            entity,
            index=f"{id_prefix}{index}",
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            curve_segments=curve_segments,
            fallback_width=fallback_width,
            monochrome=monochrome,
            palette=palette,
            unresolved_color=unresolved_color,
            mask=mask,
        )
        indent = "    " + "  " * len(active)
        output.extend(indent + line.lstrip() for line in content)
    while active:
        output.append("    " + "  " * (len(active) - 1) + "</g>")
        active.pop()
    return output


def _painted_entity_bounds(entity: Entity) -> Bounds2D | None:
    bounds = entity.bbox()
    if bounds is None:
        return None
    padding = max(entity.style.nominal_stroke_width or 0.0, 0.0) / 2.0
    if entity.glyph_outline and entity.style.font_bold:
        padding = max(padding, (entity.style.font_height or 0.0) * 0.01)
    if padding == 0.0:
        return bounds
    left, bottom, right, top = bounds
    return (left - padding, bottom - padding, right + padding, top + padding)


def _merge_bounds(first: Bounds2D | None, second: Bounds2D) -> Bounds2D:
    if first is None:
        return second
    return (
        min(first[0], second[0]),
        min(first[1], second[1]),
        max(first[2], second[2]),
        max(first[3], second[3]),
    )


def _compositing_group_definitions(
    group: CompositingGroup,
    *,
    key: str,
    bounds: Bounds2D | None,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
) -> list[str]:
    output: list[str] = []
    if group.clip is not None:
        output.extend(
            _clip_definitions(
                (group.clip,),
                index=f"canvas-{key}",
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
                curve_segments=curve_segments,
            )
        )
    if group.opacity_mask is not None:
        output.extend(
            _opacity_mask_definition(
                group.opacity_mask,
                identifier=f"canvas-mask-{key}",
                bounds=bounds,
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
            )
        )
    return output


def _path_segment_commands(
    segment: PathSegment,
    *,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
) -> list[str]:
    if segment.kind == "line":
        return [f"L {formatter.point(_screen(segment.end, min_x, max_y))}"]
    if (
        segment.kind == "cubic_bezier"
        and segment.control1 is not None
        and segment.control2 is not None
    ):
        return [
            "C "
            + " ".join(
                formatter.point(_screen(point, min_x, max_y))
                for point in (segment.control1, segment.control2, segment.end)
            )
        ]
    if segment.kind == "quadratic_bezier" and segment.control is not None:
        return [
            "Q "
            + " ".join(
                formatter.point(_screen(point, min_x, max_y))
                for point in (segment.control, segment.end)
            )
        ]
    if segment.kind == "elliptical_arc":
        sampled = _sample_path_arc(segment, curve_segments)
        return [
            f"L {formatter.point(_screen(point, min_x, max_y))}"
            for point in sampled[1:]
        ]
    return [f"L {formatter.point(_screen(segment.end, min_x, max_y))}"]


def _path_commands(
    figures: Sequence[PathFigure],
    *,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
    filled_only: bool = False,
) -> list[str]:
    commands: list[str] = []
    for figure in figures:
        if filled_only and not figure.filled:
            continue
        commands.append(f"M {formatter.point(_screen(figure.start, min_x, max_y))}")
        for segment in figure.segments:
            commands.extend(
                _path_segment_commands(
                    segment,
                    min_x=min_x,
                    max_y=max_y,
                    formatter=formatter,
                    curve_segments=curve_segments,
                )
            )
        if figure.closed:
            commands.append("Z")
    return commands


def _stroke_path_commands(
    figures: Sequence[PathFigure],
    *,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
) -> list[str]:
    commands: list[str] = []
    for figure in figures:
        current = figure.start
        run_open = False
        all_stroked = all(segment.stroked for segment in figure.segments)
        for segment in figure.segments:
            if segment.stroked:
                if not run_open:
                    commands.append(
                        f"M {formatter.point(_screen(current, min_x, max_y))}"
                    )
                commands.extend(
                    _path_segment_commands(
                        segment,
                        min_x=min_x,
                        max_y=max_y,
                        formatter=formatter,
                        curve_segments=curve_segments,
                    )
                )
                run_open = True
            else:
                run_open = False
            current = segment.end
        if figure.closed:
            if all_stroked and figure.segments:
                commands.append("Z")
            else:
                if not run_open:
                    commands.append(
                        f"M {formatter.point(_screen(current, min_x, max_y))}"
                    )
                commands.append(
                    f"L {formatter.point(_screen(figure.start, min_x, max_y))}"
                )
    return commands


def _clip_definitions(
    clips: Sequence[ClipPath],
    *,
    index: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
) -> list[str]:
    output = ["    <defs>"]
    for clip_index, clip in enumerate(clips):
        commands = _path_commands(
            clip.figures,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            curve_segments=curve_segments,
            filled_only=True,
        )
        output.append(
            "      <clipPath"
            + _attributes(
                {
                    "id": f"clip-{index}-{clip_index}",
                    "clipPathUnits": "userSpaceOnUse",
                }
            )
            + ">"
        )
        if commands:
            output.append(
                "        <path"
                + _attributes(
                    {
                        "d": " ".join(commands),
                        "fill-rule": "nonzero"
                        if clip.fill_rule == "nonzero"
                        else "evenodd",
                        "clip-rule": "nonzero"
                        if clip.fill_rule == "nonzero"
                        else "evenodd",
                    }
                )
                + "/>"
            )
        output.append("      </clipPath>")
    output.append("    </defs>")
    return output


def _render_entity(
    entity: Entity,
    *,
    index: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
    fallback_width: float,
    monochrome: bool,
    palette: Mapping[int, PaletteColor] | None,
    unresolved_color: str | Rgb,
    mask: bool = False,
) -> list[str]:
    content = _render_entity_unclipped(
        entity,
        index=index,
        min_x=min_x,
        max_y=max_y,
        formatter=formatter,
        curve_segments=curve_segments,
        fallback_width=fallback_width,
        monochrome=monochrome,
        palette=palette,
        unresolved_color=unresolved_color,
        mask=mask,
    )
    clips = entity.local_clips if entity.compositing_groups else entity.clips
    opacity_masks = (
        entity.local_opacity_masks
        if entity.compositing_groups
        else entity.opacity_masks
    )
    if not content or (not clips and not opacity_masks):
        return content

    output: list[str] = []
    if clips:
        output.extend(
            _clip_definitions(
                clips,
                index=index,
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
                curve_segments=curve_segments,
            )
        )
    if opacity_masks:
        output.extend(
            _opacity_mask_definitions(
                entity,
                opacity_masks=opacity_masks,
                index=index,
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
            )
        )
    indent = "    "
    for clip_index in range(len(clips)):
        output.append(
            indent
            + "<g"
            + _attributes({"clip-path": f"url(#clip-{index}-{clip_index})"})
            + ">"
        )
        indent += "  "
    for mask_index in range(len(opacity_masks)):
        output.append(
            indent
            + "<g"
            + _attributes({"mask": f"url(#opacity-mask-{index}-{mask_index})"})
            + ">"
        )
        indent += "  "
    output.extend(indent + line.lstrip() for line in content)
    for _ in reversed(opacity_masks):
        indent = indent[:-2]
        output.append(indent + "</g>")
    for _ in reversed(clips):
        indent = indent[:-2]
        output.append(indent + "</g>")
    return output


def _render_entity_unclipped(
    entity: Entity,
    *,
    index: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    curve_segments: int,
    fallback_width: float,
    monochrome: bool,
    palette: Mapping[int, PaletteColor] | None,
    unresolved_color: str | Rgb,
    mask: bool,
) -> list[str]:
    base = {
        "id": f"e{index}",
        "data-type": entity.kind,
        "data-layer": entity.layer,
        "data-source-offset": str(entity.source.offset),
        "data-markup": "true" if entity.is_markup else None,
    }
    style = _style_attributes(
        entity,
        formatter=formatter,
        fallback_width=fallback_width,
        monochrome=monochrome,
        palette=palette,
        unresolved_color=unresolved_color,
    )
    if mask:
        for role in ("fill", "stroke"):
            if style.get(role) != "none":
                style[role] = "#ffffff"
    prefix: list[str] = []
    brush_pairs = (
        ("fill", entity.style.fill_brush),
        ("stroke", entity.style.stroke_brush),
    )
    for role, brush in brush_pairs:
        if brush is None or isinstance(brush, SolidBrush) or monochrome:
            continue
        definition, paint, paint_opacity = _brush_definition(
            brush,
            identifier=f"{role}-brush-{index}",
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            mask=mask,
        )
        if definition and paint is not None:
            prefix.extend(definition)
            style[role] = paint
            style[f"{role}-opacity"] = (
                formatter.number(paint_opacity)
                if paint_opacity is not None and paint_opacity < 1.0
                else None
            )
    if entity.kind == "LINE" and len(entity.points) == 2:
        start, end = (_screen(point, min_x, max_y) for point in entity.points)
        return [
            "    <line"
            + _attributes(
                {
                    **base,
                    **style,
                    "x1": formatter.number(start.x),
                    "y1": formatter.number(start.y),
                    "x2": formatter.number(end.x),
                    "y2": formatter.number(end.y),
                    "fill": "none",
                }
            )
            + "/>"
        ]
    if entity.kind in {"POLYLINE", "POLYGON"}:
        tag = "polygon" if entity.kind == "POLYGON" else "polyline"
        return [
            f"    <{tag}"
            + _attributes(
                {
                    **base,
                    **style,
                    "points": _points(entity.points, min_x, max_y, formatter),
                    "fill": style["fill"] if tag == "polygon" else "none",
                }
            )
            + "/>"
        ]
    if entity.kind == "POLYBEZIER" and len(entity.points) >= 4:
        points = tuple(_screen(point, min_x, max_y) for point in entity.points)
        path = [f"M {formatter.point(points[0])}"]
        for offset in range(1, len(points) - 2, 3):
            path.append(
                "C "
                + " ".join(
                    formatter.point(point) for point in points[offset : offset + 3]
                )
            )
        return [
            "    <path"
            + _attributes({**base, **style, "d": " ".join(path), "fill": "none"})
            + "/>"
        ]
    if entity.kind == "PATH" and entity.path:
        commands = _path_commands(
            entity.path,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            curve_segments=curve_segments,
        )
        has_sampled_arcs = any(
            segment.kind == "elliptical_arc"
            for figure in entity.path
            for segment in figure.segments
        )
        has_smooth_joins = any(
            segment.smooth_join for figure in entity.path for segment in figure.segments
        )
        has_partial_paint = any(
            not figure.filled or any(not segment.stroked for segment in figure.segments)
            for figure in entity.path
        )
        if commands:
            if has_partial_paint:
                fill_commands = _path_commands(
                    entity.path,
                    min_x=min_x,
                    max_y=max_y,
                    formatter=formatter,
                    curve_segments=curve_segments,
                    filled_only=True,
                )
                stroke_commands = _stroke_path_commands(
                    entity.path,
                    min_x=min_x,
                    max_y=max_y,
                    formatter=formatter,
                    curve_segments=curve_segments,
                )
                group = {
                    **base,
                    "opacity": style.get("opacity"),
                    "data-preview": "sampled-arcs" if has_sampled_arcs else None,
                    "data-xps-smooth-join": "preserved" if has_smooth_joins else None,
                }
                component_style = {
                    name: value for name, value in style.items() if name != "opacity"
                }
                output = prefix + ["    <g" + _attributes(group) + ">"]
                if fill_commands and style["fill"] != "none":
                    output.append(
                        "      <path"
                        + _attributes(
                            {
                                **component_style,
                                "d": " ".join(fill_commands),
                                "stroke": "none",
                                "fill-rule": "nonzero"
                                if entity.fill_rule == "nonzero"
                                else "evenodd",
                                "data-xps-component": "fill",
                            }
                        )
                        + "/>"
                    )
                if stroke_commands and style["stroke"] != "none":
                    output.append(
                        "      <path"
                        + _attributes(
                            {
                                **component_style,
                                "d": " ".join(stroke_commands),
                                "fill": "none",
                                "data-xps-component": "stroke",
                            }
                        )
                        + "/>"
                    )
                output.append("    </g>")
                return output
            return prefix + [
                "    <path"
                + _attributes(
                    {
                        **base,
                        **style,
                        "d": " ".join(commands),
                        "fill-rule": "nonzero"
                        if entity.fill_rule == "nonzero"
                        else "evenodd",
                        "data-preview": "sampled-arcs" if has_sampled_arcs else None,
                        "data-xps-smooth-join": "preserved"
                        if has_smooth_joins
                        else None,
                    }
                )
                + "/>"
            ]
        return prefix
    if entity.kind in {"CIRCLE", "ARC", "ELLIPSE"}:
        sampled = _sample_ellipse(entity, curve_segments)
        if not sampled:
            return []
        tag = "polygon" if entity.closed else "polyline"
        return [
            f"    <{tag}"
            + _attributes(
                {
                    **base,
                    **style,
                    "points": _points(sampled, min_x, max_y, formatter),
                    "fill": style["fill"] if entity.closed else "none",
                    "data-preview": "sampled-ellipse",
                }
            )
            + "/>"
        ]
    if entity.kind in {"POLYTRIANGLE", "TEXTURED_POLYTRIANGLE"}:
        output = [f"    <g{_attributes(base)}>"]
        for triangle_index in range(max(0, len(entity.points) - 2)):
            triangle = entity.points[triangle_index : triangle_index + 3]
            output.append(
                "      <polygon"
                + _attributes(
                    {
                        **style,
                        "points": _points(triangle, min_x, max_y, formatter),
                        "fill": style["fill"],
                        "data-strip-index": str(triangle_index),
                    }
                )
                + "/>"
            )
        output.append("    </g>")
        return output
    if entity.kind == "CONTOUR_SET" and entity.contours:
        commands = []
        for contour in entity.contours:
            if not contour:
                continue
            screen = tuple(_screen(point, min_x, max_y) for point in contour)
            commands.append(
                "M " + " L ".join(formatter.point(point) for point in screen) + " Z"
            )
        return (
            [
                "    <path"
                + _attributes(
                    {**base, **style, "d": " ".join(commands), "fill-rule": "evenodd"}
                )
                + "/>"
            ]
            if commands
            else []
        )
    if entity.kind in {"GOURAUD_POLYLINE", "GOURAUD_POLYTRIANGLE"}:
        output = [
            f"    <g{_attributes({**base, 'data-preview': 'vertex-color-average'})}>"
        ]
        if entity.kind == "GOURAUD_POLYLINE":
            for item_index in range(max(0, len(entity.colored_points) - 1)):
                first, second = entity.colored_points[item_index : item_index + 2]
                start_point = _screen(first.point, min_x, max_y)
                end_point = _screen(second.point, min_x, max_y)
                color, alpha = _average_rgba((first.color, second.color))
                output.append(
                    "      <line"
                    + _attributes(
                        {
                            **style,
                            "x1": formatter.number(start_point.x),
                            "y1": formatter.number(start_point.y),
                            "x2": formatter.number(end_point.x),
                            "y2": formatter.number(end_point.y),
                            "stroke": color,
                            "stroke-opacity": formatter.number(alpha)
                            if alpha < 1
                            else None,
                            "fill": "none",
                        }
                    )
                    + "/>"
                )
        else:
            for item_index in range(max(0, len(entity.colored_points) - 2)):
                triangle = entity.colored_points[item_index : item_index + 3]
                color, alpha = _average_rgba(tuple(item.color for item in triangle))
                output.append(
                    "      <polygon"
                    + _attributes(
                        {
                            **style,
                            "points": _points(
                                tuple(item.point for item in triangle),
                                min_x,
                                max_y,
                                formatter,
                            ),
                            "fill": color,
                            "fill-opacity": formatter.number(alpha)
                            if alpha < 1
                            else None,
                            "stroke": color,
                        }
                    )
                    + "/>"
                )
        output.append("    </g>")
        return output
    if entity.kind == "IMAGE" and entity.image is not None:
        first = _screen(entity.image.min, min_x, max_y)
        second = _screen(entity.image.max, min_x, max_y)
        x, y = min(first.x, second.x), min(first.y, second.y)
        width, height = abs(second.x - first.x), abs(second.y - first.y)
        uri = _image_data_uri(entity.image)
        if uri is not None:
            return [
                "    <image"
                + _attributes(
                    {
                        **base,
                        "x": formatter.number(x),
                        "y": formatter.number(y),
                        "width": formatter.number(width),
                        "height": formatter.number(height),
                        "href": uri,
                        "preserveAspectRatio": "none",
                        "data-image-format": entity.image.format,
                    }
                )
                + "/>"
            ]
        return [
            "    <rect"
            + _attributes(
                {
                    **base,
                    **style,
                    "x": formatter.number(x),
                    "y": formatter.number(y),
                    "width": formatter.number(width),
                    "height": formatter.number(height),
                    "fill": "none",
                    "data-image-format": entity.image.format,
                    "data-preview": "unsupported-raster-placeholder",
                }
            )
            + "/>"
        ]
    if entity.kind == "TEXT" and entity.points and entity.text is not None:
        if entity.glyph_outline:
            commands = _path_commands(
                entity.glyph_outline,
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
                curve_segments=curve_segments,
                filled_only=True,
            )
            if commands:
                outline_fill = (
                    style["fill"] if style["fill"] != "none" else style["stroke"]
                )
                outline_alpha = (
                    style.get("fill-opacity")
                    if style["fill"] != "none"
                    else style.get("stroke-opacity")
                )
                bold_width = (entity.style.font_height or 0.0) * 0.02
                return prefix + [
                    "    <path"
                    + _attributes(
                        {
                            **base,
                            "d": " ".join(commands),
                            "fill": outline_fill,
                            "fill-opacity": outline_alpha,
                            "fill-rule": "nonzero",
                            "stroke": outline_fill
                            if entity.style.font_bold and bold_width > 0.0
                            else "none",
                            "stroke-opacity": outline_alpha,
                            "stroke-width": formatter.number(bold_width)
                            if entity.style.font_bold and bold_width > 0.0
                            else None,
                            "stroke-linejoin": "round",
                            "opacity": style.get("opacity"),
                            "data-xps-glyph-outline": "packaged-font",
                        }
                    )
                    + "/>"
                ]
        position = _screen(entity.points[0], min_x, max_y)
        font_size = entity.style.font_height or fallback_width * 12.0
        rotation = -(entity.style.font_rotation_degrees or 0.0)
        text_fill = style["fill"] if style["fill"] != "none" else style["stroke"]
        text_style = {
            "fill": text_fill,
            "fill-opacity": style.get("fill-opacity")
            if style["fill"] != "none"
            else style.get("stroke-opacity"),
            "font-family": entity.style.font_name
            or entity.style.font_canonical_name
            or "sans-serif",
            "font-size": formatter.number(max(font_size, fallback_width)),
            "font-weight": "bold" if entity.style.font_bold else None,
            "font-style": "italic" if entity.style.font_italic else None,
            "text-decoration": "underline" if entity.style.font_underlined else None,
            "xml:space": "preserve",
        }
        if rotation:
            text_style["transform"] = (
                f"rotate({formatter.number(rotation)} "
                f"{formatter.number(position.x)} {formatter.number(position.y)})"
            )
        return prefix + [
            "    <text"
            + _attributes(
                {
                    **base,
                    **text_style,
                    "x": formatter.number(position.x),
                    "y": formatter.number(position.y),
                }
            )
            + ">"
            + html.escape(entity.text)
            + "</text>"
        ]
    return []


def _style_attributes(
    entity: Entity,
    *,
    formatter: _Formatter,
    fallback_width: float,
    monochrome: bool,
    palette: Mapping[int, PaletteColor] | None,
    unresolved_color: str | Rgb,
) -> dict[str, str | None]:
    color, alpha = _entity_color(
        entity,
        monochrome=monochrome,
        palette=palette,
        unresolved_color=unresolved_color,
    )
    stroke_width = entity.style.nominal_stroke_width
    if stroke_width is None or stroke_width <= 0.0 or not math.isfinite(stroke_width):
        stroke_width = fallback_width
    is_xps = entity.resource_role == "xps fixed page"
    stroke, stroke_alpha = _style_color(
        entity.style.stroke_color,
        fallback=(color, alpha),
        monochrome=monochrome,
    )
    fill_color, fill_alpha = _style_color(
        entity.style.fill_color,
        fallback=(color, alpha),
        monochrome=monochrome,
    )
    if is_xps and entity.style.stroke_color is None:
        stroke = "none"
        stroke_alpha = 1.0
    fill = fill_color if entity.style.fill else "none"
    if is_xps and entity.style.fill_color is None and entity.style.fill_image is None:
        fill = "none"
        fill_alpha = 1.0
    attributes: dict[str, str | None] = {
        "stroke": stroke,
        "stroke-opacity": formatter.number(stroke_alpha)
        if stroke != "none" and stroke_alpha < 1.0
        else None,
        "stroke-width": formatter.number(stroke_width),
        "stroke-linecap": "round",
        "stroke-linejoin": "round",
        "fill": fill,
        "fill-opacity": formatter.number(fill_alpha)
        if fill != "none" and fill_alpha < 1.0
        else None,
        "opacity": formatter.number(entity.style.opacity)
        if entity.style.opacity < 1.0
        else None,
    }
    pattern = (entity.style.line_pattern or "").casefold()
    dash = entity.style.stroke_dash_array or _DASH_PATTERNS.get(pattern)
    if dash:
        scale = 1.0 if entity.style.stroke_dash_array else stroke_width
        attributes["stroke-dasharray"] = " ".join(
            formatter.number(value * scale) for value in dash
        )
        if entity.style.stroke_dash_offset:
            attributes["stroke-dashoffset"] = formatter.number(
                entity.style.stroke_dash_offset
            )
    return attributes


def _style_color(
    value: Rgba | None,
    *,
    fallback: tuple[str, float],
    monochrome: bool,
) -> tuple[str, float]:
    if monochrome:
        return "#000000", 1.0
    if value is None:
        return fallback
    _validate_channels(value, expected=4)
    return _rgb_hex(value[:3]), value[3] / 255.0


def _brush_definition(
    brush: Brush,
    *,
    identifier: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    mask: bool,
) -> tuple[list[str], str | None, float | None]:
    if isinstance(brush, SolidBrush):
        alpha = brush.color[3] / 255.0 * brush.opacity
        return [], "#ffffff" if mask else _rgb_hex(brush.color[:3]), alpha
    if isinstance(brush, ImageBrush):
        definition = _image_brush_definition(
            brush,
            identifier=identifier,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
        )
        return definition, f"url(#{identifier})" if definition else None, None
    if isinstance(brush, VisualBrush):
        definition = _visual_brush_definition(
            brush,
            identifier=identifier,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            mask=mask,
        )
        return definition, f"url(#{identifier})" if definition else None, None
    if isinstance(brush, GradientBrush):
        definition = _gradient_brush_definition(
            brush,
            identifier=identifier,
            min_x=min_x,
            max_y=max_y,
            formatter=formatter,
            mask=mask,
        )
        return definition, f"url(#{identifier})" if definition else None, None
    if isinstance(brush, UnsupportedBrush):
        return [], None, None
    return [], None, None


def _gradient_brush_definition(
    brush: GradientBrush,
    *,
    identifier: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    mask: bool,
) -> list[str]:
    spread_method = brush.spread_method.casefold()
    if spread_method not in {"pad", "reflect", "repeat"}:
        spread_method = "pad"
    common: dict[str, object] = {
        "id": identifier,
        "gradientUnits": "userSpaceOnUse",
        "spreadMethod": spread_method,
    }
    if brush.kind == "linear_gradient":
        if brush.start_point is None or brush.end_point is None:
            return []
        start = _screen(brush.start_point, min_x, max_y)
        end = _screen(brush.end_point, min_x, max_y)
        tag = "linearGradient"
        common.update(
            {
                "x1": formatter.number(start.x),
                "y1": formatter.number(start.y),
                "x2": formatter.number(end.x),
                "y2": formatter.number(end.y),
            }
        )
    else:
        if (
            brush.center is None
            or brush.gradient_origin is None
            or brush.x_axis is None
            or brush.y_axis is None
        ):
            return []
        center = _screen(brush.center, min_x, max_y)
        origin = _screen(brush.gradient_origin, min_x, max_y)
        x_axis = Point2D(brush.x_axis.x, -brush.x_axis.y)
        y_axis = Point2D(brush.y_axis.x, -brush.y_axis.y)
        determinant = x_axis.x * y_axis.y - x_axis.y * y_axis.x
        if abs(determinant) <= 1e-15:
            return []
        delta_x = origin.x - center.x
        delta_y = origin.y - center.y
        focus_x = (delta_x * y_axis.y - delta_y * y_axis.x) / determinant
        focus_y = (x_axis.x * delta_y - x_axis.y * delta_x) / determinant
        tag = "radialGradient"
        common.update(
            {
                "cx": "0",
                "cy": "0",
                "r": "1",
                "fx": formatter.number(focus_x),
                "fy": formatter.number(focus_y),
                "gradientTransform": "matrix("
                + " ".join(
                    formatter.number(value)
                    for value in (
                        x_axis.x,
                        x_axis.y,
                        y_axis.x,
                        y_axis.y,
                        center.x,
                        center.y,
                    )
                )
                + ")",
            }
        )
    output = [f"    <defs><{tag}{_attributes(common)}>"]
    for color_value, resolved_color, offset in _gradient_stops_for_svg(brush):
        color = resolved_color or (0, 0, 0, 255)
        opacity = color[3] / 255.0 * brush.opacity
        output.append(
            "      <stop"
            + _attributes(
                {
                    "offset": formatter.number(offset),
                    "stop-color": "#ffffff" if mask else _rgb_hex(color[:3]),
                    "stop-opacity": formatter.number(opacity)
                    if opacity < 1.0
                    else None,
                    "data-xps-unresolved-color": color_value
                    if resolved_color is None
                    else None,
                }
            )
            + "/>"
        )
    output.append(f"    </{tag}></defs>")
    return output


def _gradient_stops_for_svg(
    brush: GradientBrush,
) -> list[tuple[str, Rgba | None, float]]:
    """Apply the OpenXPS boundary-stop preprocessing used by the SVG preview."""

    ordered = sorted(
        ((stop.color_value, stop.color, stop.offset) for stop in brush.gradient_stops),
        key=lambda stop: stop[2],
    )
    collapsed: list[tuple[str, Rgba | None, float]] = []
    index = 0
    while index < len(ordered):
        end = index + 1
        while end < len(ordered) and ordered[end][2] == ordered[index][2]:
            end += 1
        group = ordered[index:end]
        collapsed.extend(group if len(group) <= 2 else (group[0], group[-1]))
        index = end

    for boundary in (0.0, 1.0):
        if any(stop[2] == boundary for stop in collapsed):
            continue
        lower = next((stop for stop in reversed(collapsed) if stop[2] < boundary), None)
        upper = next((stop for stop in collapsed if stop[2] > boundary), None)
        if lower is not None and upper is not None:
            collapsed.append(_interpolate_gradient_stop(lower, upper, boundary))
        else:
            source = lower or upper
            if source is not None:
                collapsed.append((source[0], source[1], boundary))
        collapsed.sort(key=lambda stop: stop[2])

    return [stop for stop in collapsed if 0.0 <= stop[2] <= 1.0]


def _interpolate_gradient_stop(
    lower: tuple[str, Rgba | None, float],
    upper: tuple[str, Rgba | None, float],
    offset: float,
) -> tuple[str, Rgba | None, float]:
    span = upper[2] - lower[2]
    ratio = 0.0 if span == 0.0 else (offset - lower[2]) / span
    color = None
    if lower[1] is not None and upper[1] is not None:
        color = cast(
            Rgba,
            tuple(
                round(first + (second - first) * ratio)
                for first, second in zip(lower[1], upper[1], strict=True)
            ),
        )
    return (f"interpolated({lower[0]}, {upper[0]})", color, offset)


def _image_brush_definition(
    brush: ImageBrush,
    *,
    identifier: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
) -> list[str]:
    if not brush.data:
        return []
    mime = brush.content_type or _image_mime(brush.data)
    if mime is None or not mime.casefold().startswith("image/"):
        return []
    uri = f"data:{mime};base64,{base64.b64encode(brush.data).decode('ascii')}"
    source_viewport = brush.source_viewport
    attributes: dict[str, object] = {
        "id": identifier,
        "patternUnits": "objectBoundingBox",
        "x": "0",
        "y": "0",
        "width": "1",
        "height": "1",
        "data-xps-tile-mode": brush.tile_mode or "None",
    }
    tile_box: tuple[float, float, float, float] | None = None
    if source_viewport is not None and len(brush.transform) == 6:
        x, y, width, height = source_viewport
        a, b, c, d, e, f = brush.transform
        tile_mode = (brush.tile_mode or "None").casefold()
        flip_x = tile_mode in {"flipx", "flipxy"}
        flip_y = tile_mode in {"flipy", "flipxy"}
        pattern_width = width * (2.0 if flip_x else 1.0)
        pattern_height = height * (2.0 if flip_y else 1.0)
        if tile_mode == "none":
            pattern_width = max(width, 1.0) * 1_000_000.0
            pattern_height = max(height, 1.0) * 1_000_000.0
        if width > 0.0 and height > 0.0:
            tile_box = (x, y, width, height)
            attributes.update(
                {
                    "patternUnits": "userSpaceOnUse",
                    "x": formatter.number(x),
                    "y": formatter.number(y),
                    "width": formatter.number(pattern_width),
                    "height": formatter.number(pattern_height),
                    "patternTransform": "matrix("
                    + " ".join(
                        formatter.number(value)
                        for value in (a, -b, c, -d, e - min_x, max_y - f)
                    )
                    + ")",
                }
            )
    elif brush.viewport is not None:
        left, bottom, right, top = brush.viewport
        screen = _screen(Point2D(left, top), min_x, max_y)
        width = abs(right - left)
        height = abs(top - bottom)
        if width > 0.0 and height > 0.0:
            tile_box = (screen.x, screen.y, width, height)
            attributes.update(
                {
                    "patternUnits": "userSpaceOnUse",
                    "x": formatter.number(screen.x),
                    "y": formatter.number(screen.y),
                    "width": formatter.number(width),
                    "height": formatter.number(height),
                }
            )
    if tile_box is None:
        return []
    x, y, width, height = tile_box
    physical_width, physical_height = brush.physical_size_dip or (
        brush.viewbox[2] if brush.viewbox is not None else width,
        brush.viewbox[3] if brush.viewbox is not None else height,
    )
    viewbox = brush.viewbox or (0.0, 0.0, physical_width, physical_height)
    if (
        physical_width <= 0.0
        or physical_height <= 0.0
        or viewbox[2] <= 0.0
        or viewbox[3] <= 0.0
    ):
        return []
    output = [f"    <defs><pattern{_attributes(attributes)}>"]
    tile = [
        "      <svg"
        + _attributes(
            {
                "id": f"{identifier}-tile",
                "x": formatter.number(x),
                "y": formatter.number(y),
                "width": formatter.number(width),
                "height": formatter.number(height),
                "viewBox": " ".join(formatter.number(value) for value in viewbox),
                "preserveAspectRatio": "none",
                "overflow": "hidden",
                "opacity": formatter.number(brush.opacity)
                if brush.opacity < 1.0
                else None,
                "data-xps-viewbox-crop": "dpi",
                "data-xps-dpi": (
                    f"{formatter.number(brush.dpi_x)} {formatter.number(brush.dpi_y)}"
                    if brush.dpi_x is not None and brush.dpi_y is not None
                    else None
                ),
            }
        )
        + ">",
        "        <image"
        + _attributes(
            {
                "x": "0",
                "y": "0",
                "width": formatter.number(physical_width),
                "height": formatter.number(physical_height),
                "href": uri,
                "preserveAspectRatio": "none",
            }
        )
        + "/>",
        "      </svg>",
    ]
    output.extend(tile)
    for transform in _flip_tile_transforms(
        brush.tile_mode, x=x, y=y, width=width, height=height, formatter=formatter
    ):
        output.append(
            "      <use"
            + _attributes(
                {
                    "href": f"#{identifier}-tile",
                    "transform": transform,
                }
            )
            + "/>"
        )
    output.append("    </pattern></defs>")
    return output


def _flip_tile_transforms(
    tile_mode: str | None,
    *,
    x: float,
    y: float,
    width: float,
    height: float,
    formatter: _Formatter,
) -> tuple[str, ...]:
    mode = (tile_mode or "None").casefold()
    transforms: list[str] = []
    if mode in {"flipx", "flipxy"}:
        transforms.append(
            f"translate({formatter.number(2 * (x + width))} 0) scale(-1 1)"
        )
    if mode in {"flipy", "flipxy"}:
        transforms.append(
            f"translate(0 {formatter.number(2 * (y + height))}) scale(1 -1)"
        )
    if mode == "flipxy":
        transforms.append(
            "translate("
            f"{formatter.number(2 * (x + width))} "
            f"{formatter.number(2 * (y + height))}) scale(-1 -1)"
        )
    return tuple(transforms)


def _visual_brush_definition(
    brush: VisualBrush,
    *,
    identifier: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
    mask: bool,
) -> list[str]:
    view_x, view_y, view_width, view_height = brush.viewbox
    x, y, width, height = brush.source_viewport
    if width <= 0.0 or height <= 0.0 or view_width <= 0.0 or view_height <= 0.0:
        return []
    a, b, c, d, e, f = brush.transform
    tile_mode = (brush.tile_mode or "None").casefold()
    flip_x = tile_mode in {"flipx", "flipxy"}
    flip_y = tile_mode in {"flipy", "flipxy"}
    pattern_width = width * (2.0 if flip_x else 1.0)
    pattern_height = height * (2.0 if flip_y else 1.0)
    if tile_mode == "none":
        pattern_width = max(width, 1.0) * 1_000_000.0
        pattern_height = max(height, 1.0) * 1_000_000.0
    attributes = {
        "id": identifier,
        "patternUnits": "userSpaceOnUse",
        "x": formatter.number(x),
        "y": formatter.number(y),
        "width": formatter.number(pattern_width),
        "height": formatter.number(pattern_height),
        "patternTransform": "matrix("
        + " ".join(
            formatter.number(value) for value in (a, -b, c, -d, e - min_x, max_y - f)
        )
        + ")",
        "data-xps-tile-mode": brush.tile_mode or "None",
    }
    fallback_width = max(view_width, view_height) / 1_000.0
    inner = _render_entities(
        brush.entities,
        min_x=view_x,
        max_y=-view_y,
        formatter=formatter,
        curve_segments=96,
        fallback_width=max(fallback_width, 0.001),
        monochrome=False,
        palette=None,
        unresolved_color="#000000",
        include_invisible=False,
        show_text=True,
        id_prefix=f"{identifier}-",
        mask=mask,
    )
    tile = [
        "      <svg"
        + _attributes(
            {
                "id": f"{identifier}-tile",
                "x": formatter.number(x),
                "y": formatter.number(y),
                "width": formatter.number(width),
                "height": formatter.number(height),
                "viewBox": (
                    f"0 0 {formatter.number(view_width)} "
                    f"{formatter.number(view_height)}"
                ),
                "preserveAspectRatio": "none",
                "overflow": "hidden",
                "opacity": formatter.number(brush.opacity)
                if brush.opacity < 1.0
                else None,
                "data-xps-viewbox": " ".join(
                    formatter.number(value) for value in brush.viewbox
                ),
            }
        )
        + ">",
        *("  " + line for line in inner),
        "      </svg>",
    ]
    output = [f"    <defs><pattern{_attributes(attributes)}>", *tile]
    for transform in _flip_tile_transforms(
        brush.tile_mode, x=x, y=y, width=width, height=height, formatter=formatter
    ):
        output.append(
            "      <use"
            + _attributes(
                {
                    "href": f"#{identifier}-tile",
                    "transform": transform,
                }
            )
            + "/>"
        )
    output.append("    </pattern></defs>")
    return output


def _opacity_mask_definitions(
    entity: Entity,
    *,
    opacity_masks: Sequence[Brush],
    index: str,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
) -> list[str]:
    bounds = entity.bbox() or (min_x, max_y - 1.0, min_x + 1.0, max_y)
    left, bottom, right, top = bounds
    padding = max(entity.style.nominal_stroke_width or 0.0, 1e-6)
    left -= padding
    bottom -= padding
    right += padding
    top += padding
    output: list[str] = []
    padded_bounds = (left, bottom, right, top)
    for mask_index, brush in enumerate(opacity_masks):
        output.extend(
            _opacity_mask_definition(
                brush,
                identifier=f"opacity-mask-{index}-{mask_index}",
                bounds=padded_bounds,
                min_x=min_x,
                max_y=max_y,
                formatter=formatter,
            )
        )
    return output


def _opacity_mask_definition(
    brush: Brush,
    *,
    identifier: str,
    bounds: Bounds2D | None,
    min_x: float,
    max_y: float,
    formatter: _Formatter,
) -> list[str]:
    left, bottom, right, top = bounds or (
        min_x,
        max_y - 1.0,
        min_x + 1.0,
        max_y,
    )
    screen = _screen(Point2D(left, top), min_x, max_y)
    width = max(right - left, 1e-6)
    height = max(top - bottom, 1e-6)
    brush_id = f"{identifier}-brush"
    definition, paint, opacity = _brush_definition(
        brush,
        identifier=brush_id,
        min_x=min_x,
        max_y=max_y,
        formatter=formatter,
        mask=True,
    )
    output = list(definition)
    output.append(
        "    <defs><mask"
        + _attributes(
            {
                "id": identifier,
                "maskUnits": "userSpaceOnUse",
                "maskContentUnits": "userSpaceOnUse",
                "x": formatter.number(screen.x),
                "y": formatter.number(screen.y),
                "width": formatter.number(width),
                "height": formatter.number(height),
                "style": "mask-type:alpha",
                "data-preview": "unsupported-brush" if paint is None else None,
            }
        )
        + ">"
    )
    output.append(
        "      <rect"
        + _attributes(
            {
                "x": formatter.number(screen.x),
                "y": formatter.number(screen.y),
                "width": formatter.number(width),
                "height": formatter.number(height),
                "fill": paint or "#ffffff",
                "fill-opacity": formatter.number(opacity)
                if opacity is not None and opacity < 1.0
                else None,
            }
        )
        + "/>"
    )
    output.append("    </mask></defs>")
    return output


def _image_mime(data: bytes) -> str | None:
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if data.startswith(b"\xff\xd8"):
        return "image/jpeg"
    if data.startswith((b"II*\x00", b"MM\x00*")):
        return "image/tiff"
    return None


def _average_rgba(colors: tuple[Rgba, ...]) -> tuple[str, float]:
    count = max(1, len(colors))
    channels = tuple(
        round(sum(color[index] for color in colors) / count) for index in range(4)
    )
    rgba = cast(Rgba, channels)
    return _rgb_hex(rgba[:3]), rgba[3] / 255.0


def _image_data_uri(image: Image) -> str | None:
    format_name = image.format.casefold()
    if format_name == "png" and image.data.startswith(b"\x89PNG\r\n\x1a\n"):
        mime = "image/png"
        payload = image.data
    elif format_name in {"jpeg", "jpg"} and image.data.startswith(b"\xff\xd8"):
        mime = "image/jpeg"
        payload = image.data
    else:
        pixels = _image_rgba_pixels(image)
        if pixels is None:
            return None
        mime = "image/png"
        payload = _encode_rgba_png(image.columns, image.rows, pixels)
    return f"data:{mime};base64,{base64.b64encode(payload).decode('ascii')}"


def _image_rgba_pixels(image: Image) -> bytes | None:
    if image.columns <= 0 or image.rows <= 0:
        return None
    pixel_count = image.columns * image.rows
    format_name = image.format.casefold()
    if format_name == "rgba" and len(image.data) == pixel_count * 4:
        return image.data
    if format_name == "rgb" and len(image.data) == pixel_count * 3:
        output = bytearray()
        for offset in range(0, len(image.data), 3):
            output.extend(image.data[offset : offset + 3])
            output.append(255)
        return bytes(output)
    if format_name == "bitonal_mapped" and len(image.color_map) >= 2:
        row_stride = (image.columns + 7) // 8
        if len(image.data) != row_stride * image.rows:
            return None
        output = bytearray()
        for row in range(image.rows):
            row_offset = row * row_stride
            for column in range(image.columns):
                value = image.data[row_offset + column // 8]
                index = (value >> (7 - column % 8)) & 1
                output.extend(image.color_map[index])
        return bytes(output)
    if format_name in {"indexed", "mapped"} and len(image.data) == pixel_count:
        if not image.color_map:
            return None
        output = bytearray()
        for index in image.data:
            if index >= len(image.color_map):
                return None
            output.extend(image.color_map[index])
        return bytes(output)
    return None


def _encode_rgba_png(width: int, height: int, pixels: bytes) -> bytes:
    def chunk(name: bytes, data: bytes) -> bytes:
        body = name + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", binascii.crc32(body) & 0xFFFFFFFF)
        )

    stride = width * 4
    rows = b"".join(
        b"\x00" + pixels[offset : offset + stride]
        for offset in range(0, len(pixels), stride)
    )
    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def _entity_color(
    entity: Entity,
    *,
    monochrome: bool,
    palette: Mapping[int, PaletteColor] | None,
    unresolved_color: str | Rgb,
) -> tuple[str, float]:
    if monochrome:
        return "#000000", 1.0
    color: str | Sequence[int]
    if entity.style.color is not None:
        color = entity.style.color
    elif entity.style.color_index is not None and palette is not None:
        color = palette.get(entity.style.color_index, unresolved_color)
    else:
        color = unresolved_color
    if isinstance(color, str):
        return color, 1.0
    channels = tuple(int(value) for value in color)
    if len(channels) == 3:
        return _rgb_hex(cast(Rgb, channels)), 1.0
    if len(channels) == 4:
        rgba = cast(Rgba, channels)
        _validate_channels(rgba, expected=4)
        return _rgb_hex(rgba[:3]), rgba[3] / 255.0
    raise ValueError("palette colors must contain three or four channels")


def _sample_ellipse(entity: Entity, curve_segments: int) -> tuple[Point2D, ...]:
    if entity.center is None or entity.x_axis is None or entity.y_axis is None:
        return ()
    start = entity.start_angle_degrees or 0.0
    if entity.closed:
        span = 360.0
    else:
        end = entity.end_angle_degrees
        if end is None:
            return ()
        span = end - start
        while span <= 0.0:
            span += 360.0
    count = max(2, math.ceil(abs(span) / 360.0 * curve_segments) + 1)
    points = []
    for index in range(count):
        angle = math.radians(start + span * index / (count - 1))
        points.append(
            Point2D(
                entity.center.x
                + entity.x_axis.x * math.cos(angle)
                + entity.y_axis.x * math.sin(angle),
                entity.center.y
                + entity.x_axis.y * math.cos(angle)
                + entity.y_axis.y * math.sin(angle),
            )
        )
    return tuple(points)


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
    points = []
    for index in range(count):
        angle = math.radians(
            segment.start_angle_degrees
            + segment.sweep_angle_degrees * index / (count - 1)
        )
        points.append(
            Point2D(
                segment.center.x
                + segment.x_axis.x * math.cos(angle)
                + segment.y_axis.x * math.sin(angle),
                segment.center.y
                + segment.x_axis.y * math.cos(angle)
                + segment.y_axis.y * math.sin(angle),
            )
        )
    points[-1] = segment.end
    return tuple(points)


def _screen(point: Point2D, min_x: float, max_y: float) -> Point2D:
    return Point2D(point.x - min_x, max_y - point.y)


def _points(
    points: Sequence[Point2D],
    min_x: float,
    max_y: float,
    formatter: _Formatter,
) -> str:
    return " ".join(formatter.point(_screen(point, min_x, max_y)) for point in points)


def _attributes(values: Mapping[str, object]) -> str:
    return "".join(
        f' {name}="{html.escape(str(value), quote=True)}"'
        for name, value in values.items()
        if value is not None
    )


def _validate_channels(values: Sequence[int], *, expected: int):
    channels = tuple(int(value) for value in values)
    if len(channels) != expected or any(not 0 <= value <= 255 for value in channels):
        raise ValueError(
            f"color must contain {expected} integer channels between 0 and 255"
        )
    return channels


def _rgb_hex(color: Sequence[int]) -> str:
    red, green, blue = _validate_channels(color, expected=3)
    return f"#{red:02x}{green:02x}{blue:02x}"


class _Formatter:
    __slots__ = ("precision",)

    def __init__(self, precision: int) -> None:
        self.precision = precision

    def number(self, value: float) -> str:
        if not math.isfinite(value):
            raise ValueError("SVG coordinates must be finite")
        text = f"{value:.{self.precision}f}".rstrip("0").rstrip(".")
        return "0" if text in {"", "-0"} else text

    def point(self, point: Point2D) -> str:
        return f"{self.number(point.x)},{self.number(point.y)}"


__all__ = ["PaletteColor", "render_svg", "save_svg"]
