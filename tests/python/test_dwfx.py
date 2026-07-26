from __future__ import annotations

import json
import struct
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path

import pytest
from conftest import _PIXEL_PNG, make_dwf, make_dwfx

import ezdwf


def test_inspect_dwfx_traverses_opc_graph_and_retains_xps(dwfx_bytes: bytes) -> None:
    package = ezdwf.inspect_dwfx(dwfx_bytes)

    assert package.format.is_dwfx
    assert package.document_sequence == "Documents/1/FixedDocumentSequence.fdseq"
    assert package.sheet_count == 1
    assert package.entity_count == 4
    assert len(package.documents) == 1
    assert package.diagnostics == ()
    assert any(
        relationship.target_mode == "External"
        and relationship.normalized_target is None
        for relationship in package.relationships
    )

    page = package.pages[0]
    assert page.part_name == "Documents/1/Pages/1.fpage"
    assert (page.name, page.language, page.width, page.height) == (
        "Fixture DWFx",
        "en-US",
        100.0,
        50.0,
    )
    assert tuple(entity.kind for entity in page.entities) == (
        "path",
        "path",
        "path",
        "glyphs",
    )

    outline = page.entities[0]
    assert outline.canvas_name == "Fixture Layer"
    assert outline.path is not None
    assert outline.path.fill_rule == "nonzero"
    assert tuple(segment.kind for segment in outline.path.figures[0].segments) == (
        "line",
        "line",
    )
    assert outline.style.stroke is not None
    assert outline.style.stroke.color == (17, 34, 51, 255)
    assert outline.style.fill is not None
    assert outline.style.fill.color == (68, 85, 102, 128)
    assert outline.style.fill.attributes["Color"] == "#80445566"

    curves = page.entities[1]
    assert curves.path is not None
    assert tuple(segment.kind for segment in curves.path.figures[0].segments) == (
        "cubic_bezier",
        "quadratic_bezier",
        "arc",
    )

    image = page.entities[2].style.fill
    assert image is not None
    assert image.kind == "image"
    assert image.normalized_source == "Documents/1/Resources/pixel.png"
    assert image.content_type == "image/png"
    assert image.data.startswith(b"\x89PNG\r\n\x1a\n")

    glyphs = page.entities[3].glyphs
    assert glyphs is not None
    assert glyphs.unicode_string == "DWFx"
    assert glyphs.normalized_font_uri == "Documents/1/Resources/font.odttf"


def test_read_dwfx_normalizes_coordinates_styles_and_raw_links(
    dwfx_bytes: bytes,
) -> None:
    drawing = ezdwf.read(dwfx_bytes)

    assert drawing.is_dwfx
    assert not drawing.is_legacy
    assert drawing.package is None
    assert drawing.legacy_stream is None
    assert drawing.stats() == {
        "sheet_count": 1,
        "entity_count": 4,
        "visible_count": 4,
        "by_type": {"PATH": 3, "TEXT": 1},
        "by_layer": {"Fixture Layer": 4},
    }
    sheet = drawing.modelspace()
    assert sheet.units == "dip"
    assert sheet.paper_bounds == (0.0, 0.0, 100.0, 50.0)
    assert sheet.content_bounds == pytest.approx((2.0, 7.0, 62.0, 47.0))
    assert sheet.layers == ("Fixture Layer",)
    assert len(sheet.query('PATH[layer=="Fixture Layer"]')) == 3
    assert drawing.dwfx_package is not None
    assert sheet.raw is drawing.dwfx_package.pages[0]

    outline = sheet.entities[0]
    assert outline.dxftype() == "PATH"
    assert outline.path[0].start == ezdwf.Point2D(2.0, 47.0)
    assert outline.path[0].segments[-1].end == ezdwf.Point2D(12.0, 37.0)
    assert outline.style.stroke_color == (17, 34, 51, 255)
    assert outline.style.fill_color == (68, 85, 102, 128)
    assert outline.style.nominal_stroke_width == 2.0
    assert outline.style.stroke_dash_array == (4.0, 2.0)
    assert outline.style.opacity == 1.0
    assert len(outline.compositing_groups) == 1
    assert outline.compositing_groups[0].opacity == 0.75
    assert outline.raw is drawing.dwfx_package.pages[0].entities[0]
    assert outline.source.opcode == "Path"

    curves = sheet.entities[1]
    assert tuple(segment.kind for segment in curves.path[0].segments) == (
        "cubic_bezier",
        "quadratic_bezier",
        "elliptical_arc",
    )
    arc = curves.path[0].segments[-1]
    assert arc.center is not None
    assert arc.x_axis is not None
    assert arc.y_axis is not None
    assert arc.end == ezdwf.Point2D(57.0, 27.0)

    image = sheet.entities[2].style.fill_image
    assert image is not None
    assert image.viewport == (32.0, 7.0, 42.0, 17.0)
    assert image.data.startswith(b"\x89PNG")

    label = sheet.entities[3]
    assert (label.kind, label.text, label.points) == (
        "TEXT",
        "DWFx",
        (ezdwf.Point2D(62.0, 27.0),),
    )
    assert label.style.font_height == 8.0
    json.dumps(sheet.snapshot())


