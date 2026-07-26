use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::{ArchiveEntry, Diagnostic, DwfFormat};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpcContentType {
    pub extension: Option<String>,
    pub part_name: Option<String>,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpcRelationship {
    /// `None` identifies the package root; otherwise this is the source part.
    pub source: Option<String>,
    pub id: String,
    pub relationship_type: String,
    pub target: String,
    pub target_mode: String,
    /// Resolved package part for an internal target. External targets remain `None`.
    pub normalized_target: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct XpsPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct XpsMatrix {
    pub m11: f64,
    pub m12: f64,
    pub m21: f64,
    pub m22: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

impl Default for XpsMatrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl XpsMatrix {
    pub const IDENTITY: Self = Self {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
    };

    /// Compose matrices so the returned matrix applies `local`, then `self`.
    #[must_use]
    pub fn compose(self, local: Self) -> Self {
        Self {
            m11: self.m11.mul_add(local.m11, self.m21 * local.m12),
            m12: self.m12.mul_add(local.m11, self.m22 * local.m12),
            m21: self.m11.mul_add(local.m21, self.m21 * local.m22),
            m22: self.m12.mul_add(local.m21, self.m22 * local.m22),
            offset_x: self.m11.mul_add(
                local.offset_x,
                self.m21.mul_add(local.offset_y, self.offset_x),
            ),
            offset_y: self.m12.mul_add(
                local.offset_x,
                self.m22.mul_add(local.offset_y, self.offset_y),
            ),
        }
    }

    #[must_use]
    pub fn transform_point(self, point: XpsPoint) -> XpsPoint {
        XpsPoint {
            x: self
                .m11
                .mul_add(point.x, self.m21.mul_add(point.y, self.offset_x)),
            y: self
                .m12
                .mul_add(point.x, self.m22.mul_add(point.y, self.offset_y)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XpsSourceSpan {
    pub offset: usize,
    pub length: usize,
    pub element: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XpsPathSegment {
    Line {
        end: XpsPoint,
        stroked: bool,
        smooth_join: bool,
    },
    CubicBezier {
        control1: XpsPoint,
        control2: XpsPoint,
        end: XpsPoint,
        stroked: bool,
        smooth_join: bool,
    },
    QuadraticBezier {
        control: XpsPoint,
        end: XpsPoint,
        stroked: bool,
        smooth_join: bool,
    },
    Arc {
        radius: XpsPoint,
        rotation_degrees: f64,
        large_arc: bool,
        sweep_clockwise: bool,
        end: XpsPoint,
        stroked: bool,
        smooth_join: bool,
    },
}

impl XpsPathSegment {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::CubicBezier { .. } => "cubic_bezier",
            Self::QuadraticBezier { .. } => "quadratic_bezier",
            Self::Arc { .. } => "arc",
        }
    }

    #[must_use]
    pub const fn end(&self) -> XpsPoint {
        match self {
            Self::Line { end, .. }
            | Self::CubicBezier { end, .. }
            | Self::QuadraticBezier { end, .. }
            | Self::Arc { end, .. } => *end,
        }
    }

    #[must_use]
    pub const fn stroked(&self) -> bool {
        match self {
            Self::Line { stroked, .. }
            | Self::CubicBezier { stroked, .. }
            | Self::QuadraticBezier { stroked, .. }
            | Self::Arc { stroked, .. } => *stroked,
        }
    }

    #[must_use]
    pub const fn smooth_join(&self) -> bool {
        match self {
            Self::Line { smooth_join, .. }
            | Self::CubicBezier { smooth_join, .. }
            | Self::QuadraticBezier { smooth_join, .. }
            | Self::Arc { smooth_join, .. } => *smooth_join,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsPathFigure {
    pub start: XpsPoint,
    pub segments: Vec<XpsPathSegment>,
    pub closed: bool,
    pub filled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsPathGeometry {
    pub fill_rule: String,
    pub figures: Vec<XpsPathFigure>,
    /// Original abbreviated geometry when one was present.
    pub data: Option<String>,
    /// Local geometry transform applied before the owning visual transform.
    pub transform: XpsMatrix,
}

impl XpsPathGeometry {
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.figures
            .iter()
            .map(|figure| figure.segments.len())
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsGradientStop {
    /// Resolved sRGB preview color. ContextColor values remain unresolved.
    pub color: Option<[u8; 4]>,
    pub color_value: String,
    pub offset: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsImageMetadata {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub dpi_x: f64,
    pub dpi_y: f64,
}

impl XpsImageMetadata {
    #[must_use]
    pub fn physical_size_dip(&self) -> [f64; 2] {
        [
            f64::from(self.pixel_width) * 96.0 / self.dpi_x,
            f64::from(self.pixel_height) * 96.0 / self.dpi_y,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsVisual {
    pub entities: Vec<XpsEntity>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XpsBrush {
    Solid {
        color: [u8; 4],
        opacity: f64,
        attributes: BTreeMap<String, String>,
    },
    Image {
        source: String,
        /// Part containing the ImageBrush declaration. Relative image URIs use
        /// this base, which differs from the FixedPage for remote dictionaries.
        resource_part: String,
        normalized_source: Option<String>,
        content_type: Option<String>,
        data: Vec<u8>,
        image_metadata: Option<XpsImageMetadata>,
        viewbox: Option<[f64; 4]>,
        viewport: Option<[f64; 4]>,
        viewbox_units: String,
        viewport_units: String,
        tile_mode: Option<String>,
        transform: XpsMatrix,
        opacity: f64,
        attributes: BTreeMap<String, String>,
    },
    Visual {
        visual: Option<Arc<XpsVisual>>,
        viewbox: [f64; 4],
        viewport: [f64; 4],
        viewbox_units: String,
        viewport_units: String,
        tile_mode: Option<String>,
        transform: XpsMatrix,
        opacity: f64,
        attributes: BTreeMap<String, String>,
    },
    LinearGradient {
        start_point: XpsPoint,
        end_point: XpsPoint,
        spread_method: String,
        mapping_mode: String,
        transform: XpsMatrix,
        gradient_stops: Vec<XpsGradientStop>,
        opacity: f64,
        attributes: BTreeMap<String, String>,
    },
    RadialGradient {
        center: XpsPoint,
        gradient_origin: XpsPoint,
        radius_x: f64,
        radius_y: f64,
        spread_method: String,
        mapping_mode: String,
        transform: XpsMatrix,
        gradient_stops: Vec<XpsGradientStop>,
        opacity: f64,
        attributes: BTreeMap<String, String>,
    },
    Unsupported {
        brush_type: String,
        attributes: BTreeMap<String, String>,
    },
}

impl XpsBrush {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Solid { .. } => "solid",
            Self::Image { .. } => "image",
            Self::LinearGradient { .. } => "linear_gradient",
            Self::RadialGradient { .. } => "radial_gradient",
            Self::Visual { .. } => "visual",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

/// One opacity brush in the effective ancestor-to-visual mask chain.
///
/// `transform` maps the coordinate space of the element declaring the mask to
/// FixedPage coordinates. The brush's own transform remains on `brush`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsOpacityMask {
    pub brush: Arc<XpsBrush>,
    pub transform: XpsMatrix,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsStyle {
    pub fill: Option<XpsBrush>,
    pub stroke: Option<XpsBrush>,
    pub stroke_thickness: f64,
    pub stroke_dash_array: Vec<f64>,
    pub stroke_dash_offset: f64,
    pub stroke_start_line_cap: Option<String>,
    pub stroke_end_line_cap: Option<String>,
    pub stroke_dash_cap: Option<String>,
    pub stroke_line_join: Option<String>,
    pub stroke_miter_limit: Option<f64>,
    pub opacity: f64,
}

impl Default for XpsStyle {
    fn default() -> Self {
        Self {
            fill: None,
            stroke: None,
            stroke_thickness: 1.0,
            stroke_dash_array: Vec::new(),
            stroke_dash_offset: 0.0,
            stroke_start_line_cap: None,
            stroke_end_line_cap: None,
            stroke_dash_cap: None,
            stroke_line_join: None,
            stroke_miter_limit: None,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsGlyphs {
    pub unicode_string: String,
    pub origin: XpsPoint,
    pub font_uri: String,
    /// Part containing the Glyphs declaration. Relative FontUri values use this base.
    pub font_resource_part: String,
    pub normalized_font_uri: Option<String>,
    pub font_rendering_em_size: f64,
    pub indices: Option<String>,
    pub style_simulations: Option<String>,
    pub bidi_level: Option<u32>,
    pub sideways: bool,
    pub font_part: Option<String>,
    pub font_content_type: Option<String>,
    pub font_obfuscated: bool,
    /// Combined positioned glyph outlines in the Glyphs local coordinate space.
    pub outline: Option<XpsPathGeometry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsCanvasGroup {
    pub id: usize,
    pub name: Option<String>,
    /// Local Canvas opacity. Ancestor opacity is represented by ancestor groups.
    pub opacity: f64,
    /// Effective Canvas transform from this group's local space to FixedPage space.
    pub transform: XpsMatrix,
    pub clip: Option<Arc<XpsPathGeometry>>,
    pub opacity_mask: Option<Arc<XpsBrush>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XpsGeometry {
    Path { geometry: XpsPathGeometry },
    Glyphs { glyphs: Box<XpsGlyphs> },
}

/// One clip in the effective ancestor-to-visual clip chain.
///
/// `transform` maps the clip's local coordinates into FixedPage coordinates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsClip {
    pub geometry: Arc<XpsPathGeometry>,
    pub transform: XpsMatrix,
}

impl XpsGeometry {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Path { .. } => "path",
            Self::Glyphs { .. } => "glyphs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsEntity {
    pub name: Option<String>,
    pub canvas_name: Option<String>,
    pub navigate_uri: Option<String>,
    pub transform: XpsMatrix,
    /// The visual's local clip, retained for source-oriented inspection.
    pub clip: Option<XpsPathGeometry>,
    /// Effective Canvas clips followed by the visual's local clip.
    pub clip_chain: Vec<XpsClip>,
    /// The visual's local opacity mask, retained for source inspection.
    pub opacity_mask: Option<XpsBrush>,
    /// Effective Canvas masks followed by the visual's local mask.
    pub opacity_mask_chain: Vec<XpsOpacityMask>,
    /// Ancestor Canvas groups in outer-to-inner order for isolated compositing.
    pub canvas_groups: Vec<XpsCanvasGroup>,
    pub style: XpsStyle,
    pub geometry: XpsGeometry,
    pub source: XpsSourceSpan,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsPage {
    pub part_name: String,
    pub name: String,
    pub language: Option<String>,
    pub width: f64,
    pub height: f64,
    pub content_box: Option<[f64; 4]>,
    pub bleed_box: Option<[f64; 4]>,
    /// Package parts referenced by remote ResourceDictionary elements.
    pub resource_dictionaries: Vec<String>,
    pub relationships: Vec<OpcRelationship>,
    pub entities: Vec<XpsEntity>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XpsDocument {
    pub part_name: String,
    pub relationships: Vec<OpcRelationship>,
    pub pages: Vec<XpsPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DwfxPackage {
    pub format: DwfFormat,
    pub entries: Vec<ArchiveEntry>,
    pub content_types: Vec<OpcContentType>,
    pub relationships: Vec<OpcRelationship>,
    pub document_sequence: String,
    pub documents: Vec<XpsDocument>,
    pub diagnostics: Vec<Diagnostic>,
}

impl DwfxPackage {
    #[must_use]
    pub fn sheet_count(&self) -> usize {
        self.documents
            .iter()
            .map(|document| document.pages.len())
            .sum()
    }

    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.documents
            .iter()
            .flat_map(|document| &document.pages)
            .map(|page| page.entities.len())
            .sum()
    }

    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn pages(&self) -> impl Iterator<Item = &XpsPage> {
        self.documents.iter().flat_map(|document| &document.pages)
    }
}
