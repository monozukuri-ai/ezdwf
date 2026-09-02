use std::collections::BTreeMap;

use super::ascii::{parse_record, Node};
use super::compression::{expand_lz, expand_zlib};
use super::model::{
    W2dBlockRef, W2dColoredPoint, W2dEmbeddedFont, W2dEntity, W2dGeometry, W2dImage, W2dLayer,
    W2dPoint, W2dRendition, W2dSourceSpan, W2dStream, W2dUnits, W2dViewport,
};
use crate::{Diagnostic, DiagnosticSeverity, DwfError, ParseOptions};

const W2D_HEADER_PREFIX: &str = "V";

/// Decode a standalone W2D display-list resource.
///
/// Package-level role, MIME, transform, and clip metadata are attached by the
/// DWF package reader after this function returns.
pub fn decode_w2d(
    data: &[u8],
    resource: &str,
    options: ParseOptions,
) -> Result<W2dStream, DwfError> {
    let source_limit = options.max_file_size.min(options.max_entry_size);
    if data.len() > source_limit {
        return Err(DwfError::W2dSourceSizeLimitExceeded {
            resource: resource.to_owned(),
            actual: data.len(),
            limit: source_limit,
        });
    }
    Decoder::new(data, resource, options).decode()
}

struct Decoder<'a> {
    data: Vec<u8>,
    /// Piecewise source mappings. The number of entries grows with compressed
    /// wrappers, not with the number of input or expanded bytes.
    source_segments: Vec<SourceSegment>,
    source_size: usize,
    resource: &'a str,
    options: ParseOptions,
    position: usize,
    record_count: usize,
    total_points: usize,
    current_point: W2dPoint,
    rendition: W2dRendition,
    layer_definitions: BTreeMap<i32, W2dLayer>,
    units: Option<W2dUnits>,
    viewports: Vec<W2dViewport>,
    current_color_map: Vec<[u8; 4]>,
    color_maps: Vec<Vec<[u8; 4]>>,
    embedded_fonts: Vec<W2dEmbeddedFont>,
    block_refs: Vec<W2dBlockRef>,
    entities: Vec<W2dEntity>,
    diagnostics: Vec<Diagnostic>,
    version: Option<String>,
    source_format: Option<String>,
    complete: bool,
    end_of_dwf_seen: bool,
    compressed_blocks: usize,
}

#[derive(Debug, Clone, Copy)]
struct SourceLocation {
    offset: usize,
    length: usize,
    decoded_offset: Option<usize>,
    compression_depth: usize,
}

#[derive(Debug, Clone, Copy)]
struct SourceSegment {
    start: usize,
    end: usize,
    location: SourceLocation,
}