def test_dwfx_svg_renders_paths_image_brush_and_glyphs(dwfx_bytes: bytes) -> None:
    svg = ezdwf.render_svg(ezdwf.read(dwfx_bytes), precision=4)
    root = ET.fromstring(svg)
    namespace = {"svg": "http://www.w3.org/2000/svg"}

    assert len(root.findall(".//svg:path", namespace)) == 3
    assert len(root.findall(".//svg:text", namespace)) == 1
    assert len(root.findall(".//svg:pattern", namespace)) == 1
    image = root.find(".//svg:image", namespace)
    assert image is not None
    assert image.attrib["href"].startswith("data:image/png;base64,")
    assert 'data-preview="sampled-arcs"' in svg
    assert ">DWFx</text>" in svg


def test_dwfx_explicit_path_geometry_is_supported() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Stroke="#000000">
        <Path.Data>
          <PathGeometry FillRule="NonZero">
            <PathFigure StartPoint="1,2" IsClosed="true">
              <PolyLineSegment Points="3,4 5,6"/>
              <PolyBezierSegment Points="6,7 7,8 8,9"/>
              <PolyQuadraticBezierSegment Points="9,10 10,11"/>
              <ArcSegment Size="2,3" RotationAngle="20" IsLargeArc="false"
                          SweepDirection="Clockwise" Point="12,13"/>
            </PathFigure>
          </PathGeometry>
        </Path.Data>
      </Path>
    </FixedPage>"""

    path = ezdwf.inspect_dwfx(make_dwfx(fixed_page=page)).pages[0].entities[0].path
    assert path is not None
    assert path.fill_rule == "nonzero"
    assert path.figures[0].closed
    assert tuple(segment.kind for segment in path.figures[0].segments) == (
        "line",
        "line",
        "cubic_bezier",
        "quadratic_bezier",
        "arc",
    )


def test_dwfx_clip_chain_and_partial_path_paint_reach_svg() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="100" Height="80">
      <Canvas RenderTransform="1,0,0,1,10,5"
              Clip="M 0,0 L 40,0 L 40,40 L 0,40 Z">
        <Canvas>
          <Canvas.RenderTransform>
            <MatrixTransform Matrix="2,0,0,2,0,0"/>
          </Canvas.RenderTransform>
          <Canvas.Clip>
            <PathGeometry FillRule="NonZero">
              <PathFigure StartPoint="1,1" IsClosed="true">
                <PolyLineSegment Points="15,1 15,15 1,15"/>
              </PathFigure>
            </PathGeometry>
          </Canvas.Clip>
          <Path Fill="#4080C0" Stroke="#102030" RenderTransform="1,0,0,1,1,2">
            <Path.Clip>
              <PathGeometry Figures="M 2,2 L 12,2 L 12,12 L 2,12 Z"/>
            </Path.Clip>
            <Path.Data>
              <PathGeometry FillRule="NonZero">
                <PathFigure StartPoint="0,0" IsFilled="false">
                  <PolyLineSegment Points="10,0 10,10"
                    IsStroked="false" IsSmoothJoin="true"/>
                  <PolyLineSegment Points="0,10"/>
                </PathFigure>
                <PathFigure StartPoint="20,0" IsClosed="true">
                  <PolyLineSegment Points="30,0 30,10 20,10"/>
                </PathFigure>
              </PathGeometry>
            </Path.Data>
          </Path>
          <Glyphs FontUri="../Resources/font.odttf" FontRenderingEmSize="8"
                  OriginX="3" OriginY="20" UnicodeString="clip"
                  Fill="#000000" Clip="M 0,10 L 30,10 L 30,22 L 0,22 Z"/>
        </Canvas>
      </Canvas>
    </FixedPage>"""
    package = ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))
    raw_path, raw_glyph = package.pages[0].entities

    assert len(raw_path.clip_chain) == 3
    assert raw_path.clip == raw_path.clip_chain[-1].geometry
    assert raw_path.clip_chain[0].transform == (1.0, 0.0, 0.0, 1.0, 10.0, 5.0)
    assert raw_path.clip_chain[1].transform == (2.0, 0.0, 0.0, 2.0, 10.0, 5.0)
    assert raw_path.clip_chain[2].transform == (2.0, 0.0, 0.0, 2.0, 12.0, 9.0)
    assert len(raw_glyph.clip_chain) == 3

    raw_figure = raw_path.path.figures[0]
    assert not raw_figure.filled
    assert tuple(segment.stroked for segment in raw_figure.segments) == (
        False,
        False,
        True,
    )
    assert tuple(segment.smooth_join for segment in raw_figure.segments) == (
        True,
        True,
        False,
    )

    drawing = ezdwf.read(make_dwfx(fixed_page=page))
    path = drawing.modelspace().entities[0]
    assert len(path.clips) == 3
    assert path.clips[0].figures[0].start == ezdwf.Point2D(10.0, 75.0)
    assert path.clips[1].figures[0].start == ezdwf.Point2D(12.0, 73.0)
    assert path.clips[2].figures[0].start == ezdwf.Point2D(16.0, 67.0)
    assert not path.path[0].segments[0].stroked
    assert path.path[0].segments[0].smooth_join

    svg = ezdwf.render_svg(drawing, precision=4)
    root = ET.fromstring(svg)
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    assert len(root.findall(".//svg:clipPath", namespace)) == 4
    assert len(root.findall(".//svg:g[@id='canvas-0']", namespace)) == 1
    assert len(root.findall(".//svg:g[@id='canvas-1']", namespace)) == 1
    assert len(root.findall('.//svg:path[@data-xps-component="fill"]', namespace)) == 1
    assert (
        len(root.findall('.//svg:path[@data-xps-component="stroke"]', namespace)) == 1
    )
    assert 'data-xps-smooth-join="preserved"' in svg


