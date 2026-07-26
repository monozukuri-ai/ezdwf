use thiserror::Error;

use crate::DwfFormat;

/// Errors produced while identifying or inspecting a DWF file.
#[derive(Debug, Error, PartialEq)]
pub enum DwfError {
    #[error(
        "input is too short to identify DWF format: need at least {needed} bytes, got {actual}"
    )]
    InputTooShort { needed: usize, actual: usize },

    #[error("unrecognized DWF signature: {signature}")]
    UnrecognizedFormat { signature: String },

    #[error("malformed DWF header: {context}")]
    InvalidDwfHeader { context: String },

    #[error("{format} is recognized but is not supported by the DWF 6 package inspector")]
    UnsupportedFormat { format: DwfFormat },

    #[error("input size {actual} bytes exceeds configured limit {limit} bytes")]
    FileSizeLimitExceeded { actual: usize, limit: usize },

    #[error("invalid ZIP package: {context}")]
    InvalidArchive { context: String },

    #[error("ZIP entry count {actual} exceeds configured limit {limit}")]
    ArchiveEntryLimitExceeded { actual: usize, limit: usize },

    #[error("ZIP entry {name:?} declares {actual} uncompressed bytes, exceeding configured limit {limit}")]
    EntrySizeLimitExceeded {
        name: String,
        actual: u64,
        limit: usize,
    },

    #[error(
        "ZIP entries declare {actual} total uncompressed bytes, exceeding configured limit {limit}"
    )]
    TotalUncompressedSizeLimitExceeded { actual: u64, limit: usize },

    #[error(
        "ZIP entry {name:?} has compression ratio {actual}, exceeding configured limit {limit}"
    )]
    CompressionRatioLimitExceeded {
        name: String,
        actual: u64,
        limit: usize,
    },

    #[error("unsafe ZIP entry name {name:?}: {reason}")]
    InvalidEntryName { name: String, reason: String },

    #[error("ZIP entries {first:?} and {second:?} normalize to the same path {normalized:?}")]
    DuplicateEntryName {
        normalized: String,
        first: String,
        second: String,
    },

    #[error(
        "ZIP central directory contains {actual} entries but only {unique} unique names; duplicate entry names normalize to the same path"
    )]
    DuplicateArchiveEntryNames { actual: usize, unique: usize },

    #[error("required ZIP entry {name:?} is missing")]
    MissingEntry { name: String },

    #[error("encrypted ZIP entry {name:?} is not supported")]
    EncryptedEntry { name: String },

    #[error("XML resource {document:?} is {actual} bytes, exceeding configured limit {limit}")]
    XmlSizeLimitExceeded {
        document: String,
        actual: usize,
        limit: usize,
    },

    #[error("XML resource {document:?} exceeds configured nesting depth {limit}")]
    XmlDepthLimitExceeded { document: String, limit: usize },

    #[error("invalid XML in {document:?}: {context}")]
    InvalidXml { document: String, context: String },

    #[error("invalid DWF manifest: {context}")]
    InvalidManifest { context: String },

    #[error("invalid ePlot descriptor for section {section:?}: {context}")]
    InvalidEPlot { section: String, context: String },

    #[error("invalid paper-coordinate transform for resource {resource:?} in section {section:?}: {context}")]
    InvalidTransform {
        section: String,
        resource: String,
        context: String,
    },

    #[error("resource {href:?} referenced by section {section:?} is not present in the package (normalized path {normalized:?})")]
    MissingResource {
        section: String,
        href: String,
        normalized: String,
    },

    #[error("invalid W2D resource {resource:?} at byte offset {offset}: {context}")]
    InvalidW2d {
        resource: String,
        offset: usize,
        context: String,
    },

    #[error("unsupported W2D single-byte opcode {opcode} in {resource:?} at byte offset {offset}")]
    UnsupportedW2dOpcode {
        resource: String,
        offset: usize,
        opcode: String,
    },

    #[error("W2D version {version} in {resource:?} is recognized but unsupported")]
    UnsupportedW2dVersion { resource: String, version: String },

    #[error("W2D resource {resource:?} is {actual} bytes, exceeding configured limit {limit}")]
    W2dSourceSizeLimitExceeded {
        resource: String,
        actual: usize,
        limit: usize,
    },

    #[error("W2D resource {resource:?} contains more than {limit} records")]
    W2dRecordLimitExceeded { resource: String, limit: usize },

    #[error("W2D drawable in {resource:?} at byte offset {offset} declares {actual} points, exceeding configured limit {limit}")]
    W2dPointLimitExceeded {
        resource: String,
        offset: usize,
        actual: usize,
        limit: usize,
    },

    #[error("W2D resource {resource:?} declares more than {limit} aggregate points")]
    W2dTotalPointLimitExceeded { resource: String, limit: usize },

    #[error(
        "W2D string in {resource:?} at byte offset {offset} exceeds configured limit {limit} bytes"
    )]
    W2dStringLimitExceeded {
        resource: String,
        offset: usize,
        limit: usize,
    },

    #[error("W2D extended ASCII record in {resource:?} at byte offset {offset} exceeds configured nesting depth {limit}")]
    W2dNestingLimitExceeded {
        resource: String,
        offset: usize,
        limit: usize,
    },

    #[error("expanded W2D data in {resource:?} exceeds configured limit {limit} bytes")]
    W2dDecompressedSizeLimitExceeded { resource: String, limit: usize },

    #[error("compressed W2D data in {resource:?} exceeds configured nesting depth {limit}")]
    W2dCompressionDepthLimitExceeded { resource: String, limit: usize },

    #[error("invalid OPC package part {part:?}: {context}")]
    InvalidOpc { part: String, context: String },

    #[error("OPC relationship from {source_part:?} targets missing package part {target:?} (normalized path {normalized:?})")]
    MissingOpcPart {
        source_part: String,
        target: String,
        normalized: String,
    },

    #[error("invalid XPS part {part:?}: {context}")]
    InvalidXps { part: String, context: String },

    #[error("XPS FixedPage {page:?} contains more than {limit} visual elements")]
    XpsVisualLimitExceeded { page: String, limit: usize },

    #[error("XPS FixedPage {page:?} contains more than {limit} path segments")]
    XpsPathSegmentLimitExceeded { page: String, limit: usize },
}