fn advance_source_location(mut location: SourceLocation, amount: usize) -> SourceLocation {
    if location.compression_depth == 0 {
        location.offset = location.offset.saturating_add(amount);
    } else {
        location.decoded_offset = location
            .decoded_offset
            .map(|offset| offset.saturating_add(amount));
    }
    location
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8], resource: &'a str, options: ParseOptions) -> Self {
        Self {
            data: data.to_vec(),
            source_segments: if data.is_empty() {
                Vec::new()
            } else {
                vec![SourceSegment {
                    start: 0,
                    end: data.len(),
                    location: SourceLocation {
                        offset: 0,
                        length: data.len(),
                        decoded_offset: None,
                        compression_depth: 0,
                    },
                }]
            },
            source_size: data.len(),
            resource,
            options,
            position: 0,
            record_count: 0,
            total_points: 0,
            current_point: W2dPoint::default(),
            rendition: W2dRendition::default(),
            layer_definitions: BTreeMap::new(),
            units: None,
            viewports: Vec::new(),
            current_color_map: Vec::new(),
            color_maps: Vec::new(),
            embedded_fonts: Vec::new(),
            block_refs: Vec::new(),
            entities: Vec::new(),
            diagnostics: Vec::new(),
            version: None,
            source_format: None,
            complete: true,
            end_of_dwf_seen: false,
            compressed_blocks: 0,
        }
    }

    fn decode(mut self) -> Result<W2dStream, DwfError> {
        self.skip_whitespace();
        if self.peek() != Some(b'(') {
            return Err(self.error_at(0, "missing (W2D Vxx.xx) or (DWF Vxx.xx) header"));
        }
        let header_start = self.position;
        let header_end = self.scan_extended_ascii(header_start)?;
        self.position = header_end;
        self.bump_record_count()?;
        let header = self.data[header_start..header_end].to_vec();
        self.parse_header(&header, header_start)?;

        while self.position < self.data.len() {
            self.skip_whitespace();
            if self.position == self.data.len() {
                break;
            }
            self.bump_record_count()?;
            let start = self.position;
            match self.peek().expect("position was checked") {
                b'(' => {
                    let end = self.scan_extended_ascii(start)?;
                    self.position = end;
                    self.decode_extended_ascii(start, end)?;
                }
                b'{' => {
                    if !self.decode_extended_binary(start)? {
                        break;
                    }
                }
                opcode => {
                    self.position += 1;
                    self.decode_single_byte(opcode, start)?;
                }
            }
        }

        let logical_bounds = logical_bounds(&self.entities);
        Ok(W2dStream {
            href: self.resource.to_owned(),
            role: String::new(),
            mime: String::new(),
            source_format: self.source_format.unwrap_or_else(|| "w2d".to_owned()),
            version: self.version.unwrap_or_default(),
            source_size: self.source_size,
            decompressed_size: self.data.len(),
            compressed_blocks: self.compressed_blocks,
            complete: self.complete,
            end_of_dwf_seen: self.end_of_dwf_seen,
            logical_bounds,
            transform: None,
            clip: None,
            units: self.units,
            layers: self.layer_definitions.into_values().collect(),
            viewports: self.viewports,
            color_maps: self.color_maps,
            embedded_fonts: self.embedded_fonts,
            block_refs: self.block_refs,
            entities: self.entities,
            diagnostics: self.diagnostics,
        })
    }

    fn parse_header(&mut self, raw: &[u8], offset: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, offset, self.options)?;
        let values = parsed
            .list()
            .ok_or_else(|| self.error_at(offset, "header is not a list"))?;
        let header_kind = values
            .first()
            .and_then(Node::text)
            .ok_or_else(|| self.error_at(offset, "W2D/DWF header opcode is missing"))?;
        if !matches!(header_kind, "W2D" | "DWF") {
            return Err(self.error_at(offset, "expected a W2D or legacy DWF header"));
        }
        let version = values
            .get(1)
            .and_then(Node::text)
            .ok_or_else(|| self.error_at(offset, "W2D header version is missing"))?;
        let version = version.strip_prefix(W2D_HEADER_PREFIX).unwrap_or(version);
        let Some((major, minor)) = version.split_once('.') else {
            return Err(self.error_at(offset, "W2D header version must contain a decimal point"));
        };
        if major.len() != 2
            || minor.len() != 2
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(self.error_at(offset, "W2D header version must have the form V00.00"));
        }
        let supported = match header_kind {
            "W2D" => major == "06",
            "DWF" => major == "00" && matches!(minor, "34" | "36" | "42" | "55"),
            _ => false,
        };
        if !supported {
            return Err(DwfError::UnsupportedW2dVersion {
                resource: self.resource.to_owned(),
                version: version.to_owned(),
            });
        }
        self.source_format = Some(if header_kind == "DWF" {
            "legacy_dwf".to_owned()
        } else {
            "w2d".to_owned()
        });
        self.version = Some(version.to_owned());
        Ok(())
    }

    fn scan_extended_ascii(&self, start: usize) -> Result<usize, DwfError> {
        let mut position = start;
        let mut depth = 0_usize;
        let mut quote = None;
        let mut escaped = false;
        let mut quoted_size = 0_usize;

        while let Some(byte) = self.data.get(position).copied() {
            if let Some(expected_quote) = quote {
                position += 1;
                quoted_size = quoted_size.saturating_add(1);
                if quoted_size > self.options.max_w2d_string_size {
                    return Err(DwfError::W2dStringLimitExceeded {
                        resource: self.resource.to_owned(),
                        offset: start,
                        limit: self.options.max_w2d_string_size,
                    });
                }
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == expected_quote {
                    quote = None;
                    quoted_size = 0;
                }
                continue;
            }

            match byte {
                b'\'' | b'"' => {
                    quote = Some(byte);
                    quoted_size = 0;
                    position += 1;
                }
                b'(' => {
                    depth = depth.saturating_add(1);
                    if depth > self.options.max_w2d_nesting_depth {
                        return Err(DwfError::W2dNestingLimitExceeded {
                            resource: self.resource.to_owned(),
                            offset: start,
                            limit: self.options.max_w2d_nesting_depth,
                        });
                    }
                    position += 1;
                }
                b')' => {
                    if depth == 0 {
                        return Err(self.error_at(position, "unmatched closing parenthesis"));
                    }
                    depth -= 1;
                    position += 1;
                    if depth == 0 {
                        return Ok(position);
                    }
                }
                b'{' => {
                    // Inside an extended ASCII opcode a brace record is a WHIP!
                    // binary Unicode string: '{' + int32 *character* count +
                    // UTF-16LE code units + '}' (e.g. `(SourceFilename {...})`
                    // written by AutoCAD's DWF ePlot driver). Byte-counted blobs
                    // (with or without the brace counted) are accepted as fallbacks.
                    let length_offset = position.saturating_add(1);
                    let length = self.read_u32_at(length_offset)? as usize;
                    let payload_offset = length_offset
                        .checked_add(4)
                        .ok_or_else(|| self.error_at(position, "binary record length overflow"))?;
                    let candidates = [length.checked_mul(2), Some(length), length.checked_sub(1)];
                    let mut next = None;
                    for payload_len in candidates.into_iter().flatten() {
                        let Some(brace_index) = payload_offset.checked_add(payload_len) else {
                            continue;
                        };
                        if self.data.get(brace_index) == Some(&b'}') {
                            next = Some(brace_index + 1);
                            break;
                        }
                    }
                    let Some(end) = next else {
                        return Err(
                            self.error_at(position, "invalid embedded binary record length")
                        );
                    };
                    position = end;
                }
                _ => position += 1,
            }
        }
        Err(self.error_at(start, "unterminated extended ASCII record"))
    }

    fn decode_extended_binary(&mut self, start: usize) -> Result<bool, DwfError> {
        self.position += 1;
        let length = self.read_u32()? as usize;
        let opcode = self.read_u16()?;
        if length == 0 {
            if matches!(opcode, 0x0010 | 0x0011 | 0x0123) {
                self.expand_compressed_wrapper(start, opcode)?;
                return Ok(true);
            }
            let name = format!("0x{opcode:04X}");
            self.complete = false;
            self.push_diagnostic(
                "W2D_UNSKIPPABLE_EXTENDED_BINARY",
                DiagnosticSeverity::Error,
                format!("extended binary opcode {name} has a zero length and cannot be skipped"),
                "stopped decoding this W2D resource",
                start,
                Some(name),
            );
            self.position = self.data.len();
            return Ok(false);
        }
        if length < 3 {
            return Err(self.error_at(
                start,
                "extended binary length is smaller than opcode + trailer",
            ));
        }
        let end = start
            .checked_add(5)
            .and_then(|value| value.checked_add(length))
            .ok_or_else(|| self.error_at(start, "extended binary record length overflow"))?;
        if end > self.data.len() {
            return Err(self.error_at(start, "truncated extended binary record"));
        }
        if self.data[end - 1] != b'}' {
            return Err(self.error_at(start, "extended binary record has no closing brace"));
        }
        let payload_end = end - 1;
        match opcode {
            0x0001 => self.decode_binary_color_map(start, payload_end)?,
            0x0002..=0x0009 | 0x000C | 0x000D => {
                self.decode_binary_image(opcode, start, payload_end)?;
            }
            0x013E => self.decode_binary_embedded_font(start, payload_end)?,
            0x015D | 0x015E => self.decode_binary_block_ref(opcode, start, payload_end)?,
            _ => {
                self.position = end;
                self.push_diagnostic(
                    "W2D_UNSUPPORTED_EXTENDED_BINARY",
                    DiagnosticSeverity::Warning,
                    format!("extended binary opcode 0x{opcode:04X} is not decoded"),
                    "skipped the length-delimited record",
                    start,
                    Some(format!("0x{opcode:04X}")),
                );
                return Ok(true);
            }
        }
        self.position = end;
        Ok(true)
    }

    fn decode_binary_color_map(
        &mut self,
        start: usize,
        payload_end: usize,
    ) -> Result<(), DwfError> {
        let colors = self.read_binary_color_map(payload_end, start)?;
        self.set_color_map(colors);
        self.require_payload_end(payload_end, start, "ColorMap")
    }

    fn decode_binary_image(
        &mut self,
        opcode: u16,
        start: usize,
        payload_end: usize,
    ) -> Result<(), DwfError> {
        let format = image_format_for_opcode(opcode).to_owned();
        let columns = self.read_u16()?;
        let rows = self.read_u16()?;
        let min = self.read_relative_point_i32()?;
        let max = self.read_relative_point_i32()?;
        let identifier = self.read_i32()?;
        let color_map = if matches!(opcode, 0x0002 | 0x0003 | 0x0005 | 0x000D) {
            self.read_binary_color_map(payload_end, start)?
        } else if opcode == 0x0004 {
            self.current_color_map.clone()
        } else {
            Vec::new()
        };
        let data_size = usize::try_from(self.read_i32()?)
            .map_err(|_| self.error_at(start, "Image data size cannot be negative"))?;
        if self.position.checked_add(data_size) != Some(payload_end) {
            return Err(self.error_at(
                start,
                format!(
                    "Image declares {data_size} data bytes but its wrapper contains {}",
                    payload_end.saturating_sub(self.position)
                ),
            ));
        }
        let data = self.data[self.position..payload_end].to_vec();
        self.position = payload_end;
        self.emit(
            W2dGeometry::Image {
                image: W2dImage {
                    format,
                    identifier,
                    columns,
                    rows,
                    min,
                    max,
                    color_map,
                    data,
                },
            },
            start,
            payload_end + 1 - start,
            &format!("0x{opcode:04X}"),
        )
    }

    fn decode_binary_embedded_font(
        &mut self,
        start: usize,
        payload_end: usize,
    ) -> Result<(), DwfError> {
        let request = self.read_u32()?;
        let privilege = self.read_u8()?;
        let charset = self.read_u8()?;
        let typeface_name = self.read_sized_utf8(payload_end, start, "typeface name")?;
        let logfont_name = self.read_sized_utf8(payload_end, start, "LOGFONT name")?;
        let data_size = self.read_bounded_i32_size(payload_end, start, "embedded font data")?;
        let data_end = self
            .position
            .checked_add(data_size)
            .ok_or_else(|| self.error_at(start, "embedded font data length overflow"))?;
        if data_end != payload_end {
            return Err(self.error_at(
                start,
                "embedded font data length does not match its wrapper",
            ));
        }
        let data = self.data[self.position..data_end].to_vec();
        self.position = data_end;
        self.embedded_fonts.push(W2dEmbeddedFont {
            request,
            privilege,
            charset,
            typeface_name,
            logfont_name,
            data,
            source: self.source_span(start, payload_end + 1 - start, "0x013E"),
        });
        Ok(())
    }

    fn decode_binary_block_ref(
        &mut self,
        opcode: u16,
        start: usize,
        payload_end: usize,
    ) -> Result<(), DwfError> {
        if payload_end.saturating_sub(self.position) < 2 {
            return Err(self.error_at(start, "BlockRef is missing its format operand"));
        }
        let format = self.read_u16()?;
        let payload = self.data[self.position..payload_end].to_vec();
        self.position = payload_end;
        self.block_refs.push(W2dBlockRef {
            format: format.to_string(),
            payload,
            source: self.source_span(start, payload_end + 1 - start, &format!("0x{opcode:04X}")),
        });
        Ok(())
    }

    fn read_binary_color_map(
        &mut self,
        payload_end: usize,
        start: usize,
    ) -> Result<Vec<[u8; 4]>, DwfError> {
        let encoded = self.read_u8()?;
        let count = if encoded == 0 {
            256
        } else {
            usize::from(encoded)
        };
        let byte_count = count
            .checked_mul(4)
            .ok_or_else(|| self.error_at(start, "ColorMap byte count overflow"))?;
        if self
            .position
            .checked_add(byte_count)
            .is_none_or(|end| end > payload_end)
        {
            return Err(self.error_at(start, "truncated ColorMap"));
        }
        let mut colors = Vec::with_capacity(count);
        for _ in 0..count {
            colors.push(self.read_exact()?);
        }
        Ok(colors)
    }

    fn read_bounded_i32_size(
        &mut self,
        payload_end: usize,
        start: usize,
        label: &str,
    ) -> Result<usize, DwfError> {
        let size = usize::try_from(self.read_i32()?)
            .map_err(|_| self.error_at(start, format!("{label} size cannot be negative")))?;
        if self
            .position
            .checked_add(size)
            .is_none_or(|end| end > payload_end)
        {
            return Err(self.error_at(start, format!("truncated {label}")));
        }
        Ok(size)
    }

    fn read_sized_utf8(
        &mut self,
        payload_end: usize,
        start: usize,
        label: &str,
    ) -> Result<String, DwfError> {
        let size = self.read_bounded_i32_size(payload_end, start, label)?;
        if size > self.options.max_w2d_string_size {
            return Err(DwfError::W2dStringLimitExceeded {
                resource: self.resource.to_owned(),
                offset: start,
                limit: self.options.max_w2d_string_size,
            });
        }
        let end = self.position + size;
        let value = std::str::from_utf8(&self.data[self.position..end])
            .map_err(|_| self.error_at(start, format!("{label} is not UTF-8")))?
            .to_owned();
        self.position = end;
        Ok(value)
    }

    fn require_payload_end(
        &self,
        payload_end: usize,
        start: usize,
        label: &str,
    ) -> Result<(), DwfError> {
        if self.position == payload_end {
            Ok(())
        } else {
            Err(self.error_at(
                start,
                format!(
                    "{label} leaves {} unexpected operand bytes",
                    payload_end.saturating_sub(self.position)
                ),
            ))
        }
    }

    fn expand_compressed_wrapper(&mut self, start: usize, opcode: u16) -> Result<(), DwfError> {
        let location = self.source_location(start).unwrap_or(SourceLocation {
            offset: start,
            length: 0,
            decoded_offset: None,
            compression_depth: 0,
        });
        let depth = location.compression_depth.saturating_add(1);
        if depth > self.options.max_w2d_compression_depth {
            return Err(DwfError::W2dCompressionDepthLimitExceeded {
                resource: self.resource.to_owned(),
                limit: self.options.max_w2d_compression_depth,
            });
        }

        let compressed_start = self.position;
        let available = self.data.get(compressed_start..).ok_or_else(|| {
            self.error_at(start, "compressed-data wrapper has no compressed payload")
        })?;
        let revision = self
            .version
            .as_deref()
            .and_then(decimal_revision)
            .unwrap_or(0);
        let expanded_result = if opcode == 0x0011 {
            expand_zlib(available, self.options.max_w2d_decompressed_size)
        } else {
            expand_lz(available, revision, self.options.max_w2d_decompressed_size)
        };
        let expanded = expanded_result.map_err(|context| {
            if context == "expanded data exceeds configured limit" {
                DwfError::W2dDecompressedSizeLimitExceeded {
                    resource: self.resource.to_owned(),
                    limit: self.options.max_w2d_decompressed_size,
                }
            } else {
                self.error_at(start, context)
            }
        })?;
        let trailer = compressed_start
            .checked_add(expanded.consumed)
            .ok_or_else(|| self.error_at(start, "compressed-data length overflow"))?;
        if self.data.get(trailer) != Some(&b'}') {
            return Err(self.error_at(
                start,
                "compressed-data stream is not followed by a closing brace",
            ));
        }
        let end = trailer + 1;
        let wrapper_length = end - start;
        let resulting_size = self
            .data
            .len()
            .checked_sub(wrapper_length)
            .and_then(|size| size.checked_add(expanded.bytes.len()))
            .ok_or_else(|| self.error_at(start, "expanded W2D size overflow"))?;
        if resulting_size > self.options.max_w2d_decompressed_size {
            return Err(DwfError::W2dDecompressedSizeLimitExceeded {
                resource: self.resource.to_owned(),
                limit: self.options.max_w2d_decompressed_size,
            });
        }

        let physical_length = if location.compression_depth == 0 {
            wrapper_length
        } else {
            location.length
        };
        let decoded_base = location.decoded_offset.unwrap_or(0);
        let expanded_length = expanded.bytes.len();
        self.replace_source_range(
            start,
            end,
            expanded_length,
            SourceLocation {
                offset: location.offset,
                length: physical_length,
                decoded_offset: Some(decoded_base),
                compression_depth: depth,
            },
        )?;
        self.data.splice(start..end, expanded.bytes);
        self.position = start;
        self.compressed_blocks = self.compressed_blocks.saturating_add(1);
        Ok(())
    }

    fn replace_source_range(
        &mut self,
        start: usize,
        end: usize,
        replacement_length: usize,
        replacement: SourceLocation,
    ) -> Result<(), DwfError> {
        let replacement_end = start
            .checked_add(replacement_length)
            .ok_or_else(|| self.error_at(start, "expanded source mapping overflow"))?;
        let mut updated = Vec::with_capacity(self.source_segments.len().saturating_add(2));

        for segment in self.source_segments.drain(..) {
            if segment.end <= start {
                updated.push(segment);
                continue;
            }
            if segment.start >= end {
                let start_distance = segment.start - end;
                let end_distance = segment.end - end;
                updated.push(SourceSegment {
                    start: replacement_end + start_distance,
                    end: replacement_end + end_distance,
                    ..segment
                });
                continue;
            }
            if segment.start < start {
                updated.push(SourceSegment {
                    end: start,
                    ..segment
                });
            }
            if segment.end > end {
                updated.push(SourceSegment {
                    start: replacement_end,
                    end: replacement_end + (segment.end - end),
                    location: advance_source_location(
                        segment.location,
                        end.saturating_sub(segment.start),
                    ),
                });
            }
        }

        if replacement_length > 0 {
            updated.push(SourceSegment {
                start,
                end: replacement_end,
                location: replacement,
            });
        }
        updated.sort_unstable_by_key(|segment| segment.start);
        self.source_segments = updated;
        Ok(())
    }

    fn source_location(&self, position: usize) -> Option<SourceLocation> {
        let index = self
            .source_segments
            .partition_point(|segment| segment.start <= position)
            .checked_sub(1)?;
        let segment = self.source_segments.get(index)?;
        if position >= segment.end {
            return None;
        }
        Some(advance_source_location(
            segment.location,
            position - segment.start,
        ))
    }

    fn bump_record_count(&mut self) -> Result<(), DwfError> {
        self.record_count = self.record_count.saturating_add(1);
        if self.record_count > self.options.max_w2d_records {
            return Err(DwfError::W2dRecordLimitExceeded {
                resource: self.resource.to_owned(),
                limit: self.options.max_w2d_records,
            });
        }
        Ok(())
    }

    fn push_diagnostic(
        &mut self,
        code: &str,
        severity: DiagnosticSeverity,
        message: String,
        action: &str,
        offset: usize,
        opcode: Option<String>,
    ) {
        let mut details = BTreeMap::new();
        if let Some(opcode) = opcode {
            details.insert("opcode".to_owned(), opcode);
        }
        self.diagnostics.push(Diagnostic {
            code: code.to_owned(),
            severity,
            message,
            action: action.to_owned(),
            section: None,
            resource: Some(self.resource.to_owned()),
            offset: Some(offset),
            details,
        });
    }

    fn error_at(&self, offset: usize, context: impl Into<String>) -> DwfError {
        DwfError::InvalidW2d {
            resource: self.resource.to_owned(),
            offset,
            context: context.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(is_w2d_whitespace) {
            self.position += 1;
        }
    }

    fn decode_extended_ascii(&mut self, start: usize, end: usize) -> Result<(), DwfError> {
        let raw = self.data[start..end].to_vec();
        let name = extended_name(&raw).ok_or_else(|| {
            self.error_at(
                start,
                "extended ASCII record does not contain an opcode name",
            )
        })?;
        match name {
            "W2D" | "DWF" => Err(self.error_at(start, "duplicate W2D/DWF header")),
            "Layer" => self.decode_ascii_layer(&raw, start),
            "Color" => self.decode_ascii_color(&raw, start),
            "ColorMap" => self.decode_ascii_color_map(&raw, start),
            "LineWeight" => self.decode_ascii_line_weight(&raw, start),
            "LinePattern" => self.decode_ascii_line_pattern(&raw, start),
            "LineStyle" => self.decode_ascii_line_style(&raw, start),
            "FillPattern" => self.decode_ascii_fill_pattern(&raw, start),
            "Font" => self.decode_ascii_font(&raw, start),
            "FontExtension" => self.decode_ascii_font_extension(&raw, start),
            "Units" => {
                let parsed = parse_record(&raw, self.resource, start, self.options)?;
                self.units = Some(parse_units(&parsed, self.resource, start)?);
                Ok(())
            }
            "Viewport" => self.decode_ascii_viewport(&raw, start),
            "Line" => self.decode_ascii_line_record(&raw, start),
            "Polyline" => self.decode_ascii_point_record(&raw, start, false),
            "Polygon" => self.decode_ascii_point_record(&raw, start, true),
            "Bezier" => self.decode_ascii_bezier(&raw, start),
            "Circle" => self.decode_ascii_circle(&raw, start),
            "Ellipse" => self.decode_ascii_ellipse(&raw, start),
            "Text" => self.decode_ascii_text(&raw, start),
            "Contour" => self.decode_ascii_contour(&raw, start),
            "Gouraud" => self.decode_ascii_gouraud(&raw, start, true),
            "GourLine" => self.decode_ascii_gouraud(&raw, start, false),
            "Texture" => self.decode_ascii_texture(&raw, start),
            "Image" | "Group4PNGImage" => self.decode_ascii_image(&raw, start, name),
            "Embedded_Font" | "EmbeddedFont" => self.decode_ascii_embedded_font(&raw, start, name),
            "BlockRef" => self.decode_ascii_block_ref(&raw, start),
            "EndOfDWF" => {
                self.end_of_dwf_seen = true;
                Ok(())
            }
            "Encryption" | "Psswd" | "Password" | "SignData" => {
                self.complete = false;
                self.push_diagnostic(
                    "W2D_RESTRICTED_CONTENT",
                    DiagnosticSeverity::Error,
                    format!("{name} content is detected but is not supported"),
                    "marked this W2D resource incomplete",
                    start,
                    Some(format!("({name}")),
                );
                Ok(())
            }
            _ if is_informational_extended_opcode(name) => Ok(()),
            _ => {
                self.push_diagnostic(
                    "W2D_UNKNOWN_EXTENDED_ASCII",
                    DiagnosticSeverity::Warning,
                    format!("unknown extended ASCII opcode ({name} ...)"),
                    "skipped the balanced record",
                    start,
                    Some(format!("({name}")),
                );
                Ok(())
            }
        }
    }

    fn decode_ascii_layer(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Layer", self.resource, start)?;
        let number = required_i64(values.get(1), self.resource, start, "Layer number")?;
        let number = i32::try_from(number)
            .map_err(|_| self.error_at(start, "Layer number is outside the 32-bit range"))?;
        let name = values.get(2).and_then(Node::text).map(str::to_owned);
        let layer = if let Some(name) = name {
            let layer = W2dLayer {
                number,
                name: Some(name),
            };
            self.layer_definitions.insert(number, layer.clone());
            layer
        } else {
            self.layer_definitions
                .get(&number)
                .cloned()
                .unwrap_or(W2dLayer { number, name: None })
        };
        self.rendition.layer = Some(layer);
        Ok(())
    }

    fn decode_ascii_color(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Color", self.resource, start)?;
        let channels = values
            .iter()
            .skip(1)
            .map(|value| required_u8(Some(value), self.resource, start, "Color channel"))
            .collect::<Result<Vec<_>, _>>()?;
        let color: [u8; 4] = channels.try_into().map_err(|values: Vec<u8>| {
            self.error_at(
                start,
                format!("Color requires four channels, got {}", values.len()),
            )
        })?;
        self.rendition.color = Some(color);
        self.rendition.color_index = None;
        Ok(())
    }

    fn decode_ascii_color_map(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "ColorMap", self.resource, start)?;
        let count = required_usize(values.get(1), self.resource, start, "ColorMap count")?;
        if !(1..=256).contains(&count) {
            return Err(self.error_at(start, "ColorMap count must be between 1 and 256"));
        }
        let channels = collect_i64(&values[2..], self.resource, start)?;
        if channels.len() != count * 4 {
            return Err(self.error_at(
                start,
                format!(
                    "ColorMap declares {count} colors but contains {} channels",
                    channels.len()
                ),
            ));
        }
        let colors = channels
            .chunks_exact(4)
            .map(|rgba| {
                rgba.iter()
                    .map(|channel| {
                        u8::try_from(*channel).map_err(|_| {
                            self.error_at(start, "ColorMap channel is outside the byte range")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .try_into()
                    .map_err(|_| self.error_at(start, "invalid ColorMap entry"))
            })
            .collect::<Result<Vec<[u8; 4]>, DwfError>>()?;
        self.set_color_map(colors);
        Ok(())
    }

    fn set_color_map(&mut self, colors: Vec<[u8; 4]>) {
        self.current_color_map = colors.clone();
        self.color_maps.push(colors);
        if let Some(index) = self.rendition.color_index {
            self.rendition.color = self.current_color_map.get(usize::from(index)).copied();
        }
    }

    fn resolve_indexed_color(&mut self) {
        self.rendition.color = self
            .rendition
            .color_index
            .and_then(|index| self.current_color_map.get(usize::from(index)).copied());
    }

    fn decode_ascii_line_weight(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "LineWeight", self.resource, start)?;
        self.rendition.line.weight = Some(required_i32(
            values.get(1),
            self.resource,
            start,
            "LineWeight",
        )?);
        Ok(())
    }

    fn decode_ascii_line_pattern(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "LinePattern", self.resource, start)?;
        self.rendition.line.pattern = Some(required_text(
            values.get(1),
            self.resource,
            start,
            "LinePattern",
        )?);
        Ok(())
    }

    fn decode_ascii_line_style(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "LineStyle", self.resource, start)?;
        for option in values.iter().skip(1).filter_map(Node::list) {
            let Some(name) = option.first().and_then(Node::text) else {
                continue;
            };
            let value = option.get(1).and_then(Node::text);
            match name {
                "AdaptPatterns" => self.rendition.line.adapt_patterns = value.and_then(parse_bool),
                "LinePatternScale" => {
                    self.rendition.line.pattern_scale = value.and_then(|value| value.parse().ok())
                }
                "LineStartCap" => self.rendition.line.line_start_cap = value.map(str::to_owned),
                "LineEndCap" => self.rendition.line.line_end_cap = value.map(str::to_owned),
                "DashStartCap" => self.rendition.line.dash_start_cap = value.map(str::to_owned),
                "DashEndCap" => self.rendition.line.dash_end_cap = value.map(str::to_owned),
                "LineJoin" => self.rendition.line.line_join = value.map(str::to_owned),
                "MiterAngle" => {
                    self.rendition.line.miter_angle = value.and_then(|value| value.parse().ok())
                }
                "MiterLength" => {
                    self.rendition.line.miter_length = value.and_then(|value| value.parse().ok())
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn decode_ascii_fill_pattern(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "FillPattern", self.resource, start)?;
        self.rendition.fill_pattern = values.get(1).and_then(Node::text).map(str::to_owned);
        Ok(())
    }

    fn decode_ascii_font(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Font", self.resource, start)?;
        for option in values.iter().skip(1).filter_map(Node::list) {
            let Some(name) = option.first().and_then(Node::text) else {
                continue;
            };
            let value = option.get(1).and_then(Node::text);
            match name {
                "Name" => self.rendition.font.name = value.map(str::to_owned),
                "Charset" => {
                    self.rendition.font.charset = value.and_then(|value| value.parse().ok())
                }
                "Pitch" => self.rendition.font.pitch = value.and_then(|value| value.parse().ok()),
                "Family" => self.rendition.font.family = value.and_then(|value| value.parse().ok()),
                "Style" => {
                    let flags = option
                        .iter()
                        .skip(1)
                        .filter_map(Node::text)
                        .collect::<Vec<_>>();
                    self.rendition.font.bold = Some(flags.contains(&"bold"));
                    self.rendition.font.italic = Some(flags.contains(&"italic"));
                    self.rendition.font.underlined = Some(flags.contains(&"underlined"));
                }
                "Height" => self.rendition.font.height = value.and_then(|value| value.parse().ok()),
                "Rotation" => {
                    self.rendition.font.rotation = value.and_then(|value| value.parse().ok())
                }
                "Widthscale" => {
                    self.rendition.font.width_scale = value.and_then(|value| value.parse().ok())
                }
                "Spacing" => {
                    self.rendition.font.spacing = value.and_then(|value| value.parse().ok())
                }
                "Oblique" => {
                    self.rendition.font.oblique = value.and_then(|value| value.parse().ok())
                }
                "Flags" => self.rendition.font.flags = value.and_then(|value| value.parse().ok()),
                _ => {}
            }
        }
        Ok(())
    }

    fn decode_ascii_font_extension(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "FontExtension", self.resource, start)?;
        if let Some(name) = values.get(1).and_then(Node::text) {
            self.rendition.font.name = Some(name.to_owned());
        }
        self.rendition.font.canonical_name = values.get(2).and_then(Node::text).map(str::to_owned);
        Ok(())
    }

    fn decode_ascii_viewport(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Viewport", self.resource, start)?;
        let name = values.get(1).and_then(Node::text).unwrap_or("").to_owned();
        let mut contours = Vec::new();
        let mut units = None;
        for child in values.iter().skip(2) {
            let Some(child_values) = child.list() else {
                continue;
            };
            match child_values.first().and_then(Node::text) {
                Some("Contour") => {
                    contours = parse_contours(child, self.resource, start, self.options)?;
                }
                Some("Units") => units = Some(parse_units(child, self.resource, start)?),
                _ => {}
            }
        }
        let viewport_points = contours.iter().map(Vec::len).sum();
        self.account_points(viewport_points)?;
        self.rendition.viewport = Some(name.clone());
        self.viewports.push(W2dViewport {
            name,
            contours,
            units,
        });
        Ok(())
    }

    fn decode_ascii_line_record(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Line", self.resource, start)?;
        let points = parse_points_from_nodes(&values[1..], self.resource, start)?;
        if points.len() != 2 {
            return Err(self.error_at(start, "Line requires exactly two points"));
        }
        self.emit(
            W2dGeometry::Line {
                start: points[0],
                end: points[1],
            },
            start,
            raw.len(),
            "(Line",
        )
    }

    fn decode_ascii_point_record(
        &mut self,
        raw: &[u8],
        start: usize,
        polygon: bool,
    ) -> Result<(), DwfError> {
        let expected = if polygon { "Polygon" } else { "Polyline" };
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, expected, self.resource, start)?;
        let count = required_usize(values.get(1), self.resource, start, "point count")?;
        self.check_entity_points(count, start)?;
        let points = parse_points_from_nodes(&values[2..], self.resource, start)?;
        if points.len() != count {
            return Err(self.error_at(
                start,
                format!(
                    "{expected} declares {count} points but contains {}",
                    points.len()
                ),
            ));
        }
        let geometry = if polygon {
            W2dGeometry::Polygon { points }
        } else {
            W2dGeometry::Polyline { points }
        };
        self.emit(geometry, start, raw.len(), &format!("({expected}"))
    }

    fn decode_ascii_bezier(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Bezier", self.resource, start)?;
        let curve_count = required_usize(values.get(1), self.resource, start, "Bezier count")?;
        let point_count = curve_count
            .checked_mul(3)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| self.error_at(start, "Bezier point count overflow"))?;
        self.check_entity_points(point_count, start)?;
        let points = parse_points_from_nodes(&values[2..], self.resource, start)?;
        if points.len() != point_count {
            return Err(self.error_at(
                start,
                format!(
                    "Bezier declares {curve_count} curves ({point_count} points) but contains {} points",
                    points.len()
                ),
            ));
        }
        self.emit(
            W2dGeometry::PolyBezier { points },
            start,
            raw.len(),
            "(Bezier",
        )
    }

    fn decode_ascii_circle(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Circle", self.resource, start)?;
        let numbers = collect_i64(&values[1..], self.resource, start)?;
        if numbers.len() != 3 && numbers.len() != 5 {
            return Err(self.error_at(start, "Circle requires 3 or 5 integer operands"));
        }
        let radius = numbers[2];
        if radius < 0 {
            return Err(self.error_at(start, "Circle radius cannot be negative"));
        }
        let (start_angle, end_angle) = if numbers.len() == 5 {
            (
                angle(numbers[3], self.resource, start)?,
                angle(numbers[4], self.resource, start)?,
            )
        } else {
            (0, 65_536)
        };
        self.emit(
            W2dGeometry::Circle {
                center: W2dPoint {
                    x: numbers[0],
                    y: numbers[1],
                },
                radius,
                start_angle,
                end_angle,
            },
            start,
            raw.len(),
            "(Circle",
        )
    }

    fn decode_ascii_ellipse(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Ellipse", self.resource, start)?;
        let numbers = collect_i64(&values[1..], self.resource, start)?;
        if numbers.len() != 7 {
            return Err(self.error_at(start, "Ellipse requires seven integer operands"));
        }
        if numbers[2] < 0 || numbers[3] < 0 {
            return Err(self.error_at(start, "Ellipse axes cannot be negative"));
        }
        self.emit(
            W2dGeometry::Ellipse {
                center: W2dPoint {
                    x: numbers[0],
                    y: numbers[1],
                },
                major: numbers[2],
                minor: numbers[3],
                start_angle: angle(numbers[4], self.resource, start)?,
                end_angle: angle(numbers[5], self.resource, start)?,
                tilt: angle(numbers[6], self.resource, start)?,
            },
            start,
            raw.len(),
            "(Ellipse",
        )
    }

    fn decode_ascii_text(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Text", self.resource, start)?;
        let x = required_i64(values.get(1), self.resource, start, "Text x")?;
        let y = required_i64(values.get(2), self.resource, start, "Text y")?;
        let text = required_text(values.get(3), self.resource, start, "Text string")?;
        let mut bounds = None;
        for option in values.iter().skip(4).filter_map(Node::list) {
            if option.first().and_then(Node::text) == Some("Bounds") {
                let points = parse_points_from_nodes(&option[1..], self.resource, start)?;
                bounds = Some(points.try_into().map_err(|points: Vec<W2dPoint>| {
                    self.error_at(
                        start,
                        format!("Text Bounds requires four points, got {}", points.len()),
                    )
                })?);
            }
        }
        self.emit(
            W2dGeometry::Text {
                position: W2dPoint { x, y },
                text,
                bounds,
            },
            start,
            raw.len(),
            "(Text",
        )
    }

    fn decode_ascii_contour(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let contours = parse_contours(&parsed, self.resource, start, self.options)?;
        self.emit(
            W2dGeometry::ContourSet { contours },
            start,
            raw.len(),
            "(Contour",
        )
    }

    fn decode_ascii_gouraud(
        &mut self,
        raw: &[u8],
        start: usize,
        polytriangle: bool,
    ) -> Result<(), DwfError> {
        let name = if polytriangle { "Gouraud" } else { "GourLine" };
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, name, self.resource, start)?;
        let declared = required_usize(values.get(1), self.resource, start, "Gouraud count")?;
        let point_count = if polytriangle {
            declared
                .checked_add(2)
                .ok_or_else(|| self.error_at(start, "Gouraud vertex count overflow"))?
        } else {
            declared
        };
        self.check_entity_points(point_count, start)?;
        let operands = collect_i64(&values[2..], self.resource, start)?;
        if operands.len() != point_count.saturating_mul(6) {
            return Err(self.error_at(
                start,
                format!(
                    "{name} requires {point_count} point/color tuples but contains {} scalar operands",
                    operands.len()
                ),
            ));
        }
        let points = operands
            .chunks_exact(6)
            .map(|values| {
                Ok(W2dColoredPoint {
                    point: W2dPoint {
                        x: values[0],
                        y: values[1],
                    },
                    color: [
                        color_channel(values[2], self.resource, start)?,
                        color_channel(values[3], self.resource, start)?,
                        color_channel(values[4], self.resource, start)?,
                        color_channel(values[5], self.resource, start)?,
                    ],
                })
            })
            .collect::<Result<Vec<_>, DwfError>>()?;
        let geometry = if polytriangle {
            W2dGeometry::GouraudPolytriangle { points }
        } else {
            W2dGeometry::GouraudPolyline { points }
        };
        self.emit(geometry, start, raw.len(), &format!("({name}"))
    }

    fn decode_ascii_texture(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "Texture", self.resource, start)?;
        let count = required_usize(values.get(1), self.resource, start, "Texture count")?;
        self.check_entity_points(count, start)?;
        let points = parse_points_from_nodes(&values[2..], self.resource, start)?;
        if points.len() != count {
            return Err(self.error_at(
                start,
                format!(
                    "Texture declares {count} points but contains {}",
                    points.len()
                ),
            ));
        }
        self.emit(
            W2dGeometry::TexturedPolytriangle { points },
            start,
            raw.len(),
            "(Texture",
        )
    }

    fn decode_ascii_image(&mut self, raw: &[u8], start: usize, name: &str) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, name, self.resource, start)?;
        let format = required_text(values.get(1), self.resource, start, "Image format")?;
        let identifier = required_i32(values.get(2), self.resource, start, "Image identifier")?;
        let columns = required_u16(values.get(3), self.resource, start, "Image columns")?;
        let rows = required_u16(values.get(4), self.resource, start, "Image rows")?;
        let min = W2dPoint {
            x: required_i64(values.get(5), self.resource, start, "Image minimum x")?,
            y: required_i64(values.get(6), self.resource, start, "Image minimum y")?,
        };
        let max = W2dPoint {
            x: required_i64(values.get(7), self.resource, start, "Image maximum x")?,
            y: required_i64(values.get(8), self.resource, start, "Image maximum y")?,
        };
        let mut color_map = Vec::new();
        let mut data = None;
        for child in values.iter().skip(9) {
            let Some(items) = child.list() else {
                continue;
            };
            if items.first().and_then(Node::text) == Some("ColorMap")
                || items.first().and_then(Node::text) == Some("Colormap")
            {
                color_map = parse_ascii_color_map_nodes(items, self.resource, start)?;
            } else if let Some(size_node) = items.first() {
                let size =
                    required_usize(Some(size_node), self.resource, start, "Image data size")?;
                let bytes = parse_hex_bytes(&items[1..], self.resource, start)?;
                if bytes.len() != size {
                    return Err(self.error_at(
                        start,
                        format!(
                            "Image declares {size} data bytes but contains {}",
                            bytes.len()
                        ),
                    ));
                }
                data = Some(bytes);
            }
        }
        let data = data.ok_or_else(|| self.error_at(start, "Image data operand is missing"))?;
        if color_map.is_empty() && normalize_image_format(&format) == "indexed" {
            color_map = self.current_color_map.clone();
        }
        self.emit(
            W2dGeometry::Image {
                image: W2dImage {
                    format: normalize_image_format(&format),
                    identifier,
                    columns,
                    rows,
                    min,
                    max,
                    color_map,
                    data,
                },
            },
            start,
            raw.len(),
            &format!("({name}"),
        )
    }

    fn decode_ascii_embedded_font(
        &mut self,
        raw: &[u8],
        start: usize,
        name: &str,
    ) -> Result<(), DwfError> {
        let mut position = 1;
        let opcode = self.read_embedded_ascii_token(raw, &mut position, start, "opcode")?;
        if opcode != name.as_bytes() {
            return Err(self.error_at(start, "embedded font opcode name does not match"));
        }
        let request = u32::try_from(self.read_embedded_ascii_uint(
            raw,
            &mut position,
            start,
            "font request",
        )?)
        .map_err(|_| self.error_at(start, "font request is outside the 32-bit range"))?;
        let privilege = u8::try_from(self.read_embedded_ascii_uint(
            raw,
            &mut position,
            start,
            "font privilege",
        )?)
        .map_err(|_| self.error_at(start, "font privilege is outside the byte range"))?;
        let charset = u8::try_from(self.read_embedded_ascii_uint(
            raw,
            &mut position,
            start,
            "font charset",
        )?)
        .map_err(|_| self.error_at(start, "font charset is outside the byte range"))?;
        let typeface_size = usize::try_from(self.read_embedded_ascii_uint(
            raw,
            &mut position,
            start,
            "typeface size",
        )?)
        .map_err(|_| self.error_at(start, "typeface size exceeds addressable memory"))?;
        let typeface_name = self.read_embedded_ascii_string(
            raw,
            &mut position,
            start,
            typeface_size,
            "typeface name",
        )?;
        let logfont_size = usize::try_from(self.read_embedded_ascii_uint(
            raw,
            &mut position,
            start,
            "LOGFONT size",
        )?)
        .map_err(|_| self.error_at(start, "LOGFONT size exceeds addressable memory"))?;
        let logfont_name = self.read_embedded_ascii_string(
            raw,
            &mut position,
            start,
            logfont_size,
            "LOGFONT name",
        )?;
        let data = self.read_embedded_ascii_data(raw, &mut position, start)?;
        self.embedded_fonts.push(W2dEmbeddedFont {
            request,
            privilege,
            charset,
            typeface_name,
            logfont_name,
            data,
            source: self.source_span(start, raw.len(), &format!("({name}")),
        });
        Ok(())
    }

    fn read_embedded_ascii_token<'b>(
        &self,
        raw: &'b [u8],
        position: &mut usize,
        start: usize,
        label: &str,
    ) -> Result<&'b [u8], DwfError> {
        while raw
            .get(*position)
            .is_some_and(|byte| is_w2d_whitespace(*byte))
        {
            *position += 1;
        }
        let token_start = *position;
        while raw
            .get(*position)
            .is_some_and(|byte| !is_w2d_whitespace(*byte) && !matches!(*byte, b'(' | b')'))
        {
            *position += 1;
        }
        if token_start == *position {
            return Err(self.error_at(start.saturating_add(*position), format!("missing {label}")));
        }
        Ok(&raw[token_start..*position])
    }

    fn read_embedded_ascii_uint(
        &self,
        raw: &[u8],
        position: &mut usize,
        start: usize,
        label: &str,
    ) -> Result<u64, DwfError> {
        let token = self.read_embedded_ascii_token(raw, position, start, label)?;
        std::str::from_utf8(token)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| self.error_at(start, format!("{label} must be an unsigned integer")))
    }

    fn read_embedded_ascii_string(
        &self,
        raw: &[u8],
        position: &mut usize,
        start: usize,
        size: usize,
        label: &str,
    ) -> Result<String, DwfError> {
        if size > self.options.max_w2d_string_size {
            return Err(DwfError::W2dStringLimitExceeded {
                resource: self.resource.to_owned(),
                offset: start,
                limit: self.options.max_w2d_string_size,
            });
        }
        while raw
            .get(*position)
            .is_some_and(|byte| is_w2d_whitespace(*byte))
        {
            *position += 1;
        }
        let end = position
            .checked_add(size)
            .ok_or_else(|| self.error_at(start, format!("{label} length overflow")))?;
        let bytes = raw
            .get(*position..end)
            .ok_or_else(|| self.error_at(start, format!("truncated {label}")))?;
        let value = std::str::from_utf8(bytes)
            .map_err(|_| self.error_at(start, format!("{label} is not UTF-8")))?
            .to_owned();
        *position = end;
        Ok(value)
    }

    fn read_embedded_ascii_data(
        &self,
        raw: &[u8],
        position: &mut usize,
        start: usize,
    ) -> Result<Vec<u8>, DwfError> {
        while raw
            .get(*position)
            .is_some_and(|byte| is_w2d_whitespace(*byte))
        {
            *position += 1;
        }
        if raw.get(*position) != Some(&b'(') {
            return Err(self.error_at(start, "embedded font data operand is missing"));
        }
        *position += 1;
        let data_size = usize::try_from(self.read_embedded_ascii_uint(
            raw,
            position,
            start,
            "font data size",
        )?)
        .map_err(|_| self.error_at(start, "font data size exceeds addressable memory"))?;
        let hex_size = data_size
            .checked_mul(2)
            .ok_or_else(|| self.error_at(start, "font data size overflow"))?;
        let mut nibbles = Vec::with_capacity(hex_size.min(raw.len()));
        loop {
            let Some(byte) = raw.get(*position).copied() else {
                return Err(self.error_at(start, "unterminated embedded font data"));
            };
            *position += 1;
            if is_w2d_whitespace(byte) {
                continue;
            }
            if byte == b')' {
                break;
            }
            let nibble = hex_nibble(byte)
                .ok_or_else(|| self.error_at(start, "embedded font data is not hexadecimal"))?;
            nibbles.push(nibble);
            if nibbles.len() > hex_size {
                return Err(
                    self.error_at(start, "embedded font data length exceeds its declaration")
                );
            }
        }
        if nibbles.len() != hex_size {
            return Err(self.error_at(
                start,
                "embedded font data length does not match its declaration",
            ));
        }
        while raw
            .get(*position)
            .is_some_and(|byte| is_w2d_whitespace(*byte))
        {
            *position += 1;
        }
        if raw.get(*position) != Some(&b')') || position.saturating_add(1) != raw.len() {
            return Err(self.error_at(start, "invalid embedded font record terminator"));
        }
        *position += 1;
        Ok(nibbles
            .chunks_exact(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect())
    }

    fn decode_ascii_block_ref(&mut self, raw: &[u8], start: usize) -> Result<(), DwfError> {
        let parsed = parse_record(raw, self.resource, start, self.options)?;
        let values = root_values(&parsed, "BlockRef", self.resource, start)?;
        let format = values.get(1).and_then(Node::text).unwrap_or("").to_owned();
        self.block_refs.push(W2dBlockRef {
            format,
            payload: raw.to_vec(),
            source: self.source_span(start, raw.len(), "(BlockRef"),
        });
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn decode_single_byte(&mut self, opcode: u8, start: usize) -> Result<(), DwfError> {
        match opcode {
            b'V' => self.rendition.visibility = true,
            b'v' => self.rendition.visibility = false,
            b'F' => self.rendition.fill = true,
            b'f' => self.rendition.fill = false,
            0x03 => {
                self.rendition.color = Some([
                    self.read_u8()?,
                    self.read_u8()?,
                    self.read_u8()?,
                    self.read_u8()?,
                ]);
                self.rendition.color_index = None;
            }
            b'C' => {
                let index = self.read_ascii_i64()?;
                self.rendition.color_index = Some(u16::try_from(index).map_err(|_| {
                    self.error_at(start, "indexed color is outside the 16-bit range")
                })?);
                self.resolve_indexed_color();
            }
            b'c' => {
                self.rendition.color_index = Some(u16::from(self.read_u8()?));
                self.resolve_indexed_color();
            }
            0x06 => self.decode_binary_font(start)?,
            0x17 => self.rendition.line.weight = Some(self.read_i32()?),
            0xAC => {
                let number = self.read_count(start)?;
                let number = i32::try_from(number)
                    .map_err(|_| self.error_at(start, "Layer number exceeds 32-bit range"))?;
                self.rendition.layer = Some(
                    self.layer_definitions
                        .get(&number)
                        .cloned()
                        .unwrap_or(W2dLayer { number, name: None }),
                );
            }
            0xCC => {
                let pattern = self.read_count(start)?;
                self.rendition.line.pattern = Some(line_pattern_name(pattern));
            }
            b'O' => {
                self.current_point = W2dPoint {
                    x: i64::from(self.read_i32()?),
                    y: i64::from(self.read_i32()?),
                };
            }
            0x0C => {
                let points = self.read_relative_points_i16(2, start)?;
                self.emit(
                    W2dGeometry::Line {
                        start: points[0],
                        end: points[1],
                    },
                    start,
                    self.position - start,
                    "0x0C",
                )?;
            }
            b'l' => {
                let points = self.read_relative_points_i32(2, start)?;
                self.emit(
                    W2dGeometry::Line {
                        start: points[0],
                        end: points[1],
                    },
                    start,
                    self.position - start,
                    "l",
                )?;
            }
            0x8C => {
                let count = self.read_count(start)?;
                let point_count = count
                    .checked_mul(2)
                    .ok_or_else(|| self.error_at(start, "line point count overflow"))?;
                let points = self.read_relative_points_i16(point_count, start)?;
                for pair in points.chunks_exact(2) {
                    self.emit(
                        W2dGeometry::Line {
                            start: pair[0],
                            end: pair[1],
                        },
                        start,
                        self.position - start,
                        "0x8C",
                    )?;
                }
            }
            b'L' => {
                let first = self.read_ascii_point()?;
                let second = self.read_ascii_point()?;
                self.emit(
                    W2dGeometry::Line {
                        start: first,
                        end: second,
                    },
                    start,
                    self.position - start,
                    "L",
                )?;
            }
            0x10 | 0x14 => {
                let count = self.read_count(start)?;
                let points = self.read_relative_points_i16(count, start)?;
                let geometry = if opcode == 0x14 {
                    W2dGeometry::Polytriangle { points }
                } else if self.rendition.fill {
                    W2dGeometry::Polygon { points }
                } else {
                    W2dGeometry::Polyline { points }
                };
                self.emit(
                    geometry,
                    start,
                    self.position - start,
                    if opcode == 0x14 { "0x14" } else { "0x10" },
                )?;
            }
            b'p' | b't' => {
                let count = self.read_count(start)?;
                let points = self.read_relative_points_i32(count, start)?;
                let geometry = if opcode == b't' {
                    W2dGeometry::Polytriangle { points }
                } else if self.rendition.fill {
                    W2dGeometry::Polygon { points }
                } else {
                    W2dGeometry::Polyline { points }
                };
                self.emit(
                    geometry,
                    start,
                    self.position - start,
                    if opcode == b't' { "t" } else { "p" },
                )?;
            }
            b'P' | b'T' => {
                let count = self.read_ascii_usize(start)?;
                self.check_entity_points(count, start)?;
                let mut points = Vec::with_capacity(count);
                for _ in 0..count {
                    points.push(self.read_ascii_point()?);
                }
                let geometry = if opcode == b'T' {
                    W2dGeometry::Polytriangle { points }
                } else if self.rendition.fill {
                    W2dGeometry::Polygon { points }
                } else {
                    W2dGeometry::Polyline { points }
                };
                self.emit(
                    geometry,
                    start,
                    self.position - start,
                    if opcode == b'T' { "T" } else { "P" },
                )?;
            }
            b'b' | 0x02 => {
                let curve_count = self.read_count(start)?;
                let point_count = curve_count
                    .checked_mul(3)
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| self.error_at(start, "Bezier point count overflow"))?;
                let points = if opcode == b'b' {
                    self.read_relative_points_i32(point_count, start)?
                } else {
                    self.read_relative_points_i16(point_count, start)?
                };
                self.emit(
                    W2dGeometry::PolyBezier { points },
                    start,
                    self.position - start,
                    if opcode == b'b' { "b" } else { "0x02" },
                )?;
            }
            b'B' => {
                return Err(DwfError::UnsupportedW2dOpcode {
                    resource: self.resource.to_owned(),
                    offset: start,
                    opcode: "B".to_owned(),
                });
            }
            0x12 => {
                let center = self.read_relative_point_i16()?;
                let radius = i64::from(self.read_u16()?);
                self.emit(
                    W2dGeometry::Circle {
                        center,
                        radius,
                        start_angle: 0,
                        end_angle: 65_536,
                    },
                    start,
                    self.position - start,
                    "0x12",
                )?;
            }
            b'r' => {
                let center = self.read_relative_point_i32()?;
                let radius = i64::from(self.read_u32()?);
                self.emit(
                    W2dGeometry::Circle {
                        center,
                        radius,
                        start_angle: 0,
                        end_angle: 65_536,
                    },
                    start,
                    self.position - start,
                    "r",
                )?;
            }
            0x92 => {
                let center = self.read_relative_point_i32()?;
                let radius = i64::from(self.read_u32()?);
                let start_angle = u32::from(self.read_u16()?);
                let end_angle = u32::from(self.read_u16()?);
                self.emit(
                    W2dGeometry::Circle {
                        center,
                        radius,
                        start_angle,
                        end_angle,
                    },
                    start,
                    self.position - start,
                    "0x92",
                )?;
            }
            b'R' => {
                let center = self.read_ascii_point()?;
                let radius = self.read_ascii_i64()?;
                if radius < 0 {
                    return Err(self.error_at(start, "Circle radius cannot be negative"));
                }
                self.emit(
                    W2dGeometry::Circle {
                        center,
                        radius,
                        start_angle: 0,
                        end_angle: 65_536,
                    },
                    start,
                    self.position - start,
                    "R",
                )?;
            }
            b'e' => {
                let center = self.read_relative_point_i32()?;
                let major = i64::from(self.read_u32()?);
                let minor = i64::from(self.read_u32()?);
                let start_angle = u32::from(self.read_u16()?);
                let end_angle = u32::from(self.read_u16()?);
                let tilt = u32::from(self.read_u16()?);
                self.emit(
                    W2dGeometry::Ellipse {
                        center,
                        major,
                        minor,
                        start_angle,
                        end_angle,
                        tilt,
                    },
                    start,
                    self.position - start,
                    "e",
                )?;
            }
            b'E' => {
                let center = self.read_ascii_point()?;
                let axes = self.read_ascii_point()?;
                if axes.x < 0 || axes.y < 0 {
                    return Err(self.error_at(start, "Ellipse axes cannot be negative"));
                }
                self.emit(
                    W2dGeometry::Ellipse {
                        center,
                        major: axes.x,
                        minor: axes.y,
                        start_angle: 0,
                        end_angle: 65_536,
                        tilt: 0,
                    },
                    start,
                    self.position - start,
                    "E",
                )?;
            }
            b'x' => {
                let position = self.read_relative_point_i32()?;
                let text = self.read_w2d_string(start)?;
                self.emit(
                    W2dGeometry::Text {
                        position,
                        text,
                        bounds: None,
                    },
                    start,
                    self.position - start,
                    "x",
                )?;
            }
            0x18 => self.decode_advanced_binary_text(start)?,
            0x07 | b'g' => self.decode_binary_gouraud(opcode, start, true)?,
            0x11 | b'q' => self.decode_binary_gouraud(opcode, start, false)?,
            0x0B | b'k' => self.decode_binary_contour(opcode, start)?,
            b'M' => self.skip_ascii_point_set("M", start)?,
            b'm' | 0x8D => self.skip_binary_point_set(opcode, start)?,
            b'G' | b'S' => {
                let _ = self.read_ascii_i64()?;
            }
            b's' => {
                let _ = self.read_u32()?;
            }
            0x87 => {
                let _ = self.read_u16()?;
            }
            b'N' => {
                let _ = self.read_u32()?;
            }
            b'n' => {
                let _ = self.read_i16()?;
            }
            0x0E => {}
            _ => {
                return Err(DwfError::UnsupportedW2dOpcode {
                    resource: self.resource.to_owned(),
                    offset: start,
                    opcode: printable_opcode(opcode),
                });
            }
        }
        Ok(())
    }

    fn decode_binary_font(&mut self, start: usize) -> Result<(), DwfError> {
        let fields = self.read_u16()?;
        if fields & 0x0001 != 0 {
            self.rendition.font.name = Some(self.read_w2d_string(start)?);
        }
        if fields & 0x0002 != 0 {
            self.rendition.font.charset = Some(self.read_u8()?);
        }
        if fields & 0x0004 != 0 {
            self.rendition.font.pitch = Some(self.read_u8()?);
        }
        if fields & 0x0008 != 0 {
            self.rendition.font.family = Some(self.read_u8()?);
        }
        if fields & 0x0010 != 0 {
            let style = self.read_u8()?;
            self.rendition.font.bold = Some(style & 0x01 != 0);
            self.rendition.font.italic = Some(style & 0x02 != 0);
            self.rendition.font.underlined = Some(style & 0x04 != 0);
        }
        if fields & 0x0020 != 0 {
            self.rendition.font.height = Some(self.read_i32()?);
        }
        if fields & 0x0040 != 0 {
            self.rendition.font.rotation = Some(self.read_u16()?);
        }
        if fields & 0x0080 != 0 {
            self.rendition.font.width_scale = Some(self.read_u16()?);
        }
        if fields & 0x0100 != 0 {
            self.rendition.font.spacing = Some(self.read_u16()?);
        }
        if fields & 0x0200 != 0 {
            self.rendition.font.oblique = Some(self.read_u16()?);
        }
        if fields & 0x0400 != 0 {
            self.rendition.font.flags = Some(self.read_u32()?);
        }
        Ok(())
    }

    fn decode_advanced_binary_text(&mut self, start: usize) -> Result<(), DwfError> {
        let position = self.read_relative_point_i32()?;
        let text = self.read_w2d_string(start)?;
        self.skip_count_vector(start)?;
        self.skip_count_vector(start)?;
        let mut bounds = [W2dPoint::default(); 4];
        for point in &mut bounds {
            *point = self.read_relative_point_i32()?;
        }
        self.skip_count_vector(start)?;
        self.emit(
            W2dGeometry::Text {
                position,
                text,
                bounds: Some(bounds),
            },
            start,
            self.position - start,
            "0x18",
        )
    }

    fn skip_count_vector(&mut self, start: usize) -> Result<(), DwfError> {
        let encoded_count = self.read_count(start)?;
        let count = encoded_count
            .checked_sub(1)
            .ok_or_else(|| self.error_at(start, "text option count cannot be zero"))?;
        for _ in 0..count {
            let value = self.read_count(start)?;
            if value == 0 {
                return Err(self.error_at(start, "text option position cannot be zero"));
            }
        }
        Ok(())
    }

    fn decode_binary_gouraud(
        &mut self,
        opcode: u8,
        start: usize,
        polytriangle: bool,
    ) -> Result<(), DwfError> {
        let declared = self.read_count(start)?;
        let point_count = if polytriangle {
            declared
                .checked_add(2)
                .ok_or_else(|| self.error_at(start, "Gouraud vertex count overflow"))?
        } else {
            declared
        };
        self.check_entity_points(point_count, start)?;
        let mut points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            let point = if matches!(opcode, 0x07 | 0x11) {
                self.read_relative_point_i16()?
            } else {
                self.read_relative_point_i32()?
            };
            points.push(W2dColoredPoint {
                point,
                color: self.read_exact()?,
            });
        }
        let geometry = if polytriangle {
            W2dGeometry::GouraudPolytriangle { points }
        } else {
            W2dGeometry::GouraudPolyline { points }
        };
        self.emit(
            geometry,
            start,
            self.position - start,
            &printable_opcode(opcode),
        )
    }

    fn decode_binary_contour(&mut self, opcode: u8, start: usize) -> Result<(), DwfError> {
        let contour_count = self.read_count(start)?;
        self.check_entity_points(contour_count, start)?;
        let mut counts = Vec::with_capacity(contour_count);
        let mut total = 0_usize;
        for _ in 0..contour_count {
            let count = self.read_count(start)?;
            counts.push(count);
            total = total
                .checked_add(count)
                .ok_or_else(|| self.error_at(start, "contour point count overflow"))?;
        }
        self.check_entity_points(total, start)?;
        let points = if opcode == 0x0B {
            self.read_relative_points_i16(total, start)?
        } else {
            self.read_relative_points_i32(total, start)?
        };
        let mut point_iter = points.into_iter();
        let contours = counts
            .into_iter()
            .map(|count| point_iter.by_ref().take(count).collect())
            .collect();
        self.emit(
            W2dGeometry::ContourSet { contours },
            start,
            self.position - start,
            &printable_opcode(opcode),
        )
    }

    /// `M` draw-polymarker (ASCII): a marker glyph at each absolute point.
    fn skip_ascii_point_set(&mut self, opcode: &str, start: usize) -> Result<(), DwfError> {
        let count = self.read_ascii_usize(start)?;
        self.check_entity_points(count, start)?;
        let mut points = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            points.push(self.read_ascii_point()?);
        }
        if points.is_empty() {
            return Ok(());
        }
        self.emit(
            W2dGeometry::Polymarker { points },
            start,
            self.position - start,
            opcode,
        )
    }

    /// `m` (32-bit) / `0x8D` (16-bit relative) draw-polymarker: markers at each
    /// point, updating the current point like the other relative drawables.
    fn skip_binary_point_set(&mut self, opcode: u8, start: usize) -> Result<(), DwfError> {
        let count = self.read_count(start)?;
        self.check_entity_points(count, start)?;
        let mut points = Vec::with_capacity(count.min(4096));
        if opcode == 0x8D {
            for _ in 0..count {
                points.push(self.read_relative_point_i16()?);
            }
        } else {
            for _ in 0..count {
                points.push(self.read_relative_point_i32()?);
            }
        }
        if points.is_empty() {
            return Ok(());
        }
        let opcode_label = printable_opcode(opcode);
        self.emit(
            W2dGeometry::Polymarker { points },
            start,
            self.position - start,
            &opcode_label,
        )
    }

    fn emit(
        &mut self,
        geometry: W2dGeometry,
        offset: usize,
        length: usize,
        opcode: &str,
    ) -> Result<(), DwfError> {
        let points = geometry_point_count(&geometry);
        self.check_entity_points(points, offset)?;
        self.account_points(points)?;
        self.entities.push(W2dEntity {
            geometry,
            rendition: self.rendition.clone(),
            source: self.source_span(offset, length, opcode),
        });
        Ok(())
    }

    fn source_span(&self, offset: usize, length: usize, opcode: &str) -> W2dSourceSpan {
        let Some(location) = self.source_location(offset) else {
            return W2dSourceSpan {
                offset,
                length,
                opcode: opcode.to_owned(),
                decoded_offset: None,
                decoded_length: None,
                compression_depth: 0,
            };
        };
        if location.compression_depth == 0 {
            W2dSourceSpan {
                offset: location.offset,
                length,
                opcode: opcode.to_owned(),
                decoded_offset: None,
                decoded_length: None,
                compression_depth: 0,
            }
        } else {
            W2dSourceSpan {
                offset: location.offset,
                length: location.length,
                opcode: opcode.to_owned(),
                decoded_offset: location.decoded_offset,
                decoded_length: Some(length),
                compression_depth: location.compression_depth,
            }
        }
    }

    fn account_points(&mut self, count: usize) -> Result<(), DwfError> {
        self.total_points = self.total_points.checked_add(count).ok_or_else(|| {
            DwfError::W2dTotalPointLimitExceeded {
                resource: self.resource.to_owned(),
                limit: self.options.max_w2d_total_points,
            }
        })?;
        if self.total_points > self.options.max_w2d_total_points {
            return Err(DwfError::W2dTotalPointLimitExceeded {
                resource: self.resource.to_owned(),
                limit: self.options.max_w2d_total_points,
            });
        }
        Ok(())
    }

    fn check_entity_points(&self, count: usize, offset: usize) -> Result<(), DwfError> {
        if count > self.options.max_w2d_points_per_entity {
            return Err(DwfError::W2dPointLimitExceeded {
                resource: self.resource.to_owned(),
                offset,
                actual: count,
                limit: self.options.max_w2d_points_per_entity,
            });
        }
        Ok(())
    }

    fn read_exact<const N: usize>(&mut self) -> Result<[u8; N], DwfError> {
        let start = self.position;
        let end = start
            .checked_add(N)
            .ok_or_else(|| self.error_at(start, "operand length overflow"))?;
        let bytes = self
            .data
            .get(start..end)
            .ok_or_else(|| self.error_at(start, format!("truncated {N}-byte operand")))?;
        self.position = end;
        Ok(bytes.try_into().expect("fixed-size slice"))
    }

    fn read_u8(&mut self) -> Result<u8, DwfError> {
        Ok(self.read_exact::<1>()?[0])
    }

    fn read_i16(&mut self) -> Result<i16, DwfError> {
        Ok(i16::from_le_bytes(self.read_exact()?))
    }

    fn read_u16(&mut self) -> Result<u16, DwfError> {
        Ok(u16::from_le_bytes(self.read_exact()?))
    }

    fn read_i32(&mut self) -> Result<i32, DwfError> {
        Ok(i32::from_le_bytes(self.read_exact()?))
    }

    fn read_u32(&mut self) -> Result<u32, DwfError> {
        Ok(u32::from_le_bytes(self.read_exact()?))
    }

    fn read_count(&mut self, _offset: usize) -> Result<usize, DwfError> {
        let first = self.read_u8()?;
        let count = if first == 0 {
            usize::from(self.read_u16()?).saturating_add(256)
        } else {
            usize::from(first)
        };
        Ok(count)
    }

    fn read_relative_point_i16(&mut self) -> Result<W2dPoint, DwfError> {
        let dx = i64::from(self.read_i16()?);
        let dy = i64::from(self.read_i16()?);
        self.apply_delta(dx, dy)
    }

    fn read_relative_point_i32(&mut self) -> Result<W2dPoint, DwfError> {
        let dx = i64::from(self.read_i32()?);
        let dy = i64::from(self.read_i32()?);
        self.apply_delta(dx, dy)
    }

    fn apply_delta(&mut self, dx: i64, dy: i64) -> Result<W2dPoint, DwfError> {
        let offset = self.position.saturating_sub(1);
        self.current_point.x = self
            .current_point
            .x
            .checked_add(dx)
            .ok_or_else(|| self.error_at(offset, "relative x coordinate overflow"))?;
        self.current_point.y = self
            .current_point
            .y
            .checked_add(dy)
            .ok_or_else(|| self.error_at(offset, "relative y coordinate overflow"))?;
        Ok(self.current_point)
    }

    fn read_relative_points_i16(
        &mut self,
        count: usize,
        offset: usize,
    ) -> Result<Vec<W2dPoint>, DwfError> {
        self.check_entity_points(count, offset)?;
        let byte_count = count
            .checked_mul(4)
            .ok_or_else(|| self.error_at(offset, "16-bit point byte count overflow"))?;
        self.ensure_remaining(byte_count)?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            points.push(self.read_relative_point_i16()?);
        }
        Ok(points)
    }

    fn read_relative_points_i32(
        &mut self,
        count: usize,
        offset: usize,
    ) -> Result<Vec<W2dPoint>, DwfError> {
        self.check_entity_points(count, offset)?;
        let byte_count = count
            .checked_mul(8)
            .ok_or_else(|| self.error_at(offset, "32-bit point byte count overflow"))?;
        self.ensure_remaining(byte_count)?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            points.push(self.read_relative_point_i32()?);
        }
        Ok(points)
    }

    fn ensure_remaining(&self, count: usize) -> Result<(), DwfError> {
        if self
            .position
            .checked_add(count)
            .is_none_or(|end| end > self.data.len())
        {
            return Err(self.error_at(self.position, format!("truncated {count}-byte operand")));
        }
        Ok(())
    }

    fn read_ascii_i64(&mut self) -> Result<i64, DwfError> {
        self.skip_ascii_separators();
        let start = self.position;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.position += 1;
        }
        let digits_start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        if self.position == digits_start {
            return Err(self.error_at(start, "expected an ASCII integer operand"));
        }
        let text = std::str::from_utf8(&self.data[start..self.position])
            .map_err(|_| self.error_at(start, "ASCII integer is not UTF-8"))?;
        text.parse::<i64>()
            .map_err(|error| self.error_at(start, format!("invalid ASCII integer: {error}")))
    }

    fn read_ascii_usize(&mut self, offset: usize) -> Result<usize, DwfError> {
        let value = self.read_ascii_i64()?;
        let value = usize::try_from(value)
            .map_err(|_| self.error_at(offset, "point count must be non-negative"))?;
        self.check_entity_points(value, offset)?;
        Ok(value)
    }

    fn read_ascii_point(&mut self) -> Result<W2dPoint, DwfError> {
        Ok(W2dPoint {
            x: self.read_ascii_i64()?,
            y: self.read_ascii_i64()?,
        })
    }

    fn skip_ascii_separators(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| is_w2d_whitespace(byte) || byte == b',')
        {
            self.position += 1;
        }
    }

    fn read_w2d_string(&mut self, source_offset: usize) -> Result<String, DwfError> {
        self.skip_whitespace();
        let start = self.position;
        match self.peek() {
            Some(b'\'') => {
                self.position += 1;
                let mut output = Vec::new();
                let mut escaped = false;
                while let Some(byte) = self.peek() {
                    self.position += 1;
                    if escaped {
                        output.push(byte);
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'\'' {
                        return Ok(String::from_utf8_lossy(&output).into_owned());
                    } else {
                        output.push(byte);
                    }
                    self.check_string_size(output.len(), start)?;
                }
                Err(self.error_at(start, "unterminated quoted W2D string"))
            }
            Some(b'"') => {
                self.position += 1;
                let digits_start = self.position;
                while self.peek().is_some_and(|byte| byte != b'"') {
                    if !self.peek().expect("checked").is_ascii_hexdigit() {
                        return Err(
                            self.error_at(self.position, "Unicode string contains non-hex data")
                        );
                    }
                    self.position += 1;
                    self.check_string_size((self.position - digits_start) / 4, start)?;
                }
                if self.peek() != Some(b'"') {
                    return Err(self.error_at(start, "unterminated Unicode hex string"));
                }
                let digits = &self.data[digits_start..self.position];
                self.position += 1;
                if digits.len() % 4 != 0 {
                    return Err(
                        self.error_at(start, "Unicode hex string length is not divisible by four")
                    );
                }
                let units = digits
                    .chunks_exact(4)
                    .map(|digits| {
                        let digits = std::str::from_utf8(digits)
                            .map_err(|_| self.error_at(start, "Unicode string is not ASCII hex"))?;
                        u16::from_str_radix(digits, 16).map_err(|_| {
                            self.error_at(start, "Unicode string contains invalid hex")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(String::from_utf16_lossy(&units))
            }
            Some(b'{') => {
                self.position += 1;
                let character_count = usize::try_from(self.read_i32()?).map_err(|_| {
                    self.error_at(start, "binary Unicode string length cannot be negative")
                })?;
                let byte_count = character_count
                    .checked_mul(2)
                    .ok_or_else(|| self.error_at(start, "binary Unicode string length overflow"))?;
                self.check_string_size(byte_count, start)?;
                self.ensure_remaining(byte_count.saturating_add(1))?;
                let mut units = Vec::with_capacity(character_count);
                for _ in 0..character_count {
                    units.push(self.read_u16()?);
                }
                if self.read_u8()? != b'}' {
                    return Err(self.error_at(start, "binary Unicode string has no closing brace"));
                }
                Ok(String::from_utf16_lossy(&units))
            }
            Some(_) => {
                while self
                    .peek()
                    .is_some_and(|byte| !is_w2d_whitespace(byte) && !matches!(byte, b'(' | b')'))
                {
                    self.position += 1;
                    self.check_string_size(self.position - start, start)?;
                }
                if self.position == start {
                    return Err(self.error_at(source_offset, "expected a W2D string operand"));
                }
                Ok(String::from_utf8_lossy(&self.data[start..self.position]).into_owned())
            }
            None => Err(self.error_at(source_offset, "missing W2D string operand")),
        }
    }

    fn check_string_size(&self, size: usize, offset: usize) -> Result<(), DwfError> {
        if size > self.options.max_w2d_string_size {
            return Err(DwfError::W2dStringLimitExceeded {
                resource: self.resource.to_owned(),
                offset,
                limit: self.options.max_w2d_string_size,
            });
        }
        Ok(())
    }

    fn read_u32_at(&self, offset: usize) -> Result<u32, DwfError> {
        let bytes = self
            .data
            .get(offset..offset.saturating_add(4))
            .ok_or_else(|| self.error_at(offset, "truncated 32-bit operand"))?;
        Ok(u32::from_le_bytes(
            bytes.try_into().expect("four-byte slice"),
        ))
    }
}

fn root_values<'a>(
    node: &'a Node,
    expected_name: &str,
    resource: &str,
    offset: usize,
) -> Result<&'a [Node], DwfError> {
    let values = node.list().ok_or_else(|| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: "extended ASCII record is not a list".to_owned(),
    })?;
    if values.first().and_then(Node::text) != Some(expected_name) {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: format!("expected ({expected_name} ...) record"),
        });
    }
    Ok(values)
}

