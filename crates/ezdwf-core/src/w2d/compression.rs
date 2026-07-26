//! Decoders for W2D's zero-length compressed-data wrappers.
//!
//! The zlib preset dictionary is generated from the public WHIP/W2D format
//! definition instead of being stored as an opaque binary asset.

use std::collections::VecDeque;

use flate2::{Decompress, FlushDecompress, Status};

const OUTPUT_CHUNK: usize = 64 * 1024;
const EXPECTED_DICTIONARY_ADLER32: u32 = 0x5CBC_15B9;

pub(super) struct ExpandedBlock {
    pub bytes: Vec<u8>,
    pub consumed: usize,
}

pub(super) fn expand_zlib(input: &[u8], limit: usize) -> Result<ExpandedBlock, String> {
    let dictionary = w2d_zlib_dictionary();
    debug_assert_eq!(adler32(&dictionary), EXPECTED_DICTIONARY_ADLER32);

    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut buffer = [0_u8; OUTPUT_CHUNK];
    let mut dictionary_supplied = false;

    loop {
        let input_offset = usize::try_from(decoder.total_in())
            .map_err(|_| "compressed input position exceeds addressable memory".to_owned())?;
        let before_out = decoder.total_out();
        let result = decoder.decompress(
            input.get(input_offset..).unwrap_or_default(),
            &mut buffer,
            FlushDecompress::None,
        );

        let status = match result {
            Ok(status) => status,
            Err(error) => {
                if let Some(required) = error.needs_dictionary() {
                    if dictionary_supplied {
                        return Err("zlib stream rejected the W2D preset dictionary".to_owned());
                    }
                    if required != EXPECTED_DICTIONARY_ADLER32 {
                        return Err(format!(
                            "zlib stream requires unknown preset dictionary 0x{required:08X}"
                        ));
                    }
                    decoder
                        .set_dictionary(&dictionary)
                        .map_err(|error| format!("failed to set W2D preset dictionary: {error}"))?;
                    dictionary_supplied = true;
                    continue;
                }
                return Err(format!("invalid zlib compressed data: {error}"));
            }
        };

        let produced = usize::try_from(decoder.total_out() - before_out)
            .map_err(|_| "expanded output size exceeds addressable memory".to_owned())?;
        if output
            .len()
            .checked_add(produced)
            .is_none_or(|size| size > limit)
        {
            return Err("expanded data exceeds configured limit".to_owned());
        }
        output.extend_from_slice(&buffer[..produced]);

        if status == Status::StreamEnd {
            return Ok(ExpandedBlock {
                bytes: output,
                consumed: usize::try_from(decoder.total_in())
                    .map_err(|_| "compressed input size exceeds addressable memory".to_owned())?,
            });
        }
        if decoder.total_in() as usize >= input.len() && produced == 0 {
            return Err("truncated zlib compressed data".to_owned());
        }
        if produced == 0 && decoder.total_in() as usize == input_offset {
            return Err("zlib decompressor made no progress".to_owned());
        }
    }
}