impl DwfError {
    /// Returns true when the error represents a configured resource limit.
    #[must_use]
    pub const fn is_limit_error(&self) -> bool {
        matches!(
            self,
            Self::FileSizeLimitExceeded { .. }
                | Self::ArchiveEntryLimitExceeded { .. }
                | Self::EntrySizeLimitExceeded { .. }
                | Self::TotalUncompressedSizeLimitExceeded { .. }
                | Self::CompressionRatioLimitExceeded { .. }
                | Self::XmlSizeLimitExceeded { .. }
                | Self::XmlDepthLimitExceeded { .. }
                | Self::W2dSourceSizeLimitExceeded { .. }
                | Self::W2dRecordLimitExceeded { .. }
                | Self::W2dPointLimitExceeded { .. }
                | Self::W2dTotalPointLimitExceeded { .. }
                | Self::W2dStringLimitExceeded { .. }
                | Self::W2dNestingLimitExceeded { .. }
                | Self::W2dDecompressedSizeLimitExceeded { .. }
                | Self::W2dCompressionDepthLimitExceeded { .. }
                | Self::XpsVisualLimitExceeded { .. }
                | Self::XpsPathSegmentLimitExceeded { .. }
        )
    }

    /// Returns true when a recognized DWF family is outside this reader's scope.
    #[must_use]
    pub const fn is_unsupported_error(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedFormat { .. }
                | Self::EncryptedEntry { .. }
                | Self::UnsupportedW2dOpcode { .. }
                | Self::UnsupportedW2dVersion { .. }
        )
    }
}