fn extended_name(raw: &[u8]) -> Option<&str> {
    let start = raw
        .get(1..)?
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())?
        .saturating_add(1);
    let end = raw[start..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b')')
        .map(|length| start + length)
        .unwrap_or(raw.len());
    std::str::from_utf8(&raw[start..end]).ok()
}

fn required_text(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<String, DwfError> {
    node.and_then(Node::text)
        .map(str::to_owned)
        .ok_or_else(|| DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: format!("{label} is missing or is not a scalar"),
        })
}

fn required_i64(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<i64, DwfError> {
    let value = required_text(node, resource, offset, label)?;
    value.parse::<i64>().map_err(|error| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("invalid {label} {value:?}: {error}"),
    })
}

fn required_i32(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<i32, DwfError> {
    let value = required_i64(node, resource, offset, label)?;
    i32::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("{label} is outside the 32-bit range"),
    })
}

fn required_u8(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<u8, DwfError> {
    let value = required_i64(node, resource, offset, label)?;
    u8::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("{label} is outside the byte range"),
    })
}

fn required_u16(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<u16, DwfError> {
    let value = required_i64(node, resource, offset, label)?;
    u16::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("{label} is outside the 16-bit unsigned range"),
    })
}

fn color_channel(value: i64, resource: &str, offset: usize) -> Result<u8, DwfError> {
    u8::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("color channel {value} is outside the byte range"),
    })
}

