use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::DwfFormat;
use crate::W2dStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub action: String,
    pub section: Option<String>,
    pub resource: Option<String>,
    pub offset: Option<usize>,
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveEntry {
    pub original_name: String,
    pub normalized_name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub compression_method: String,
    pub is_directory: bool,
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DwfProperty {
    pub name: String,
    pub category: Option<String>,
    pub value: String,
    pub value_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DwfInterface {
    pub object_id: Option<String>,
    pub name: String,
    pub href: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DwfSource {
    pub provider: Option<String>,
    pub href: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DwfResource {
    pub role: String,
    pub mime: String,
    pub href: String,
    pub normalized_href: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EPlotPaper {
    pub show: Option<bool>,
    pub units: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub clip: Option<Vec<f64>>,
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EPlotResource {
    pub kind: String,
    pub role: String,
    pub mime: String,
    pub href: String,
    pub normalized_href: String,
    pub title: Option<String>,
    pub size: Option<u64>,
    pub object_id: Option<String>,
    pub parent_object_id: Option<String>,
    pub transform: Option<Vec<f64>>,
    pub clip: Option<Vec<f64>>,
    pub extents: Option<Vec<f64>>,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EPlotPage {
    pub version: String,
    pub name: String,
    pub object_id: Option<String>,
    pub plot_order: Option<i32>,
    pub color: Option<[u8; 3]>,
    pub paper: Option<EPlotPaper>,
    pub properties: Vec<DwfProperty>,
    pub resources: Vec<EPlotResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DwfSection {
    pub section_type: String,
    pub name: String,
    pub title: Option<String>,
    pub source: Option<DwfSource>,
    pub resources: Vec<DwfResource>,
    pub page: Option<EPlotPage>,
    pub w2d_streams: Vec<W2dStream>,
}

impl DwfSection {
    #[must_use]
    pub fn is_eplot_sheet(&self) -> bool {
        self.resources.iter().any(|resource| {
            resource.role.eq_ignore_ascii_case("2d streaming graphics")
                || (resource.mime.eq_ignore_ascii_case("application/x-w2d")
                    && !resource.role.to_ascii_lowercase().contains("markup"))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DwfManifest {
    pub version: String,
    pub object_id: Option<String>,
    pub properties: Vec<DwfProperty>,
    pub interfaces: Vec<DwfInterface>,
    pub sections: Vec<DwfSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DwfPackage {
    pub format: DwfFormat,
    pub entries: Vec<ArchiveEntry>,
    pub manifest: DwfManifest,
    pub diagnostics: Vec<Diagnostic>,
}

impl DwfPackage {
    #[must_use]
    pub fn sheet_count(&self) -> usize {
        self.manifest
            .sections
            .iter()
            .filter(|section| section.page.is_some())
            .count()
    }

    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn sheets(&self) -> impl Iterator<Item = &DwfSection> {
        self.manifest
            .sections
            .iter()
            .filter(|section| section.page.is_some())
    }
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        };
        formatter.write_str(label)
    }
}
