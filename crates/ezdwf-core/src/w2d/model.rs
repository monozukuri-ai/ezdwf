use serde::Serialize;

use crate::Diagnostic;

/// A raw logical-coordinate point from a W2D display list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct W2dPoint {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct W2dColoredPoint {
    pub point: W2dPoint,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct W2dImage {
    pub format: String,
    pub identifier: i32,
    pub columns: u16,
    pub rows: u16,
    pub min: W2dPoint,
    pub max: W2dPoint,
    pub color_map: Vec<[u8; 4]>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct W2dEmbeddedFont {
    pub request: u32,
    pub privilege: u8,
    pub charset: u8,
    pub typeface_name: String,
    pub logfont_name: String,
    pub data: Vec<u8>,
    pub source: W2dSourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct W2dBlockRef {
    pub format: String,
    /// The complete operand bytes are retained because BlockRef was a
    /// short-lived 0.55 metadata envelope with many revisioned fields.
    pub payload: Vec<u8>,
    pub source: W2dSourceSpan,
}

/// The source record responsible for one decoded entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct W2dSourceSpan {
    /// Physical byte offset in the original resource. For an entity decoded
    /// from compressed data this points at the containing wrapper.
    pub offset: usize,
    /// Physical source length, or the record length for uncompressed data.
    pub length: usize,
    pub opcode: String,
    /// Offset within the expanded wrapper, when compression was involved.
    pub decoded_offset: Option<usize>,
    /// Record length in expanded bytes, when compression was involved.
    pub decoded_length: Option<usize>,
    pub compression_depth: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct W2dLayer {
    pub number: i32,
    pub name: Option<String>,
}

/// Font rendition values. Numeric angles remain in native W2D units
/// (65,536 units per full turn), and scale values remain unnormalized.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct W2dFont {
    pub name: Option<String>,
    pub canonical_name: Option<String>,
    pub charset: Option<u8>,
    pub pitch: Option<u8>,
    pub family: Option<u8>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub height: Option<i32>,
    pub rotation: Option<u16>,
    pub width_scale: Option<u16>,
    pub spacing: Option<u16>,
    pub oblique: Option<u16>,
    pub flags: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct W2dLineStyle {
    pub pattern: Option<String>,
    pub weight: Option<i32>,
    pub adapt_patterns: Option<bool>,
    pub pattern_scale: Option<f64>,
    pub line_start_cap: Option<String>,
    pub line_end_cap: Option<String>,
    pub dash_start_cap: Option<String>,
    pub dash_end_cap: Option<String>,
    pub line_join: Option<String>,
    pub miter_angle: Option<i32>,
    pub miter_length: Option<i32>,
}

/// Resolved rendition state captured when an entity is emitted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct W2dRendition {
    pub color: Option<[u8; 4]>,
    pub color_index: Option<u16>,
    pub layer: Option<W2dLayer>,
    pub line: W2dLineStyle,
    pub fill: bool,
    pub fill_pattern: Option<String>,
    pub font: W2dFont,
    pub visibility: bool,
    pub viewport: Option<String>,
}

impl Default for W2dRendition {
    fn default() -> Self {
        Self {
            color: None,
            color_index: None,
            layer: None,
            line: W2dLineStyle::default(),
            fill: false,
            fill_pattern: None,
            font: W2dFont::default(),
            visibility: true,
            viewport: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum W2dGeometry {
    Line {
        start: W2dPoint,
        end: W2dPoint,
    },
    Polyline {
        points: Vec<W2dPoint>,
    },
    /// Marker glyphs at each point (`M` / `m` / `0x8D` draw-polymarker opcodes).
    Polymarker {
        points: Vec<W2dPoint>,
    },
    Polygon {
        points: Vec<W2dPoint>,
    },
    Circle {
        center: W2dPoint,
        radius: i64,
        start_angle: u32,
        end_angle: u32,
    },
    Ellipse {
        center: W2dPoint,
        major: i64,
        minor: i64,
        start_angle: u32,
        end_angle: u32,
        tilt: u32,
    },
    PolyBezier {
        points: Vec<W2dPoint>,
    },
    Text {
        position: W2dPoint,
        text: String,
        bounds: Option<[W2dPoint; 4]>,
    },
    Polytriangle {
        points: Vec<W2dPoint>,
    },
    GouraudPolyline {
        points: Vec<W2dColoredPoint>,
    },
    GouraudPolytriangle {
        points: Vec<W2dColoredPoint>,
    },
    TexturedPolytriangle {
        points: Vec<W2dPoint>,
    },
    ContourSet {
        contours: Vec<Vec<W2dPoint>>,
    },
    Image {
        image: W2dImage,
    },
}

impl W2dGeometry {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::Polyline { .. } => "polyline",
            Self::Polymarker { .. } => "polymarker",
            Self::Polygon { .. } => "polygon",
            Self::Circle { .. } => "circle",
            Self::Ellipse { .. } => "ellipse",
            Self::PolyBezier { .. } => "poly_bezier",
            Self::Text { .. } => "text",
            Self::Polytriangle { .. } => "polytriangle",
            Self::GouraudPolyline { .. } => "gouraud_polyline",
            Self::GouraudPolytriangle { .. } => "gouraud_polytriangle",
            Self::TexturedPolytriangle { .. } => "textured_polytriangle",
            Self::ContourSet { .. } => "contour_set",
            Self::Image { .. } => "image",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct W2dEntity {
    pub geometry: W2dGeometry,
    pub rendition: W2dRendition,
    pub source: W2dSourceSpan,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct W2dUnits {
    pub name: String,
    pub transform: [f64; 16],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct W2dViewport {
    pub name: String,
    pub contours: Vec<Vec<W2dPoint>>,
    pub units: Option<W2dUnits>,
}

/// One decoded `application/x-w2d` package resource.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct W2dStream {
    pub href: String,
    pub role: String,
    pub mime: String,
    /// `w2d` for package streams or `legacy_dwf` for WHIP 00.xx files.
    pub source_format: String,
    pub version: String,
    pub source_size: usize,
    pub decompressed_size: usize,
    pub compressed_blocks: usize,
    pub complete: bool,
    pub end_of_dwf_seen: bool,
    /// Conservative bounds in raw W2D logical coordinates: min-x, min-y,
    /// max-x, max-y. Ellipse rotation and arc sweep are intentionally not
    /// used to shrink this envelope.
    pub logical_bounds: Option<[i64; 4]>,
    pub transform: Option<Vec<f64>>,
    pub clip: Option<Vec<f64>>,
    pub units: Option<W2dUnits>,
    pub layers: Vec<W2dLayer>,
    pub viewports: Vec<W2dViewport>,
    pub color_maps: Vec<Vec<[u8; 4]>>,
    pub embedded_fonts: Vec<W2dEmbeddedFont>,
    pub block_refs: Vec<W2dBlockRef>,
    pub entities: Vec<W2dEntity>,
    pub diagnostics: Vec<Diagnostic>,
}