fn required_usize(
    node: Option<&Node>,
    resource: &str,
    offset: usize,
    label: &str,
) -> Result<usize, DwfError> {
    let value = required_i64(node, resource, offset, label)?;
    usize::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("{label} must be non-negative"),
    })
}

fn collect_i64(nodes: &[Node], resource: &str, offset: usize) -> Result<Vec<i64>, DwfError> {
    fn visit(
        node: &Node,
        output: &mut Vec<i64>,
        resource: &str,
        offset: usize,
    ) -> Result<(), DwfError> {
        match node {
            Node::Atom(value) => {
                output.push(value.parse::<i64>().map_err(|error| DwfError::InvalidW2d {
                    resource: resource.to_owned(),
                    offset,
                    context: format!("invalid integer operand {value:?}: {error}"),
                })?);
            }
            Node::String(value) => {
                return Err(DwfError::InvalidW2d {
                    resource: resource.to_owned(),
                    offset,
                    context: format!("expected an integer, got string {value:?}"),
                });
            }
            Node::List(values) => {
                for value in values {
                    visit(value, output, resource, offset)?;
                }
            }
        }
        Ok(())
    }

    let mut output = Vec::new();
    for node in nodes {
        visit(node, &mut output, resource, offset)?;
    }
    Ok(output)
}

