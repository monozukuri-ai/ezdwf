use ttf_parser::{Face, GlyphId, OutlineBuilder};

use super::{XpsGlyphs, XpsPathFigure, XpsPathGeometry, XpsPathSegment, XpsPoint};

const OBFUSCATED_FONT_CONTENT_TYPE: &str = "application/vnd.ms-package.obfuscated-opentype";

pub(super) fn prepare_font_data(
    packaged_data: &[u8],
    font_part: &str,
    content_type: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut font_data = packaged_data.to_vec();
    let obfuscated =
        content_type.is_some_and(|value| value.eq_ignore_ascii_case(OBFUSCATED_FONT_CONTENT_TYPE));
    if obfuscated {
        deobfuscate_font(&mut font_data, font_part)?;
    }
    Ok(font_data)
}

pub(super) fn build_glyph_outline(
    glyphs: &XpsGlyphs,
    font_data: &[u8],
) -> Result<Option<XpsPathGeometry>, String> {
    let face_index = font_face_index(&glyphs.font_uri)?;
    let face = Face::parse(font_data, face_index)
        .map_err(|error| format!("packaged OpenType font could not be parsed: {error:?}"))?;
    let placements = glyph_placements(glyphs, &face)?;
    let scale = glyphs.font_rendering_em_size / f64::from(face.units_per_em());
    let rtl = glyphs.bidi_level.is_some_and(|level| level % 2 == 1);
    let italic = glyphs
        .style_simulations
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("italic"));
    let bold = glyphs
        .style_simulations
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("bold"));
    let mut pen_x = glyphs.origin.x;
    let pen_y = glyphs.origin.y;
    let mut figures = Vec::new();
    for placement in placements {
        let direction = if rtl { -1.0 } else { 1.0 };
        let base_x = if rtl {
            pen_x - placement.advance
        } else {
            pen_x
        };
        let simulation_offset = if bold {
            glyphs.font_rendering_em_size * 0.01
        } else {
            0.0
        };
        let origin = XpsPoint {
            x: base_x + direction * placement.u_offset + simulation_offset,
            y: pen_y - placement.v_offset - simulation_offset,
        };
        let mut collector = GlyphOutlineCollector::new(
            origin,
            scale,
            glyphs.sideways,
            italic,
            sideways_top_origin(&face, placement.glyph_id) * scale,
            face.glyph_hor_advance(placement.glyph_id)
                .map_or(0.0, |value| f64::from(value) * scale),
        );
        if face
            .outline_glyph(placement.glyph_id, &mut collector)
            .is_some()
        {
            figures.extend(collector.finish());
        }
        pen_x += direction * placement.advance;
    }
    Ok(Some(XpsPathGeometry {
        fill_rule: "nonzero".to_owned(),
        figures,
        data: None,
        transform: super::XpsMatrix::IDENTITY,
    }))
}

pub(super) fn validate_glyph_spec(glyphs: &XpsGlyphs) -> Result<(), String> {
    if glyphs.sideways && glyphs.bidi_level.is_some_and(|level| level % 2 == 1) {
        return Err("Glyphs cannot combine odd BidiLevel with IsSideways=true".to_owned());
    }
    if let Some(value) = glyphs.style_simulations.as_deref() {
        let normalized = value.to_ascii_lowercase();
        if !matches!(
            normalized.as_str(),
            "none" | "boldsimulation" | "italicsimulation" | "bolditalicsimulation"
        ) {
            return Err(format!("invalid Glyphs.StyleSimulations value {value:?}"));
        }
    }
    if let Some(indices) = glyphs.indices.as_deref() {
        parse_indices(indices)?;
    }
    Ok(())
}

pub(super) fn is_obfuscated_font(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| value.eq_ignore_ascii_case(OBFUSCATED_FONT_CONTENT_TYPE))
}