def test_dwfx_accepts_utf16_fixed_page_xml() -> None:
    page = """<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Data="M 1,2 L 3,4" Stroke="#000000"/>
    </FixedPage>""".encode("utf-16")

    package = ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))
    assert package.entity_count == 1
    assert package.pages[0].entities[0].path is not None


@pytest.mark.parametrize(
    "path_markup",
    (
        """<Path Data="M 0,0 L 1,1">
          <Path.Data><PathGeometry Figures="M 2,2 L 3,3"/></Path.Data>
        </Path>""",
        """<Path><Path.Data>
          <PathGeometry Figures="M 0,0 L 1,1">
            <PathFigure StartPoint="2,2"><PolyLineSegment Points="3,3"/></PathFigure>
          </PathGeometry>
        </Path.Data></Path>""",
    ),
)
def test_dwfx_rejects_ambiguous_path_geometry(path_markup: str) -> None:
    page = f"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">{path_markup}</FixedPage>""".encode()

    with pytest.raises(ezdwf.InvalidDwfError, match="more than once|cannot combine"):
        ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))


def test_dwfx_scrgb_and_context_color_have_explicit_preview_boundaries() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Data="M 0,0 L 5,5" Stroke="sc#0.5,1.0,0.0"/>
      <Path Data="M 1,1 L 6,6"
            Stroke="ContextColor /Resources/profile.icc 1.0,0.2,0.3,0.4"/>
    </FixedPage>"""
    package = ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))

    assert package.pages[0].entities[0].style.stroke is not None
    assert package.pages[0].entities[0].style.stroke.color == (188, 255, 0, 255)
    context = package.pages[0].entities[1].style.stroke
    assert context is not None
    assert context.kind == "unsupported"
    assert context.attributes["Color"].startswith("ContextColor ")
    assert any(
        diagnostic.code == "unsupported_xps_brush" for diagnostic in package.diagnostics
    )