fn parse_points_from_nodes(
    nodes: &[Node],
    resource: &str,
    offset: usize,
) -> Result<Vec<W2dPoint>, DwfError> {
    let numbers = collect_i64(nodes, resource, offset)?;
    if numbers.len() % 2 != 0 {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: format!("point list contains {} coordinate values", numbers.len()),
        });
    }
    Ok(numbers
        .chunks_exact(2)
        .map(|point| W2dPoint {
            x: point[0],
            y: point[1],
        })
        .collect())
}

fn parse_ascii_color_map_nodes(
    values: &[Node],
    resource: &str,
    offset: usize,
) -> Result<Vec<[u8; 4]>, DwfError> {
    let count = required_usize(values.get(1), resource, offset, "ColorMap count")?;
    if !(1..=256).contains(&count) {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: "ColorMap count must be between 1 and 256".to_owned(),
        });
    }
    let channels = collect_i64(&values[2..], resource, offset)?;
    if channels.len() != count * 4 {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: format!(
                "ColorMap declares {count} colors but contains {} channels",
                channels.len()
            ),
        });
    }
    channels
        .chunks_exact(4)
        .map(|rgba| {
            Ok([
                color_channel(rgba[0], resource, offset)?,
                color_channel(rgba[1], resource, offset)?,
                color_channel(rgba[2], resource, offset)?,
                color_channel(rgba[3], resource, offset)?,
            ])
        })
        .collect()
}