fn font_face_index(uri: &str) -> Result<u32, String> {
    let Some((_, fragment)) = uri.rsplit_once('#') else {
        return Ok(0);
    };
    if fragment.is_empty() || !fragment.bytes().all(|value| value.is_ascii_digit()) {
        return Err(format!(
            "invalid TrueType collection face fragment {fragment:?}"
        ));
    }
    fragment
        .parse()
        .map_err(|_| format!("TrueType collection face index is too large: {fragment:?}"))
}

fn deobfuscate_font(data: &mut [u8], part: &str) -> Result<(), String> {
    if data.len() < 32 {
        return Err("obfuscated font is shorter than 32 bytes".to_owned());
    }
    let filename = part.rsplit('/').next().unwrap_or(part);
    let stem = filename.split('.').next().unwrap_or(filename);
    let groups = stem.split('-').collect::<Vec<_>>();
    if groups.len() != 5
        || groups[0].len() != 8
        || groups[1].len() != 4
        || groups[2].len() != 4
        || groups[3].len() != 4
        || groups[4].len() != 12
    {
        return Err(format!(
            "obfuscated font part name has no GUID: {filename:?}"
        ));
    }
    let raw = groups
        .iter()
        .flat_map(|group| group.as_bytes().chunks_exact(2))
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|value| u8::from_str_radix(value, 16).ok())
                .ok_or_else(|| format!("invalid GUID in obfuscated font part {filename:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let guid = [
        raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10],
        raw[11], raw[12], raw[13], raw[14], raw[15],
    ];
    let key = [
        guid[15], guid[14], guid[13], guid[12], guid[11], guid[10], guid[9], guid[8], guid[6],
        guid[7], guid[4], guid[5], guid[0], guid[1], guid[2], guid[3],
    ];
    for (index, byte) in data[..32].iter_mut().enumerate() {
        *byte ^= key[index % key.len()];
    }
    Ok(())
}

#[derive(Debug)]
struct GlyphPlacement {
    glyph_id: GlyphId,
    advance: f64,
    u_offset: f64,
    v_offset: f64,
}

#[derive(Debug)]
struct IndexMapping {
    cluster_units: usize,
    cluster_glyphs: usize,
    glyph_index: Option<u16>,
    advance_percent: Option<f64>,
    u_offset_percent: f64,
    v_offset_percent: f64,
}

fn glyph_placements(glyphs: &XpsGlyphs, face: &Face<'_>) -> Result<Vec<GlyphPlacement>, String> {
    let mappings = glyphs
        .indices
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(parse_indices)
        .transpose()?;
    let utf16 = glyphs.unicode_string.encode_utf16().collect::<Vec<_>>();
    let em = glyphs.font_rendering_em_size;
    let bold = glyphs
        .style_simulations
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("bold"));
    let mut output = Vec::new();
    let mut unit_cursor = 0usize;
    let mut pending_cluster: Option<(usize, Vec<u16>, usize)> = None;
    if let Some(mappings) = mappings {
        for mapping in mappings {
            let (cluster, glyph_position) = if let Some((remaining, units, position)) =
                pending_cluster.take()
            {
                if mapping.cluster_units != 1 || mapping.cluster_glyphs != 1 {
                    return Err("nested Glyphs cluster mapping is invalid".to_owned());
                }
                let cluster = units.clone();
                if remaining > 1 {
                    pending_cluster = Some((remaining - 1, units, position + 1));
                }
                (cluster, position)
            } else {
                let end = unit_cursor
                    .checked_add(mapping.cluster_units)
                    .ok_or_else(|| "Glyphs cluster length overflowed".to_owned())?;
                if !utf16.is_empty() && end > utf16.len() {
                    return Err(
                        "Glyphs.Indices consumes more UTF-16 units than UnicodeString".to_owned(),
                    );
                }
                let units = utf16.get(unit_cursor..end).unwrap_or_default().to_vec();
                unit_cursor = end.min(utf16.len());
                if mapping.cluster_glyphs > 1 {
                    pending_cluster = Some((mapping.cluster_glyphs - 1, units.clone(), 1));
                }
                (units, 0)
            };
            let glyph_id = match mapping.glyph_index {
                Some(value) if value < face.number_of_glyphs() => GlyphId(value),
                Some(value) => {
                    return Err(format!("Glyphs.Indices references missing glyph {value}"))
                }
                None => implicit_glyph(face, &cluster, glyph_position)?,
            };
            output.push(placement(glyphs, face, glyph_id, &mapping, em, bold));
        }
        if pending_cluster.is_some() {
            return Err("Glyphs.Indices cluster ended before all glyphs were specified".to_owned());
        }
    }
    for character in String::from_utf16_lossy(&utf16[unit_cursor..]).chars() {
        let glyph_id = face.glyph_index(character).unwrap_or(GlyphId(0));
        output.push(default_placement(glyphs, face, glyph_id, em, bold));
    }
    Ok(output)
}

