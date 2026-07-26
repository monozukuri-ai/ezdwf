//! Pure Rust core for the `ezdwf` package.
//!
//! The core identifies the DWF family, inspects bounded DWF 6 and DWFx
//! packages, decodes stateful W2D/XPS display lists, and projects decoded
//! geometry into a common paper space without discarding the raw representation.

#![forbid(unsafe_code)]

mod dwfx;
mod error;
mod format;
mod model;
mod normalized;
mod options;
mod package;
mod w2d;

pub use dwfx::{
    inspect_dwfx, inspect_dwfx_without_glyph_outlines, DwfxPackage, OpcContentType,
    OpcRelationship, XpsBrush, XpsCanvasGroup, XpsClip, XpsDocument, XpsEntity, XpsGeometry,
    XpsGlyphs, XpsGradientStop, XpsImageMetadata, XpsMatrix, XpsOpacityMask, XpsPage,
    XpsPathFigure, XpsPathGeometry, XpsPathSegment, XpsPoint, XpsSourceSpan, XpsStyle, XpsVisual,
};
pub use error::DwfError;
pub use format::{detect_format, DwfFormat, DwfVersion, DWF_PACKAGE_HEADER_LEN};
pub use model::{
    ArchiveEntry, Diagnostic, DiagnosticSeverity, DwfInterface, DwfManifest, DwfPackage,
    DwfProperty, DwfResource, DwfSection, DwfSource, EPlotPage, EPlotPaper, EPlotResource,
};
pub use normalized::{
    normalize_dwfx, normalize_package, normalize_stream, Affine2D, NormalizedBrush, NormalizedClip,
    NormalizedColoredPoint, NormalizedCompositingGroup, NormalizedDrawing, NormalizedEntity,
    NormalizedGeometry, NormalizedGradientStop, NormalizedImage, NormalizedImageBrush,
    NormalizedPathFigure, NormalizedPathSegment, NormalizedSheet, NormalizedStyle,
    NormalizedVisualBrush, Point2D,
};
pub use options::{
    ParseOptions, DEFAULT_MAX_ARCHIVE_ENTRIES, DEFAULT_MAX_COMPRESSION_RATIO,
    DEFAULT_MAX_ENTRY_SIZE_BYTES, DEFAULT_MAX_FILE_SIZE_BYTES,
    DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE_BYTES, DEFAULT_MAX_W2D_COMPRESSION_DEPTH,
    DEFAULT_MAX_W2D_DECOMPRESSED_SIZE_BYTES, DEFAULT_MAX_W2D_NESTING_DEPTH,
    DEFAULT_MAX_W2D_POINTS_PER_ENTITY, DEFAULT_MAX_W2D_RECORDS, DEFAULT_MAX_W2D_STRING_SIZE_BYTES,
    DEFAULT_MAX_W2D_TOTAL_POINTS, DEFAULT_MAX_XML_DEPTH, DEFAULT_MAX_XML_SIZE_BYTES,
    DEFAULT_MAX_XPS_PATH_SEGMENTS, DEFAULT_MAX_XPS_VISUALS,
};
pub use package::inspect_package;
pub use w2d::{
    decode_w2d, W2dBlockRef, W2dColoredPoint, W2dEmbeddedFont, W2dEntity, W2dFont, W2dGeometry,
    W2dImage, W2dLayer, W2dLineStyle, W2dPoint, W2dRendition, W2dSourceSpan, W2dStream, W2dUnits,
    W2dViewport,
};

/// Version of the Rust core bundled with the Python package.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the version of the Rust core.
#[must_use]
pub const fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
    }
}