fn parse_hex_bytes(nodes: &[Node], resource: &str, offset: usize) -> Result<Vec<u8>, DwfError> {
    let mut digits = String::new();
    fn collect(node: &Node, digits: &mut String) -> bool {
        match node {
            Node::Atom(value) | Node::String(value) => {
                digits.push_str(value);
                true
            }
            Node::List(values) => values.iter().all(|value| collect(value, digits)),
        }
    }
    if !nodes.iter().all(|node| collect(node, &mut digits))
        || digits.len() % 2 != 0
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: "hex data contains invalid digits or an odd digit count".to_owned(),
        });
    }
    digits
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("ASCII hex was checked");
            u8::from_str_radix(text, 16).map_err(|_| DwfError::InvalidW2d {
                resource: resource.to_owned(),
                offset,
                context: "invalid hex data".to_owned(),
            })
        })
        .collect()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_image_format(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        "bitonal" | "bitonal_mapped" => "bitonal_mapped".to_owned(),
        "group3x" | "group_3x" | "group3x_mapped" | "group_3x_mapped" => {
            "group3x_mapped".to_owned()
        }
        "group4x" | "group_4x" | "group4x_mapped" | "group_4x_mapped" => {
            "group4x_mapped".to_owned()
        }
        _ => normalized,
    }
}

fn image_format_for_opcode(opcode: u16) -> &'static str {
    match opcode {
        0x0002 => "bitonal_mapped",
        0x0003 => "group3x_mapped",
        0x0004 => "indexed",
        0x0005 => "mapped",
        0x0006 => "rgb",
        0x0007 => "rgba",
        0x0008 => "jpeg",
        0x0009 => "group4",
        0x000C => "png",
        0x000D => "group4x_mapped",
        _ => "unknown",
    }
}

fn parse_units(node: &Node, resource: &str, offset: usize) -> Result<W2dUnits, DwfError> {
    let values = root_values(node, "Units", resource, offset)?;
    let name = values.get(1).and_then(Node::text).unwrap_or("").to_owned();
    let mut numbers = Vec::new();
    for value in values.iter().skip(2) {
        value.numbers(&mut numbers);
    }
    let transform: [f64; 16] =
        numbers
            .try_into()
            .map_err(|numbers: Vec<f64>| DwfError::InvalidW2d {
                resource: resource.to_owned(),
                offset,
                context: format!("Units transform requires 16 numbers, got {}", numbers.len()),
            })?;
    Ok(W2dUnits { name, transform })
}