pub(super) fn expand_lz(
    input: &[u8],
    decimal_revision: u16,
    limit: usize,
) -> Result<ExpandedBlock, String> {
    const HISTORY_LIMIT: usize = 65_536;
    let mut history = if decimal_revision >= 23 {
        VecDeque::from(w2d_lz_history())
    } else {
        VecDeque::new()
    };
    let mut output = Vec::new();
    let mut position = 0_usize;

    loop {
        let code = *input
            .get(position)
            .ok_or_else(|| "truncated LZ compressed data".to_owned())?;
        position += 1;
        if code == 0 {
            return Ok(ExpandedBlock {
                bytes: output,
                consumed: position,
            });
        }

        let mut literal_count = usize::from(code & 0x0F);
        let mut compressed_count = usize::from(code >> 4);
        if literal_count == 15 {
            literal_count += usize::from(
                *input
                    .get(position)
                    .ok_or_else(|| "truncated extended LZ literal length".to_owned())?,
            );
            position += 1;
        }
        let literal_end = position
            .checked_add(literal_count)
            .ok_or_else(|| "LZ literal length overflow".to_owned())?;
        let literals = input
            .get(position..literal_end)
            .ok_or_else(|| "truncated LZ literal run".to_owned())?;
        append_limited(&mut output, literals, limit)?;
        append_history(&mut history, literals, HISTORY_LIMIT);
        position = literal_end;

        if compressed_count == 0 {
            continue;
        }
        compressed_count += 3;
        if (code >> 4) == 15 {
            compressed_count += usize::from(
                *input
                    .get(position)
                    .ok_or_else(|| "truncated extended LZ compressed length".to_owned())?,
            );
            position += 1;
        }
        let encoded_offset = u16::from_le_bytes(
            input
                .get(position..position.saturating_add(2))
                .ok_or_else(|| "truncated LZ history offset".to_owned())?
                .try_into()
                .expect("two-byte slice"),
        );
        position += 2;
        let mut history_offset = if decimal_revision >= 23 {
            history
                .len()
                .checked_sub(usize::from(encoded_offset).saturating_add(1))
                .ok_or_else(|| "LZ history offset is outside the recall buffer".to_owned())?
        } else {
            usize::from(encoded_offset)
        };
        for _ in 0..compressed_count {
            let byte = *history
                .get(history_offset)
                .ok_or_else(|| "LZ copy exceeds the available recall buffer".to_owned())?;
            append_limited(&mut output, &[byte], limit)?;
            history.push_back(byte);
            history_offset += 1;
            if history.len() > HISTORY_LIMIT {
                history.pop_front();
                history_offset = history_offset.saturating_sub(1);
            }
        }
    }
}

