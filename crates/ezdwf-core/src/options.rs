/// Default complete-file safety limit (1 GiB).
pub const DEFAULT_MAX_FILE_SIZE_BYTES: usize = 1024 * 1024 * 1024;

/// Default maximum number of ZIP entries in one DWF package.
pub const DEFAULT_MAX_ARCHIVE_ENTRIES: usize = 10_000;

/// Default maximum uncompressed size of one ZIP entry (256 MiB).
pub const DEFAULT_MAX_ENTRY_SIZE_BYTES: usize = 256 * 1024 * 1024;

/// Default maximum aggregate uncompressed size of all ZIP entries (1 GiB).
pub const DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE_BYTES: usize = 1024 * 1024 * 1024;

/// Default maximum ratio of declared uncompressed bytes to compressed bytes.
pub const DEFAULT_MAX_COMPRESSION_RATIO: usize = 1_000;

/// Default maximum XML resource size (16 MiB).
pub const DEFAULT_MAX_XML_SIZE_BYTES: usize = 16 * 1024 * 1024;

/// Default maximum element nesting depth for manifest and descriptor XML.
pub const DEFAULT_MAX_XML_DEPTH: usize = 128;

/// Default maximum number of top-level W2D records in one resource.
pub const DEFAULT_MAX_W2D_RECORDS: usize = 5_000_000;

/// Default maximum number of logical points in one W2D drawable.
pub const DEFAULT_MAX_W2D_POINTS_PER_ENTITY: usize = 1_000_000;

/// Default maximum aggregate number of decoded logical points in one W2D resource.
pub const DEFAULT_MAX_W2D_TOTAL_POINTS: usize = 20_000_000;

/// Default maximum byte length of one W2D string operand (1 MiB).
pub const DEFAULT_MAX_W2D_STRING_SIZE_BYTES: usize = 1024 * 1024;

/// Default maximum parenthesis nesting depth of an extended ASCII W2D record.
pub const DEFAULT_MAX_W2D_NESTING_DEPTH: usize = 128;

/// Default maximum expanded size of one internally compressed W2D stream
/// (256 MiB).
pub const DEFAULT_MAX_W2D_DECOMPRESSED_SIZE_BYTES: usize = 256 * 1024 * 1024;

/// Default maximum number of nested W2D compressed-data wrappers.
pub const DEFAULT_MAX_W2D_COMPRESSION_DEPTH: usize = 4;

/// Default maximum number of visual elements across one XPS FixedPage.
pub const DEFAULT_MAX_XPS_VISUALS: usize = 5_000_000;

/// Default maximum number of path segments across one XPS FixedPage.
pub const DEFAULT_MAX_XPS_PATH_SEGMENTS: usize = 20_000_000;

/// Resource limits applied while identifying and inspecting a DWF package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    pub max_file_size: usize,
    pub max_archive_entries: usize,
    pub max_entry_size: usize,
    pub max_total_uncompressed_size: usize,
    pub max_compression_ratio: usize,
    pub max_xml_size: usize,
    pub max_xml_depth: usize,
    pub max_w2d_records: usize,
    pub max_w2d_points_per_entity: usize,
    pub max_w2d_total_points: usize,
    pub max_w2d_string_size: usize,
    pub max_w2d_nesting_depth: usize,
    pub max_w2d_decompressed_size: usize,
    pub max_w2d_compression_depth: usize,
    pub max_xps_visuals: usize,
    pub max_xps_path_segments: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_file_size: DEFAULT_MAX_FILE_SIZE_BYTES,
            max_archive_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            max_entry_size: DEFAULT_MAX_ENTRY_SIZE_BYTES,
            max_total_uncompressed_size: DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE_BYTES,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
            max_xml_size: DEFAULT_MAX_XML_SIZE_BYTES,
            max_xml_depth: DEFAULT_MAX_XML_DEPTH,
            max_w2d_records: DEFAULT_MAX_W2D_RECORDS,
            max_w2d_points_per_entity: DEFAULT_MAX_W2D_POINTS_PER_ENTITY,
            max_w2d_total_points: DEFAULT_MAX_W2D_TOTAL_POINTS,
            max_w2d_string_size: DEFAULT_MAX_W2D_STRING_SIZE_BYTES,
            max_w2d_nesting_depth: DEFAULT_MAX_W2D_NESTING_DEPTH,
            max_w2d_decompressed_size: DEFAULT_MAX_W2D_DECOMPRESSED_SIZE_BYTES,
            max_w2d_compression_depth: DEFAULT_MAX_W2D_COMPRESSION_DEPTH,
            max_xps_visuals: DEFAULT_MAX_XPS_VISUALS,
            max_xps_path_segments: DEFAULT_MAX_XPS_PATH_SEGMENTS,
        }
    }
}