def test_missing_remote_resource_dictionary_fails_closed() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <FixedPage.Resources><ResourceDictionary Source="remote.xaml"/></FixedPage.Resources>
      <Path Data="M 0,0 L 5,5" Stroke="#000000"/>
    </FixedPage>"""
    with pytest.raises(ezdwf.InvalidDwfError, match="remote.xaml"):
        ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))


def test_dwfx_remote_resources_gradients_masks_and_geometry_transform() -> None:
    remote_dictionary = b"""<ResourceDictionary
      xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
      <MatrixTransform x:Key="brushShift" Matrix="1,0,0,1,1,2"/>
      <LinearGradientBrush x:Key="sharedPaint" MappingMode="Absolute"
          StartPoint="0,0" EndPoint="10,0" SpreadMethod="Reflect">
        <LinearGradientBrush.GradientStops>
          <GradientStop Color="#FF0000" Offset="0"/>
          <GradientStop Color="#800000FF" Offset="1"/>
        </LinearGradientBrush.GradientStops>
      </LinearGradientBrush>
      <ImageBrush x:Key="remoteImage" ImageSource="pixel.png"
          Viewbox="0,0,1,1" ViewboxUnits="Absolute"
          Viewport="2,3,4,5" ViewportUnits="Absolute" TileMode="FlipXY"
          Transform="{StaticResource brushShift}"/>
      <PathGeometry x:Key="remoteShape" FillRule="NonZero">
        <PathGeometry.Transform>
          <MatrixTransform Matrix="2,0,0,2,1,1"/>
        </PathGeometry.Transform>
        <PathFigure StartPoint="0,0" IsClosed="true">
          <PolyLineSegment Points="5,0 5,5 0,5"/>
        </PathFigure>
      </PathGeometry>
    </ResourceDictionary>"""
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="50" Height="50">
      <Canvas OpacityMask="{StaticResource sharedPaint}">
        <Canvas.Resources>
          <ResourceDictionary Source="../Resources/shared.dict"/>
        </Canvas.Resources>
        <Path Name="outer" Data="{StaticResource remoteShape}"
              Fill="{StaticResource sharedPaint}" Stroke="#000000">
          <Path.OpacityMask>
            <RadialGradientBrush MappingMode="Absolute" Center="5,5"
                GradientOrigin="4,4" RadiusX="5" RadiusY="3">
              <RadialGradientBrush.GradientStops>
                <GradientStop Color="#00000000" Offset="0"/>
                <GradientStop Color="#FF000000" Offset="1"/>
              </RadialGradientBrush.GradientStops>
            </RadialGradientBrush>
          </Path.OpacityMask>
        </Path>
        <Path Name="image" Data="M 2,3 L 6,3 6,8 2,8 Z"
              Fill="{StaticResource remoteImage}"/>
        <Canvas>
          <Canvas.Resources>
            <ResourceDictionary>
              <SolidColorBrush x:Key="sharedPaint" Color="#FF00FF00"/>
            </ResourceDictionary>
          </Canvas.Resources>
          <Path Name="inner" Data="M 20,0 L 25,0 25,5 Z"
                Fill="{StaticResource sharedPaint}"/>
        </Canvas>
        <Path Name="after" Data="M 30,0 L 35,0 35,5 Z"
              Fill="{StaticResource sharedPaint}"/>
        <Glyphs Name="gradientLabel" FontUri="../Resources/font.odttf"
                FontRenderingEmSize="5" OriginX="30" OriginY="15"
                UnicodeString="gradient" Fill="{StaticResource sharedPaint}"/>
      </Canvas>
    </FixedPage>"""
    relationships = b"""<Relationships
      xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="RDictionary"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/shared.dict"/>
      <Relationship Id="RImage"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/pixel.png"/>
      <Relationship Id="RFont"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/font.odttf"/>
    </Relationships>"""
    data = make_dwfx(
        fixed_page=page,
        extra_entries={
            "Documents/1/Resources/shared.dict": remote_dictionary,
            "Documents/1/Pages/_rels/1.fpage.rels": relationships,
        },
    )

    package = ezdwf.inspect_dwfx(data)
    assert package.diagnostics == ()
    raw_page = package.pages[0]
    assert raw_page.resource_dictionaries == ("Documents/1/Resources/shared.dict",)
    outer, image_entity, inner, after, label = raw_page.entities
    assert outer.path is not None
    assert outer.path.transform == (2.0, 0.0, 0.0, 2.0, 1.0, 1.0)
    assert outer.style.fill is not None
    assert outer.style.fill.kind == "linear_gradient"
    assert tuple(stop.offset for stop in outer.style.fill.gradient_stops) == (0.0, 1.0)
    assert len(outer.opacity_mask_chain) == 2
    assert outer.opacity_mask_chain[-1].brush.kind == "radial_gradient"
    assert inner.style.fill is not None and inner.style.fill.kind == "solid"
    assert inner.style.fill.color == (0, 255, 0, 255)
    assert after.style.fill is not None and after.style.fill.kind == "linear_gradient"
    assert label.style.fill is not None and label.style.fill.kind == "linear_gradient"
    image_brush = image_entity.style.fill
    assert image_brush is not None and image_brush.kind == "image"
    assert image_brush.resource_part == "Documents/1/Resources/shared.dict"
    assert image_brush.normalized_source == "Documents/1/Resources/pixel.png"

    drawing = ezdwf.read(data)
    (
        normalized_outer,
        normalized_image,
        normalized_inner,
        normalized_after,
        normalized_label,
    ) = drawing.modelspace().entities
    assert normalized_outer.path[0].start == ezdwf.Point2D(1.0, 49.0)
    assert normalized_outer.style.nominal_stroke_width == 1.0
    assert isinstance(normalized_outer.style.fill_brush, ezdwf.GradientBrush)
    assert len(normalized_outer.opacity_masks) == 2
    assert isinstance(normalized_outer.opacity_masks[-1], ezdwf.GradientBrush)
    assert isinstance(normalized_inner.style.fill_brush, ezdwf.SolidBrush)
    assert isinstance(normalized_after.style.fill_brush, ezdwf.GradientBrush)
    assert isinstance(normalized_label.style.fill_brush, ezdwf.GradientBrush)
    assert normalized_image.style.fill_image is not None
    assert normalized_image.style.fill_image.source_viewport == (2.0, 3.0, 4.0, 5.0)
    assert normalized_image.style.fill_image.viewport == pytest.approx(
        (3.0, 40.0, 7.0, 45.0)
    )

    svg = ezdwf.render_svg(drawing, precision=4)
    root = ET.fromstring(svg)
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    assert root.findall(".//svg:linearGradient", namespace)
    assert root.findall(".//svg:radialGradient", namespace)
    assert len(root.findall(".//svg:mask", namespace)) == 2
    assert root.find(".//svg:mask[@id='canvas-mask-0']", namespace) is not None
    pattern = root.find(".//svg:pattern", namespace)
    assert pattern is not None
    assert pattern.attrib["data-xps-tile-mode"] == "FlipXY"
    assert "patternTransform" in pattern.attrib
    text = root.find(".//svg:text", namespace)
    assert text is not None
    assert text.attrib["fill"] == "url(#fill-brush-4)"
    assert root.find(".//svg:linearGradient[@id='fill-brush-4']", namespace) is not None