fn append_limited(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), String> {
    if output
        .len()
        .checked_add(bytes.len())
        .is_none_or(|size| size > limit)
    {
        return Err("expanded data exceeds configured limit".to_owned());
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn append_history(history: &mut VecDeque<u8>, bytes: &[u8], limit: usize) {
    for byte in bytes {
        history.push_back(*byte);
        if history.len() > limit {
            history.pop_front();
        }
    }
}

fn w2d_lz_history() -> Vec<u8> {
    let mut history = w2d_zlib_dictionary();
    let white = legacy_aci_palette(true);
    let black = legacy_aci_palette(false);
    debug_assert_eq!(history.last(), white.first());
    history.extend_from_slice(&white[1..]);
    history.extend_from_slice(&black);
    debug_assert_eq!(history.len(), 13_523);
    history
}

fn legacy_aci_palette(white_background: bool) -> Vec<u8> {
    const WHITE_FIRST: [[u8; 3]; 10] = [
        [255, 255, 255],
        [0, 0, 255],
        [0, 255, 255],
        [0, 255, 0],
        [255, 255, 0],
        [255, 0, 0],
        [255, 0, 255],
        [0, 0, 0],
        [128, 128, 128],
        [192, 192, 192],
    ];
    const BLACK_FIRST: [[u8; 3]; 10] = [
        [0, 0, 0],
        [0, 0, 255],
        [0, 255, 255],
        [0, 255, 0],
        [255, 255, 0],
        [255, 0, 0],
        [255, 0, 255],
        [255, 255, 255],
        [128, 128, 128],
        [192, 192, 192],
    ];
    // Standard AutoCAD R13 ACI intensities. Columns correspond to the
    // 0, 1/4, 1/2, 5/8, 3/4, 7/8, and full channel levels.
    const WHITE_LEVELS: [[u8; 7]; 5] = [
        [0, 63, 127, 159, 191, 223, 255],
        [0, 41, 82, 103, 124, 145, 165],
        [0, 31, 63, 79, 95, 111, 127],
        [0, 19, 38, 47, 57, 66, 76],
        [0, 9, 19, 23, 28, 33, 38],
    ];
    const BLACK_LEVELS: [[u8; 7]; 5] = [
        [0, 63, 127, 159, 191, 223, 255],
        [0, 51, 102, 127, 153, 178, 204],
        [0, 38, 76, 95, 114, 133, 153],
        [0, 31, 63, 79, 95, 111, 127],
        [0, 19, 38, 47, 57, 66, 76],
    ];
    let first = if white_background {
        WHITE_FIRST
    } else {
        BLACK_FIRST
    };
    let levels = if white_background {
        WHITE_LEVELS
    } else {
        BLACK_LEVELS
    };
    let mut output = Vec::with_capacity(1024);
    for color in first {
        output.extend_from_slice(&[color[0], color[1], color[2], 255]);
    }
    for hue in 0..24 {
        let sector = hue / 4;
        let remainder = hue % 4;
        for row in levels {
            let low_index = match remainder {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 4,
            };
            let high_index = match remainder {
                0 => 6,
                1 => 6,
                2 => 6,
                _ => 6,
            };
            let low = row[low_index];
            let high = row[high_index];
            let descending = match remainder {
                0 => high,
                1 => row[4],
                2 => row[2],
                _ => row[1],
            };
            let (red, green, blue) = match sector {
                0 => (high, low, 0),
                1 => (descending, high, 0),
                2 => (0, high, low),
                3 => (0, descending, high),
                4 => (low, 0, high),
                _ => (high, 0, descending),
            };
            output.extend_from_slice(&[blue, green, red, 255]);

            let pastel_index = match remainder {
                0 => 2,
                1 => 3,
                2 => 4,
                _ => 5,
            };
            let pastel_low = row[pastel_index];
            let pastel_descending = match remainder {
                0 => high,
                1 => row[5],
                2 => row[4],
                _ => row[3],
            };
            let (red, green, blue) = match sector {
                0 => (high, pastel_low, row[2]),
                1 => (pastel_descending, high, row[2]),
                2 => (row[2], high, pastel_low),
                3 => (row[2], pastel_descending, high),
                4 => (pastel_low, row[2], high),
                _ => (high, row[2], pastel_descending),
            };
            output.extend_from_slice(&[blue, green, red, 255]);
        }
    }
    let grays: &[u8; 6] = if white_background {
        &[0, 45, 91, 137, 183, 179]
    } else {
        &[51, 91, 132, 173, 214, 255]
    };
    for gray in grays {
        output.extend_from_slice(&[*gray, *gray, *gray, 255]);
    }
    debug_assert_eq!(output.len(), 1024);
    output
}

fn w2d_zlib_dictionary() -> Vec<u8> {
    let mut dictionary = Vec::with_capacity(11_476);
    dictionary
        .extend_from_slice(b"(DashPattern (LineStyle (Copyright (Keywords (Viewport (CodePage ");
    append_binary_color_map(&mut dictionary);

    for text in [
        "(NamedView ",
        "(Author ",
        "(Background ",
        "(Bezier ",
        "(Bounds ",
        "(Clip ",
        "(Color ",
        "(ColorMap ",
        "(Comment ",
        "(Created ",
        "(Creator ",
        "(Description ",
        "(DrawingInfo ",
        ")(Embed 'image/vnd.dwg;' 'AutoCAD-r13' 'unknown.dwg' '')",
        "(View ",
        "(Gouraud ",
        "(Image ",
        "(Layer ",
        "(LineCap ",
        "(LineJoin ",
        "(LinePattern ",
        "(LineWeight ",
        "(Modified ",
        "(Projection ",
        "(Scale ",
        "(SourceCreated ",
        "(SourceFilename ",
        "(SourceModified ",
        "(Text ",
        "(URL 'http://www.com')",
        "'ftp://ftp.",
    ] {
        dictionary.extend_from_slice(text.as_bytes());
    }
    append_decimal_de_bruijn(&mut dictionary);
    dictionary.extend_from_slice(&[b'{', 0x04, 0x04, 0, 0, 1, 0, 0, 0xFF]);
    debug_assert_eq!(dictionary.len(), 11_476);
    dictionary
}

fn append_binary_color_map(output: &mut Vec<u8>) {
    output.extend_from_slice(&[b'{', 0x04, 0x04, 0, 0, 1, 0, 0]);
    const LEVELS: [u8; 6] = [0, 51, 102, 153, 204, 255];
    const GRAYS: [u8; 20] = [
        0, 13, 26, 40, 53, 67, 80, 93, 107, 120, 134, 147, 161, 174, 187, 201, 214, 228, 241, 255,
    ];
    const FIRST: [[u8; 3]; 10] = [
        [0, 0, 0],
        [0, 0, 128],
        [0, 128, 0],
        [0, 128, 128],
        [128, 0, 0],
        [128, 0, 128],
        [128, 128, 0],
        [192, 192, 192],
        [192, 220, 192],
        [240, 202, 166],
    ];
    const LAST: [[u8; 3]; 10] = [
        [240, 251, 255],
        [164, 160, 160],
        [128, 128, 128],
        [0, 0, 255],
        [0, 255, 0],
        [0, 255, 255],
        [255, 0, 0],
        [255, 0, 255],
        [255, 255, 0],
        [255, 255, 255],
    ];
    for rgb in FIRST {
        output.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    for blue in LEVELS {
        for green in LEVELS {
            for red in LEVELS {
                output.extend_from_slice(&[red, green, blue, 255]);
            }
        }
    }
    for gray in GRAYS {
        output.extend_from_slice(&[gray, gray, gray, 255]);
    }
    for rgb in LAST {
        output.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
}

fn append_decimal_de_bruijn(output: &mut Vec<u8>) {
    fn generate(t: usize, period: usize, state: &mut [u8], sequence: &mut Vec<u8>) {
        const ALPHABET: u8 = 10;
        const ORDER: usize = 4;
        if t > ORDER {
            if ORDER % period == 0 {
                sequence.extend_from_slice(&state[1..=period]);
            }
            return;
        }
        state[t] = state[t - period];
        generate(t + 1, period, state, sequence);
        for digit in state[t - period].saturating_add(1)..ALPHABET {
            state[t] = digit;
            generate(t + 1, t, state, sequence);
        }
    }

    let mut state = [0_u8; 40];
    let mut sequence = Vec::with_capacity(10_000);
    generate(1, 1, &mut state, &mut sequence);
    debug_assert_eq!(sequence.len(), 10_000);
    output.extend(
        sequence
            .iter()
            .chain(sequence.iter().take(3))
            .map(|digit| b'0' + digit),
    );
}

fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    #[test]
    fn generated_dictionary_matches_the_w2d_identifier() {
        let dictionary = w2d_zlib_dictionary();
        assert_eq!(dictionary.len(), 11_476);
        assert_eq!(adler32(&dictionary), EXPECTED_DICTIONARY_ADLER32);
        assert!(dictionary
            .windows(b"(LineWeight ".len())
            .any(|part| part == b"(LineWeight "));
    }

    #[test]
    fn generated_legacy_history_matches_the_standard_identifier() {
        let history = w2d_lz_history();
        assert_eq!(history.len(), 13_523);
        assert_eq!(adler32(&history), 0x70C6_4AB6);
    }

    #[test]
    fn w2d_6_revision_uses_the_preloaded_legacy_history() {
        let encoded_offset = (13_523_u16 - 1).to_le_bytes();
        let encoded = [0x10, encoded_offset[0], encoded_offset[1], 0];

        let result = expand_lz(&encoded, 600, 16).unwrap();

        assert_eq!(result.bytes, b"(Das");
        assert_eq!(result.consumed, encoded.len());
    }

    #[test]
    fn expands_a_dictionary_zlib_stream_and_reports_consumed_bytes() {
        let dictionary = w2d_zlib_dictionary();
        let source = b"(Line 0,0 10,10)(EndOfDWF)";
        let mut encoder = Compress::new(Compression::default(), true);
        encoder.set_dictionary(&dictionary).unwrap();
        let mut encoded = Vec::with_capacity(256);
        encoder
            .compress_vec(source, &mut encoded, FlushCompress::Finish)
            .unwrap();
        encoded.extend_from_slice(b"}tail");

        let result = expand_zlib(&encoded, 1024).unwrap();
        assert_eq!(result.bytes, source);
        assert_eq!(&encoded[result.consumed..], b"}tail");
    }
}