fn implicit_glyph(face: &Face<'_>, units: &[u16], position: usize) -> Result<GlyphId, String> {
    let characters = char::decode_utf16(units.iter().copied())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Glyphs cluster contains an unpaired UTF-16 surrogate".to_owned())?;
    let character = if characters.len() == 1 {
        characters[0]
    } else {
        *characters.get(position).ok_or_else(|| {
            "Glyphs cluster without explicit glyph indices is not one-to-one".to_owned()
        })?
    };
    Ok(face.glyph_index(character).unwrap_or(GlyphId(0)))
}

fn placement(
    glyphs: &XpsGlyphs,
    face: &Face<'_>,
    glyph_id: GlyphId,
    mapping: &IndexMapping,
    em: f64,
    bold: bool,
) -> GlyphPlacement {
    let advance = mapping.advance_percent.map_or_else(
        || default_advance(glyphs, face, glyph_id, em, bold),
        |value| value * em / 100.0,
    );
    GlyphPlacement {
        glyph_id,
        advance,
        u_offset: mapping.u_offset_percent * em / 100.0,
        v_offset: mapping.v_offset_percent * em / 100.0,
    }
}

fn default_placement(
    glyphs: &XpsGlyphs,
    face: &Face<'_>,
    glyph_id: GlyphId,
    em: f64,
    bold: bool,
) -> GlyphPlacement {
    GlyphPlacement {
        glyph_id,
        advance: default_advance(glyphs, face, glyph_id, em, bold),
        u_offset: 0.0,
        v_offset: 0.0,
    }
}

fn default_advance(
    glyphs: &XpsGlyphs,
    face: &Face<'_>,
    glyph_id: GlyphId,
    em: f64,
    bold: bool,
) -> f64 {
    let units = if glyphs.sideways {
        face.glyph_ver_advance(glyph_id).map_or_else(
            || i32::from(face.ascender()) - i32::from(face.descender()),
            i32::from,
        )
    } else {
        i32::from(face.glyph_hor_advance(glyph_id).unwrap_or(0))
    };
    let mut advance = f64::from(units) * em / f64::from(face.units_per_em());
    if bold {
        advance += em * 0.02;
    }
    advance
}

fn sideways_top_origin(face: &Face<'_>, glyph_id: GlyphId) -> f64 {
    face.glyph_y_origin(glyph_id)
        .map(f64::from)
        .or_else(|| {
            Some(
                f64::from(face.glyph_bounding_box(glyph_id)?.y_max)
                    + f64::from(face.glyph_ver_side_bearing(glyph_id)?),
            )
        })
        .or_else(|| face.typographic_ascender().map(f64::from))
        .unwrap_or_else(|| f64::from(face.ascender()))
}

fn parse_indices(value: &str) -> Result<Vec<IndexMapping>, String> {
    value.split(';').map(parse_index_mapping).collect()
}