fn parse_contours(
    node: &Node,
    resource: &str,
    offset: usize,
    options: ParseOptions,
) -> Result<Vec<Vec<W2dPoint>>, DwfError> {
    let values = root_values(node, "Contour", resource, offset)?;
    let contour_count = required_usize(values.get(1), resource, offset, "contour count")?;
    if contour_count > options.max_w2d_points_per_entity {
        return Err(DwfError::W2dPointLimitExceeded {
            resource: resource.to_owned(),
            offset,
            actual: contour_count,
            limit: options.max_w2d_points_per_entity,
        });
    }
    if values.len() < 2 + contour_count {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: "Contour record is missing per-contour point counts".to_owned(),
        });
    }
    let counts = values[2..2 + contour_count]
        .iter()
        .map(|value| required_usize(Some(value), resource, offset, "contour point count"))
        .collect::<Result<Vec<_>, _>>()?;
    let total = counts.iter().try_fold(0_usize, |total, count| {
        total
            .checked_add(*count)
            .ok_or_else(|| DwfError::InvalidW2d {
                resource: resource.to_owned(),
                offset,
                context: "Contour point count overflow".to_owned(),
            })
    })?;
    if total > options.max_w2d_points_per_entity {
        return Err(DwfError::W2dPointLimitExceeded {
            resource: resource.to_owned(),
            offset,
            actual: total,
            limit: options.max_w2d_points_per_entity,
        });
    }
    let points = parse_points_from_nodes(&values[2 + contour_count..], resource, offset)?;
    if points.len() != total {
        return Err(DwfError::InvalidW2d {
            resource: resource.to_owned(),
            offset,
            context: format!(
                "Contour declares {total} points but contains {}",
                points.len()
            ),
        });
    }
    let mut point_iter = points.into_iter();
    Ok(counts
        .into_iter()
        .map(|count| point_iter.by_ref().take(count).collect())
        .collect())
}

fn angle(value: i64, resource: &str, offset: usize) -> Result<u32, DwfError> {
    u32::try_from(value).map_err(|_| DwfError::InvalidW2d {
        resource: resource.to_owned(),
        offset,
        context: format!("angle {value} must be non-negative"),
    })
}