@pytest.mark.parametrize(
    ("brush_markup", "message"),
    (
        (
            """<LinearGradientBrush MappingMode="RelativeToBoundingBox"
                 StartPoint="0,0" EndPoint="1,0">
                 <GradientStop Color="#000000" Offset="0"/>
               </LinearGradientBrush>""",
            "MappingMode must be Absolute",
        ),
        (
            """<LinearGradientBrush MappingMode="Absolute"
                 StartPoint="0,0" EndPoint="1,0">
                 <GradientStop Color="#000000" Offset="1.1"/>
               </LinearGradientBrush>""",
            "at least two GradientStop",
        ),
        (
            """<ImageBrush ImageSource="../Resources/pixel.png"
                 Viewbox="0,0,1,1" ViewboxUnits="RelativeToBoundingBox"
                 Viewport="0,0,1,1" ViewportUnits="Absolute"/>""",
            "ViewboxUnits must be Absolute",
        ),
        (
            """<ImageBrush ImageSource="../Resources/pixel.png"
                 Viewport="0,0,1,1" ViewportUnits="Absolute"/>""",
            "missing required.*Viewbox",
        ),
        (
            """<ImageBrush ImageSource="../Resources/pixel.png"
                 Viewbox="0,0,1,1" Viewport="0,0,1,1"
                 TileMode="Mirror"/>""",
            "TileMode is invalid",
        ),
    ),
)
def test_dwfx_rejects_nonconforming_phase7_brushes(
    brush_markup: str, message: str
) -> None:
    page = f"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Data="M 0,0 L 5,0 5,5 Z"><Path.Fill>{brush_markup}</Path.Fill></Path>
    </FixedPage>""".encode()

    with pytest.raises(ezdwf.InvalidDwfError, match=message):
        ezdwf.inspect_dwfx(make_dwfx(fixed_page=page))


def test_dwfx_preserves_and_preprocesses_out_of_range_gradient_stops() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Data="M 0,0 L 5,0 5,5 Z">
        <Path.Fill>
          <LinearGradientBrush MappingMode="Absolute"
              StartPoint="0,0" EndPoint="5,0">
            <GradientStop Color="#FFFF0000" Offset="-1"/>
            <GradientStop Color="#FF0000FF" Offset="2"/>
          </LinearGradientBrush>
        </Path.Fill>
      </Path>
    </FixedPage>"""
    data = make_dwfx(fixed_page=page)

    package = ezdwf.inspect_dwfx(data)
    brush = package.pages[0].entities[0].style.fill
    assert brush is not None
    assert tuple(stop.offset for stop in brush.gradient_stops) == (-1.0, 2.0)

    root = ET.fromstring(ezdwf.render_svg(ezdwf.read(data)))
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    stops = root.findall(".//svg:linearGradient/svg:stop", namespace)
    assert [stop.attrib["offset"] for stop in stops] == ["0", "1"]
    assert [stop.attrib["stop-color"] for stop in stops] == ["#aa0055", "#5500aa"]


