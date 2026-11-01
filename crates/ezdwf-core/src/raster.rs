//! Raster images of W2D `Image` records as PNG.
//!
//! W2D stores an image in one of ten encodings. PNG and JPEG are containers of
//! their own; RGB, RGBA, indexed, mapped and bitonal images are raw rows, top
//! row first; Group 4 images are CCITT T.6 streams, bare or wrapped in a TIFF
//! (`group4x_mapped`). This module turns the raw and Group 4 forms into PNG so
//! that readers need one decoder.
//!
//! A two-color image takes its colors from the record's color map by raw bit
//! value: index 0 for the pixels of T.6 "white" runs (bit 0), index 1 for
//! "black" runs (bit 1). The TIFF wrapper's PhotometricInterpretation is not
//! consulted: Autodesk's writer puts the same template header on every image,
//! and real files carry maps that contradict it.

use std::collections::BTreeMap;
use std::io::Write as _;

use fax::decoder::decode_g4;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::normalized::NormalizedImage;
use crate::DwfError;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
const BLACK_ON_WHITE: [[u8; 4]; 2] = [[255, 255, 255, 255], [0, 0, 0, 255]];

/// Returns the PNG bytes of a normalized image; see [`image_to_png`].
pub fn normalized_image_to_png(image: &NormalizedImage) -> Result<Vec<u8>, DwfError> {
    image_to_png(
        &image.format,
        image.columns,
        image.rows,
        &image.color_map,
        &image.data,
    )
}

/// Returns the PNG bytes of a W2D image.
///
/// PNG data is returned as it is. Raw rows (`rgb`, `rgba`, `indexed`, `mapped`,
/// `bitonal_mapped`) and Group 4 streams (`group4`, `group4x_mapped`; bare or
/// wrapped in a single-bit TIFF) are converted. JPEG data is a container of its
/// own and is refused with [`DwfError::UnsupportedImage`], as is Group 3 data;
/// data that does not decode is refused with [`DwfError::InvalidImage`].
pub fn image_to_png(
    format: &str,
    columns: u16,
    rows: u16,
    color_map: &[[u8; 4]],
    data: &[u8],
) -> Result<Vec<u8>, DwfError> {
    let format = format.trim().to_ascii_lowercase();
    if data.starts_with(&PNG_SIGNATURE) {
        return Ok(data.to_vec());
    }
    if format == "png" {
        return Err(invalid(
            &format,
            "data does not start with the PNG signature",
        ));
    }
    if matches!(format.as_str(), "jpeg" | "jpg") {
        return Err(unsupported(
            &format,
            "JPEG data is a container of its own; use it as it is",
        ));
    }
    if columns == 0 || rows == 0 {
        return Err(invalid(&format, "image has no pixels"));
    }
    let (width, height) = (u32::from(columns), u32::from(rows));
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        let bits = tiff_bilevel_rows(&format, width, height, data)?;
        return encode_png(width, height, 1, 3, Some(&two_colors(color_map)), &bits);
    }
    match format.as_str() {
        "rgb" => {
            expect_len(&format, data, pixel_count(width, height) * 3)?;
            encode_png(width, height, 8, 2, None, data)
        }
        "rgba" => {
            expect_len(&format, data, pixel_count(width, height) * 4)?;
            encode_png(width, height, 8, 6, None, data)
        }
        "indexed" | "mapped" => {
            expect_len(&format, data, pixel_count(width, height))?;
            if color_map.is_empty() || color_map.len() > 256 {
                return Err(invalid(
                    &format,
                    format!("color map has {} entries", color_map.len()),
                ));
            }
            if let Some(index) = data
                .iter()
                .copied()
                .find(|index| usize::from(*index) >= color_map.len())
            {
                return Err(invalid(
                    &format,
                    format!("pixel index {index} is outside the color map"),
                ));
            }
            encode_png(width, height, 8, 3, Some(color_map), data)
        }
        "bitonal_mapped" => {
            expect_len(&format, data, row_stride(width) * height as usize)?;
            encode_png(width, height, 1, 3, Some(&two_colors(color_map)), data)
        }
        "group4" | "group4x_mapped" => {
            let mut bits = Vec::with_capacity(row_stride(width) * height as usize);
            group4_rows(&format, data, width, height, &mut bits)?;
            encode_png(width, height, 1, 3, Some(&two_colors(color_map)), &bits)
        }
        _ => Err(unsupported(&format, "no decoder for this image encoding")),
    }
}

