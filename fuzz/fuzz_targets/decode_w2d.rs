#![no_main]

use ezdwf_core::{decode_w2d, ParseOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let options = ParseOptions {
        max_file_size: 1024 * 1024,
        max_entry_size: 1024 * 1024,
        max_w2d_records: 20_000,
        max_w2d_points_per_entity: 20_000,
        max_w2d_total_points: 100_000,
        max_w2d_string_size: 64 * 1024,
        max_w2d_nesting_depth: 32,
        max_w2d_decompressed_size: 1024 * 1024,
        max_w2d_compression_depth: 2,
        ..ParseOptions::default()
    };
    let _ = decode_w2d(data, "<fuzz.w2d>", options);
});
