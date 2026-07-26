use super::XpsImageMetadata;

const DEFAULT_DPI: f64 = 96.0;

pub(super) fn image_metadata(data: &[u8]) -> Option<XpsImageMetadata> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png_metadata(data);
    }
    if data.starts_with(b"\xff\xd8") {
        return jpeg_metadata(data);
    }
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return tiff_metadata(data, 0);
    }
    None
}

fn png_metadata(data: &[u8]) -> Option<XpsImageMetadata> {
    if data.len() < 24 || &data[12..16] != b"IHDR" {
        return None;
    }
    let pixel_width = be_u32(data, 16)?;
    let pixel_height = be_u32(data, 20)?;
    if pixel_width == 0 || pixel_height == 0 {
        return None;
    }
    let mut dpi = None;
    let mut offset = 8usize;
    while offset.checked_add(12)? <= data.len() {
        let length = usize::try_from(be_u32(data, offset)?).ok()?;
        let kind_start = offset.checked_add(4)?;
        let body_start = offset.checked_add(8)?;
        let body_end = body_start.checked_add(length)?;
        let chunk_end = body_end.checked_add(4)?;
        if chunk_end > data.len() {
            return None;
        }
        if &data[kind_start..body_start] == b"pHYs" && length == 9 && data[body_start + 8] == 1 {
            let x = be_u32(data, body_start)?;
            let y = be_u32(data, body_start + 4)?;
            if x != 0 && y != 0 {
                dpi = Some((f64::from(x) * 0.0254, f64::from(y) * 0.0254));
            }
        }
        offset = chunk_end;
        if &data[kind_start..body_start] == b"IEND" {
            break;
        }
    }
    let (dpi_x, dpi_y) = dpi.unwrap_or((DEFAULT_DPI, DEFAULT_DPI));
    valid_metadata(pixel_width, pixel_height, dpi_x, dpi_y)
}

fn jpeg_metadata(data: &[u8]) -> Option<XpsImageMetadata> {
    let mut offset = 2usize;
    let mut dimensions = None;
    let mut density = None;
    let mut exif_density = None;
    while offset < data.len() {
        while data.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *data.get(offset)?;
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let length = usize::from(be_u16(data, offset)?);
        if length < 2 {
            return None;
        }
        let body_start = offset.checked_add(2)?;
        let body_end = offset.checked_add(length)?;
        if body_end > data.len() {
            return None;
        }
        let body = &data[body_start..body_end];
        if marker == 0xe0 && body.len() >= 12 && body.starts_with(b"JFIF\0") {
            let units = body[7];
            let x = f64::from(u16::from_be_bytes([body[8], body[9]]));
            let y = f64::from(u16::from_be_bytes([body[10], body[11]]));
            if x > 0.0 && y > 0.0 {
                density = match units {
                    1 => Some((x, y)),
                    2 => Some((x * 2.54, y * 2.54)),
                    _ => None,
                };
            }
        } else if marker == 0xe1 && body.starts_with(b"Exif\0\0") {
            exif_density = tiff_density(&body[6..], 0);
        }
        if is_start_of_frame(marker) && body.len() >= 5 {
            let height = u32::from(u16::from_be_bytes([body[1], body[2]]));
            let width = u32::from(u16::from_be_bytes([body[3], body[4]]));
            if width != 0 && height != 0 {
                dimensions = Some((width, height));
            }
        }
        offset = body_end;
    }
    let (pixel_width, pixel_height) = dimensions?;
    let (dpi_x, dpi_y) = density
        .or(exif_density)
        .unwrap_or((DEFAULT_DPI, DEFAULT_DPI));
    valid_metadata(pixel_width, pixel_height, dpi_x, dpi_y)
}

