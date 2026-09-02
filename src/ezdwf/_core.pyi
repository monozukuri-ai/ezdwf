from typing import Any, Final

DEFAULT_MAX_FILE_SIZE_BYTES: Final[int]
DEFAULT_MAX_ARCHIVE_ENTRIES: Final[int]
DEFAULT_MAX_ENTRY_SIZE_BYTES: Final[int]
DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE_BYTES: Final[int]
DEFAULT_MAX_COMPRESSION_RATIO: Final[int]
DEFAULT_MAX_XML_SIZE_BYTES: Final[int]
DEFAULT_MAX_XML_DEPTH: Final[int]
DEFAULT_MAX_W2D_RECORDS: Final[int]
DEFAULT_MAX_W2D_POINTS_PER_ENTITY: Final[int]
DEFAULT_MAX_W2D_TOTAL_POINTS: Final[int]
DEFAULT_MAX_W2D_STRING_SIZE_BYTES: Final[int]
DEFAULT_MAX_W2D_NESTING_DEPTH: Final[int]
DEFAULT_MAX_W2D_DECOMPRESSED_SIZE_BYTES: Final[int]
DEFAULT_MAX_W2D_COMPRESSION_DEPTH: Final[int]
DEFAULT_MAX_XPS_VISUALS: Final[int]
DEFAULT_MAX_XPS_PATH_SEGMENTS: Final[int]

class DwfError(Exception): ...
class InvalidDwfError(DwfError): ...
class UnsupportedDwfError(DwfError): ...
class DwfLimitError(DwfError): ...

class DrawingHandle:
    def kind(self) -> str: ...
    def sheet_count(self) -> int: ...
    def sheet(self, index: int) -> dict[str, Any]: ...
    def package_shell(self) -> dict[str, Any]: ...
    def stream_entities(
        self, section_index: int, stream_index: int
    ) -> list[dict[str, Any]]: ...
    def legacy_stream(self) -> dict[str, Any]: ...
    def dwfx_package(self) -> dict[str, Any]: ...

def core_version() -> str: ...
def detect_format_bytes(
    data: bytes,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> tuple[str, str | None, int]: ...
def inspect_package_bytes(
    data: bytes,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> dict[str, Any]: ...
def inspect_dwfx_bytes(
    data: bytes,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> dict[str, Any]: ...
def read_drawing_bytes(
    data: bytes,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> dict[str, Any]: ...
def read_drawing_handle(
    data: bytes,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> DrawingHandle: ...
def decode_w2d_bytes(
    data: bytes,
    resource: str,
    max_file_size: int,
    max_archive_entries: int,
    max_entry_size: int,
    max_total_uncompressed_size: int,
    max_compression_ratio: int,
    max_xml_size: int,
    max_xml_depth: int,
    max_w2d_records: int,
    max_w2d_points_per_entity: int,
    max_w2d_total_points: int,
    max_w2d_string_size: int,
    max_w2d_nesting_depth: int,
    max_w2d_decompressed_size: int,
    max_w2d_compression_depth: int,
    max_xps_visuals: int,
    max_xps_path_segments: int,
) -> dict[str, Any]: ...