fn parse_index_mapping(value: &str) -> Result<IndexMapping, String> {
    let value = value.trim();
    let (cluster_units, cluster_glyphs, tail) = if let Some(body) = value.strip_prefix('(') {
        let close = body
            .find(')')
            .ok_or_else(|| "Glyphs.Indices cluster is missing ')'".to_owned())?;
        let cluster = &body[..close];
        let (units, glyphs) = cluster.split_once(':').unwrap_or((cluster, "1"));
        let units = positive_usize(units, "cluster code-unit count")?;
        let glyphs = positive_usize(glyphs, "cluster glyph count")?;
        (units, glyphs, &body[close + 1..])
    } else {
        (1, 1, value)
    };
    let mut fields = tail.split(',');
    let glyph = fields.next().unwrap_or_default();
    let glyph_index = if glyph.is_empty() {
        None
    } else {
        Some(
            glyph
                .parse::<u16>()
                .map_err(|_| format!("invalid Glyphs glyph index {glyph:?}"))?,
        )
    };
    let advance_percent = optional_number(fields.next(), "advance width")?;
    if advance_percent.is_some_and(|number| number < 0.0) {
        return Err("Glyphs advance width must be non-negative".to_owned());
    }
    let u_offset_percent = optional_number(fields.next(), "u offset")?.unwrap_or(0.0);
    let v_offset_percent = optional_number(fields.next(), "v offset")?.unwrap_or(0.0);
    if fields.next().is_some() {
        return Err("Glyphs.Indices mapping has more than four fields".to_owned());
    }
    Ok(IndexMapping {
        cluster_units,
        cluster_glyphs,
        glyph_index,
        advance_percent,
        u_offset_percent,
        v_offset_percent,
    })
}

fn positive_usize(value: &str, name: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| format!("invalid Glyphs {name} {value:?}"))?;
    (value != 0)
        .then_some(value)
        .ok_or_else(|| format!("Glyphs {name} must be positive"))
}

fn optional_number(value: Option<&str>, name: &str) -> Result<Option<f64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let number = value
        .parse::<f64>()
        .map_err(|_| format!("invalid Glyphs {name} {value:?}"))?;
    if !number.is_finite() {
        return Err(format!("Glyphs {name} is not finite"));
    }
    Ok(Some(number))
}

struct GlyphOutlineCollector {
    origin: XpsPoint,
    scale: f64,
    sideways: bool,
    italic: bool,
    ascender: f64,
    glyph_width: f64,
    figures: Vec<XpsPathFigure>,
    current: Option<XpsPathFigure>,
}

impl GlyphOutlineCollector {
    fn new(
        origin: XpsPoint,
        scale: f64,
        sideways: bool,
        italic: bool,
        ascender: f64,
        glyph_width: f64,
    ) -> Self {
        Self {
            origin,
            scale,
            sideways,
            italic,
            ascender,
            glyph_width,
            figures: Vec::new(),
            current: None,
        }
    }

    fn point(&self, x: f32, y: f32) -> XpsPoint {
        let mut local_x = f64::from(x) * self.scale;
        let local_y = -f64::from(y) * self.scale;
        if self.italic {
            let skew = f64::from(y) * self.scale * 20_f64.to_radians().tan();
            local_x += if self.sideways { -skew } else { skew };
        }
        if self.sideways {
            let centered_x = local_x - self.glyph_width / 2.0;
            let top_y = local_y + self.ascender;
            XpsPoint {
                x: self.origin.x + top_y,
                y: self.origin.y - centered_x,
            }
        } else {
            XpsPoint {
                x: self.origin.x + local_x,
                y: self.origin.y + local_y,
            }
        }
    }

    fn finish_current(&mut self, closed: bool) {
        if let Some(mut figure) = self.current.take() {
            figure.closed = closed;
            self.figures.push(figure);
        }
    }

    fn finish(mut self) -> Vec<XpsPathFigure> {
        self.finish_current(false);
        self.figures
    }
}