def test_dwfx_executes_inline_and_static_visual_brushes() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="40" Height="30">
      <FixedPage.Resources><ResourceDictionary>
        <Canvas x:Key="dot">
          <Path Data="M 0,0 L 2,0 L 2,2 L 0,2 Z" Fill="#ff0000"/>
        </Canvas>
        <VisualBrush x:Key="dotBrush" Visual="{StaticResource dot}"
            Viewbox="0,0,2,2" Viewport="0,0,4,4" TileMode="Tile"/>
      </ResourceDictionary></FixedPage.Resources>
      <Canvas Name="isolated" Opacity="0.5">
        <Path Name="static" Data="M 0,0 L 20,0 L 20,10 L 0,10 Z"
              Fill="{StaticResource dotBrush}"/>
        <Path Name="inline" Data="M 20,0 L 40,0 L 40,10 L 20,10 Z">
          <Path.Fill>
            <VisualBrush Viewbox="0,0,5,5" Viewport="20,0,5,5" TileMode="Tile">
              <VisualBrush.Visual><Canvas>
                <Path Data="M 0,0 L 5,5" Stroke="#0000ff"/>
              </Canvas></VisualBrush.Visual>
            </VisualBrush>
          </Path.Fill>
        </Path>
      </Canvas>
    </FixedPage>"""
    data = make_dwfx(fixed_page=page)

    raw = ezdwf.inspect_dwfx(data)
    raw_page = raw.pages[0]
    assert [entity.style.fill.kind for entity in raw_page.entities] == [
        "visual",
        "visual",
    ]
    assert all(len(entity.style.fill.entities) == 1 for entity in raw_page.entities)
    assert len(raw_page.canvas_groups) == 1
    assert raw_page.entities[0].canvas_groups[0] is raw_page.canvas_groups[0]
    assert raw_page.entities[1].canvas_groups[0] is raw_page.canvas_groups[0]

    entities = ezdwf.read(data).modelspace().entities
    assert all(
        isinstance(entity.style.fill_brush, ezdwf.VisualBrush) for entity in entities
    )
    assert all(entity.compositing_groups[0].opacity == 0.5 for entity in entities)

    root = ET.fromstring(ezdwf.render_svg(ezdwf.read(data), background="none"))
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    assert len(root.findall(".//svg:pattern", namespace)) == 2
    assert len(root.findall(".//svg:g[@data-xps-canvas='isolated']", namespace)) == 1
    assert len(root.findall(".//svg:pattern/svg:svg", namespace)) == 2


def test_visual_resource_resolves_packaged_font_from_dictionary_part() -> None:
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="20" Height="20">
      <FixedPage.Resources><ResourceDictionary Source="../Resources/shared.dict"/>
      </FixedPage.Resources>
      <Path Data="M 0,0 L 20,0 20,20 0,20 Z" Fill="{StaticResource labelBrush}"/>
    </FixedPage>"""
    dictionary = b"""<ResourceDictionary
      xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
      <Canvas x:Key="labelVisual">
        <Glyphs FontUri="font.odttf" FontRenderingEmSize="8"
                OriginX="1" OriginY="9" UnicodeString="remote" Fill="#000000"/>
      </Canvas>
      <VisualBrush x:Key="labelBrush" Visual="{StaticResource labelVisual}"
                   Viewbox="0,0,10,10" Viewport="0,0,10,10"/>
    </ResourceDictionary>"""
    relationships = b"""<Relationships
      xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="dictionary"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/shared.dict"/>
      <Relationship Id="font"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/font.odttf"/>
    </Relationships>"""
    data = make_dwfx(
        fixed_page=page,
        extra_entries={
            "Documents/1/Resources/shared.dict": dictionary,
            "Documents/1/Pages/_rels/1.fpage.rels": relationships,
        },
    )

    brush = ezdwf.inspect_dwfx(data).pages[0].entities[0].style.fill
    assert brush is not None and brush.kind == "visual"
    glyphs = brush.entities[0].glyphs
    assert glyphs is not None
    assert glyphs.font_resource_part == "Documents/1/Resources/shared.dict"
    assert glyphs.normalized_font_uri == "Documents/1/Resources/font.odttf"