fn invalid(format: &str, context: impl Into<String>) -> DwfError {
    DwfError::InvalidImage {
        format: format.to_owned(),
        context: context.into(),
    }
}

fn unsupported(format: &str, context: impl Into<String>) -> DwfError {
    DwfError::UnsupportedImage {
        format: format.to_owned(),
        context: context.into(),
    }
}

fn pixel_count(width: u32, height: u32) -> usize {
    width as usize * height as usize
}

fn row_stride(width: u32) -> usize {
    (width as usize).div_ceil(8)
}

fn expect_len(format: &str, data: &[u8], expected: usize) -> Result<(), DwfError> {
    if data.len() == expected {
        Ok(())
    } else {
        Err(invalid(
            format,
            format!(
                "image declares {expected} bytes of pixels but carries {}",
                data.len()
            ),
        ))
    }
}

/// The two colors of a bilevel image: the record's first two map entries, or
/// black on white when the record has no map.
fn two_colors(color_map: &[[u8; 4]]) -> [[u8; 4]; 2] {
    match color_map {
        [first, second, ..] => [*first, *second],
        _ => BLACK_ON_WHITE,
    }
}

/// Decode a T.6 stream of `rows` rows into packed 1-bit rows appended to `out`.
fn group4_rows(
    format: &str,
    stream: &[u8],
    width: u32,
    rows: u32,
    out: &mut Vec<u8>,
) -> Result<(), DwfError> {
    let stride = row_stride(width);
    let mut decoded = 0u32;
    decode_g4(stream.iter().copied(), width, Some(rows), |transitions| {
        if decoded < rows {
            let start = out.len();
            out.resize(start + stride, 0);
            pack_row(transitions, width, &mut out[start..]);
            decoded += 1;
        }
    });
    if decoded != rows {
        return Err(invalid(
            format,
            format!("Group 4 data ends after {decoded} of {rows} rows"),
        ));
    }
    Ok(())
}

/// Set the bits of the black runs of one row. `transitions` are the pixel
/// positions where the color changes, starting from white, as the decoder
/// reports them.
fn pack_row(transitions: &[u32], width: u32, row: &mut [u8]) {
    let mut black = false;
    let mut start = 0u32;
    for &position in transitions {
        let end = position.clamp(start, width);
        if black && end > start {
            set_bits(row, start, end);
        }
        black = !black;
        start = end;
    }
    if black && start < width {
        set_bits(row, start, width);
    }
}

/// Set bits `[from, to)` of a row stored most significant bit first.
fn set_bits(row: &mut [u8], from: u32, to: u32) {
    let (from, to) = (from as usize, to as usize);
    let first = from / 8;
    let last = (to - 1) / 8;
    let head = 0xFFu8 >> (from % 8);
    let tail = 0xFFu8 << (8 - (to - last * 8));
    if first == last {
        row[first] |= head & tail;
        return;
    }
    row[first] |= head;
    for byte in &mut row[first + 1..last] {
        *byte = 0xFF;
    }
    row[last] |= tail;
}

