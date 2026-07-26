#![no_main]

use ezdwf_core::{inspect_dwfx, ParseOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let options = ParseOptions {
        max_file_size: 1024 * 1024,
        max_archive_entries: 128,
        max_entry_size: 512 * 1024,
        max_total_uncompressed_size: 2 * 1024 * 1024,
        max_compression_ratio: 100,
        max_xml_size: 256 * 1024,
        max_xml_depth: 64,
        max_xps_visuals: 20_000,
        max_xps_path_segments: 100_000,
        ..ParseOptions::default()
    };
    let _ = inspect_dwfx(data, options);
});
