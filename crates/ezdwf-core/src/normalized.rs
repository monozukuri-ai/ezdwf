//! Backend-neutral 2D entities in bottom-left-origin paper coordinates.
//!
//! W2D logical integers remain available through the raw model.  This module
//! applies DWF ePlot or XPS transforms and keeps source indexes so callers can
//! move between normalized and raw entities without reparsing or guessing.

use serde::Serialize;

use crate::{
    DwfError, DwfPackage, DwfxPackage, W2dEntity, W2dFont, W2dGeometry, W2dRendition,
    W2dSourceSpan, W2dStream, XpsBrush, XpsEntity, XpsGeometry, XpsMatrix, XpsPathGeometry,
    XpsPathSegment, XpsPoint, XpsStyle,
};

const W2D_FULL_TURN: f64 = 65_536.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

impl Point2D {
    #[must_use]
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }
}

/// A two-dimensional affine matrix using SVG's `(a, b, c, d, e, f)` order.
///
/// Points are transformed as `x' = a*x + c*y + e` and
/// `y' = b*x + d*y + f`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Affine2D {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Affine2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2D {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn from_resource(
        values: Option<&[f64]>,
        section: &str,
        resource: &str,
    ) -> Result<Self, DwfError> {
        let Some(values) = values else {
            return Ok(Self::IDENTITY);
        };
        if values.len() != 16 {
            return Err(DwfError::InvalidTransform {
                section: section.to_owned(),
                resource: resource.to_owned(),
                context: format!("expected 16 matrix values, got {}", values.len()),
            });
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(DwfError::InvalidTransform {
                section: section.to_owned(),
                resource: resource.to_owned(),
                context: "matrix contains a non-finite value".to_owned(),
            });
        }
        // DWF/ePlot serializes its 4x4 transform in column-major order.
        Ok(Self {
            a: values[0],
            b: values[1],
            c: values[4],
            d: values[5],
            e: values[12],
            f: values[13],
        })
    }

    #[must_use]
    pub fn transform_point(self, point: crate::W2dPoint) -> Point2D {
        let x = point.x as f64;
        let y = point.y as f64;
        Point2D {
            x: self.a.mul_add(x, self.c.mul_add(y, self.e)),
            y: self.b.mul_add(x, self.d.mul_add(y, self.f)),
        }
    }

    #[must_use]
    pub fn transform_xps_point(self, point: XpsPoint) -> Point2D {
        Point2D {
            x: self.a.mul_add(point.x, self.c.mul_add(point.y, self.e)),
            y: self.b.mul_add(point.x, self.d.mul_add(point.y, self.f)),
        }
    }

    #[must_use]
    pub const fn from_xps(matrix: XpsMatrix) -> Self {
        Self {
            a: matrix.m11,
            b: matrix.m12,
            c: matrix.m21,
            d: matrix.m22,
            e: matrix.offset_x,
            f: matrix.offset_y,
        }
    }

    /// Compose transforms so the returned transform applies `local`, then `self`.
    #[must_use]
    pub fn compose(self, local: Self) -> Self {
        Self {
            a: self.a.mul_add(local.a, self.c * local.b),
            b: self.b.mul_add(local.a, self.d * local.b),
            c: self.a.mul_add(local.c, self.c * local.d),
            d: self.b.mul_add(local.c, self.d * local.d),
            e: self.a.mul_add(local.e, self.c.mul_add(local.f, self.e)),
            f: self.b.mul_add(local.e, self.d.mul_add(local.f, self.f)),
        }
    }

    #[must_use]
    pub fn transform_vector(self, vector: Point2D) -> Point2D {
        Point2D {
            x: self.a.mul_add(vector.x, self.c * vector.y),
            y: self.b.mul_add(vector.x, self.d * vector.y),
        }
    }

    #[must_use]
    pub fn nominal_scale(self) -> f64 {
        let x_scale = self.a.hypot(self.b);
        let y_scale = self.c.hypot(self.d);
        (x_scale + y_scale) * 0.5
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedStyle {
    pub layer_number: Option<i32>,
    pub layer_name: Option<String>,
    pub color: Option<[u8; 4]>,
    pub color_index: Option<u16>,
    pub line_pattern: Option<String>,
    pub line_weight_logical: Option<i32>,
    /// Nominal paper-space width. For anisotropic transforms this uses the
    /// mean of the transformed x/y unit-vector lengths; the raw value remains
    /// available in `line_weight_logical`.
    pub nominal_stroke_width: Option<f64>,
    pub fill: bool,
    pub fill_pattern: Option<String>,
    pub font: W2dFont,
    pub font_height: Option<f64>,
    pub font_rotation_degrees: Option<f64>,
    pub visible: bool,
    pub viewport: Option<String>,
    /// XPS can use different stroke and fill brushes; `color` remains the
    /// backend-neutral primary color for query compatibility.
    pub stroke_color: Option<[u8; 4]>,
    pub fill_color: Option<[u8; 4]>,
    pub opacity: f64,
    pub stroke_dash_array: Vec<f64>,
    pub stroke_dash_offset: f64,
    pub fill_brush: Option<NormalizedBrush>,
    pub stroke_brush: Option<NormalizedBrush>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedImageBrush {
    pub source: String,
    pub resource_part: String,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
    pub pixel_width: Option<u32>,
    pub pixel_height: Option<u32>,
    pub dpi_x: Option<f64>,
    pub dpi_y: Option<f64>,
    /// Intrinsic raster size in XPS device-independent pixels.
    pub physical_size_dip: Option<[f64; 2]>,
    pub viewbox: Option<[f64; 4]>,
    /// Axis-aligned paper-space bounds retained for compatibility.
    pub viewport: Option<[f64; 4]>,
    /// Original viewport before applying `transform`.
    pub source_viewport: Option<[f64; 4]>,
    pub viewbox_units: String,
    pub viewport_units: String,
    pub tile_mode: Option<String>,
    /// Maps brush-local coordinates to normalized paper coordinates.
    pub transform: Affine2D,
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedVisualBrush {
    /// Brush-local visuals in bottom-left, positive-Y-up coordinates.
    pub entities: Vec<NormalizedEntity>,
    /// Original XPS `(x, y, width, height)` viewbox.
    pub viewbox: [f64; 4],
    /// Axis-aligned paper-space bounds retained for compatibility.
    pub viewport: [f64; 4],
    /// Original viewport before applying `transform`.
    pub source_viewport: [f64; 4],
    pub viewbox_units: String,
    pub viewport_units: String,
    pub tile_mode: Option<String>,
    /// Maps brush-local coordinates to normalized paper coordinates.
    pub transform: Affine2D,
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedGradientStop {
    pub color: Option<[u8; 4]>,
    pub color_value: String,
    pub offset: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedBrush {
    Solid {
        color: [u8; 4],
        opacity: f64,
    },
    Image {
        brush: NormalizedImageBrush,
    },
    Visual {
        brush: NormalizedVisualBrush,
    },
    LinearGradient {
        start_point: Point2D,
        end_point: Point2D,
        spread_method: String,
        mapping_mode: String,
        gradient_stops: Vec<NormalizedGradientStop>,
        opacity: f64,
    },
    RadialGradient {
        center: Point2D,
        gradient_origin: Point2D,
        x_axis: Point2D,
        y_axis: Point2D,
        spread_method: String,
        mapping_mode: String,
        gradient_stops: Vec<NormalizedGradientStop>,
        opacity: f64,
    },
    Unsupported {
        brush_type: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct NormalizedColoredPoint {
    pub point: Point2D,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedImage {
    pub format: String,
    pub identifier: i32,
    pub columns: u16,
    pub rows: u16,
    pub min: Point2D,
    pub max: Point2D,
    pub color_map: Vec<[u8; 4]>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedPathSegment {
    Line {
        end: Point2D,
        stroked: bool,
        smooth_join: bool,
    },
    CubicBezier {
        control1: Point2D,
        control2: Point2D,
        end: Point2D,
        stroked: bool,
        smooth_join: bool,
    },
    QuadraticBezier {
        control: Point2D,
        end: Point2D,
        stroked: bool,
        smooth_join: bool,
    },
    EllipticalArc {
        center: Point2D,
        x_axis: Point2D,
        y_axis: Point2D,
        start_angle_degrees: f64,
        sweep_angle_degrees: f64,
        end: Point2D,
        stroked: bool,
        smooth_join: bool,
    },
}

impl NormalizedPathSegment {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::CubicBezier { .. } => "cubic_bezier",
            Self::QuadraticBezier { .. } => "quadratic_bezier",
            Self::EllipticalArc { .. } => "elliptical_arc",
        }
    }

    #[must_use]
    pub const fn end(&self) -> Point2D {
        match self {
            Self::Line { end, .. }
            | Self::CubicBezier { end, .. }
            | Self::QuadraticBezier { end, .. }
            | Self::EllipticalArc { end, .. } => *end,
        }
    }

    #[must_use]
    pub const fn stroked(&self) -> bool {
        match self {
            Self::Line { stroked, .. }
            | Self::CubicBezier { stroked, .. }
            | Self::QuadraticBezier { stroked, .. }
            | Self::EllipticalArc { stroked, .. } => *stroked,
        }
    }

    #[must_use]
    pub const fn smooth_join(&self) -> bool {
        match self {
            Self::Line { smooth_join, .. }
            | Self::CubicBezier { smooth_join, .. }
            | Self::QuadraticBezier { smooth_join, .. }
            | Self::EllipticalArc { smooth_join, .. } => *smooth_join,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedPathFigure {
    pub start: Point2D,
    pub segments: Vec<NormalizedPathSegment>,
    pub closed: bool,
    pub filled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedClip {
    pub fill_rule: String,
    pub figures: Vec<NormalizedPathFigure>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedCompositingGroup {
    pub id: usize,
    pub name: Option<String>,
    /// Local Canvas opacity. Ancestor values are represented by ancestor groups.
    pub opacity: f64,
    pub clip: Option<NormalizedClip>,
    pub opacity_mask: Option<NormalizedBrush>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedGeometry {
    Line {
        points: [Point2D; 2],
    },
    Polyline {
        points: Vec<Point2D>,
    },
    /// Marker glyphs at each point (draw-polymarker opcodes).
    Polymarker {
        points: Vec<Point2D>,
    },
    Polygon {
        points: Vec<Point2D>,
    },
    Circle {
        center: Point2D,
        x_axis: Point2D,
        y_axis: Point2D,
    },
    Arc {
        center: Point2D,
        x_axis: Point2D,
        y_axis: Point2D,
        start_angle_degrees: f64,
        end_angle_degrees: f64,
    },
    Ellipse {
        center: Point2D,
        x_axis: Point2D,
        y_axis: Point2D,
        start_angle_degrees: f64,
        end_angle_degrees: f64,
        closed: bool,
    },
    PolyBezier {
        points: Vec<Point2D>,
    },
    Text {
        position: Point2D,
        text: String,
        bounds: Option<[Point2D; 4]>,
    },
    Polytriangle {
        points: Vec<Point2D>,
    },
    GouraudPolyline {
        points: Vec<NormalizedColoredPoint>,
    },
    GouraudPolytriangle {
        points: Vec<NormalizedColoredPoint>,
    },
    TexturedPolytriangle {
        points: Vec<Point2D>,
    },
    ContourSet {
        contours: Vec<Vec<Point2D>>,
    },
    Image {
        image: NormalizedImage,
    },
    Path {
        fill_rule: String,
        figures: Vec<NormalizedPathFigure>,
    },
}

impl NormalizedGeometry {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Line { .. } => "LINE",
            Self::Polyline { .. } => "POLYLINE",
            Self::Polymarker { .. } => "POLYMARKER",
            Self::Polygon { .. } => "POLYGON",
            Self::Circle { .. } => "CIRCLE",
            Self::Arc { .. } => "ARC",
            Self::Ellipse { .. } => "ELLIPSE",
            Self::PolyBezier { .. } => "POLYBEZIER",
            Self::Text { .. } => "TEXT",
            Self::Polytriangle { .. } => "POLYTRIANGLE",
            Self::GouraudPolyline { .. } => "GOURAUD_POLYLINE",
            Self::GouraudPolytriangle { .. } => "GOURAUD_POLYTRIANGLE",
            Self::TexturedPolytriangle { .. } => "TEXTURED_POLYTRIANGLE",
            Self::ContourSet { .. } => "CONTOUR_SET",
            Self::Image { .. } => "IMAGE",
            Self::Path { .. } => "PATH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedEntity {
    pub section_index: usize,
    pub stream_index: usize,
    pub entity_index: usize,
    pub resource_href: String,
    pub resource_role: String,
    pub is_markup: bool,
    pub geometry: NormalizedGeometry,
    /// Effective ancestor-to-visual clip chain retained for query compatibility.
    pub clips: Vec<NormalizedClip>,
    /// The visual's local clip, rendered inside its Canvas compositing groups.
    pub local_clips: Vec<NormalizedClip>,
    /// Effective ancestor-to-visual mask chain retained for query compatibility.
    pub opacity_masks: Vec<NormalizedBrush>,
    /// The visual's local opacity mask, rendered inside Canvas groups.
    pub local_opacity_masks: Vec<NormalizedBrush>,
    /// Ancestor Canvas groups in outer-to-inner order.
    pub compositing_groups: Vec<NormalizedCompositingGroup>,
    /// Positioned packaged-font outlines for XPS Glyphs, when available.
    pub glyph_outline: Option<Vec<NormalizedPathFigure>>,
    pub style: NormalizedStyle,
    pub source: W2dSourceSpan,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalizedSheet {
    pub section_index: usize,
    pub name: String,
    pub title: Option<String>,
    pub plot_order: Option<i32>,
    pub units: Option<String>,
    pub paper_bounds: Option<[f64; 4]>,
    pub clip: Option<[f64; 4]>,
    pub background_color: Option<[u8; 3]>,
    /// Conservative paper-space geometry bounds. Curves use their complete
    /// transformed ellipse or control hull rather than a sweep-tight bound.
    pub content_bounds: Option<[f64; 4]>,
    pub entities: Vec<NormalizedEntity>,
    pub markup_entities: Vec<NormalizedEntity>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NormalizedDrawing {
    pub sheets: Vec<NormalizedSheet>,
}

/// Build a backend-neutral drawing in each ePlot page's paper coordinates.
///
/// Markup-role streams remain available in the raw package but are excluded
/// from this primary drawing view.
pub fn normalize_package(package: &DwfPackage) -> Result<NormalizedDrawing, DwfError> {
    let mut sheets = Vec::new();
    for (section_index, section) in package.manifest.sections.iter().enumerate() {
        let Some(page) = &section.page else {
            continue;
        };
        let mut entities = Vec::new();
        let mut markup_entities = Vec::new();
        for (stream_index, stream) in section.w2d_streams.iter().enumerate() {
            let is_markup = stream.role.to_ascii_lowercase().contains("markup");
            let transform =
                Affine2D::from_resource(stream.transform.as_deref(), &section.name, &stream.href)?;
            for (entity_index, entity) in stream.entities.iter().enumerate() {
                let normalized = normalize_entity(
                    entity,
                    NormalizeEntityContext {
                        transform,
                        section_index,
                        stream_index,
                        entity_index,
                        resource_href: &stream.href,
                        resource_role: &stream.role,
                        is_markup,
                    },
                );
                if is_markup {
                    markup_entities.push(normalized);
                } else {
                    entities.push(normalized);
                }
            }
        }
        let paper_bounds = page
            .paper
            .as_ref()
            .and_then(|paper| Some([0.0, 0.0, paper.width?, paper.height?]));
        let clip = page
            .paper
            .as_ref()
            .and_then(|paper| paper.clip.as_deref())
            .and_then(slice_box);
        let content_bounds = geometry_bounds(&entities);
        sheets.push(NormalizedSheet {
            section_index,
            name: page.name.clone(),
            title: section.title.clone(),
            plot_order: page.plot_order,
            units: page.paper.as_ref().and_then(|paper| paper.units.clone()),
            paper_bounds,
            clip,
            background_color: page.paper.as_ref().and_then(|paper| paper.color),
            content_bounds,
            entities,
            markup_entities,
        });
    }
    Ok(NormalizedDrawing { sheets })
}

/// Build the backend-neutral paper-space view for a DWFx OPC/XPS package.
///
/// XPS coordinates use a top-left origin with positive Y downward. The
/// normalized API uses the same bottom-left, positive-Y-up paper convention
/// as DWF ePlot, so the page-height reflection is applied after every XPS
/// visual transform.
#[must_use]
pub fn normalize_dwfx(package: &DwfxPackage) -> NormalizedDrawing {
    let mut sheets = Vec::new();
    let mut page_index = 0;
    for (document_index, document) in package.documents.iter().enumerate() {
        for page in &document.pages {
            let page_transform = Affine2D {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: 0.0,
                f: page.height,
            };
            let entities = page
                .entities
                .iter()
                .enumerate()
                .map(|(entity_index, entity)| {
                    normalize_xps_entity(
                        entity,
                        page_transform,
                        page_index,
                        document_index,
                        entity_index,
                        &page.part_name,
                    )
                })
                .collect::<Vec<_>>();
            let content_bounds = geometry_bounds(&entities);
            sheets.push(NormalizedSheet {
                section_index: page_index,
                name: page.name.clone(),
                title: None,
                plot_order: i32::try_from(page_index).ok(),
                units: Some("dip".to_owned()),
                paper_bounds: Some([0.0, 0.0, page.width, page.height]),
                clip: page
                    .content_box
                    .map(|value| xps_box_to_paper(value, page.height)),
                background_color: Some([255, 255, 255]),
                content_bounds,
                entities,
                markup_entities: Vec::new(),
            });
            page_index += 1;
        }
    }
    NormalizedDrawing { sheets }
}

/// Build the same high-level view for a standalone legacy WHIP/DWF stream.
#[must_use]
pub fn normalize_stream(stream: &W2dStream) -> NormalizedDrawing {
    let transform = Affine2D::IDENTITY;
    let entities = stream
        .entities
        .iter()
        .enumerate()
        .map(|(entity_index, entity)| {
            normalize_entity(
                entity,
                NormalizeEntityContext {
                    transform,
                    section_index: 0,
                    stream_index: 0,
                    entity_index,
                    resource_href: &stream.href,
                    resource_role: &stream.role,
                    is_markup: false,
                },
            )
        })
        .collect::<Vec<_>>();
    let content_bounds = geometry_bounds(&entities);
    NormalizedDrawing {
        sheets: vec![NormalizedSheet {
            section_index: 0,
            name: "Model".to_owned(),
            title: None,
            plot_order: Some(0),
            units: stream
                .units
                .as_ref()
                .map(|units| units.name.clone())
                .filter(|name| !name.is_empty()),
            paper_bounds: None,
            clip: None,
            background_color: None,
            content_bounds,
            entities,
            markup_entities: Vec::new(),
        }],
    }
}

struct NormalizeEntityContext<'a> {
    transform: Affine2D,
    section_index: usize,
    stream_index: usize,
    entity_index: usize,
    resource_href: &'a str,
    resource_role: &'a str,
    is_markup: bool,
}

fn normalize_entity(entity: &W2dEntity, context: NormalizeEntityContext<'_>) -> NormalizedEntity {
    let NormalizeEntityContext {
        transform,
        section_index,
        stream_index,
        entity_index,
        resource_href,
        resource_role,
        is_markup,
    } = context;
    let geometry = match &entity.geometry {
        W2dGeometry::Line { start, end } => NormalizedGeometry::Line {
            points: [
                transform.transform_point(*start),
                transform.transform_point(*end),
            ],
        },
        W2dGeometry::Polyline { points } => NormalizedGeometry::Polyline {
            points: transform_points(points, transform),
        },
        W2dGeometry::Polymarker { points } => NormalizedGeometry::Polymarker {
            points: transform_points(points, transform),
        },
        W2dGeometry::Polygon { points } => NormalizedGeometry::Polygon {
            points: transform_points(points, transform),
        },
        W2dGeometry::Circle {
            center,
            radius,
            start_angle,
            end_angle,
        } => {
            let center = transform.transform_point(*center);
            let x_axis = transform.transform_vector(Point2D {
                x: *radius as f64,
                y: 0.0,
            });
            let y_axis = transform.transform_vector(Point2D {
                x: 0.0,
                y: *radius as f64,
            });
            if is_full_turn(*start_angle, *end_angle) {
                NormalizedGeometry::Circle {
                    center,
                    x_axis,
                    y_axis,
                }
            } else {
                NormalizedGeometry::Arc {
                    center,
                    x_axis,
                    y_axis,
                    start_angle_degrees: angle_degrees(*start_angle),
                    end_angle_degrees: angle_degrees(*end_angle),
                }
            }
        }
        W2dGeometry::Ellipse {
            center,
            major,
            minor,
            start_angle,
            end_angle,
            tilt,
        } => {
            let tilt_radians = angle_degrees(*tilt).to_radians();
            let (sin_tilt, cos_tilt) = tilt_radians.sin_cos();
            let x_axis = transform.transform_vector(Point2D {
                x: *major as f64 * cos_tilt,
                y: *major as f64 * sin_tilt,
            });
            let y_axis = transform.transform_vector(Point2D {
                x: -*minor as f64 * sin_tilt,
                y: *minor as f64 * cos_tilt,
            });
            NormalizedGeometry::Ellipse {
                center: transform.transform_point(*center),
                x_axis,
                y_axis,
                start_angle_degrees: angle_degrees(*start_angle),
                end_angle_degrees: angle_degrees(*end_angle),
                closed: is_full_turn(*start_angle, *end_angle),
            }
        }
        W2dGeometry::PolyBezier { points } => NormalizedGeometry::PolyBezier {
            points: transform_points(points, transform),
        },
        W2dGeometry::Text {
            position,
            text,
            bounds,
        } => NormalizedGeometry::Text {
            position: transform.transform_point(*position),
            text: text.clone(),
            bounds: bounds.map(|bounds| bounds.map(|point| transform.transform_point(point))),
        },
        W2dGeometry::Polytriangle { points } => NormalizedGeometry::Polytriangle {
            points: transform_points(points, transform),
        },
        W2dGeometry::GouraudPolyline { points } => NormalizedGeometry::GouraudPolyline {
            points: points
                .iter()
                .map(|value| NormalizedColoredPoint {
                    point: transform.transform_point(value.point),
                    color: value.color,
                })
                .collect(),
        },
        W2dGeometry::GouraudPolytriangle { points } => NormalizedGeometry::GouraudPolytriangle {
            points: points
                .iter()
                .map(|value| NormalizedColoredPoint {
                    point: transform.transform_point(value.point),
                    color: value.color,
                })
                .collect(),
        },
        W2dGeometry::TexturedPolytriangle { points } => NormalizedGeometry::TexturedPolytriangle {
            points: transform_points(points, transform),
        },
        W2dGeometry::ContourSet { contours } => NormalizedGeometry::ContourSet {
            contours: contours
                .iter()
                .map(|points| transform_points(points, transform))
                .collect(),
        },
        W2dGeometry::Image { image } => NormalizedGeometry::Image {
            image: NormalizedImage {
                format: image.format.clone(),
                identifier: image.identifier,
                columns: image.columns,
                rows: image.rows,
                min: transform.transform_point(image.min),
                max: transform.transform_point(image.max),
                color_map: image.color_map.clone(),
                data: image.data.clone(),
            },
        },
    };

    NormalizedEntity {
        section_index,
        stream_index,
        entity_index,
        resource_href: resource_href.to_owned(),
        resource_role: resource_role.to_owned(),
        is_markup,
        geometry,
        clips: Vec::new(),
        local_clips: Vec::new(),
        opacity_masks: Vec::new(),
        local_opacity_masks: Vec::new(),
        compositing_groups: Vec::new(),
        glyph_outline: None,
        style: normalize_style(&entity.rendition, transform),
        source: entity.source.clone(),
    }
}

fn normalize_style(rendition: &W2dRendition, transform: Affine2D) -> NormalizedStyle {
    let font_rotation_degrees = rendition.font.rotation.map(|angle| {
        let raw_angle = angle_degrees(u32::from(angle)).to_radians();
        let direction = transform.transform_vector(Point2D {
            x: raw_angle.cos(),
            y: raw_angle.sin(),
        });
        direction.y.atan2(direction.x).to_degrees()
    });
    let font_height = rendition.font.height.map(|height| {
        let angle = rendition
            .font
            .rotation
            .map_or(0.0, |value| angle_degrees(u32::from(value)).to_radians());
        transform
            .transform_vector(Point2D {
                x: -(height as f64) * angle.sin(),
                y: height as f64 * angle.cos(),
            })
            .length()
    });
    NormalizedStyle {
        layer_number: rendition.layer.as_ref().map(|layer| layer.number),
        layer_name: rendition
            .layer
            .as_ref()
            .and_then(|layer| layer.name.clone()),
        color: rendition.color,
        color_index: rendition.color_index,
        line_pattern: rendition.line.pattern.clone(),
        line_weight_logical: rendition.line.weight,
        nominal_stroke_width: rendition
            .line
            .weight
            .map(|weight| f64::from(weight.unsigned_abs()) * transform.nominal_scale()),
        fill: rendition.fill,
        fill_pattern: rendition.fill_pattern.clone(),
        font: rendition.font.clone(),
        font_height,
        font_rotation_degrees,
        visible: rendition.visibility,
        viewport: rendition.viewport.clone(),
        stroke_color: rendition.color,
        fill_color: rendition.fill.then_some(rendition.color).flatten(),
        opacity: 1.0,
        stroke_dash_array: Vec::new(),
        stroke_dash_offset: 0.0,
        fill_brush: None,
        stroke_brush: None,
    }
}

fn normalize_xps_entity(
    entity: &XpsEntity,
    page_transform: Affine2D,
    page_index: usize,
    document_index: usize,
    entity_index: usize,
    resource_href: &str,
) -> NormalizedEntity {
    let transform = page_transform.compose(Affine2D::from_xps(entity.transform));
    let (geometry, font, glyph_outline) = match &entity.geometry {
        XpsGeometry::Path { geometry } => {
            let geometry_transform = transform.compose(Affine2D::from_xps(geometry.transform));
            (
                NormalizedGeometry::Path {
                    fill_rule: geometry.fill_rule.clone(),
                    figures: normalize_xps_path(geometry, geometry_transform),
                },
                W2dFont::default(),
                None,
            )
        }
        XpsGeometry::Glyphs { glyphs } => {
            let position = transform.transform_xps_point(glyphs.origin);
            let glyph_outline = glyphs.outline.as_ref().map(|outline| {
                let outline_transform = transform.compose(Affine2D::from_xps(outline.transform));
                normalize_xps_path(outline, outline_transform)
            });
            (
                NormalizedGeometry::Text {
                    position,
                    text: glyphs.unicode_string.clone(),
                    bounds: None,
                },
                W2dFont {
                    name: Some(glyphs.font_uri.clone()),
                    canonical_name: glyphs.normalized_font_uri.clone(),
                    bold: glyphs
                        .style_simulations
                        .as_ref()
                        .map(|value| value.to_ascii_lowercase().contains("bold")),
                    italic: glyphs
                        .style_simulations
                        .as_ref()
                        .map(|value| value.to_ascii_lowercase().contains("italic")),
                    ..W2dFont::default()
                },
                glyph_outline,
            )
        }
    };
    let clips = entity
        .clip_chain
        .iter()
        .map(|clip| {
            let clip_transform = page_transform
                .compose(Affine2D::from_xps(clip.transform))
                .compose(Affine2D::from_xps(clip.geometry.transform));
            NormalizedClip {
                fill_rule: clip.geometry.fill_rule.clone(),
                figures: normalize_xps_path(&clip.geometry, clip_transform),
            }
        })
        .collect();
    let opacity_masks = entity
        .opacity_mask_chain
        .iter()
        .map(|mask| {
            normalize_xps_brush(
                mask.brush.as_ref(),
                page_transform.compose(Affine2D::from_xps(mask.transform)),
            )
        })
        .collect();
    let local_clips = entity
        .clip
        .as_ref()
        .map(|clip| {
            let clip_transform = transform.compose(Affine2D::from_xps(clip.transform));
            vec![NormalizedClip {
                fill_rule: clip.fill_rule.clone(),
                figures: normalize_xps_path(clip, clip_transform),
            }]
        })
        .unwrap_or_default();
    let local_opacity_masks = entity
        .opacity_mask
        .as_ref()
        .map(|mask| vec![normalize_xps_brush(mask, transform)])
        .unwrap_or_default();
    let compositing_groups = entity
        .canvas_groups
        .iter()
        .map(|group| {
            let group_transform = page_transform.compose(Affine2D::from_xps(group.transform));
            NormalizedCompositingGroup {
                id: group.id,
                name: group.name.clone(),
                opacity: group.opacity,
                clip: group.clip.as_ref().map(|clip| {
                    let clip_transform =
                        group_transform.compose(Affine2D::from_xps(clip.transform));
                    NormalizedClip {
                        fill_rule: clip.fill_rule.clone(),
                        figures: normalize_xps_path(clip, clip_transform),
                    }
                }),
                opacity_mask: group
                    .opacity_mask
                    .as_ref()
                    .map(|mask| normalize_xps_brush(mask, group_transform)),
            }
        })
        .collect::<Vec<_>>();
    let mut style = normalize_xps_style(&entity.style, &entity.geometry, transform, font);
    style.visible &= compositing_groups.iter().all(|group| group.opacity > 0.0);
    style.layer_name.clone_from(&entity.canvas_name);
    NormalizedEntity {
        section_index: page_index,
        stream_index: document_index,
        entity_index,
        resource_href: resource_href.to_owned(),
        resource_role: "xps fixed page".to_owned(),
        is_markup: false,
        geometry,
        clips,
        local_clips,
        opacity_masks,
        local_opacity_masks,
        compositing_groups,
        glyph_outline,
        style,
        source: W2dSourceSpan {
            offset: entity.source.offset,
            length: entity.source.length,
            opcode: entity.source.element.clone(),
            decoded_offset: None,
            decoded_length: None,
            compression_depth: 0,
        },
    }
}

fn normalize_xps_path(
    geometry: &XpsPathGeometry,
    transform: Affine2D,
) -> Vec<NormalizedPathFigure> {
    geometry
        .figures
        .iter()
        .map(|figure| {
            let mut current = figure.start;
            let mut segments = Vec::with_capacity(figure.segments.len());
            for segment in &figure.segments {
                let normalized = match segment {
                    XpsPathSegment::Line {
                        end,
                        stroked,
                        smooth_join,
                    } => NormalizedPathSegment::Line {
                        end: transform.transform_xps_point(*end),
                        stroked: *stroked,
                        smooth_join: *smooth_join,
                    },
                    XpsPathSegment::CubicBezier {
                        control1,
                        control2,
                        end,
                        stroked,
                        smooth_join,
                    } => NormalizedPathSegment::CubicBezier {
                        control1: transform.transform_xps_point(*control1),
                        control2: transform.transform_xps_point(*control2),
                        end: transform.transform_xps_point(*end),
                        stroked: *stroked,
                        smooth_join: *smooth_join,
                    },
                    XpsPathSegment::QuadraticBezier {
                        control,
                        end,
                        stroked,
                        smooth_join,
                    } => NormalizedPathSegment::QuadraticBezier {
                        control: transform.transform_xps_point(*control),
                        end: transform.transform_xps_point(*end),
                        stroked: *stroked,
                        smooth_join: *smooth_join,
                    },
                    XpsPathSegment::Arc {
                        radius,
                        rotation_degrees,
                        large_arc,
                        sweep_clockwise,
                        end,
                        stroked,
                        smooth_join,
                    } => arc_parameters(
                        current,
                        *end,
                        *radius,
                        *rotation_degrees,
                        *large_arc,
                        *sweep_clockwise,
                    )
                    .map_or_else(
                        || NormalizedPathSegment::Line {
                            end: transform.transform_xps_point(*end),
                            stroked: *stroked,
                            smooth_join: *smooth_join,
                        },
                        |arc| NormalizedPathSegment::EllipticalArc {
                            center: transform.transform_xps_point(arc.center),
                            x_axis: transform.transform_vector(arc.x_axis),
                            y_axis: transform.transform_vector(arc.y_axis),
                            start_angle_degrees: arc.start_angle_degrees,
                            sweep_angle_degrees: arc.sweep_angle_degrees,
                            end: transform.transform_xps_point(*end),
                            stroked: *stroked,
                            smooth_join: *smooth_join,
                        },
                    ),
                };
                current = segment.end();
                segments.push(normalized);
            }
            NormalizedPathFigure {
                start: transform.transform_xps_point(figure.start),
                segments,
                closed: figure.closed,
                filled: figure.filled,
            }
        })
        .collect()
}

struct ArcParameters {
    center: XpsPoint,
    x_axis: Point2D,
    y_axis: Point2D,
    start_angle_degrees: f64,
    sweep_angle_degrees: f64,
}

fn arc_parameters(
    start: XpsPoint,
    end: XpsPoint,
    radius: XpsPoint,
    rotation_degrees: f64,
    large_arc: bool,
    sweep_clockwise: bool,
) -> Option<ArcParameters> {
    let mut rx = radius.x.abs();
    let mut ry = radius.y.abs();
    if rx == 0.0 || ry == 0.0 || (start.x == end.x && start.y == end.y) {
        return None;
    }
    let phi = rotation_degrees.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let half_dx = (start.x - end.x) * 0.5;
    let half_dy = (start.y - end.y) * 0.5;
    let x_prime = cos_phi.mul_add(half_dx, sin_phi * half_dy);
    let y_prime = (-sin_phi).mul_add(half_dx, cos_phi * half_dy);
    let radii_scale = x_prime.powi(2) / rx.powi(2) + y_prime.powi(2) / ry.powi(2);
    if !radii_scale.is_finite() {
        return None;
    }
    if radii_scale > 1.0 {
        let scale = radii_scale.sqrt();
        rx *= scale;
        ry *= scale;
    }
    let numerator = (rx * ry).powi(2) - (rx * y_prime).powi(2) - (ry * x_prime).powi(2);
    let denominator = (rx * y_prime).powi(2) + (ry * x_prime).powi(2);
    if !numerator.is_finite() || !denominator.is_finite() || denominator <= 0.0 {
        return None;
    }
    let sign = if large_arc == sweep_clockwise {
        -1.0
    } else {
        1.0
    };
    let coefficient = sign * (numerator.max(0.0) / denominator).sqrt();
    if !coefficient.is_finite() {
        return None;
    }
    let center_prime_x = coefficient * rx * y_prime / ry;
    let center_prime_y = coefficient * -ry * x_prime / rx;
    let center = XpsPoint {
        x: cos_phi.mul_add(
            center_prime_x,
            (-sin_phi).mul_add(center_prime_y, (start.x + end.x) * 0.5),
        ),
        y: sin_phi.mul_add(
            center_prime_x,
            cos_phi.mul_add(center_prime_y, (start.y + end.y) * 0.5),
        ),
    };
    let start_vector = (
        (x_prime - center_prime_x) / rx,
        (y_prime - center_prime_y) / ry,
    );
    let end_vector = (
        (-x_prime - center_prime_x) / rx,
        (-y_prime - center_prime_y) / ry,
    );
    let start_angle = start_vector.1.atan2(start_vector.0);
    let mut sweep_angle = vector_angle(start_vector, end_vector);
    if sweep_clockwise && sweep_angle < 0.0 {
        sweep_angle += std::f64::consts::TAU;
    } else if !sweep_clockwise && sweep_angle > 0.0 {
        sweep_angle -= std::f64::consts::TAU;
    }
    Some(ArcParameters {
        center,
        x_axis: Point2D {
            x: rx * cos_phi,
            y: rx * sin_phi,
        },
        y_axis: Point2D {
            x: -ry * sin_phi,
            y: ry * cos_phi,
        },
        start_angle_degrees: start_angle.to_degrees(),
        sweep_angle_degrees: sweep_angle.to_degrees(),
    })
}

fn vector_angle(left: (f64, f64), right: (f64, f64)) -> f64 {
    let cross = left.0.mul_add(right.1, -left.1 * right.0);
    let dot = left.0.mul_add(right.0, left.1 * right.1);
    cross.atan2(dot)
}

fn normalize_xps_style(
    style: &XpsStyle,
    geometry: &XpsGeometry,
    transform: Affine2D,
    font: W2dFont,
) -> NormalizedStyle {
    let stroke_color = solid_color(style.stroke.as_ref());
    let fill_color = solid_color(style.fill.as_ref());
    let fill_brush = style
        .fill
        .as_ref()
        .map(|brush| normalize_xps_brush(brush, transform));
    let stroke_brush = style
        .stroke
        .as_ref()
        .map(|brush| normalize_xps_brush(brush, transform));
    let (font_height, font_rotation_degrees) = match geometry {
        XpsGeometry::Glyphs { glyphs } => {
            let height = transform
                .transform_vector(Point2D {
                    x: 0.0,
                    y: glyphs.font_rendering_em_size,
                })
                .length();
            let direction = transform.transform_vector(Point2D { x: 1.0, y: 0.0 });
            (
                Some(height),
                Some(direction.y.atan2(direction.x).to_degrees()),
            )
        }
        XpsGeometry::Path { .. } => (None, None),
    };
    NormalizedStyle {
        layer_number: None,
        layer_name: None,
        color: stroke_color.or(fill_color),
        color_index: None,
        line_pattern: None,
        line_weight_logical: None,
        nominal_stroke_width: style
            .stroke
            .as_ref()
            .map(|_| style.stroke_thickness * transform.nominal_scale()),
        fill: style.fill.is_some(),
        fill_pattern: style.fill.as_ref().and_then(|brush| match brush {
            XpsBrush::Unsupported { brush_type, .. } => Some(brush_type.clone()),
            _ => None,
        }),
        font,
        font_height,
        font_rotation_degrees,
        visible: style.opacity > 0.0,
        viewport: None,
        stroke_color,
        fill_color,
        opacity: style.opacity,
        stroke_dash_array: style
            .stroke_dash_array
            .iter()
            .map(|value| value * style.stroke_thickness * transform.nominal_scale())
            .collect(),
        stroke_dash_offset: style.stroke_dash_offset
            * style.stroke_thickness
            * transform.nominal_scale(),
        fill_brush,
        stroke_brush,
    }
}

fn normalize_xps_brush(brush: &XpsBrush, transform: Affine2D) -> NormalizedBrush {
    match brush {
        XpsBrush::Solid { color, opacity, .. } => NormalizedBrush::Solid {
            color: *color,
            opacity: *opacity,
        },
        XpsBrush::Image {
            source,
            resource_part,
            content_type,
            data,
            image_metadata,
            viewbox,
            viewport,
            viewbox_units,
            viewport_units,
            tile_mode,
            transform: brush_transform,
            opacity,
            ..
        } => {
            let effective = transform.compose(Affine2D::from_xps(*brush_transform));
            NormalizedBrush::Image {
                brush: NormalizedImageBrush {
                    source: source.clone(),
                    resource_part: resource_part.clone(),
                    content_type: content_type.clone(),
                    data: data.clone(),
                    pixel_width: image_metadata.as_ref().map(|value| value.pixel_width),
                    pixel_height: image_metadata.as_ref().map(|value| value.pixel_height),
                    dpi_x: image_metadata.as_ref().map(|value| value.dpi_x),
                    dpi_y: image_metadata.as_ref().map(|value| value.dpi_y),
                    physical_size_dip: image_metadata
                        .as_ref()
                        .map(|value| value.physical_size_dip()),
                    viewbox: *viewbox,
                    viewport: viewport.map(|value| transform_box(value, effective)),
                    source_viewport: *viewport,
                    viewbox_units: viewbox_units.clone(),
                    viewport_units: viewport_units.clone(),
                    tile_mode: tile_mode.clone(),
                    transform: effective,
                    opacity: *opacity,
                },
            }
        }
        XpsBrush::Visual {
            visual,
            viewbox,
            viewport,
            viewbox_units,
            viewport_units,
            tile_mode,
            transform: brush_transform,
            opacity,
            ..
        } => {
            let effective = transform.compose(Affine2D::from_xps(*brush_transform));
            let visual_transform = Affine2D {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: 0.0,
                f: 0.0,
            };
            let entities = visual
                .as_ref()
                .map(|visual| {
                    visual
                        .entities
                        .iter()
                        .enumerate()
                        .map(|(index, entity)| {
                            normalize_xps_entity(
                                entity,
                                visual_transform,
                                0,
                                0,
                                index,
                                "visual-brush",
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            NormalizedBrush::Visual {
                brush: NormalizedVisualBrush {
                    entities,
                    viewbox: *viewbox,
                    viewport: transform_box(*viewport, effective),
                    source_viewport: *viewport,
                    viewbox_units: viewbox_units.clone(),
                    viewport_units: viewport_units.clone(),
                    tile_mode: tile_mode.clone(),
                    transform: effective,
                    opacity: *opacity,
                },
            }
        }
        XpsBrush::LinearGradient {
            start_point,
            end_point,
            spread_method,
            mapping_mode,
            transform: brush_transform,
            gradient_stops,
            opacity,
            ..
        } => {
            let effective = transform.compose(Affine2D::from_xps(*brush_transform));
            NormalizedBrush::LinearGradient {
                start_point: effective.transform_xps_point(*start_point),
                end_point: effective.transform_xps_point(*end_point),
                spread_method: spread_method.clone(),
                mapping_mode: mapping_mode.clone(),
                gradient_stops: gradient_stops
                    .iter()
                    .map(|stop| NormalizedGradientStop {
                        color: stop.color,
                        color_value: stop.color_value.clone(),
                        offset: stop.offset,
                    })
                    .collect(),
                opacity: *opacity,
            }
        }
        XpsBrush::RadialGradient {
            center,
            gradient_origin,
            radius_x,
            radius_y,
            spread_method,
            mapping_mode,
            transform: brush_transform,
            gradient_stops,
            opacity,
            ..
        } => {
            let effective = transform.compose(Affine2D::from_xps(*brush_transform));
            NormalizedBrush::RadialGradient {
                center: effective.transform_xps_point(*center),
                gradient_origin: effective.transform_xps_point(*gradient_origin),
                x_axis: effective.transform_vector(Point2D {
                    x: *radius_x,
                    y: 0.0,
                }),
                y_axis: effective.transform_vector(Point2D {
                    x: 0.0,
                    y: *radius_y,
                }),
                spread_method: spread_method.clone(),
                mapping_mode: mapping_mode.clone(),
                gradient_stops: gradient_stops
                    .iter()
                    .map(|stop| NormalizedGradientStop {
                        color: stop.color,
                        color_value: stop.color_value.clone(),
                        offset: stop.offset,
                    })
                    .collect(),
                opacity: *opacity,
            }
        }
        XpsBrush::Unsupported { brush_type, .. } => NormalizedBrush::Unsupported {
            brush_type: brush_type.clone(),
        },
    }
}

fn solid_color(brush: Option<&XpsBrush>) -> Option<[u8; 4]> {
    match brush {
        Some(XpsBrush::Solid { color, opacity, .. }) => {
            let mut color = *color;
            color[3] = (f64::from(color[3]) * opacity).round().clamp(0.0, 255.0) as u8;
            Some(color)
        }
        _ => None,
    }
}

fn transform_box(value: [f64; 4], transform: Affine2D) -> [f64; 4] {
    let [x, y, width, height] = value;
    let points = [
        transform.transform_xps_point(XpsPoint { x, y }),
        transform.transform_xps_point(XpsPoint { x: x + width, y }),
        transform.transform_xps_point(XpsPoint { x, y: y + height }),
        transform.transform_xps_point(XpsPoint {
            x: x + width,
            y: y + height,
        }),
    ];
    let mut bounds = None;
    include_points(&mut bounds, &points);
    bounds.expect("four points always produce bounds")
}

fn xps_box_to_paper(value: [f64; 4], page_height: f64) -> [f64; 4] {
    let [x, y, width, height] = value;
    [x, page_height - y - height, x + width, page_height - y]
}

fn transform_points(points: &[crate::W2dPoint], transform: Affine2D) -> Vec<Point2D> {
    points
        .iter()
        .map(|point| transform.transform_point(*point))
        .collect()
}

fn angle_degrees(angle: u32) -> f64 {
    f64::from(angle) * 360.0 / W2D_FULL_TURN
}

fn is_full_turn(start: u32, end: u32) -> bool {
    start == end || end.saturating_sub(start) >= 65_536
}

fn slice_box(values: &[f64]) -> Option<[f64; 4]> {
    values.try_into().ok()
}

fn geometry_bounds(entities: &[NormalizedEntity]) -> Option<[f64; 4]> {
    let mut bounds = None;
    for entity in entities {
        match &entity.geometry {
            NormalizedGeometry::Line { points } => include_points(&mut bounds, points),
            NormalizedGeometry::Polyline { points }
            | NormalizedGeometry::Polymarker { points }
            | NormalizedGeometry::Polygon { points }
            | NormalizedGeometry::PolyBezier { points }
            | NormalizedGeometry::Polytriangle { points }
            | NormalizedGeometry::TexturedPolytriangle { points } => {
                include_points(&mut bounds, points);
            }
            NormalizedGeometry::GouraudPolyline { points }
            | NormalizedGeometry::GouraudPolytriangle { points } => {
                for point in points {
                    include_point(&mut bounds, point.point);
                }
            }
            NormalizedGeometry::ContourSet { contours } => {
                for contour in contours {
                    include_points(&mut bounds, contour);
                }
            }
            NormalizedGeometry::Image { image } => {
                include_point(&mut bounds, image.min);
                include_point(&mut bounds, image.max);
            }
            NormalizedGeometry::Path { figures, .. } => {
                for figure in figures {
                    include_point(&mut bounds, figure.start);
                    for segment in &figure.segments {
                        match segment {
                            NormalizedPathSegment::Line { end, .. } => {
                                include_point(&mut bounds, *end);
                            }
                            NormalizedPathSegment::CubicBezier {
                                control1,
                                control2,
                                end,
                                ..
                            } => include_points(&mut bounds, &[*control1, *control2, *end]),
                            NormalizedPathSegment::QuadraticBezier { control, end, .. } => {
                                include_points(&mut bounds, &[*control, *end]);
                            }
                            NormalizedPathSegment::EllipticalArc {
                                center,
                                x_axis,
                                y_axis,
                                end,
                                ..
                            } => {
                                include_ellipse(&mut bounds, *center, *x_axis, *y_axis);
                                include_point(&mut bounds, *end);
                            }
                        }
                    }
                }
            }
            NormalizedGeometry::Circle {
                center,
                x_axis,
                y_axis,
            }
            | NormalizedGeometry::Arc {
                center,
                x_axis,
                y_axis,
                ..
            }
            | NormalizedGeometry::Ellipse {
                center,
                x_axis,
                y_axis,
                ..
            } => include_ellipse(&mut bounds, *center, *x_axis, *y_axis),
            NormalizedGeometry::Text {
                position,
                bounds: text_bounds,
                ..
            } => {
                include_point(&mut bounds, *position);
                if let Some(text_bounds) = text_bounds {
                    include_points(&mut bounds, text_bounds);
                }
            }
        }
    }
    bounds
}

fn include_ellipse(
    bounds: &mut Option<[f64; 4]>,
    center: Point2D,
    x_axis: Point2D,
    y_axis: Point2D,
) {
    let x_radius = x_axis.x.hypot(y_axis.x);
    let y_radius = x_axis.y.hypot(y_axis.y);
    include_point(
        bounds,
        Point2D {
            x: center.x - x_radius,
            y: center.y - y_radius,
        },
    );
    include_point(
        bounds,
        Point2D {
            x: center.x + x_radius,
            y: center.y + y_radius,
        },
    );
}

fn include_points(bounds: &mut Option<[f64; 4]>, points: &[Point2D]) {
    for point in points {
        include_point(bounds, *point);
    }
}

fn include_point(bounds: &mut Option<[f64; 4]>, point: Point2D) {
    if let Some(bounds) = bounds {
        bounds[0] = bounds[0].min(point.x);
        bounds[1] = bounds[1].min(point.y);
        bounds[2] = bounds[2].max(point.x);
        bounds[3] = bounds[3].max(point.y);
    } else {
        *bounds = Some([point.x, point.y, point.x, point.y]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{W2dLayer, W2dLineStyle, W2dPoint};

    fn transform() -> Affine2D {
        Affine2D::from_resource(
            Some(&[
                2.0, 3.0, 0.0, 0.0, 4.0, 5.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 20.0, 0.0, 1.0,
            ]),
            "sheet",
            "main.w2d",
        )
        .unwrap()
    }

    #[test]
    fn applies_column_major_resource_transform() {
        assert_eq!(
            transform().transform_point(W2dPoint { x: 2, y: 7 }),
            Point2D { x: 42.0, y: 61.0 }
        );
    }

    #[test]
    fn normalizes_geometry_style_and_source_indexes() {
        let entity = W2dEntity {
            geometry: W2dGeometry::Circle {
                center: W2dPoint { x: 1, y: 2 },
                radius: 3,
                start_angle: 0,
                end_angle: 65_536,
            },
            rendition: W2dRendition {
                color: Some([1, 2, 3, 255]),
                color_index: None,
                layer: Some(W2dLayer {
                    number: 7,
                    name: Some("walls".to_owned()),
                }),
                line: W2dLineStyle {
                    weight: Some(2),
                    ..W2dLineStyle::default()
                },
                ..W2dRendition::default()
            },
            source: W2dSourceSpan {
                offset: 12,
                length: 9,
                opcode: "r".to_owned(),
                decoded_offset: None,
                decoded_length: None,
                compression_depth: 0,
            },
        };
        let normalized = normalize_entity(
            &entity,
            NormalizeEntityContext {
                transform: transform(),
                section_index: 2,
                stream_index: 3,
                entity_index: 4,
                resource_href: "main.w2d",
                resource_role: "graphics2d",
                is_markup: false,
            },
        );
        assert_eq!(normalized.geometry.kind(), "CIRCLE");
        assert_eq!(normalized.section_index, 2);
        assert_eq!(normalized.stream_index, 3);
        assert_eq!(normalized.entity_index, 4);
        assert_eq!(normalized.style.layer_name.as_deref(), Some("walls"));
        assert_eq!(normalized.style.color, Some([1, 2, 3, 255]));
        assert!(normalized.style.nominal_stroke_width.unwrap() > 7.0);
        assert!(matches!(
            normalized.geometry,
            NormalizedGeometry::Circle {
                center: Point2D { x: 20.0, y: 33.0 },
                x_axis: Point2D { x: 6.0, y: 9.0 },
                y_axis: Point2D { x: 12.0, y: 15.0 },
            }
        ));
    }
}