/// The packed 1-bit rows of a single-bit TIFF, strip by strip.
fn tiff_bilevel_rows(
    format: &str,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>, DwfError> {
    let tiff = Tiff {
        data,
        little_endian: data.starts_with(b"II"),
    };
    let tags = tiff
        .tags()
        .ok_or_else(|| invalid(format, "TIFF directory is truncated"))?;
    let first = |tag: u16, default: u32| {
        tags.get(&tag)
            .and_then(|values| values.first().copied())
            .unwrap_or(default)
    };
    let (tiff_width, tiff_height) = (first(256, 0), first(257, 0));
    if (tiff_width, tiff_height) != (width, height) {
        return Err(invalid(
            format,
            format!("TIFF is {tiff_width}x{tiff_height} pixels, the record says {width}x{height}"),
        ));
    }
    if first(258, 1) != 1 || first(277, 1) != 1 {
        return Err(unsupported(format, "only single-bit TIFF data is decoded"));
    }
    let compression = first(259, 1);
    let reverse_bits = first(266, 1) == 2;
    let offsets = tags.get(&273).cloned().unwrap_or_default();
    let counts = tags.get(&279).cloned().unwrap_or_else(|| {
        // A single strip may leave the count out; it then runs to the end.
        match offsets.as_slice() {
            [offset] => vec![(data.len() as u32).saturating_sub(*offset)],
            _ => Vec::new(),
        }
    });
    if offsets.is_empty() || offsets.len() != counts.len() {
        return Err(invalid(format, "TIFF strips are not described"));
    }
    let rows_per_strip = first(278, height).clamp(1, height);
    let stride = row_stride(width);
    let mut bits = Vec::with_capacity(stride * height as usize);
    let mut remaining = height;
    for (offset, count) in offsets.iter().zip(&counts) {
        let strip_rows = rows_per_strip.min(remaining);
        if strip_rows == 0 {
            break;
        }
        let strip = usize::try_from(*offset)
            .ok()
            .zip(usize::try_from(*count).ok())
            .and_then(|(offset, count)| data.get(offset..offset.checked_add(count)?))
            .ok_or_else(|| invalid(format, "TIFF strip lies outside the data"))?;
        let strip: Vec<u8> = if reverse_bits {
            strip.iter().map(|byte| byte.reverse_bits()).collect()
        } else {
            strip.to_vec()
        };
        match compression {
            4 => group4_rows(format, &strip, width, strip_rows, &mut bits)?,
            1 => {
                let needed = stride * strip_rows as usize;
                if strip.len() < needed {
                    return Err(invalid(format, "TIFF strip is shorter than its rows"));
                }
                bits.extend_from_slice(&strip[..needed]);
            }
            2 | 3 => {
                return Err(unsupported(
                    format,
                    "Group 3 (T.4) TIFF compression is not decoded",
                ))
            }
            other => {
                return Err(unsupported(
                    format,
                    format!("TIFF compression {other} is not decoded"),
                ))
            }
        }
        remaining -= strip_rows;
    }
    if remaining != 0 {
        return Err(invalid(
            format,
            format!("TIFF strips cover {} of {height} rows", height - remaining),
        ));
    }
    Ok(bits)
}

struct Tiff<'a> {
    data: &'a [u8],
    little_endian: bool,
}