def test_image_brush_uses_raster_dpi_for_viewbox_crop() -> None:
    physical = b"pHYs" + struct.pack(">IIB", 7_559, 7_559, 1)
    chunk = (
        struct.pack(">I", 9)
        + physical
        + struct.pack(">I", zlib.crc32(physical) & 0xFFFF_FFFF)
    )
    png = _PIXEL_PNG[:33] + chunk + _PIXEL_PNG[33:]
    page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <Path Data="M 0,0 L 10,0 L 10,10 Z"><Path.Fill>
        <ImageBrush ImageSource="../Resources/dpi.png"
          Viewbox="0.25,0,0.25,0.5" Viewport="0,0,10,10"/>
      </Path.Fill></Path>
    </FixedPage>"""
    data = make_dwfx(
        fixed_page=page,
        extra_entries={"Documents/1/Resources/dpi.png": png},
    )

    brush = ezdwf.read(data).modelspace().entities[0].style.fill_image
    assert brush is not None
    assert (brush.pixel_width, brush.pixel_height) == (1, 1)
    assert brush.dpi_x == pytest.approx(192.0, abs=0.01)
    assert brush.physical_size_dip == pytest.approx((0.5, 0.5), abs=0.001)

    root = ET.fromstring(ezdwf.render_svg(ezdwf.read(data), background="none"))
    namespace = {"svg": "http://www.w3.org/2000/svg"}
    crop = root.find(".//svg:pattern/svg:svg", namespace)
    assert crop is not None
    assert crop.attrib["viewBox"] == "0.25 0 0.25 0.5"
    assert crop.attrib["data-xps-viewbox-crop"] == "dpi"
    image = crop.find("svg:image", namespace)
    assert image is not None
    assert float(image.attrib["width"]) == pytest.approx(0.5, abs=0.001)


def test_dwfx_rejects_remote_dictionary_chaining_and_source_children() -> None:
    page_with_children = b"""<FixedPage
      xmlns="http://schemas.microsoft.com/xps/2005/06"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="20" Height="20">
      <FixedPage.Resources>
        <ResourceDictionary Source="../Resources/a.dict">
          <SolidColorBrush x:Key="local" Color="#000000"/>
        </ResourceDictionary>
      </FixedPage.Resources>
    </FixedPage>"""
    with pytest.raises(ezdwf.InvalidDwfError, match="cannot be combined"):
        ezdwf.inspect_dwfx(
            make_dwfx(
                fixed_page=page_with_children,
                extra_entries={
                    "Documents/1/Resources/a.dict": b"<ResourceDictionary/>"
                },
            )
        )

    page_with_remote = b"""<FixedPage
      xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="20" Height="20">
      <FixedPage.Resources>
        <ResourceDictionary Source="../Resources/a.dict"/>
      </FixedPage.Resources>
    </FixedPage>"""
    with pytest.raises(ezdwf.InvalidDwfError, match="cannot reference another"):
        ezdwf.inspect_dwfx(
            make_dwfx(
                fixed_page=page_with_remote,
                extra_entries={
                    "Documents/1/Resources/a.dict": (
                        b'<ResourceDictionary Source="b.dict"/>'
                    ),
                    "Documents/1/Resources/b.dict": b"<ResourceDictionary/>",
                },
            )
        )


def test_dwfx_required_resources_need_the_required_resource_relationship() -> None:
    wrong_relationship = b"""<Relationships
      xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="RImage"
        Type="http://schemas.microsoft.com/xps/2005/06/metadata"
        Target="../Resources/pixel.png"/>
      <Relationship Id="RFont"
        Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
        Target="../Resources/font.odttf"/>
    </Relationships>"""
    package = ezdwf.inspect_dwfx(
        make_dwfx(
            extra_entries={"Documents/1/Pages/_rels/1.fpage.rels": wrong_relationship}
        )
    )

    diagnostics = [
        item for item in package.diagnostics if item.code == "missing_xps_relationship"
    ]
    assert len(diagnostics) == 1
    assert "pixel.png" in diagnostics[0].message


def test_document_and_page_source_references_do_not_require_relationships() -> None:
    empty_relationships = b"""<Relationships
      xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"""
    package = ezdwf.inspect_dwfx(
        make_dwfx(
            extra_entries={
                "Documents/1/_rels/FixedDocumentSequence.fdseq.rels": (
                    empty_relationships
                ),
                "Documents/1/_rels/FixedDocument.fdoc.rels": empty_relationships,
            }
        )
    )

    assert package.sheet_count == 1
    assert package.diagnostics == ()


def test_dwfx_limits_and_unsafe_xml_fail_closed(dwfx_bytes: bytes) -> None:
    with pytest.raises(ezdwf.DwfLimitError, match="more than 3 visual"):
        ezdwf.inspect_dwfx(
            dwfx_bytes,
            limits=ezdwf.ParseLimits(max_xps_visuals=3),
        )
    with pytest.raises(ezdwf.DwfLimitError, match="more than 2 path segments"):
        ezdwf.inspect_dwfx(
            dwfx_bytes,
            limits=ezdwf.ParseLimits(max_xps_path_segments=2),
        )

    doctype_page = b"""<!DOCTYPE FixedPage [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
    <FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
               Width="10" Height="10"/>"""
    with pytest.raises(ezdwf.InvalidDwfError, match="DOCTYPE"):
        ezdwf.inspect_dwfx(make_dwfx(fixed_page=doctype_page))


def test_dwfx_rejects_relationship_target_that_escapes_package() -> None:
    unsafe_relationships = b"""<Relationships
      xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="unsafe"
        Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation"
        Target="../../escape.fdseq"/>
    </Relationships>"""
    data = make_dwfx(extra_entries={"_rels/.rels": unsafe_relationships})

    with pytest.raises(ezdwf.InvalidDwfError, match="escapes the package root"):
        ezdwf.inspect_dwfx(data)


def test_dwf_and_dwfx_share_numeric_paper_space_contract() -> None:
    xps_page = b"""<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
      Width="297" Height="210">
      <Path Data="M 2,207 L 7,202" Stroke="#000000"/>
      <Glyphs FontUri="../Resources/font.odttf" FontRenderingEmSize="10"
              OriginX="4" OriginY="204" UnicodeString="fixture" Fill="#000000"/>
    </FixedPage>"""
    dwf_sheet = ezdwf.read(make_dwf()).modelspace()
    dwfx_sheet = ezdwf.read(make_dwfx(fixed_page=xps_page)).modelspace()

    dwf_line = dwf_sheet.query("LINE").first
    dwfx_path = dwfx_sheet.query("PATH").first
    assert dwf_line is not None
    assert dwfx_path is not None
    assert (dwfx_path.path[0].start, dwfx_path.path[0].segments[0].end) == (
        dwf_line.points[0],
        dwf_line.points[1],
    )
    assert dwfx_sheet.query("TEXT").first is not None
    assert dwf_sheet.query("TEXT").first is not None
    assert dwfx_sheet.query("TEXT").first.points == dwf_sheet.query("TEXT").first.points


def test_dwfx_cli_inspect_and_render(dwfx_path: Path, tmp_path: Path, capsys) -> None:  # type: ignore[no-untyped-def]
    assert ezdwf.main(["inspect", str(dwfx_path)]) == 0
    output = capsys.readouterr().out
    assert "format: dwfx" in output
    assert "documents: 1" in output
    assert "sheets: 1" in output
    assert "entities: 4" in output

    assert ezdwf.main(["inspect", str(dwfx_path), "--json"]) == 0
    structured = json.loads(capsys.readouterr().out)
    assert structured["documents"][0]["pages"][0]["entities"][0]["kind"] == "path"

    destination = tmp_path / "fixture-dwfx.svg"
    assert ezdwf.main(["render", str(dwfx_path), str(destination)]) == 0
    assert destination.read_text(encoding="utf-8").startswith("<?xml")
