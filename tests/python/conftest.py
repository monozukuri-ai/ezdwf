from __future__ import annotations

import base64
from collections.abc import Mapping
from io import BytesIO
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

import pytest

DWF_HEADER = b"(DWF V06.00)"

DEFAULT_W2D = b"""(W2D V06.00)
(Units 'mm' ((1 0 0 0)(0 1 0 0)(0 0 1 0)(0 0 0 1)))
(Layer 4 'walls')(Color 12,34,56,255)(LineWeight 20)
F P 3 0,0 10,0 10,10 f L 0,0 5,5
(Text 2,3 'fixture' (Bounds 2,3 8,3 8,5 2,5))
(EndOfDWF)"""

DEFAULT_XPS_PAGE = b"""<?xml version="1.0" encoding="UTF-8"?>
<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06"
           xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
           Width="100" Height="50" Name="Fixture DWFx" xml:lang="en-US"
           ContentBox="0,0,100,50">
  <FixedPage.Resources>
    <ResourceDictionary>
      <SolidColorBrush x:Key="fixtureFill" Color="#80445566"/>
      <MatrixTransform x:Key="fixtureShift" Matrix="1,0,0,1,2,3"/>
      <PathGeometry x:Key="fixtureOutline" FillRule="NonZero"
                    Figures="M 0,0 L 10,0 10,10 Z"/>
    </ResourceDictionary>
  </FixedPage.Resources>
  <Canvas Name="Fixture Layer" RenderTransform="{StaticResource fixtureShift}"
          Opacity="0.75">
    <Path Name="outline" Data="{StaticResource fixtureOutline}"
          Fill="{StaticResource fixtureFill}" Stroke="#112233"
          StrokeThickness="2" StrokeDashArray="2,1"/>
    <Path Name="curves"
          Data="M 20,10 C 25,5 30,15 35,10 Q 40,5 45,10 A 5,3 30 0 1 55,20"
          Stroke="#ff0000" StrokeThickness="1"/>
    <Path Name="image" Data="M 30,30 L 40,30 40,40 30,40 Z">
      <Path.Fill>
        <ImageBrush ImageSource="../Resources/pixel.png"
                    Viewbox="0,0,1,1" Viewport="30,30,10,10"/>
      </Path.Fill>
    </Path>
    <Glyphs Name="label" FontUri="../Resources/font.odttf"
            FontRenderingEmSize="8" OriginX="60" OriginY="20"
            UnicodeString="DWFx" Fill="#000000"/>
  </Canvas>
</FixedPage>"""

_PIXEL_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZlL8AAAAASUVORK5CYII="
)


def zip_bytes(entries: Mapping[str, bytes], *, prefix: bytes = b"") -> bytes:
    output = BytesIO()
    output.write(prefix)
    with ZipFile(output, mode="a", compression=ZIP_DEFLATED) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return output.getvalue()


def make_dwf(
    *, w2d: bytes = DEFAULT_W2D, extra_entries: Mapping[str, bytes] | None = None
) -> bytes:
    manifest = b"""<?xml version="1.0" encoding="UTF-8"?>
<dwf:Manifest xmlns:dwf="DWF-Manifest:6.0" version="6.0" objectId="fixture">
  <dwf:Properties><dwf:Property name="Creator" value="pytest"/></dwf:Properties>
  <dwf:Interfaces><dwf:Interface name="ePlot"/></dwf:Interfaces>
  <dwf:Sections>
    <dwf:Section type="com.autodesk.dwf.ePlot" name="sheet" title="Fixture Sheet">
      <dwf:Source provider="pytest" href="fixture.dwg"/>
      <dwf:Toc>
        <dwf:Resource dwf:role="descriptor" dwf:mime="text/xml" dwf:href="sheet\\descriptor.xml"/>
        <dwf:Resource dwf:role="2d streaming graphics" dwf:mime="application/x-w2d" dwf:href="sheet\\main.w2d"/>
      </dwf:Toc>
    </dwf:Section>
  </dwf:Sections>
</dwf:Manifest>"""
    descriptor = b"""<?xml version="1.0" encoding="UTF-8"?>
<ePlot:Page xmlns:ePlot="DWF-ePlot:1.2" version="1.2" plotOrder="1" name="Fixture Sheet" color="128 128 128">
  <ePlot:Paper show="true" units="mm" width="297" height="210" clip="0 0 297 210" color="255 255 255"/>
  <ePlot:Resources>
    <ePlot:GraphicResource role="2d streaming graphics" mime="application/x-w2d" href="sheet\\main.w2d" size="0" transform="1 0 0 0 0 1 0 0 0 0 1 0 2 3 0 1"/>
  </ePlot:Resources>
</ePlot:Page>"""
    entries = {
        "manifest.xml": manifest,
        "sheet\\descriptor.xml": descriptor,
        "sheet\\main.w2d": w2d,
    }
    if extra_entries:
        entries.update(extra_entries)
    return zip_bytes(entries, prefix=DWF_HEADER)