impl Tiff<'_> {
    fn u16_at(&self, offset: usize) -> Option<u16> {
        let bytes: [u8; 2] = self.data.get(offset..offset + 2)?.try_into().ok()?;
        Some(if self.little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        let bytes: [u8; 4] = self.data.get(offset..offset + 4)?.try_into().ok()?;
        Some(if self.little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    /// The first image file directory as tag → values (BYTE, SHORT and LONG
    /// entries; other types are left out).
    fn tags(&self) -> Option<BTreeMap<u16, Vec<u32>>> {
        let directory = usize::try_from(self.u32_at(4)?).ok()?;
        let count = usize::from(self.u16_at(directory)?);
        let mut tags = BTreeMap::new();
        for index in 0..count {
            let entry = directory + 2 + 12 * index;
            let tag = self.u16_at(entry)?;
            let kind = self.u16_at(entry + 2)?;
            let values = usize::try_from(self.u32_at(entry + 4)?).ok()?;
            let size = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 => 4,
                _ => continue,
            };
            let base = if size * values <= 4 {
                entry + 8
            } else {
                usize::try_from(self.u32_at(entry + 8)?).ok()?
            };
            let mut row = Vec::with_capacity(values.min(1 << 16));
            for position in 0..values {
                let value = match size {
                    1 => u32::from(*self.data.get(base + position)?),
                    2 => u32::from(self.u16_at(base + 2 * position)?),
                    _ => self.u32_at(base + 4 * position)?,
                };
                row.push(value);
            }
            tags.insert(tag, row);
        }
        Some(tags)
    }
}

/// A PNG of packed rows (`bit_depth` bits per sample, no filtering).
fn encode_png(
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    palette: Option<&[[u8; 4]]>,
    rows: &[u8],
) -> Result<Vec<u8>, DwfError> {
    let channels = match color_type {
        2 => 3,
        6 => 4,
        _ => 1,
    };
    let stride = (width as usize * channels * usize::from(bit_depth)).div_ceil(8);
    let zlib = || -> std::io::Result<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(
            Vec::with_capacity(rows.len() / 4 + 64),
            Compression::default(),
        );
        for row in rows.chunks(stride) {
            encoder.write_all(&[0])?;
            encoder.write_all(row)?;
        }
        encoder.finish()
    };
    let idat = zlib().map_err(|error| DwfError::InvalidImage {
        format: "png".to_owned(),
        context: format!("zlib: {error}"),
    })?;
    let mut png = Vec::with_capacity(idat.len() + 128);
    png.extend_from_slice(&PNG_SIGNATURE);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    if let Some(palette) = palette {
        let colors: Vec<u8> = palette
            .iter()
            .flat_map(|color| [color[0], color[1], color[2]])
            .collect();
        chunk(&mut png, b"PLTE", &colors);
        if palette.iter().any(|color| color[3] != 255) {
            let alphas: Vec<u8> = palette.iter().map(|color| color[3]).collect();
            chunk(&mut png, b"tRNS", &alphas);
        }
    }
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = CRC_TABLE[usize::from((crc as u8) ^ byte)] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use fax::encoder::Encoder;
    use fax::{Color, VecWriter};
    use std::io::Read as _;

    fn chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        let mut offset = 8;
        let mut chunks = Vec::new();
        while offset < png.len() {
            let length = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8(png[offset + 4..offset + 8].to_vec()).unwrap();
            let data = png[offset + 8..offset + 8 + length].to_vec();
            let crc = u32::from_be_bytes(
                png[offset + 8 + length..offset + 12 + length]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(crc, crc32(&png[offset + 4..offset + 8 + length]));
            chunks.push((kind, data));
            offset += 12 + length;
        }
        chunks
    }

    fn rows(png: &[u8]) -> Vec<u8> {
        let idat = chunks(png)
            .into_iter()
            .find(|(kind, _)| kind == "IDAT")
            .unwrap()
            .1;
        let mut rows = Vec::new();
        flate2::read::ZlibDecoder::new(idat.as_slice())
            .read_to_end(&mut rows)
            .unwrap();
        rows
    }

    fn header(png: &[u8]) -> (u32, u32, u8, u8) {
        let ihdr = &chunks(png)[0].1;
        (
            u32::from_be_bytes(ihdr[0..4].try_into().unwrap()),
            u32::from_be_bytes(ihdr[4..8].try_into().unwrap()),
            ihdr[8],
            ihdr[9],
        )
    }

    fn palette(png: &[u8], kind: &str) -> Option<Vec<u8>> {
        chunks(png)
            .into_iter()
            .find(|(name, _)| name == kind)
            .map(|(_, data)| data)
    }

    /// 12 x 3 bitmap: a black bar on the first row, a dot on the second, white third.
    const BITMAP: [[u8; 2]; 3] = [
        [0b0111_1100, 0b0000_0000],
        [0b0000_0000, 0b0001_0000],
        [0, 0],
    ];

    fn bitmap_pels(row: usize) -> impl Iterator<Item = Color> {
        (0..12u32).map(move |column| {
            let byte = BITMAP[row][(column / 8) as usize];
            if byte & (0x80 >> (column % 8)) != 0 {
                Color::Black
            } else {
                Color::White
            }
        })
    }

    fn group4_stream() -> Vec<u8> {
        let mut encoder = Encoder::new(VecWriter::new());
        for row in 0..3 {
            encoder.encode_line(bitmap_pels(row), 12).unwrap();
        }
        encoder.finish().unwrap().finish()
    }

    #[test]
    fn crc32_matches_the_png_check_value() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn rgb_rows_become_a_truecolor_png() {
        let png = image_to_png("rgb", 2, 1, &[], &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(header(&png), (2, 1, 8, 2));
        assert_eq!(rows(&png), vec![0, 1, 2, 3, 4, 5, 6]);
        assert!(palette(&png, "PLTE").is_none());
    }

    #[test]
    fn rgba_rows_keep_their_alpha() {
        let png = image_to_png("RGBA", 1, 2, &[], &[1, 2, 3, 128, 4, 5, 6, 0]).unwrap();
        assert_eq!(header(&png), (1, 2, 8, 6));
        assert_eq!(rows(&png), vec![0, 1, 2, 3, 128, 0, 4, 5, 6, 0]);
    }

    #[test]
    fn mapped_pixels_index_the_color_map() {
        let map = [[10, 20, 30, 255], [40, 50, 60, 0]];
        let png = image_to_png("mapped", 2, 2, &map, &[0, 1, 1, 0]).unwrap();
        assert_eq!(header(&png), (2, 2, 8, 3));
        assert_eq!(palette(&png, "PLTE").unwrap(), vec![10, 20, 30, 40, 50, 60]);
        assert_eq!(palette(&png, "tRNS").unwrap(), vec![255, 0]);
        assert_eq!(rows(&png), vec![0, 0, 1, 0, 1, 0]);
        let error = image_to_png("mapped", 2, 2, &map, &[0, 1, 2, 0]).unwrap_err();
        assert!(matches!(error, DwfError::InvalidImage { .. }));
    }

    #[test]
    fn bitonal_rows_are_copied_with_the_two_colors() {
        let map = [[255, 0, 0, 255], [0, 0, 255, 128], [9, 9, 9, 9]];
        let data = [0b1010_0000, 0b0101_1100];
        let png = image_to_png("bitonal_mapped", 10, 1, &map, &data).unwrap();
        assert_eq!(header(&png), (10, 1, 1, 3));
        assert_eq!(palette(&png, "PLTE").unwrap(), vec![255, 0, 0, 0, 0, 255]);
        assert_eq!(palette(&png, "tRNS").unwrap(), vec![255, 128]);
        assert_eq!(rows(&png), vec![0, 0b1010_0000, 0b0101_1100]);
    }

    #[test]
    fn bitonal_without_a_map_is_black_on_white() {
        let png = image_to_png("bitonal_mapped", 8, 1, &[], &[0xF0]).unwrap();
        assert_eq!(palette(&png, "PLTE").unwrap(), vec![255, 255, 255, 0, 0, 0]);
        assert!(palette(&png, "tRNS").is_none());
    }

    #[test]
    fn group4_stream_decodes_to_the_bitmap() {
        let png = image_to_png("group4", 12, 3, &[], &group4_stream()).unwrap();
        assert_eq!(header(&png), (12, 3, 1, 3));
        let expected: Vec<u8> = BITMAP.iter().flat_map(|row| [0, row[0], row[1]]).collect();
        assert_eq!(rows(&png), expected);
    }

    #[test]
    fn tiff_wrapped_group4_follows_the_color_map_not_the_photometric_tag() {
        let tiff = fax::tiff::wrap(&group4_stream(), 12, 3);
        let map = [[255, 255, 255, 0], [0, 0, 0, 255]];
        let png = image_to_png("group4x_mapped", 12, 3, &map, &tiff).unwrap();
        let expected: Vec<u8> = BITMAP.iter().flat_map(|row| [0, row[0], row[1]]).collect();
        assert_eq!(rows(&png), expected);
        assert_eq!(palette(&png, "tRNS").unwrap(), vec![0, 255]);
        // The wrapper says PhotometricInterpretation 0; the map still decides.
        let flipped = [[0, 0, 0, 255], [255, 255, 255, 255]];
        let png = image_to_png("group4x_mapped", 12, 3, &flipped, &tiff).unwrap();
        assert_eq!(palette(&png, "PLTE").unwrap(), vec![0, 0, 0, 255, 255, 255]);
        assert_eq!(rows(&png), expected);
    }

    #[test]
    fn tiff_that_contradicts_the_record_is_refused() {
        let tiff = fax::tiff::wrap(&group4_stream(), 12, 3);
        let error = image_to_png("group4x_mapped", 12, 4, &[], &tiff).unwrap_err();
        assert!(matches!(error, DwfError::InvalidImage { .. }), "{error}");
        let error = image_to_png("group4", 12, 3, &[], &[]).unwrap_err();
        assert!(matches!(error, DwfError::InvalidImage { .. }), "{error}");
    }

    #[test]
    fn containers_pass_or_are_refused() {
        let png = [PNG_SIGNATURE.as_slice(), &[1, 2, 3]].concat();
        assert_eq!(
            image_to_png("group4x_mapped", 4, 4, &[], &png).unwrap(),
            png
        );
        let error = image_to_png("jpeg", 4, 4, &[], &[0xFF, 0xD8, 0xFF]).unwrap_err();
        assert!(matches!(error, DwfError::UnsupportedImage { .. }));
        assert!(error.is_unsupported_error());
        let error = image_to_png("png", 4, 4, &[], &[1, 2, 3]).unwrap_err();
        assert!(matches!(error, DwfError::InvalidImage { .. }));
        let error = image_to_png("group3x_mapped", 4, 4, &[], &[1, 2, 3]).unwrap_err();
        assert!(matches!(error, DwfError::UnsupportedImage { .. }));
    }

    #[test]
    fn set_bits_covers_byte_boundaries() {
        let mut row = [0u8; 3];
        set_bits(&mut row, 3, 5);
        assert_eq!(row, [0b0001_1000, 0, 0]);
        set_bits(&mut row, 6, 19);
        assert_eq!(row, [0b0001_1011, 0xFF, 0b1110_0000]);
        let mut row = [0u8; 1];
        set_bits(&mut row, 0, 8);
        assert_eq!(row, [0xFF]);
    }
}