impl OutlineBuilder for GlyphOutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_current(false);
        self.current = Some(XpsPathFigure {
            start: self.point(x, y),
            segments: Vec::new(),
            closed: false,
            filled: true,
        });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let end = self.point(x, y);
        if let Some(figure) = &mut self.current {
            figure.segments.push(XpsPathSegment::Line {
                end,
                stroked: false,
                smooth_join: false,
            });
        }
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let end = self.point(x, y);
        if let Some(figure) = &mut self.current {
            figure.segments.push(XpsPathSegment::QuadraticBezier {
                control,
                end,
                stroked: false,
                smooth_join: true,
            });
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let control1 = self.point(x1, y1);
        let control2 = self.point(x2, y2);
        let end = self.point(x, y);
        if let Some(figure) = &mut self.current {
            figure.segments.push(XpsPathSegment::CubicBezier {
                control1,
                control2,
                end,
                stroked: false,
                smooth_join: true,
            });
        }
    }

    fn close(&mut self) {
        self.finish_current(true);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn parses_indices_metrics_and_clusters() {
        let values = parse_indices("12,50,1,-2;(2:1)191").expect("indices");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].glyph_index, Some(12));
        assert_eq!(values[0].advance_percent, Some(50.0));
        assert_eq!((values[1].cluster_units, values[1].cluster_glyphs), (2, 1));
    }

    #[test]
    fn validates_sideways_bidi_and_style_simulations() {
        let mut glyphs = XpsGlyphs {
            unicode_string: "x".to_owned(),
            origin: XpsPoint { x: 0.0, y: 0.0 },
            font_uri: "font.ttf".to_owned(),
            font_resource_part: "Pages/1.fpage".to_owned(),
            normalized_font_uri: None,
            font_rendering_em_size: 12.0,
            indices: Some("1,50".to_owned()),
            style_simulations: Some("BoldItalicSimulation".to_owned()),
            bidi_level: Some(0),
            sideways: true,
            font_part: None,
            font_content_type: None,
            font_obfuscated: false,
            outline: None,
        };
        validate_glyph_spec(&glyphs).expect("valid glyph specification");

        glyphs.bidi_level = Some(1);
        assert!(validate_glyph_spec(&glyphs).is_err());
        glyphs.bidi_level = Some(0);
        glyphs.style_simulations = Some("synthetic".to_owned());
        assert!(validate_glyph_spec(&glyphs).is_err());
    }

    #[test]
    fn deobfuscates_twice_to_the_original_bytes() {
        let part = "Resources/00112233-4455-6677-8899-AABBCCDDEEFF.odttf";
        let mut data = (0_u8..64).collect::<Vec<_>>();
        let original = data.clone();
        deobfuscate_font(&mut data, part).expect("obfuscate");
        assert_ne!(data, original);
        deobfuscate_font(&mut data, part).expect("deobfuscate");
        assert_eq!(data, original);
    }

    #[test]
    fn outlines_a_real_packaged_font_when_the_ecma_sample_is_available() {
        let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples/external/ECMA-388.xps");
        if !sample.is_file() {
            return;
        }
        let file = std::fs::File::open(sample).expect("sample");
        let mut archive = zip::ZipArchive::new(file).expect("XPS ZIP");
        let part = "Resources/29340BCC-2505-0220-1653-905980034711.odttf";
        let mut packaged = Vec::new();
        archive
            .by_name(part)
            .expect("font part")
            .read_to_end(&mut packaged)
            .expect("font bytes");
        let font = prepare_font_data(&packaged, part, Some(OBFUSCATED_FONT_CONTENT_TYPE))
            .expect("deobfuscate");
        let glyphs = XpsGlyphs {
            unicode_string: "OpenXPS".to_owned(),
            origin: XpsPoint { x: 5.0, y: 25.0 },
            font_uri: part.to_owned(),
            font_resource_part: "Pages/1.fpage".to_owned(),
            normalized_font_uri: Some(part.to_owned()),
            font_rendering_em_size: 20.0,
            indices: None,
            style_simulations: None,
            bidi_level: None,
            sideways: false,
            font_part: Some(part.to_owned()),
            font_content_type: Some(OBFUSCATED_FONT_CONTENT_TYPE.to_owned()),
            font_obfuscated: true,
            outline: None,
        };
        let outline = build_glyph_outline(&glyphs, &font)
            .expect("outline")
            .expect("geometry");
        assert!(outline.figures.len() > 20);
    }
}