def make_dwfx(
    *,
    fixed_page: bytes = DEFAULT_XPS_PAGE,
    extra_entries: Mapping[str, bytes] | None = None,
) -> bytes:
    entries = {
        "[Content_Types].xml": b"""<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="fdseq" ContentType="application/vnd.ms-package.xps-fixeddocumentsequence+xml"/>
  <Default Extension="fdoc" ContentType="application/vnd.ms-package.xps-fixeddocument+xml"/>
  <Default Extension="fpage" ContentType="application/vnd.ms-package.xps-fixedpage+xml"/>
  <Default Extension="odttf" ContentType="application/vnd.ms-package.obfuscated-opentype"/>
  <Default Extension="png" ContentType="image/png"/>
</Types>""",
        "_rels/.rels": b"""<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="R1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation"
                Target="/Documents/1/FixedDocumentSequence.fdseq"/>
  <Relationship Id="RExternal" Type="https://example.invalid/metadata"
                Target="https://example.invalid/not-fetched" TargetMode="External"/>
</Relationships>""",
        "Documents/1/FixedDocumentSequence.fdseq": b"""<?xml version="1.0"?>
<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06">
  <DocumentReference Source="FixedDocument.fdoc"/>
</FixedDocumentSequence>""",
        "Documents/1/_rels/FixedDocumentSequence.fdseq.rels": b"""<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="RDocument" Type="http://schemas.microsoft.com/xps/2005/06/document"
                Target="FixedDocument.fdoc"/>
</Relationships>""",
        "Documents/1/FixedDocument.fdoc": b"""<?xml version="1.0"?>
<FixedDocument xmlns="http://schemas.microsoft.com/xps/2005/06">
  <PageContent Source="Pages/1.fpage"/>
</FixedDocument>""",
        "Documents/1/_rels/FixedDocument.fdoc.rels": b"""<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="RPage" Type="http://schemas.microsoft.com/xps/2005/06/page"
                Target="Pages/1.fpage"/>
</Relationships>""",
        "Documents/1/Pages/1.fpage": fixed_page,
        "Documents/1/Pages/_rels/1.fpage.rels": b"""<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="RImage" Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
                Target="../Resources/pixel.png"/>
  <Relationship Id="RFont" Type="http://schemas.microsoft.com/xps/2005/06/required-resource"
                Target="../Resources/font.odttf"/>
</Relationships>""",
        "Documents/1/Resources/pixel.png": _PIXEL_PNG,
        "Documents/1/Resources/font.odttf": b"fixture-obfuscated-font",
    }
    if extra_entries:
        entries.update(extra_entries)
    return zip_bytes(entries)


@pytest.fixture
def dwf_bytes() -> bytes:
    return make_dwf()


@pytest.fixture
def dwf_path(tmp_path: Path, dwf_bytes: bytes) -> Path:
    path = tmp_path / "fixture.dwf"
    path.write_bytes(dwf_bytes)
    return path


@pytest.fixture
def dwfx_bytes() -> bytes:
    return make_dwfx()


@pytest.fixture
def dwfx_path(tmp_path: Path, dwfx_bytes: bytes) -> Path:
    path = tmp_path / "fixture.dwfx"
    path.write_bytes(dwfx_bytes)
    return path