fn is_start_of_frame(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

fn tiff_metadata(data: &[u8], base: usize) -> Option<XpsImageMetadata> {
    let fields = tiff_fields(data, base)?;
    let pixel_width = fields.width?;
    let pixel_height = fields.height?;
    let (dpi_x, dpi_y) = normalized_tiff_density(
        fields.x_resolution,
        fields.y_resolution,
        fields.resolution_unit,
    );
    valid_metadata(pixel_width, pixel_height, dpi_x, dpi_y)
}

fn tiff_density(data: &[u8], base: usize) -> Option<(f64, f64)> {
    let fields = tiff_fields(data, base)?;
    (fields.x_resolution.is_some() || fields.y_resolution.is_some()).then(|| {
        normalized_tiff_density(
            fields.x_resolution,
            fields.y_resolution,
            fields.resolution_unit,
        )
    })
}

struct TiffFields {
    width: Option<u32>,
    height: Option<u32>,
    x_resolution: Option<f64>,
    y_resolution: Option<f64>,
    resolution_unit: u16,
}

fn tiff_fields(data: &[u8], base: usize) -> Option<TiffFields> {
    let header = data.get(base..base.checked_add(8)?)?;
    let endian = match &header[..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return None,
    };
    if read_u16(header, 2, endian)? != 42 {
        return None;
    }
    let ifd_relative = usize::try_from(read_u32(header, 4, endian)?).ok()?;
    let ifd = base.checked_add(ifd_relative)?;
    let count = usize::from(read_u16(data, ifd, endian)?);
    if count > 4096 {
        return None;
    }
    let mut width = None;
    let mut height = None;
    let mut x_resolution = None;
    let mut y_resolution = None;
    let mut resolution_unit = 2u16;
    for index in 0..count {
        let entry = ifd.checked_add(2)?.checked_add(index.checked_mul(12)?)?;
        if entry.checked_add(12)? > data.len() {
            return None;
        }
        let tag = read_u16(data, entry, endian)?;
        match tag {
            256 => width = tiff_integer(data, entry, endian),
            257 => height = tiff_integer(data, entry, endian),
            282 => x_resolution = tiff_rational(data, base, entry, endian),
            283 => y_resolution = tiff_rational(data, base, entry, endian),
            296 => {
                resolution_unit = tiff_integer(data, entry, endian)
                    .and_then(|value| u16::try_from(value).ok())
                    .unwrap_or(2);
            }
            _ => {}
        }
    }
    Some(TiffFields {
        width,
        height,
        x_resolution,
        y_resolution,
        resolution_unit,
    })
}

fn normalized_tiff_density(
    x_resolution: Option<f64>,
    y_resolution: Option<f64>,
    resolution_unit: u16,
) -> (f64, f64) {
    let factor = match resolution_unit {
        2 => 1.0,
        3 => 2.54,
        _ => return (DEFAULT_DPI, DEFAULT_DPI),
    };
    (
        x_resolution.map_or(DEFAULT_DPI, |value| value * factor),
        y_resolution.map_or(DEFAULT_DPI, |value| value * factor),
    )
}

fn tiff_integer(data: &[u8], entry: usize, endian: Endian) -> Option<u32> {
    let kind = read_u16(data, entry + 2, endian)?;
    let count = read_u32(data, entry + 4, endian)?;
    if count != 1 {
        return None;
    }
    match kind {
        3 => read_u16(data, entry + 8, endian).map(u32::from),
        4 => read_u32(data, entry + 8, endian),
        _ => None,
    }
}

fn tiff_rational(data: &[u8], base: usize, entry: usize, endian: Endian) -> Option<f64> {
    if read_u16(data, entry + 2, endian)? != 5 || read_u32(data, entry + 4, endian)? != 1 {
        return None;
    }
    let relative = usize::try_from(read_u32(data, entry + 8, endian)?).ok()?;
    let offset = base.checked_add(relative)?;
    let numerator = read_u32(data, offset, endian)?;
    let denominator = read_u32(data, offset + 4, endian)?;
    (denominator != 0).then(|| f64::from(numerator) / f64::from(denominator))
}

fn valid_metadata(
    pixel_width: u32,
    pixel_height: u32,
    dpi_x: f64,
    dpi_y: f64,
) -> Option<XpsImageMetadata> {
    (pixel_width != 0
        && pixel_height != 0
        && dpi_x.is_finite()
        && dpi_y.is_finite()
        && dpi_x > 0.0
        && dpi_y > 0.0)
        .then_some(XpsImageMetadata {
            pixel_width,
            pixel_height,
            dpi_x,
            dpi_y,
        })
}

fn be_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn be_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u16(data: &[u8], offset: usize, endian: Endian) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u16::from_le_bytes(bytes),
        Endian::Big => u16::from_be_bytes(bytes),
    })
}

fn read_u32(data: &[u8], offset: usize, endian: Endian) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn density_only_tiff(x: u32, y: u32, unit: u16) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42_u16.to_le_bytes());
        data.extend_from_slice(&8_u32.to_le_bytes());
        data.extend_from_slice(&3_u16.to_le_bytes());
        for (tag, offset) in [(282_u16, 50_u32), (283_u16, 58_u32)] {
            data.extend_from_slice(&tag.to_le_bytes());
            data.extend_from_slice(&5_u16.to_le_bytes());
            data.extend_from_slice(&1_u32.to_le_bytes());
            data.extend_from_slice(&offset.to_le_bytes());
        }
        data.extend_from_slice(&296_u16.to_le_bytes());
        data.extend_from_slice(&3_u16.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&unit.to_le_bytes());
        data.extend_from_slice(&0_u16.to_le_bytes());
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&x.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&y.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data
    }

    #[test]
    fn reads_png_dimensions_and_physical_resolution() {
        let mut png =
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x04\x08\x06\0\0\0\0\0\0\0".to_vec();
        png.extend_from_slice(b"\0\0\0\tpHYs\0\0\x1d\x88\0\0\x1d\x88\x01\0\0\0\0");
        let metadata = image_metadata(&png).expect("metadata");
        assert_eq!((metadata.pixel_width, metadata.pixel_height), (2, 4));
        assert!((metadata.dpi_x - 192.024).abs() < 0.001);
        assert!((metadata.physical_size_dip()[0] - 0.999875).abs() < 0.001);
    }

    #[test]
    fn reads_exif_density_without_tiff_dimensions() {
        let tiff = density_only_tiff(200, 100, 3);
        let mut jpeg = b"\xff\xd8\xff\xe1".to_vec();
        jpeg.extend_from_slice(&u16::try_from(tiff.len() + 8).expect("length").to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&tiff);
        jpeg.extend_from_slice(b"\xff\xc0\x00\x07\x08\x00\x14\x00\x0a\xff\xd9");

        let metadata = image_metadata(&jpeg).expect("metadata");
        assert_eq!((metadata.pixel_width, metadata.pixel_height), (10, 20));
        assert!((metadata.dpi_x - 508.0).abs() < 0.001);
        assert!((metadata.dpi_y - 254.0).abs() < 0.001);
    }

    #[test]
    fn missing_tiff_resolution_does_not_scale_the_default_dpi() {
        assert_eq!(
            normalized_tiff_density(None, None, 3),
            (DEFAULT_DPI, DEFAULT_DPI)
        );
    }
}