fn decimal_revision(version: &str) -> Option<u16> {
    let (major, minor) = version.split_once('.')?;
    major
        .parse::<u16>()
        .ok()?
        .checked_mul(100)?
        .checked_add(minor.parse::<u16>().ok()?)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn geometry_point_count(geometry: &W2dGeometry) -> usize {
    match geometry {
        W2dGeometry::Line { .. } => 2,
        W2dGeometry::Polyline { points }
        | W2dGeometry::Polymarker { points }
        | W2dGeometry::Polygon { points }
        | W2dGeometry::PolyBezier { points }
        | W2dGeometry::Polytriangle { points }
        | W2dGeometry::TexturedPolytriangle { points } => points.len(),
        W2dGeometry::GouraudPolyline { points } | W2dGeometry::GouraudPolytriangle { points } => {
            points.len()
        }
        W2dGeometry::ContourSet { contours } => contours.iter().map(Vec::len).sum(),
        W2dGeometry::Image { .. } => 2,
        W2dGeometry::Circle { .. } | W2dGeometry::Ellipse { .. } => 1,
        W2dGeometry::Text { bounds, .. } => 1 + usize::from(bounds.is_some()) * 4,
    }
}

fn logical_bounds(entities: &[W2dEntity]) -> Option<[i64; 4]> {
    fn include(bounds: &mut Option<[i64; 4]>, point: W2dPoint) {
        if let Some(bounds) = bounds {
            bounds[0] = bounds[0].min(point.x);
            bounds[1] = bounds[1].min(point.y);
            bounds[2] = bounds[2].max(point.x);
            bounds[3] = bounds[3].max(point.y);
        } else {
            *bounds = Some([point.x, point.y, point.x, point.y]);
        }
    }

    let mut bounds = None;
    for entity in entities {
        match &entity.geometry {
            W2dGeometry::Line { start, end } => {
                include(&mut bounds, *start);
                include(&mut bounds, *end);
            }
            W2dGeometry::Polyline { points }
            | W2dGeometry::Polymarker { points }
            | W2dGeometry::Polygon { points }
            | W2dGeometry::PolyBezier { points }
            | W2dGeometry::Polytriangle { points }
            | W2dGeometry::TexturedPolytriangle { points } => {
                for point in points {
                    include(&mut bounds, *point);
                }
            }
            W2dGeometry::GouraudPolyline { points }
            | W2dGeometry::GouraudPolytriangle { points } => {
                for point in points {
                    include(&mut bounds, point.point);
                }
            }
            W2dGeometry::ContourSet { contours } => {
                for point in contours.iter().flatten() {
                    include(&mut bounds, *point);
                }
            }
            W2dGeometry::Image { image } => {
                include(&mut bounds, image.min);
                include(&mut bounds, image.max);
            }
            W2dGeometry::Circle { center, radius, .. } => {
                let radius = radius.saturating_abs();
                include(
                    &mut bounds,
                    W2dPoint {
                        x: center.x.saturating_sub(radius),
                        y: center.y.saturating_sub(radius),
                    },
                );
                include(
                    &mut bounds,
                    W2dPoint {
                        x: center.x.saturating_add(radius),
                        y: center.y.saturating_add(radius),
                    },
                );
            }
            W2dGeometry::Ellipse {
                center,
                major,
                minor,
                ..
            } => {
                let radius = major.saturating_abs().max(minor.saturating_abs());
                include(
                    &mut bounds,
                    W2dPoint {
                        x: center.x.saturating_sub(radius),
                        y: center.y.saturating_sub(radius),
                    },
                );
                include(
                    &mut bounds,
                    W2dPoint {
                        x: center.x.saturating_add(radius),
                        y: center.y.saturating_add(radius),
                    },
                );
            }
            W2dGeometry::Text {
                position,
                bounds: text_bounds,
                ..
            } => {
                include(&mut bounds, *position);
                if let Some(text_bounds) = text_bounds {
                    for point in text_bounds {
                        include(&mut bounds, *point);
                    }
                }
            }
        }
    }
    bounds
}

fn line_pattern_name(index: usize) -> String {
    const NAMES: [&str; 36] = [
        "",
        "Solid",
        "Dashed",
        "Dotted",
        "Dash_Dot",
        "Short_Dash",
        "Medium_Dash",
        "Long_Dash",
        "Short_Dash_X2",
        "Medium_Dash_X2",
        "Long_Dash_X2",
        "Medium_Long_Dash",
        "Medium_Dash_Short_Dash_Short_Dash",
        "Long_Dash_Short_Dash",
        "Long_Dash_Dot_Dot",
        "Long_Dash_Dot",
        "Medium_Dash_Dot_Short_Dash_Dot",
        "Sparse_Dot",
        "ISO_Dash",
        "ISO_Dash_Space",
        "ISO_Long_Dash_Dot",
        "ISO_Long_Dash_Double_Dot",
        "ISO_Long_Dash_Triple_Dot",
        "ISO_Dot",
        "ISO_Long_Dash_Short_Dash",
        "ISO_Long_Dash_Double_Short_Dash",
        "ISO_Dash_Dot",
        "ISO_Double_Dash_Dot",
        "ISO_Dash_Double_Dot",
        "ISO_Double_Dash_Double_Dot",
        "ISO_Dash_Triple_Dot",
        "ISO_Double_Dash_Triple_Dot",
        "Decorated_Tracks",
        "Decorated_Wide_Tracks",
        "Decorated_Circle_Fence",
        "Decorated_Square_Fence",
    ];
    NAMES
        .get(index)
        .filter(|name| !name.is_empty())
        .map_or_else(|| format!("index:{index}"), |name| (*name).to_owned())
}

fn printable_opcode(opcode: u8) -> String {
    if opcode.is_ascii_graphic() {
        char::from(opcode).to_string()
    } else {
        format!("0x{opcode:02X}")
    }
}

const fn is_w2d_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_informational_extended_opcode(name: &str) -> bool {
    matches!(
        name,
        "Alignment"
            | "Author"
            | "Background"
            | "BlockMeaning"
            | "CodePage"
            | "ColorMap"
            | "Comment"
            | "Comments"
            | "Copyright"
            | "Created"
            | "Creator"
            | "Description"
            | "DrawingInfo"
            | "Embed"
            | "EmbedFile"
            | "GroupBegin"
            | "GroupEnd"
            | "Guid"
            | "GuidList"
            | "InkedArea"
            | "Keywords"
            | "LinesOverwrite"
            | "Modified"
            | "NamedView"
            | "Node"
            | "NonStdFontList"
            | "Orientation"
            | "PenPattern"
            | "PenPatOptions"
            | "PlotInfo"
            | "PlotOptimized"
            | "Projection"
            | "SourceCreated"
            | "SourceFilename"
            | "SourceModified"
            | "Time"
            | "Title"
            | "URL"
            | "UserData"
            | "View"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append_extended_binary(data: &mut Vec<u8>, opcode: u16, payload: &[u8]) {
        data.push(b'{');
        data.extend_from_slice(&(payload.len() as u32 + 3).to_le_bytes());
        data.extend_from_slice(&opcode.to_le_bytes());
        data.extend_from_slice(payload);
        data.push(b'}');
    }

    fn zlib_wrapper(payload: &[u8]) -> Vec<u8> {
        use flate2::{Compress, Compression, FlushCompress};

        let mut encoder = Compress::new(Compression::default(), true);
        let mut encoded = Vec::with_capacity(payload.len().saturating_add(64));
        encoder
            .compress_vec(payload, &mut encoded, FlushCompress::Finish)
            .unwrap();
        let mut wrapper = Vec::with_capacity(encoded.len() + 8);
        wrapper.push(b'{');
        wrapper.extend_from_slice(&0_u32.to_le_bytes());
        wrapper.extend_from_slice(&0x0011_u16.to_le_bytes());
        wrapper.extend_from_slice(&encoded);
        wrapper.push(b'}');
        wrapper
    }

    #[test]
    fn decodes_binary_unicode_strings_inside_extended_ascii_opcodes() {
        // AutoCAD's ePlot driver writes metadata and font names as WHIP! binary
        // Unicode strings ('{' + int32 character count + UTF-16LE + '}').
        fn ustr(text: &str) -> Vec<u8> {
            let units = text.encode_utf16().collect::<Vec<_>>();
            let mut out = vec![b'{'];
            out.extend_from_slice(&(units.len() as u32).to_le_bytes());
            for unit in units {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out.push(b'}');
            out
        }
        let mut data = b"(W2D V06.00)(Title ".to_vec();
        data.extend_from_slice(&ustr("C-000 表紙"));
        data.extend_from_slice(b")(Embed 'image/vnd.dwg;' 'AutoCAD' ");
        data.extend_from_slice(&ustr("C-000 表紙.dwg"));
        data.extend_from_slice(b" '')(FontExtension ");
        data.extend_from_slice(&ustr("ＭＳ ゴシック"));
        data.push(b' ');
        data.extend_from_slice(&ustr("MS Gothic"));
        data.extend_from_slice(b")L 0,0 5,5\n(Text 4,6 'hello')(EndOfDWF)");

        let stream = decode_w2d(&data, "sheet/eplot.w2d", ParseOptions::default()).unwrap();

        assert!(stream.end_of_dwf_seen);
        assert_eq!(stream.entities.len(), 2);
        let text = &stream.entities[1];
        assert_eq!(text.rendition.font.name.as_deref(), Some("ＭＳ ゴシック"));
        assert_eq!(
            text.rendition.font.canonical_name.as_deref(),
            Some("MS Gothic")
        );
    }

    #[test]
    fn accepts_legacy_dwf_v036_header() {
        let data = b"(DWF V00.36)L 0,0 5,5\n(EndOfDWF)";
        let stream = decode_w2d(data, "<legacy.dwf>", ParseOptions::default()).unwrap();
        assert_eq!(stream.version, "00.36");
        assert_eq!(stream.source_format, "legacy_dwf");
        assert_eq!(stream.entities.len(), 1);
    }

    #[test]
    fn decodes_ascii_geometry_and_rendition_state() {
        let data = br#"(W2D V06.00)
            (Units 'mm' ((1 0 0 0)(0 1 0 0)(0 0 1 0)(0 0 0 1)))
            (Layer 7 'walls')(Color 1,2,3,255)(LineWeight 25)
            (LinePattern Dashed)(LineStyle (LineJoin round) (LineStartCap square))
            (Font (Name 'Arial') (Style bold italic) (Height 120))
            F P 3 0,0 10,0 10,10 f L 0,0 5,5
            (Bezier 1 0,0 2,4 4,4 6,0)
            (Circle 10,10 5 100,200)
            (Ellipse 20,20 8,4 0,65536 50)
            (Text 4,6 'hello' (Bounds 4,6 9,6 9,8 4,8))
            (EndOfDWF)"#;
        let stream = decode_w2d(data, "sheet/main.w2d", ParseOptions::default()).unwrap();

        assert_eq!(stream.version, "06.00");
        assert!(stream.complete);
        assert!(stream.end_of_dwf_seen);
        assert_eq!(stream.units.as_ref().unwrap().name, "mm");
        assert_eq!(stream.layers[0].name.as_deref(), Some("walls"));
        assert_eq!(stream.entities.len(), 6);
        assert!(matches!(
            &stream.entities[0].geometry,
            W2dGeometry::Polygon { points } if points.len() == 3
        ));
        assert_eq!(stream.entities[0].rendition.color, Some([1, 2, 3, 255]));
        assert_eq!(stream.entities[0].rendition.line.weight, Some(25));
        assert_eq!(
            stream.entities[0].rendition.line.line_join.as_deref(),
            Some("round")
        );
        assert!(stream.entities[0].rendition.fill);
        assert!(matches!(
            &stream.entities[2].geometry,
            W2dGeometry::PolyBezier { points } if points.len() == 4
        ));
        assert!(matches!(
            &stream.entities[5].geometry,
            W2dGeometry::Text { text, bounds: Some(_), .. } if text == "hello"
        ));
        assert!(stream.diagnostics.is_empty());
    }

    #[test]
    fn decodes_binary_relative_coordinates_and_text() {
        let mut data = b"(W2D V06.00)".to_vec();
        data.push(b'O');
        data.extend_from_slice(&100_i32.to_le_bytes());
        data.extend_from_slice(&200_i32.to_le_bytes());
        data.push(0x03);
        data.extend_from_slice(&[10, 20, 30, 255]);
        data.push(0x0C);
        for value in [1_i16, 2, 3, 4] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.push(0x10);
        data.push(3);
        for value in [1_i16, 0, 0, 1, -1, 0] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.push(b'x');
        data.extend_from_slice(&2_i32.to_le_bytes());
        data.extend_from_slice(&3_i32.to_le_bytes());
        data.extend_from_slice(b"'note'");
        data.extend_from_slice(b"(EndOfDWF)");

        let stream = decode_w2d(&data, "binary.w2d", ParseOptions::default()).unwrap();
        assert_eq!(stream.entities.len(), 3);
        assert!(matches!(
            stream.entities[0].geometry,
            W2dGeometry::Line {
                start: W2dPoint { x: 101, y: 202 },
                end: W2dPoint { x: 104, y: 206 }
            }
        ));
        assert!(matches!(
            &stream.entities[1].geometry,
            W2dGeometry::Polyline { points }
                if points == &vec![
                    W2dPoint { x: 105, y: 206 },
                    W2dPoint { x: 105, y: 207 },
                    W2dPoint { x: 104, y: 207 },
                ]
        ));
        assert!(matches!(
            &stream.entities[2].geometry,
            W2dGeometry::Text { position: W2dPoint { x: 106, y: 210 }, text, .. }
                if text == "note"
        ));
    }

    #[test]
    fn safely_skips_unknown_extended_ascii_and_binary_records() {
        let mut data = b"(W2D V06.00)(Future (Nested 'ignore )'))".to_vec();
        data.push(b'{');
        data.extend_from_slice(&4_u32.to_le_bytes());
        data.extend_from_slice(&0x7777_u16.to_le_bytes());
        data.push(0xAA);
        data.push(b'}');
        data.extend_from_slice(b"(EndOfDWF)");
        let stream = decode_w2d(&data, "future.w2d", ParseOptions::default()).unwrap();
        assert!(stream.complete);
        assert_eq!(stream.diagnostics.len(), 2);
        assert_eq!(stream.diagnostics[0].offset, Some(12));
        assert_eq!(stream.diagnostics[1].details["opcode"], "0x7777");
    }

    #[test]
    fn rejects_invalid_zlib_compressed_data() {
        let mut data = b"(W2D V06.00)".to_vec();
        data.push(b'{');
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&0x0011_u16.to_le_bytes());
        data.extend_from_slice(b"compressed bytes");
        let error = decode_w2d(&data, "compressed.w2d", ParseOptions::default()).unwrap_err();
        assert!(matches!(error, DwfError::InvalidW2d { offset: 12, .. }));
    }

    #[test]
    fn expands_zlib_compressed_data_and_maps_source_offsets() {
        let inner = b"(Line 1,2 3,4)";
        let mut data = b"(W2D V06.00)".to_vec();
        let wrapper_offset = data.len();
        data.extend_from_slice(&zlib_wrapper(inner));
        let wrapper_length = data.len() - wrapper_offset;
        let tail = b"(Line 5,6 7,8)";
        let tail_offset = data.len();
        data.extend_from_slice(tail);
        data.extend_from_slice(b"(EndOfDWF)");

        let stream = decode_w2d(&data, "compressed.w2d", ParseOptions::default()).unwrap();
        assert!(stream.complete);
        assert!(stream.end_of_dwf_seen);
        assert_eq!(stream.compressed_blocks, 1);
        assert_eq!(stream.entities.len(), 2);
        assert_eq!(stream.entities[0].source.offset, wrapper_offset);
        assert_eq!(stream.entities[0].source.length, wrapper_length);
        assert_eq!(stream.entities[0].source.decoded_offset, Some(0));
        assert_eq!(stream.entities[0].source.decoded_length, Some(inner.len()));
        assert_eq!(stream.entities[0].source.compression_depth, 1);
        assert_eq!(stream.entities[1].source.offset, tail_offset);
        assert_eq!(stream.entities[1].source.length, tail.len());
        assert_eq!(stream.entities[1].source.decoded_offset, None);
        assert_eq!(stream.entities[1].source.compression_depth, 0);
    }

    #[test]
    fn maps_nested_compression_with_sparse_source_segments() {
        let first = b"(Line 0,0 1,1)";
        let inner = zlib_wrapper(b"(Line 2,2 3,3)");
        let last = b"(Line 4,4 5,5)";
        let mut outer_payload = first.to_vec();
        outer_payload.extend_from_slice(&inner);
        outer_payload.extend_from_slice(last);

        let mut data = b"(W2D V06.00)".to_vec();
        let wrapper_offset = data.len();
        data.extend_from_slice(&zlib_wrapper(&outer_payload));
        let wrapper_length = data.len() - wrapper_offset;
        data.extend_from_slice(b"(EndOfDWF)");

        let stream = decode_w2d(&data, "nested.w2d", ParseOptions::default()).unwrap();

        assert_eq!(stream.compressed_blocks, 2);
        assert_eq!(stream.entities.len(), 3);
        assert_eq!(stream.entities[0].source.decoded_offset, Some(0));
        assert_eq!(stream.entities[0].source.compression_depth, 1);
        assert_eq!(stream.entities[1].source.decoded_offset, Some(first.len()));
        assert_eq!(stream.entities[1].source.compression_depth, 2);
        assert_eq!(
            stream.entities[2].source.decoded_offset,
            Some(first.len() + inner.len())
        );
        assert_eq!(stream.entities[2].source.compression_depth, 1);
        for entity in &stream.entities {
            assert_eq!(entity.source.offset, wrapper_offset);
            assert_eq!(entity.source.length, wrapper_length);
        }
    }

    #[test]
    fn unknown_single_byte_opcode_fails_closed_with_offset() {
        let error = decode_w2d(
            b"(W2D V06.00)\x01payload",
            "unknown.w2d",
            ParseOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DwfError::UnsupportedW2dOpcode { offset: 12, .. }
        ));
    }

    #[test]
    fn enforces_record_and_point_limits() {
        let source_error = decode_w2d(
            b"(W2D V06.00)",
            "limits.w2d",
            ParseOptions {
                max_entry_size: 4,
                ..ParseOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            source_error,
            DwfError::W2dSourceSizeLimitExceeded { actual: 12, .. }
        ));

        let record_error = decode_w2d(
            b"(W2D V06.00)(Comment one)(Comment two)",
            "limits.w2d",
            ParseOptions {
                max_w2d_records: 2,
                ..ParseOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            record_error,
            DwfError::W2dRecordLimitExceeded { .. }
        ));

        let point_error = decode_w2d(
            b"(W2D V06.00)P 3 0,0 1,1 2,2",
            "limits.w2d",
            ParseOptions {
                max_w2d_points_per_entity: 2,
                ..ParseOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            point_error,
            DwfError::W2dPointLimitExceeded { .. }
        ));

        let total_point_error = decode_w2d(
            b"(W2D V06.00)(Line 0,0 1,1)(Line 2,2 3,3)",
            "limits.w2d",
            ParseOptions {
                max_w2d_total_points: 3,
                ..ParseOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            total_point_error,
            DwfError::W2dTotalPointLimitExceeded { .. }
        ));
    }

    #[test]
    fn decodes_advanced_ascii_and_legacy_metadata() {
        let data = br#"(DWF V00.55)
            (ColorMap 2 1,2,3,255 4,5,6,255) C 1
            (GourLine 2 0,0 255,0,0,255 10,0 0,0,255,255)
            (Gouraud 1 0,0 255,0,0,255 10,0 0,255,0,255 0,10 0,0,255,255)
            (Texture 3 0,0 4,0 0,4)
            (Contour 2 3 2 0,0 4,0 4,4 10,10 12,12)
            (Image 'RGB' 7 1,1 0,0 2,2 (3 FF0000))
            (Embedded_Font 1 2 0 14 Aabced Regular 6 Aabced (2 AABB))
            (BlockRef 'full' 10 20)
            (EndOfDWF)"#;
        let stream = decode_w2d(data, "legacy.dwf", ParseOptions::default()).unwrap();

        assert_eq!(stream.source_format, "legacy_dwf");
        assert_eq!(stream.version, "00.55");
        assert_eq!(stream.color_maps.len(), 1);
        assert_eq!(stream.entities.len(), 5);
        assert_eq!(stream.entities[0].rendition.color, Some([4, 5, 6, 255]));
        assert!(matches!(
            &stream.entities[0].geometry,
            W2dGeometry::GouraudPolyline { points } if points.len() == 2
        ));
        assert!(matches!(
            &stream.entities[1].geometry,
            W2dGeometry::GouraudPolytriangle { points } if points.len() == 3
        ));
        assert!(matches!(
            &stream.entities[3].geometry,
            W2dGeometry::ContourSet { contours }
                if contours.iter().map(Vec::len).collect::<Vec<_>>() == vec![3, 2]
        ));
        assert!(matches!(
            &stream.entities[4].geometry,
            W2dGeometry::Image { image }
                if image.format == "rgb" && image.data == [255, 0, 0]
        ));
        assert_eq!(stream.embedded_fonts[0].typeface_name, "Aabced Regular");
        assert_eq!(stream.embedded_fonts[0].logfont_name, "Aabced");
        assert_eq!(stream.embedded_fonts[0].data, [0xAA, 0xBB]);
        assert_eq!(stream.block_refs[0].format, "full");
        assert!(stream.complete);
    }

    #[test]
    fn decodes_advanced_binary_resources() {
        let mut data = b"(W2D V06.00)".to_vec();
        append_extended_binary(&mut data, 0x0001, &[2, 10, 20, 30, 255, 40, 50, 60, 255]);

        let mut image = Vec::new();
        image.extend_from_slice(&1_u16.to_le_bytes());
        image.extend_from_slice(&1_u16.to_le_bytes());
        for value in [0_i32, 0, 8, 6, 9] {
            image.extend_from_slice(&value.to_le_bytes());
        }
        image.extend_from_slice(&1_i32.to_le_bytes());
        image.push(1);
        append_extended_binary(&mut data, 0x0004, &image);

        let mut font = Vec::new();
        font.extend_from_slice(&1_u32.to_le_bytes());
        font.extend_from_slice(&[2, 0]);
        font.extend_from_slice(&5_i32.to_le_bytes());
        font.extend_from_slice(b"Arial");
        font.extend_from_slice(&0_i32.to_le_bytes());
        font.extend_from_slice(&2_i32.to_le_bytes());
        font.extend_from_slice(&[0xAA, 0xBB]);
        append_extended_binary(&mut data, 0x013E, &font);

        let mut block_ref = 3_u16.to_le_bytes().to_vec();
        block_ref.extend_from_slice(&[1, 2, 3, 4]);
        append_extended_binary(&mut data, 0x015E, &block_ref);
        data.extend_from_slice(b"(EndOfDWF)");

        let stream = decode_w2d(&data, "advanced.w2d", ParseOptions::default()).unwrap();
        assert_eq!(stream.color_maps[0][1], [40, 50, 60, 255]);
        assert!(matches!(
            &stream.entities[0].geometry,
            W2dGeometry::Image { image }
                if image.format == "indexed"
                    && image.color_map.len() == 2
                    && image.data == [1]
        ));
        assert_eq!(stream.embedded_fonts[0].data, [0xAA, 0xBB]);
        assert_eq!(stream.block_refs[0].format, "3");
        assert_eq!(stream.block_refs[0].payload, [1, 2, 3, 4]);
        assert!(stream.diagnostics.is_empty());
    }

    #[test]
    fn expands_legacy_lz_literal_stream() {
        let inner = b"(Line 1,2 3,4)";
        let mut data = b"(DWF V00.42)".to_vec();
        data.push(b'{');
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&0x0010_u16.to_le_bytes());
        if inner.len() < 15 {
            data.push(inner.len() as u8);
        } else {
            data.push(0x0F);
            data.push((inner.len() - 15) as u8);
        }
        data.extend_from_slice(inner);
        data.push(0);
        data.push(b'}');
        data.extend_from_slice(b"(EndOfDWF)");

        let stream = decode_w2d(&data, "legacy-lz.dwf", ParseOptions::default()).unwrap();
        assert_eq!(stream.compressed_blocks, 1);
        assert_eq!(stream.entities.len(), 1);
        assert_eq!(stream.entities[0].source.compression_depth, 1);
    }

    #[test]
    fn parses_w2d_revision_as_decimal_hundredths() {
        assert_eq!(decimal_revision("00.22"), Some(22));
        assert_eq!(decimal_revision("00.55"), Some(55));
        assert_eq!(decimal_revision("06.00"), Some(600));
        assert_eq!(decimal_revision("invalid"), None);
    }

    #[test]
    fn normalizes_ascii_image_format_names() {
        assert_eq!(normalize_image_format("bitonal"), "bitonal_mapped");
        assert_eq!(normalize_image_format("group 3X"), "group3x_mapped");
        assert_eq!(normalize_image_format("group 4X"), "group4x_mapped");
        assert_eq!(normalize_image_format("RGB"), "rgb");
    }

    #[test]
    fn malformed_and_truncated_records_never_panic() {
        let seed = b"(W2D V06.00)(ColorMap 2 0,0,0,255 255,255,255,255)(Gouraud 1 0,0 1,2,3,4 4,0 5,6,7,8 0,4 9,10,11,12)(EndOfDWF)";
        for length in 0..seed.len() {
            let _ = decode_w2d(&seed[..length], "truncated.w2d", ParseOptions::default());
        }
        for index in 0..seed.len() {
            let mut mutated = seed.to_vec();
            mutated[index] ^= 0xFF;
            let _ = decode_w2d(&mutated, "mutated.w2d", ParseOptions::default());
        }
    }

    #[test]
    fn malformed_and_truncated_compression_never_panics() {
        let mut seed = b"(W2D V06.00)".to_vec();
        seed.extend_from_slice(&zlib_wrapper(b"(Line 1,2 3,4)"));
        seed.extend_from_slice(b"(EndOfDWF)");
        for length in 0..seed.len() {
            let _ = decode_w2d(
                &seed[..length],
                "truncated-compression.w2d",
                ParseOptions::default(),
            );
        }
        for index in 12..seed.len() {
            let mut mutated = seed.clone();
            mutated[index] ^= 0xFF;
            let _ = decode_w2d(&mutated, "mutated-compression.w2d", ParseOptions::default());
        }
    }
}
