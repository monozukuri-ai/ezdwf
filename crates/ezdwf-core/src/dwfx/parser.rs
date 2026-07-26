use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::font::{
    build_glyph_outline, is_obfuscated_font, prepare_font_data, validate_glyph_spec,
};
use super::path::parse_abbreviated_geometry;
use super::raster::image_metadata;
use super::{
    DwfxPackage, OpcContentType, OpcRelationship, XpsBrush, XpsCanvasGroup, XpsClip, XpsDocument,
    XpsEntity, XpsGeometry, XpsGlyphs, XpsGradientStop, XpsMatrix, XpsOpacityMask, XpsPage,
    XpsPathFigure, XpsPathGeometry, XpsPathSegment, XpsPoint, XpsSourceSpan, XpsStyle, XpsVisual,
};
use crate::package::archive::PackageArchive;
use crate::package::xml::{
    attributes, local_name_string, normalize_xml_encoding, required, xml_error,
};
use crate::{detect_format, Diagnostic, DiagnosticSeverity, DwfError, DwfFormat, ParseOptions};

const CONTENT_TYPES_PART: &str = "[Content_Types].xml";
const ROOT_RELATIONSHIPS_PART: &str = "_rels/.rels";
const REQUIRED_RESOURCE_RELATIONSHIP_SUFFIX: &str = "/required-resource";

/// Inspect a DWFx OPC/XPS package and decode its ordered FixedPage visuals.
pub fn inspect_dwfx(data: &[u8], options: ParseOptions) -> Result<DwfxPackage, DwfError> {
    inspect_dwfx_impl(data, options, true)
}

/// Inspect DWFx without materializing packaged-font outlines.
///
/// This is intended for structure-only inspection of text-heavy packages.
pub fn inspect_dwfx_without_glyph_outlines(
    data: &[u8],
    options: ParseOptions,
) -> Result<DwfxPackage, DwfError> {
    inspect_dwfx_impl(data, options, false)
}

fn inspect_dwfx_impl(
    data: &[u8],
    options: ParseOptions,
    resolve_glyph_outlines: bool,
) -> Result<DwfxPackage, DwfError> {
    let format = detect_format(data, options)?;
    if format != DwfFormat::Dwfx {
        return Err(DwfError::UnsupportedFormat { format });
    }

    let archive = PackageArchive::open(data, 0, options)?;
    let content_types_xml = archive.read_entry(CONTENT_TYPES_PART, options.max_xml_size)?;
    let content_types = parse_content_types(&content_types_xml, CONTENT_TYPES_PART, options)?;
    let root_relationships_xml =
        archive.read_entry(ROOT_RELATIONSHIPS_PART, options.max_xml_size)?;
    let root_relationships = parse_relationships(
        &root_relationships_xml,
        ROOT_RELATIONSHIPS_PART,
        None,
        options,
    )?;

    let document_sequence = root_relationships
        .iter()
        .find(|relationship| {
            relationship.target_mode.eq_ignore_ascii_case("Internal")
                && relationship
                    .relationship_type
                    .to_ascii_lowercase()
                    .ends_with("/fixedrepresentation")
        })
        .and_then(|relationship| relationship.normalized_target.clone())
        .or_else(|| {
            content_types.iter().find_map(|content_type| {
                if content_type
                    .content_type
                    .to_ascii_lowercase()
                    .contains("fixeddocumentsequence")
                {
                    content_type.part_name.clone()
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| DwfError::InvalidOpc {
            part: ROOT_RELATIONSHIPS_PART.to_owned(),
            context: "package has no internal fixed-representation relationship".to_owned(),
        })?;
    require_part(&archive, "/", &document_sequence, &document_sequence)?;

    let mut relationships = root_relationships.clone();
    read_part_relationships(&archive, &document_sequence, options, &mut relationships)?;
    let sequence_xml = archive.read_entry(&document_sequence, options.max_xml_size)?;
    let document_sources = parse_reference_sources(
        &sequence_xml,
        &document_sequence,
        "FixedDocumentSequence",
        "DocumentReference",
        options,
    )?;
    if document_sources.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document_sequence.clone(),
            context: "FixedDocumentSequence contains no DocumentReference".to_owned(),
        });
    }

    let mut documents = Vec::with_capacity(document_sources.len());
    let mut diagnostics = Vec::new();
    let mut font_cache = BTreeMap::new();
    for source in document_sources {
        let document_part = resolve_internal_target(Some(&document_sequence), &source)?;
        require_part(&archive, &document_sequence, &source, &document_part)?;

        let document_relationships =
            read_part_relationships(&archive, &document_part, options, &mut relationships)?;
        let document_xml = archive.read_entry(&document_part, options.max_xml_size)?;
        let page_sources = parse_reference_sources(
            &document_xml,
            &document_part,
            "FixedDocument",
            "PageContent",
            options,
        )?;
        if page_sources.is_empty() {
            return Err(DwfError::InvalidXps {
                part: document_part.clone(),
                context: "FixedDocument contains no PageContent".to_owned(),
            });
        }

        let mut pages = Vec::with_capacity(page_sources.len());
        for source in page_sources {
            let page_part = resolve_internal_target(Some(&document_part), &source)?;
            require_part(&archive, &document_part, &source, &page_part)?;
            let page_relationships =
                read_part_relationships(&archive, &page_part, options, &mut relationships)?;
            let page_xml = archive.read_entry(&page_part, options.max_xml_size)?;
            let mut page = parse_fixed_page(&page_xml, &page_part, &archive, options)?;
            page.relationships = page_relationships;
            hydrate_page_resources(
                &mut page,
                &archive,
                &content_types,
                options,
                &mut diagnostics,
                &mut font_cache,
                resolve_glyph_outlines,
            )?;
            diagnostics.extend(page.diagnostics.iter().cloned());
            pages.push(page);
        }
        documents.push(XpsDocument {
            part_name: document_part,
            relationships: document_relationships,
            pages,
        });
    }

    Ok(DwfxPackage {
        format,
        entries: archive.entries().to_vec(),
        content_types,
        relationships,
        document_sequence,
        documents,
        diagnostics,
    })
}

fn parse_content_types(
    xml: &[u8],
    document: &str,
    options: ParseOptions,
) -> Result<Vec<OpcContentType>, DwfError> {
    check_xml_size(xml, document, options)?;
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut output = Vec::new();
    let mut default_extensions = BTreeSet::new();
    let mut override_parts = BTreeSet::new();
    let mut root_seen = false;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let is_empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(start, reader.decoder(), document)?;
                if stack.is_empty() {
                    if root_seen || name != "Types" {
                        return Err(invalid_opc(document, "Types must be the unique root"));
                    }
                    root_seen = true;
                } else if stack.len() == 1 && stack[0] == "Types" && name == "Default" {
                    let extension =
                        required(&values, "Extension", document, "Default")?.to_ascii_lowercase();
                    if !default_extensions.insert(extension.clone()) {
                        return Err(invalid_opc(
                            document,
                            format!("duplicate Default extension {extension:?}"),
                        ));
                    }
                    output.push(OpcContentType {
                        extension: Some(extension),
                        part_name: None,
                        content_type: required(&values, "ContentType", document, "Default")?,
                    });
                } else if stack.len() == 1 && stack[0] == "Types" && name == "Override" {
                    let raw_name = required(&values, "PartName", document, "Override")?;
                    let part_name = normalize_absolute_part_name(&raw_name)?;
                    if !override_parts.insert(part_name.clone()) {
                        return Err(invalid_opc(
                            document,
                            format!("duplicate Override part {part_name:?}"),
                        ));
                    }
                    output.push(OpcContentType {
                        extension: None,
                        part_name: Some(part_name),
                        content_type: required(&values, "ContentType", document, "Override")?,
                    });
                } else {
                    return Err(invalid_opc(
                        document,
                        format!("unexpected element {name:?} in Content Types"),
                    ));
                }
                if !is_empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(end) => pop_element(&mut stack, end.name().as_ref(), document)?,
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() {
        return Err(invalid_opc(
            document,
            "Content Types XML is incomplete or missing its root",
        ));
    }
    if output.is_empty() {
        return Err(invalid_opc(document, "Content Types table is empty"));
    }
    Ok(output)
}

fn parse_relationships(
    xml: &[u8],
    document: &str,
    source: Option<&str>,
    options: ParseOptions,
) -> Result<Vec<OpcRelationship>, DwfError> {
    check_xml_size(xml, document, options)?;
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut output = Vec::new();
    let mut identifiers = BTreeSet::new();
    let mut root_seen = false;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let is_empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(start, reader.decoder(), document)?;
                if stack.is_empty() {
                    if root_seen || name != "Relationships" {
                        return Err(invalid_opc(
                            document,
                            "Relationships must be the unique root",
                        ));
                    }
                    root_seen = true;
                } else if stack.len() == 1 && stack[0] == "Relationships" && name == "Relationship"
                {
                    let id = required(&values, "Id", document, "Relationship")?;
                    if !identifiers.insert(id.clone()) {
                        return Err(invalid_opc(
                            document,
                            format!("duplicate relationship Id {id:?}"),
                        ));
                    }
                    let target = required(&values, "Target", document, "Relationship")?;
                    let target_mode = values
                        .get("TargetMode")
                        .cloned()
                        .unwrap_or_else(|| "Internal".to_owned());
                    let normalized_target = if target_mode.eq_ignore_ascii_case("External") {
                        None
                    } else if target_mode.eq_ignore_ascii_case("Internal") {
                        Some(resolve_internal_target(source, &target)?)
                    } else {
                        return Err(invalid_opc(
                            document,
                            format!("invalid TargetMode {target_mode:?}"),
                        ));
                    };
                    output.push(OpcRelationship {
                        source: source.map(str::to_owned),
                        id,
                        relationship_type: required(&values, "Type", document, "Relationship")?,
                        target,
                        target_mode,
                        normalized_target,
                    });
                } else {
                    return Err(invalid_opc(
                        document,
                        format!("unexpected element {name:?} in Relationships"),
                    ));
                }
                if !is_empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(end) => pop_element(&mut stack, end.name().as_ref(), document)?,
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() {
        return Err(invalid_opc(
            document,
            "Relationships XML is incomplete or missing its root",
        ));
    }
    Ok(output)
}

fn parse_reference_sources(
    xml: &[u8],
    document: &str,
    expected_root: &str,
    reference_element: &str,
    options: ParseOptions,
) -> Result<Vec<String>, DwfError> {
    check_xml_size(xml, document, options)?;
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut root_seen = false;
    let mut sources = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let is_empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(start, reader.decoder(), document)?;
                if stack.is_empty() {
                    if root_seen || name != expected_root {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: format!("expected {expected_root} root, got {name}"),
                        });
                    }
                    root_seen = true;
                } else if name == reference_element {
                    sources.push(required(&values, "Source", document, reference_element)?);
                }
                if !is_empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(end) => pop_element(&mut stack, end.name().as_ref(), document)?,
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{expected_root} XML is incomplete"),
        });
    }
    Ok(sources)
}

fn parse_fixed_page(
    xml: &[u8],
    document: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
) -> Result<XpsPage, DwfError> {
    parse_fixed_page_with_resources(xml, document, archive, options, BTreeMap::new(), 0)
}

fn parse_fixed_page_with_resources(
    xml: &[u8],
    document: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
    inherited_resources: BTreeMap<String, ResourceValue>,
    visual_depth: usize,
) -> Result<XpsPage, DwfError> {
    check_xml_size(xml, document, options)?;
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let prepared_resources = prepare_page_resources(
        xml.as_ref(),
        document,
        archive,
        options,
        inherited_resources,
        visual_depth,
    )?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut parser = FixedPageParser::new(
        document,
        xml.as_ref(),
        archive,
        options,
        &prepared_resources,
        visual_depth,
    );
    loop {
        let start_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        let end_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match event {
            Event::Start(start) => {
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(&start, reader.decoder(), document)?;
                parser.start(&name, &values, &stack, start_offset, false)?;
                stack.push(name);
                check_depth(&stack, document, options)?;
            }
            Event::Empty(start) => {
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(&start, reader.decoder(), document)?;
                parser.start(&name, &values, &stack, start_offset, true)?;
                parser.finish_empty(&name, end_offset)?;
            }
            Event::End(end) => {
                let name = local_name_string(end.name().as_ref(), document)?;
                parser.end(&name, end_offset)?;
                let open = stack.pop().ok_or_else(|| DwfError::InvalidXml {
                    document: document.to_owned(),
                    context: format!("unexpected closing element {name:?}"),
                })?;
                if open != name {
                    return Err(DwfError::InvalidXml {
                        document: document.to_owned(),
                        context: format!("closing element {name:?} does not match {open:?}"),
                    });
                }
            }
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "physical EOF occurred before all elements closed".to_owned(),
        });
    }
    parser.finish()
}

fn parse_visual_brush_content(
    brush_xml: &[u8],
    document: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
    resources: BTreeMap<String, ResourceValue>,
    visual_depth: usize,
) -> Result<Option<XpsVisual>, DwfError> {
    let Some(markup) = extract_visual_brush_markup(brush_xml, document, options)? else {
        return Ok(None);
    };
    parse_visual_markup(&markup, document, archive, options, resources, visual_depth).map(Some)
}

fn extract_visual_brush_markup(
    xml: &[u8],
    document: &str,
    options: ParseOptions,
) -> Result<Option<Vec<u8>>, DwfError> {
    let mut reader = Reader::from_reader(Cursor::new(xml));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut property_seen = false;
    let mut visual_range = None;
    let mut active_visual: Option<(usize, String)> = None;
    loop {
        let start_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        let end_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                if stack.len() == 1 && name == "VisualBrush.Visual" {
                    if property_seen {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: "VisualBrush.Visual was specified more than once".to_owned(),
                        });
                    }
                    property_seen = true;
                } else if stack.len() == 2 && stack[1] == "VisualBrush.Visual" {
                    if !matches!(name.as_str(), "Canvas" | "Path" | "Glyphs") {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: format!(
                                "VisualBrush.Visual contains unsupported visual {name:?}"
                            ),
                        });
                    }
                    if visual_range.is_some() || active_visual.is_some() {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: "VisualBrush.Visual must contain exactly one visual"
                                .to_owned(),
                        });
                    }
                    if empty {
                        visual_range = Some((start_offset, end_offset));
                    } else {
                        active_visual = Some((start_offset, name.clone()));
                    }
                }
                if !empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(ref end) => {
                let name = local_name_string(end.name().as_ref(), document)?;
                if active_visual
                    .as_ref()
                    .is_some_and(|(_, visual_name)| *visual_name == name && stack.len() == 3)
                {
                    let (start, _) = active_visual.take().expect("checked");
                    visual_range = Some((start, end_offset));
                }
                pop_element(&mut stack, end.name().as_ref(), document)?;
            }
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() || active_visual.is_some() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "VisualBrush markup is incomplete".to_owned(),
        });
    }
    if property_seen && visual_range.is_none() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "VisualBrush.Visual has no visual child".to_owned(),
        });
    }
    Ok(visual_range.map(|(start, end)| xml[start..end].to_vec()))
}

fn parse_visual_markup(
    markup: &[u8],
    document: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
    resources: BTreeMap<String, ResourceValue>,
    visual_depth: usize,
) -> Result<XpsVisual, DwfError> {
    if visual_depth >= options.max_xml_depth {
        return Err(DwfError::XmlDepthLimitExceeded {
            document: document.to_owned(),
            limit: options.max_xml_depth,
        });
    }
    const PREFIX: &[u8] = br#"<FixedPage Width="1" Height="1">"#;
    const SUFFIX: &[u8] = b"</FixedPage>";
    let size = PREFIX
        .len()
        .checked_add(markup.len())
        .and_then(|value| value.checked_add(SUFFIX.len()))
        .ok_or(DwfError::XmlSizeLimitExceeded {
            document: document.to_owned(),
            actual: usize::MAX,
            limit: options.max_xml_size,
        })?;
    if size > options.max_xml_size {
        return Err(DwfError::XmlSizeLimitExceeded {
            document: document.to_owned(),
            actual: size,
            limit: options.max_xml_size,
        });
    }
    let mut wrapped = Vec::with_capacity(size);
    wrapped.extend_from_slice(PREFIX);
    wrapped.extend_from_slice(markup);
    wrapped.extend_from_slice(SUFFIX);
    let page = parse_fixed_page_with_resources(
        &wrapped,
        document,
        archive,
        options,
        resources,
        visual_depth + 1,
    )?;
    Ok(XpsVisual {
        entities: page.entities,
    })
}

fn visual_segment_count(visual: &XpsVisual) -> usize {
    let mut canvas_groups = BTreeSet::new();
    visual.entities.iter().fold(0usize, |count, entity| {
        let geometry = match &entity.geometry {
            XpsGeometry::Path { geometry } => geometry.segment_count(),
            XpsGeometry::Glyphs { .. } => 0,
        };
        let local_clip = entity
            .clip
            .as_ref()
            .map_or(0, XpsPathGeometry::segment_count);
        let brushes = [
            entity.style.fill.as_ref(),
            entity.style.stroke.as_ref(),
            entity.opacity_mask.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(brush_segment_count)
        .fold(0usize, usize::saturating_add);
        let group_segments = entity
            .canvas_groups
            .iter()
            .filter(|group| canvas_groups.insert(group.id))
            .map(|group| {
                group
                    .clip
                    .as_ref()
                    .map_or(0, |clip| clip.segment_count())
                    .saturating_add(
                        group
                            .opacity_mask
                            .as_ref()
                            .map_or(0, |mask| brush_segment_count(mask)),
                    )
            })
            .fold(0usize, usize::saturating_add);
        count
            .saturating_add(geometry)
            .saturating_add(local_clip)
            .saturating_add(brushes)
            .saturating_add(group_segments)
    })
}

fn brush_segment_count(brush: &XpsBrush) -> usize {
    match brush {
        XpsBrush::Visual {
            visual: Some(visual),
            ..
        } => visual_segment_count(visual),
        _ => 0,
    }
}

fn charge_dictionary_segments(
    remaining: &mut usize,
    count: usize,
    document: &str,
    options: ParseOptions,
) -> Result<(), DwfError> {
    if count > *remaining {
        return Err(DwfError::XpsPathSegmentLimitExceeded {
            page: document.to_owned(),
            limit: options.max_xps_path_segments,
        });
    }
    *remaining -= count;
    Ok(())
}

struct ParsedResourceDictionary {
    resources: BTreeMap<String, ResourceValue>,
    remote_part: Option<String>,
    segment_count: usize,
}

fn prepare_page_resources(
    xml: &[u8],
    document: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
    inherited_resources: BTreeMap<String, ResourceValue>,
    visual_depth: usize,
) -> Result<PreparedPageResources, DwfError> {
    let mut reader = Reader::from_reader(Cursor::new(xml));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut scopes = Vec::<(usize, BTreeMap<String, ResourceValue>)>::new();
    let mut active_dictionary: Option<(usize, usize, BTreeMap<String, ResourceValue>)> = None;
    let mut prepared = PreparedPageResources::default();

    loop {
        let start_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        let end_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                if active_dictionary.is_none() {
                    match name.as_str() {
                        "FixedPage" | "Canvas" => {
                            let resources = if name == "FixedPage" {
                                inherited_resources.clone()
                            } else {
                                BTreeMap::new()
                            };
                            scopes.push((start_offset, resources));
                        }
                        "ResourceDictionary" => {
                            if scopes.is_empty() {
                                return Err(DwfError::InvalidXps {
                                    part: document.to_owned(),
                                    context: "ResourceDictionary appeared outside FixedPage"
                                        .to_owned(),
                                });
                            }
                            if !matches!(
                                stack.last().map(String::as_str),
                                Some("FixedPage.Resources" | "Canvas.Resources")
                            ) {
                                return Err(DwfError::InvalidXps {
                                    part: document.to_owned(),
                                    context: "ResourceDictionary must be the value of FixedPage.Resources or Canvas.Resources"
                                        .to_owned(),
                                });
                            }
                            let available = merged_resource_scopes(&scopes);
                            if empty {
                                let dictionary = parse_resource_dictionary_markup(
                                    &xml[start_offset..end_offset],
                                    document,
                                    document,
                                    archive,
                                    options,
                                    available,
                                    visual_depth,
                                )?;
                                merge_prepared_dictionary(
                                    &mut prepared,
                                    scopes.last_mut().expect("checked"),
                                    dictionary,
                                    document,
                                    options.max_xps_path_segments,
                                )?;
                            } else {
                                active_dictionary = Some((start_offset, stack.len(), available));
                            }
                        }
                        _ => {}
                    }
                }
                if !empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                } else if active_dictionary.is_none()
                    && matches!(name.as_str(), "FixedPage" | "Canvas")
                {
                    let scope = scopes.pop().expect("scope was just pushed");
                    prepared.scopes.insert(scope.0, scope.1);
                }
            }
            Event::End(ref end) => {
                let name = local_name_string(end.name().as_ref(), document)?;
                if let Some((dictionary_offset, parent_depth, available)) =
                    active_dictionary.as_ref()
                {
                    if name == "ResourceDictionary" && stack.len() == parent_depth + 1 {
                        let dictionary = parse_resource_dictionary_markup(
                            &xml[*dictionary_offset..end_offset],
                            document,
                            document,
                            archive,
                            options,
                            available.clone(),
                            visual_depth,
                        )?;
                        merge_prepared_dictionary(
                            &mut prepared,
                            scopes.last_mut().expect("dictionary scope exists"),
                            dictionary,
                            document,
                            options.max_xps_path_segments,
                        )?;
                        active_dictionary = None;
                    }
                } else if matches!(name.as_str(), "FixedPage" | "Canvas") {
                    let scope = scopes.pop().ok_or_else(|| DwfError::InvalidXps {
                        part: document.to_owned(),
                        context: format!("{name} resource scope closed without matching start"),
                    })?;
                    prepared.scopes.insert(scope.0, scope.1);
                }
                let open = stack.pop().ok_or_else(|| DwfError::InvalidXml {
                    document: document.to_owned(),
                    context: format!("unexpected closing element {name:?}"),
                })?;
                if open != name {
                    return Err(DwfError::InvalidXml {
                        document: document.to_owned(),
                        context: format!("closing element {name:?} does not match {open:?}"),
                    });
                }
            }
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if active_dictionary.is_some() || !stack.is_empty() || !scopes.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "FixedPage resource scan ended with incomplete markup".to_owned(),
        });
    }
    Ok(prepared)
}

fn merged_resource_scopes(
    scopes: &[(usize, BTreeMap<String, ResourceValue>)],
) -> BTreeMap<String, ResourceValue> {
    let mut output = BTreeMap::new();
    for (_, scope) in scopes {
        output.extend(
            scope
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    output
}

fn merge_prepared_dictionary(
    prepared: &mut PreparedPageResources,
    scope: &mut (usize, BTreeMap<String, ResourceValue>),
    dictionary: ParsedResourceDictionary,
    document: &str,
    segment_limit: usize,
) -> Result<(), DwfError> {
    for (key, value) in dictionary.resources {
        if scope.1.insert(key.clone(), value).is_some() {
            return Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("duplicate XPS resource key {key:?} in one scope"),
            });
        }
    }
    prepared.segment_count = prepared
        .segment_count
        .checked_add(dictionary.segment_count)
        .ok_or(DwfError::XpsPathSegmentLimitExceeded {
            page: document.to_owned(),
            limit: usize::MAX,
        })?;
    if prepared.segment_count > segment_limit {
        return Err(DwfError::XpsPathSegmentLimitExceeded {
            page: document.to_owned(),
            limit: segment_limit,
        });
    }
    if let Some(part) = dictionary.remote_part {
        if !prepared.remote_parts.contains(&part) {
            prepared.remote_parts.push(part);
        }
    }
    Ok(())
}

struct DictionaryBrushBuilder {
    key: String,
    element: String,
    brush: XpsBrush,
    start_offset: usize,
    resources: BTreeMap<String, ResourceValue>,
}

struct DictionaryGeometryBuilder {
    key: String,
    geometry: XpsPathGeometry,
    figure: Option<XpsPathFigure>,
}

struct DictionaryVisualBuilder {
    key: String,
    element: String,
    start_offset: usize,
    resources: BTreeMap<String, ResourceValue>,
}

enum DictionaryEntryBuilder {
    Brush(Box<DictionaryBrushBuilder>),
    Geometry(DictionaryGeometryBuilder),
    Visual(DictionaryVisualBuilder),
}

fn parse_resource_dictionary_markup(
    xml: &[u8],
    page_part: &str,
    base_part: &str,
    archive: &PackageArchive<'_>,
    options: ParseOptions,
    available: BTreeMap<String, ResourceValue>,
    visual_depth: usize,
) -> Result<ParsedResourceDictionary, DwfError> {
    let xml = normalize_xml_encoding(xml, base_part, options.max_xml_size)?;
    let source = resource_dictionary_source(xml.as_ref(), base_part, options)?;
    if let Some(source) = source {
        if base_part != page_part {
            return Err(DwfError::InvalidXps {
                part: base_part.to_owned(),
                context: "a remote ResourceDictionary cannot reference another remote dictionary"
                    .to_owned(),
            });
        }
        let remote_part = resolve_internal_target(Some(base_part), &source)?;
        require_part(archive, base_part, &source, &remote_part)?;
        let remote_xml = archive.read_entry(&remote_part, options.max_xml_size)?;
        let remote_xml = normalize_xml_encoding(&remote_xml, &remote_part, options.max_xml_size)?;
        if resource_dictionary_source(remote_xml.as_ref(), &remote_part, options)?.is_some() {
            return Err(DwfError::InvalidXps {
                part: remote_part,
                context: "a remote ResourceDictionary cannot reference another remote dictionary"
                    .to_owned(),
            });
        }
        let (resources, segment_count) = parse_resource_dictionary_entries(
            remote_xml.as_ref(),
            &remote_part,
            options,
            BTreeMap::new(),
            archive,
            visual_depth,
        )?;
        return Ok(ParsedResourceDictionary {
            resources,
            remote_part: Some(remote_part),
            segment_count,
        });
    }
    let (resources, segment_count) = parse_resource_dictionary_entries(
        xml.as_ref(),
        base_part,
        options,
        available,
        archive,
        visual_depth,
    )?;
    Ok(ParsedResourceDictionary {
        resources,
        remote_part: None,
        segment_count,
    })
}

fn resource_dictionary_source(
    xml: &[u8],
    document: &str,
    options: ParseOptions,
) -> Result<Option<String>, DwfError> {
    let mut reader = Reader::from_reader(Cursor::new(xml));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut root_seen = false;
    let mut source = None;
    let mut direct_children = 0usize;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                if stack.is_empty() {
                    if root_seen || name != "ResourceDictionary" {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: format!(
                                "remote resource markup must have a ResourceDictionary root, got {name:?}"
                            ),
                        });
                    }
                    root_seen = true;
                    source = attributes(start, reader.decoder(), document)?
                        .get("Source")
                        .cloned();
                } else if stack.len() == 1 {
                    direct_children += 1;
                }
                if !empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(end) => pop_element(&mut stack, end.name().as_ref(), document)?,
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "ResourceDictionary XML is incomplete".to_owned(),
        });
    }
    if source.is_some() && direct_children != 0 {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "ResourceDictionary Source cannot be combined with child resources".to_owned(),
        });
    }
    Ok(source)
}

fn parse_resource_dictionary_entries(
    xml: &[u8],
    document: &str,
    options: ParseOptions,
    mut available: BTreeMap<String, ResourceValue>,
    archive: &PackageArchive<'_>,
    visual_depth: usize,
) -> Result<(BTreeMap<String, ResourceValue>, usize), DwfError> {
    let mut reader = Reader::from_reader(Cursor::new(xml));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut output = BTreeMap::new();
    let mut builder: Option<DictionaryEntryBuilder> = None;
    let mut segments_remaining = options.max_xps_path_segments;

    loop {
        let start_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        let end_offset = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(&event, Event::Empty(_));
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(start, reader.decoder(), document)?;
                let parent = stack.last().map(String::as_str);
                if stack.is_empty() {
                    if name != "ResourceDictionary" {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: "resource dictionary has an invalid root".to_owned(),
                        });
                    }
                } else if stack.len() == 1 && builder.is_none() {
                    let Some(key) = values.get("Key").cloned() else {
                        if !empty {
                            stack.push(name);
                            check_depth(&stack, document, options)?;
                        }
                        buffer.clear();
                        continue;
                    };
                    match name.as_str() {
                        "SolidColorBrush"
                        | "ImageBrush"
                        | "LinearGradientBrush"
                        | "RadialGradientBrush"
                        | "VisualBrush" => {
                            let scopes = [available.clone()];
                            let brush = parse_brush_element(&name, &values, document, &scopes)?;
                            if empty {
                                validate_brush(&brush, document, &name)?;
                                insert_dictionary_resource(
                                    &mut output,
                                    &mut available,
                                    key,
                                    ResourceValue::Brush(Arc::new(brush)),
                                    document,
                                )?;
                            } else {
                                builder = Some(DictionaryEntryBuilder::Brush(Box::new(
                                    DictionaryBrushBuilder {
                                        key,
                                        element: name.clone(),
                                        brush,
                                        start_offset,
                                        resources: available.clone(),
                                    },
                                )));
                            }
                        }
                        "Canvas" | "Path" | "Glyphs" => {
                            if empty {
                                let visual = parse_visual_markup(
                                    &xml[start_offset..end_offset],
                                    document,
                                    archive,
                                    options,
                                    available.clone(),
                                    visual_depth,
                                )?;
                                charge_dictionary_segments(
                                    &mut segments_remaining,
                                    visual_segment_count(&visual),
                                    document,
                                    options,
                                )?;
                                insert_dictionary_resource(
                                    &mut output,
                                    &mut available,
                                    key,
                                    ResourceValue::Visual(Arc::new(visual)),
                                    document,
                                )?;
                            } else {
                                builder =
                                    Some(DictionaryEntryBuilder::Visual(DictionaryVisualBuilder {
                                        key,
                                        element: name.clone(),
                                        start_offset,
                                        resources: available.clone(),
                                    }));
                            }
                        }
                        "PathGeometry" => {
                            let mut geometry = if let Some(figures) = values.get("Figures") {
                                parse_abbreviated_geometry(
                                    figures,
                                    document,
                                    &mut segments_remaining,
                                    options.max_xps_path_segments,
                                    false,
                                )?
                            } else {
                                XpsPathGeometry {
                                    fill_rule: parse_fill_rule(values.get("FillRule"))?,
                                    figures: Vec::new(),
                                    data: None,
                                    transform: XpsMatrix::IDENTITY,
                                }
                            };
                            if values.contains_key("FillRule") {
                                geometry.fill_rule = parse_fill_rule(values.get("FillRule"))?;
                            }
                            let scopes = [available.clone()];
                            geometry.transform = parse_resource_matrix(
                                values.get("Transform").map(String::as_str),
                                document,
                                "PathGeometry",
                                &scopes,
                            )?;
                            if empty {
                                insert_dictionary_resource(
                                    &mut output,
                                    &mut available,
                                    key,
                                    ResourceValue::Geometry(Arc::new(geometry)),
                                    document,
                                )?;
                            } else {
                                builder = Some(DictionaryEntryBuilder::Geometry(
                                    DictionaryGeometryBuilder {
                                        key,
                                        geometry,
                                        figure: None,
                                    },
                                ));
                            }
                        }
                        "MatrixTransform" => {
                            let matrix = parse_matrix(
                                &required(&values, "Matrix", document, "MatrixTransform")?,
                                document,
                                "MatrixTransform.Matrix",
                            )?;
                            insert_dictionary_resource(
                                &mut output,
                                &mut available,
                                key,
                                ResourceValue::Matrix(matrix),
                                document,
                            )?;
                        }
                        _ => {}
                    }
                } else if let Some(entry) = &mut builder {
                    match entry {
                        DictionaryEntryBuilder::Brush(brush) => match name.as_str() {
                            "GradientStop" => {
                                let stop = parse_gradient_stop_value(&values, document)?;
                                add_gradient_stop(&mut brush.brush, stop, document)?;
                            }
                            "MatrixTransform"
                                if parent
                                    == Some(format!("{}.Transform", brush.element).as_str()) =>
                            {
                                let matrix = parse_matrix(
                                    &required(&values, "Matrix", document, "MatrixTransform")?,
                                    document,
                                    "brush MatrixTransform.Matrix",
                                )?;
                                set_brush_transform(&mut brush.brush, matrix);
                            }
                            _ => {}
                        },
                        DictionaryEntryBuilder::Geometry(geometry) => match name.as_str() {
                            "PathFigure" => {
                                if geometry.figure.is_some() {
                                    return Err(DwfError::InvalidXps {
                                        part: document.to_owned(),
                                        context: "nested PathFigure resources are invalid"
                                            .to_owned(),
                                    });
                                }
                                if geometry.geometry.data.is_some()
                                    && !geometry.geometry.figures.is_empty()
                                {
                                    return Err(DwfError::InvalidXps {
                                        part: document.to_owned(),
                                        context: "PathGeometry cannot combine Figures with PathFigure children"
                                            .to_owned(),
                                    });
                                }
                                geometry.figure = Some(parse_explicit_figure(&values, document)?);
                            }
                            "PolyLineSegment"
                            | "PolyBezierSegment"
                            | "PolyQuadraticBezierSegment"
                            | "ArcSegment" => {
                                let segments = parse_explicit_segments(&name, &values, document)?;
                                if segments.len() > segments_remaining {
                                    return Err(DwfError::XpsPathSegmentLimitExceeded {
                                        page: document.to_owned(),
                                        limit: options.max_xps_path_segments,
                                    });
                                }
                                segments_remaining -= segments.len();
                                let figure = geometry.figure.as_mut().ok_or_else(|| {
                                    DwfError::InvalidXps {
                                        part: document.to_owned(),
                                        context: format!(
                                            "{name} appeared outside a resource PathFigure"
                                        ),
                                    }
                                })?;
                                figure.segments.extend(segments);
                            }
                            "MatrixTransform" if parent == Some("PathGeometry.Transform") => {
                                geometry.geometry.transform = parse_matrix(
                                    &required(&values, "Matrix", document, "MatrixTransform")?,
                                    document,
                                    "PathGeometry.Transform",
                                )?;
                            }
                            _ => {}
                        },
                        DictionaryEntryBuilder::Visual(_) => {}
                    }
                }
                if !empty {
                    stack.push(name);
                    check_depth(&stack, document, options)?;
                }
            }
            Event::End(ref end) => {
                let name = local_name_string(end.name().as_ref(), document)?;
                if let Some(entry) = &mut builder {
                    if name == "PathFigure" {
                        if let DictionaryEntryBuilder::Geometry(geometry) = entry {
                            let figure =
                                geometry.figure.take().ok_or_else(|| DwfError::InvalidXps {
                                    part: document.to_owned(),
                                    context: "PathFigure closed without matching start".to_owned(),
                                })?;
                            geometry.geometry.figures.push(figure);
                        }
                    }
                }
                let closes_entry = stack.len() == 2
                    && builder.as_ref().is_some_and(|entry| match entry {
                        DictionaryEntryBuilder::Brush(value) => value.element == name,
                        DictionaryEntryBuilder::Geometry(_) => name == "PathGeometry",
                        DictionaryEntryBuilder::Visual(value) => value.element == name,
                    });
                if closes_entry {
                    let entry = builder.take().expect("checked");
                    let (key, value) = match entry {
                        DictionaryEntryBuilder::Brush(mut value) => {
                            if let XpsBrush::Visual { visual, .. } = &mut value.brush {
                                let inline = parse_visual_brush_content(
                                    &xml[value.start_offset..end_offset],
                                    document,
                                    archive,
                                    options,
                                    value.resources,
                                    visual_depth,
                                )?;
                                if visual.is_some() && inline.is_some() {
                                    return Err(DwfError::InvalidXps {
                                        part: document.to_owned(),
                                        context: "VisualBrush cannot combine Visual with VisualBrush.Visual content"
                                            .to_owned(),
                                    });
                                }
                                if visual.is_none() {
                                    if let Some(inline) = inline {
                                        charge_dictionary_segments(
                                            &mut segments_remaining,
                                            visual_segment_count(&inline),
                                            document,
                                            options,
                                        )?;
                                        *visual = Some(Arc::new(inline));
                                    }
                                }
                            }
                            validate_brush(&value.brush, document, &value.element)?;
                            (value.key, ResourceValue::Brush(Arc::new(value.brush)))
                        }
                        DictionaryEntryBuilder::Geometry(value) => {
                            if value.figure.is_some() {
                                return Err(DwfError::InvalidXps {
                                    part: document.to_owned(),
                                    context: "PathGeometry ended inside PathFigure".to_owned(),
                                });
                            }
                            (value.key, ResourceValue::Geometry(Arc::new(value.geometry)))
                        }
                        DictionaryEntryBuilder::Visual(value) => {
                            let visual = parse_visual_markup(
                                &xml[value.start_offset..end_offset],
                                document,
                                archive,
                                options,
                                value.resources,
                                visual_depth,
                            )?;
                            charge_dictionary_segments(
                                &mut segments_remaining,
                                visual_segment_count(&visual),
                                document,
                                options,
                            )?;
                            (value.key, ResourceValue::Visual(Arc::new(visual)))
                        }
                    };
                    insert_dictionary_resource(&mut output, &mut available, key, value, document)?;
                }
                pop_element(&mut stack, end.name().as_ref(), document)?;
            }
            Event::DocType(_) => return Err(doctype_error(document)),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if builder.is_some() || !stack.is_empty() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "ResourceDictionary ended with an incomplete resource".to_owned(),
        });
    }
    Ok((output, options.max_xps_path_segments - segments_remaining))
}

fn insert_dictionary_resource(
    output: &mut BTreeMap<String, ResourceValue>,
    available: &mut BTreeMap<String, ResourceValue>,
    key: String,
    value: ResourceValue,
    document: &str,
) -> Result<(), DwfError> {
    if output.insert(key.clone(), value.clone()).is_some() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("duplicate XPS resource key {key:?}"),
        });
    }
    available.insert(key, value);
    Ok(())
}

fn parse_gradient_stop_value(
    values: &BTreeMap<String, String>,
    document: &str,
) -> Result<XpsGradientStop, DwfError> {
    let color_value = required(values, "Color", document, "GradientStop")?;
    let color = if color_value.trim_start().starts_with("ContextColor ") {
        None
    } else {
        Some(parse_color(&color_value, document)?)
    };
    let offset = parse_required_f64(values, "Offset", document, "GradientStop")?;
    Ok(XpsGradientStop {
        color,
        color_value,
        offset,
    })
}

fn add_gradient_stop(
    brush: &mut XpsBrush,
    stop: XpsGradientStop,
    document: &str,
) -> Result<(), DwfError> {
    match brush {
        XpsBrush::LinearGradient { gradient_stops, .. }
        | XpsBrush::RadialGradient { gradient_stops, .. } => {
            gradient_stops.push(stop);
            Ok(())
        }
        _ => Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "GradientStop appeared outside a gradient brush".to_owned(),
        }),
    }
}

fn validate_brush(brush: &XpsBrush, document: &str, element: &str) -> Result<(), DwfError> {
    match brush {
        XpsBrush::LinearGradient { gradient_stops, .. }
        | XpsBrush::RadialGradient { gradient_stops, .. }
            if gradient_stops.len() < 2 =>
        {
            Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("{element} must contain at least two GradientStop elements"),
            })
        }
        XpsBrush::Visual { visual: None, .. } => Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{element} has no Visual content"),
        }),
        _ => Ok(()),
    }
}

fn set_brush_transform(brush: &mut XpsBrush, matrix: XpsMatrix) {
    match brush {
        XpsBrush::Image { transform, .. }
        | XpsBrush::Visual { transform, .. }
        | XpsBrush::LinearGradient { transform, .. }
        | XpsBrush::RadialGradient { transform, .. } => *transform = matrix,
        _ => {}
    }
}

fn parse_explicit_figure(
    values: &BTreeMap<String, String>,
    document: &str,
) -> Result<XpsPathFigure, DwfError> {
    Ok(XpsPathFigure {
        start: parse_point(
            &required(values, "StartPoint", document, "PathFigure")?,
            document,
            "PathFigure.StartPoint",
        )?,
        segments: Vec::new(),
        closed: parse_optional_bool(values, "IsClosed", document, "PathFigure")?.unwrap_or(false),
        filled: parse_optional_bool(values, "IsFilled", document, "PathFigure")?.unwrap_or(true),
    })
}

fn parse_explicit_segments(
    name: &str,
    values: &BTreeMap<String, String>,
    document: &str,
) -> Result<Vec<XpsPathSegment>, DwfError> {
    let stroked = parse_optional_bool(values, "IsStroked", document, name)?.unwrap_or(true);
    let smooth_join = parse_optional_bool(values, "IsSmoothJoin", document, name)?.unwrap_or(false);
    match name {
        "PolyLineSegment" => Ok(parse_points(
            &required(values, "Points", document, name)?,
            document,
            "PolyLineSegment.Points",
        )?
        .into_iter()
        .map(|end| XpsPathSegment::Line {
            end,
            stroked,
            smooth_join,
        })
        .collect()),
        "PolyBezierSegment" => {
            let points = parse_points(
                &required(values, "Points", document, name)?,
                document,
                "PolyBezierSegment.Points",
            )?;
            if points.len() % 3 != 0 {
                return Err(DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: format!(
                        "PolyBezierSegment requires groups of 3 points, got {}",
                        points.len()
                    ),
                });
            }
            Ok(points
                .chunks_exact(3)
                .map(|row| XpsPathSegment::CubicBezier {
                    control1: row[0],
                    control2: row[1],
                    end: row[2],
                    stroked,
                    smooth_join,
                })
                .collect())
        }
        "PolyQuadraticBezierSegment" => {
            let points = parse_points(
                &required(values, "Points", document, name)?,
                document,
                "PolyQuadraticBezierSegment.Points",
            )?;
            if points.len() % 2 != 0 {
                return Err(DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: format!(
                        "PolyQuadraticBezierSegment requires groups of 2 points, got {}",
                        points.len()
                    ),
                });
            }
            Ok(points
                .chunks_exact(2)
                .map(|row| XpsPathSegment::QuadraticBezier {
                    control: row[0],
                    end: row[1],
                    stroked,
                    smooth_join,
                })
                .collect())
        }
        "ArcSegment" => Ok(vec![XpsPathSegment::Arc {
            radius: parse_point(
                &required(values, "Size", document, name)?,
                document,
                "ArcSegment.Size",
            )?,
            rotation_degrees: parse_optional(values, "RotationAngle", document, name)?
                .unwrap_or(0.0),
            large_arc: parse_optional_bool(values, "IsLargeArc", document, name)?.unwrap_or(false),
            sweep_clockwise: values
                .get("SweepDirection")
                .is_some_and(|value| value.eq_ignore_ascii_case("Clockwise")),
            end: parse_point(
                &required(values, "Point", document, name)?,
                document,
                "ArcSegment.Point",
            )?,
            stroked,
            smooth_join,
        }]),
        _ => Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("unsupported explicit path segment {name:?}"),
        }),
    }
}

#[derive(Clone)]
enum ResourceValue {
    Brush(Arc<XpsBrush>),
    Geometry(Arc<XpsPathGeometry>),
    Matrix(XpsMatrix),
    Visual(Arc<XpsVisual>),
}

#[derive(Default)]
struct PreparedPageResources {
    scopes: BTreeMap<usize, BTreeMap<String, ResourceValue>>,
    remote_parts: Vec<String>,
    segment_count: usize,
}

#[derive(Clone)]
struct CanvasState {
    id: usize,
    parent_transform: XpsMatrix,
    local_transform: XpsMatrix,
    transform: XpsMatrix,
    opacity: f64,
    group_name: Option<String>,
    name: Option<String>,
    clip: Option<Arc<XpsPathGeometry>>,
    opacity_mask: Option<Arc<XpsBrush>>,
}

struct PathBuilder {
    start_offset: usize,
    name: Option<String>,
    navigate_uri: Option<String>,
    parent_transform: XpsMatrix,
    local_transform: XpsMatrix,
    transform: XpsMatrix,
    clip: Option<XpsPathGeometry>,
    inherited_clips: Vec<XpsClip>,
    opacity_mask: Option<XpsBrush>,
    inherited_opacity_masks: Vec<XpsOpacityMask>,
    style: XpsStyle,
    geometry: Option<XpsPathGeometry>,
    attributes: BTreeMap<String, String>,
    canvas_name: Option<String>,
    canvas_groups: Vec<XpsCanvasGroup>,
}

struct GlyphBuilder {
    start_offset: usize,
    entity: XpsEntity,
    inherited_clips: Vec<XpsClip>,
    inherited_opacity_masks: Vec<XpsOpacityMask>,
}

#[derive(Clone, Copy)]
enum GeometryTarget {
    CanvasClip,
    PathClip,
    PathData,
    GlyphClip,
}

#[derive(Clone, Copy)]
enum BrushTarget {
    CanvasOpacityMask,
    PathFill,
    PathStroke,
    PathOpacityMask,
    GlyphFill,
    GlyphOpacityMask,
}

struct BrushBuilder {
    start_offset: usize,
    element: String,
    target: BrushTarget,
    brush: XpsBrush,
    resources: BTreeMap<String, ResourceValue>,
}

struct GeometryBuilder {
    target: GeometryTarget,
    geometry: XpsPathGeometry,
    explicit_figure: Option<XpsPathFigure>,
}

struct FixedPageParser<'a, 'archive> {
    document: &'a str,
    xml: &'a [u8],
    archive: &'a PackageArchive<'archive>,
    prepared_resources: &'a PreparedPageResources,
    options: ParseOptions,
    visual_depth: usize,
    root_seen: bool,
    page: Option<XpsPage>,
    canvas: Vec<CanvasState>,
    resource_scopes: Vec<BTreeMap<String, ResourceValue>>,
    resource_dictionary_depth: usize,
    path: Option<PathBuilder>,
    glyph: Option<GlyphBuilder>,
    explicit_geometry: Option<GeometryBuilder>,
    brush: Option<BrushBuilder>,
    next_canvas_id: usize,
    segments_remaining: usize,
}

impl<'a, 'archive> FixedPageParser<'a, 'archive> {
    fn new(
        document: &'a str,
        xml: &'a [u8],
        archive: &'a PackageArchive<'archive>,
        options: ParseOptions,
        prepared_resources: &'a PreparedPageResources,
        visual_depth: usize,
    ) -> Self {
        Self {
            document,
            xml,
            archive,
            prepared_resources,
            options,
            visual_depth,
            root_seen: false,
            page: None,
            canvas: Vec::new(),
            resource_scopes: Vec::new(),
            resource_dictionary_depth: 0,
            path: None,
            glyph: None,
            explicit_geometry: None,
            brush: None,
            next_canvas_id: 0,
            segments_remaining: options
                .max_xps_path_segments
                .saturating_sub(prepared_resources.segment_count),
        }
    }

    fn start(
        &mut self,
        name: &str,
        values: &BTreeMap<String, String>,
        stack: &[String],
        offset: usize,
        empty: bool,
    ) -> Result<(), DwfError> {
        let parent = stack.last().map(String::as_str);
        if stack.is_empty() && name != "FixedPage" {
            return Err(self.invalid(format!("element {name} appeared outside FixedPage")));
        }
        if name == "FixedPage" {
            if self.root_seen || !stack.is_empty() {
                return Err(self.invalid("FixedPage must be the unique root"));
            }
            self.root_seen = true;
            self.resource_scopes.push(
                self.prepared_resources
                    .scopes
                    .get(&offset)
                    .cloned()
                    .unwrap_or_default(),
            );
            let width = parse_required_f64(values, "Width", self.document, "FixedPage")?;
            let height = parse_required_f64(values, "Height", self.document, "FixedPage")?;
            if width <= 0.0 || height <= 0.0 {
                return Err(self.invalid("FixedPage Width and Height must be positive"));
            }
            self.page = Some(XpsPage {
                part_name: self.document.to_owned(),
                name: values
                    .get("Name")
                    .cloned()
                    .unwrap_or_else(|| page_name(self.document)),
                language: values.get("lang").cloned(),
                width,
                height,
                content_box: parse_optional_box(values, "ContentBox", self.document, name)?,
                bleed_box: parse_optional_box(values, "BleedBox", self.document, name)?,
                resource_dictionaries: self.prepared_resources.remote_parts.clone(),
                relationships: Vec::new(),
                entities: Vec::new(),
                diagnostics: Vec::new(),
            });
            return Ok(());
        }
        if !self.root_seen {
            return Err(self.invalid(format!("element {name} appeared before FixedPage")));
        }

        if name == "ResourceDictionary" {
            self.resource_dictionary_depth += 1;
            return Ok(());
        }
        if self.resource_dictionary_depth > 0 {
            return Ok(());
        }

        if self.brush.is_some() {
            match name {
                "GradientStop" => self.gradient_stop(values)?,
                "MatrixTransform" => self.brush_property_transform(values, parent)?,
                _ => {}
            }
            return Ok(());
        }

        match name {
            "Canvas" => {
                self.resource_scopes.push(
                    self.prepared_resources
                        .scopes
                        .get(&offset)
                        .cloned()
                        .unwrap_or_default(),
                );
                let parent_state = self.current_canvas();
                let local_transform = self.transform_attribute(values)?;
                let transform = parent_state.transform.compose(local_transform);
                let opacity =
                    parse_optional_opacity(values, self.document, "Canvas")?.unwrap_or(1.0);
                let clip = values
                    .get("Clip")
                    .map(|value| self.geometry_value(value))
                    .transpose()?
                    .map(Arc::new);
                let opacity_mask = values
                    .get("OpacityMask")
                    .map(|value| parse_brush_value(value, self.document, &self.resource_scopes))
                    .transpose()?
                    .map(Arc::new);
                let group_name = values
                    .get("Name")
                    .or_else(|| values.get("AutomationProperties.Name"))
                    .cloned();
                let id = self.next_canvas_id;
                self.next_canvas_id = self.next_canvas_id.saturating_add(1);
                self.canvas.push(CanvasState {
                    id,
                    parent_transform: parent_state.transform,
                    local_transform,
                    transform,
                    opacity,
                    group_name: group_name.clone(),
                    name: group_name.or(parent_state.name),
                    clip,
                    opacity_mask,
                });
            }
            "Path" => self.begin_path(values, offset)?,
            "Glyphs" => self.begin_glyph(values, offset)?,
            "PathGeometry"
                if self.path.is_some() || self.glyph.is_some() || !self.canvas.is_empty() =>
            {
                self.begin_path_geometry(values, parent, empty)?;
            }
            "PathFigure" if self.explicit_geometry.is_some() => {
                self.begin_explicit_figure(values)?;
            }
            "PolyLineSegment"
            | "PolyBezierSegment"
            | "PolyQuadraticBezierSegment"
            | "ArcSegment"
                if self.explicit_geometry.is_some() =>
            {
                self.explicit_segment(name, values)?;
            }
            "SolidColorBrush"
            | "ImageBrush"
            | "LinearGradientBrush"
            | "RadialGradientBrush"
            | "VisualBrush" => {
                self.visual_brush(name, values, parent, offset, empty)?;
            }
            "MatrixTransform" => self.property_transform(values, parent)?,
            _ => {}
        }
        if empty && name == "Canvas" {
            self.canvas.pop();
            self.resource_scopes.pop();
        }
        Ok(())
    }

    fn finish_empty(&mut self, name: &str, end_offset: usize) -> Result<(), DwfError> {
        if name == "ResourceDictionary" {
            self.resource_dictionary_depth = self.resource_dictionary_depth.saturating_sub(1);
            return Ok(());
        }
        if self.resource_dictionary_depth > 0 {
            return Ok(());
        }
        if self.brush.is_some() {
            return Ok(());
        }
        match name {
            "Path" => self.finish_path(end_offset)?,
            "Glyphs" => self.finish_glyph(end_offset)?,
            "PathFigure" => self.finish_explicit_figure()?,
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, name: &str, end_offset: usize) -> Result<(), DwfError> {
        if name == "ResourceDictionary" {
            self.resource_dictionary_depth = self.resource_dictionary_depth.saturating_sub(1);
            return Ok(());
        }
        if self.resource_dictionary_depth > 0 {
            return Ok(());
        }
        if self.brush.is_some() {
            if self
                .brush
                .as_ref()
                .is_some_and(|builder| builder.element == name)
            {
                self.finish_brush(end_offset)?;
            }
            return Ok(());
        }
        match name {
            "Path" => self.finish_path(end_offset)?,
            "Glyphs" => self.finish_glyph(end_offset)?,
            "PathFigure" => self.finish_explicit_figure()?,
            "PathGeometry" => self.finish_explicit_geometry()?,
            "Canvas" => {
                self.canvas.pop().ok_or_else(|| {
                    self.invalid("Canvas closed without matching start".to_owned())
                })?;
                self.resource_scopes.pop().ok_or_else(|| {
                    self.invalid("Canvas resource scope closed without matching start")
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<XpsPage, DwfError> {
        if !self.root_seen {
            return Err(self.invalid("FixedPage root is missing"));
        }
        if self.path.is_some()
            || self.glyph.is_some()
            || self.explicit_geometry.is_some()
            || self.brush.is_some()
            || !self.canvas.is_empty()
        {
            return Err(self.invalid("FixedPage ended with an incomplete visual"));
        }
        let document = self.document.to_owned();
        self.page.ok_or_else(|| DwfError::InvalidXps {
            part: document,
            context: "FixedPage root is missing".to_owned(),
        })
    }

    fn current_canvas(&self) -> CanvasState {
        self.canvas.last().cloned().unwrap_or(CanvasState {
            id: usize::MAX,
            parent_transform: XpsMatrix::IDENTITY,
            local_transform: XpsMatrix::IDENTITY,
            transform: XpsMatrix::IDENTITY,
            opacity: 1.0,
            group_name: None,
            name: None,
            clip: None,
            opacity_mask: None,
        })
    }

    fn canvas_clip_chain(&self) -> Vec<XpsClip> {
        self.canvas
            .iter()
            .filter_map(|canvas| {
                canvas.clip.clone().map(|geometry| XpsClip {
                    geometry,
                    transform: canvas.transform,
                })
            })
            .collect()
    }

    fn canvas_opacity_mask_chain(&self) -> Vec<XpsOpacityMask> {
        self.canvas
            .iter()
            .filter_map(|canvas| {
                canvas.opacity_mask.clone().map(|brush| XpsOpacityMask {
                    brush,
                    transform: canvas.transform,
                })
            })
            .collect()
    }

    fn canvas_group_chain(&self) -> Vec<XpsCanvasGroup> {
        self.canvas
            .iter()
            .map(|canvas| XpsCanvasGroup {
                id: canvas.id,
                name: canvas.group_name.clone(),
                opacity: canvas.opacity,
                transform: canvas.transform,
                clip: canvas.clip.clone(),
                opacity_mask: canvas.opacity_mask.clone(),
            })
            .collect()
    }

    fn begin_path(
        &mut self,
        values: &BTreeMap<String, String>,
        offset: usize,
    ) -> Result<(), DwfError> {
        if self.path.is_some() || self.glyph.is_some() {
            return Err(self.invalid("nested Path/Glyphs visuals are not allowed"));
        }
        let canvas = self.current_canvas();
        let inherited_clips = self.canvas_clip_chain();
        let inherited_opacity_masks = self.canvas_opacity_mask_chain();
        let canvas_groups = self.canvas_group_chain();
        let local_transform = self.transform_attribute(values)?;
        let style = parse_style(values, self.document, &self.resource_scopes)?;
        self.warn_unsupported_brushes(&style, offset);
        let geometry = values
            .get("Data")
            .map(|value| self.geometry_value(value))
            .transpose()?;
        let clip = values
            .get("Clip")
            .map(|value| self.geometry_value(value))
            .transpose()?;
        let opacity_mask = values
            .get("OpacityMask")
            .map(|value| parse_brush_value(value, self.document, &self.resource_scopes))
            .transpose()?;
        self.path = Some(PathBuilder {
            start_offset: offset,
            name: values.get("Name").cloned(),
            navigate_uri: values.get("NavigateUri").cloned(),
            parent_transform: canvas.transform,
            local_transform,
            transform: canvas.transform.compose(local_transform),
            clip,
            inherited_clips,
            opacity_mask,
            inherited_opacity_masks,
            style,
            geometry,
            attributes: values.clone(),
            canvas_name: canvas.name,
            canvas_groups,
        });
        Ok(())
    }

    fn finish_path(&mut self, end_offset: usize) -> Result<(), DwfError> {
        let mut builder = self
            .path
            .take()
            .ok_or_else(|| self.invalid("Path closed without matching start"))?;
        let geometry = builder
            .geometry
            .take()
            .ok_or_else(|| self.invalid("Path has no Data geometry"))?;
        let mut clip_chain = builder.inherited_clips;
        if let Some(clip) = builder.clip.clone() {
            clip_chain.push(XpsClip {
                geometry: Arc::new(clip),
                transform: builder.transform,
            });
        }
        let mut opacity_mask_chain = builder.inherited_opacity_masks;
        if let Some(mask) = builder.opacity_mask.clone() {
            opacity_mask_chain.push(XpsOpacityMask {
                brush: Arc::new(mask),
                transform: builder.transform,
            });
        }
        let entity = XpsEntity {
            name: builder.name,
            canvas_name: builder.canvas_name,
            navigate_uri: builder.navigate_uri,
            transform: builder.transform,
            clip: builder.clip,
            clip_chain,
            opacity_mask: builder.opacity_mask,
            opacity_mask_chain,
            canvas_groups: builder.canvas_groups,
            style: builder.style,
            geometry: XpsGeometry::Path { geometry },
            source: XpsSourceSpan {
                offset: builder.start_offset,
                length: end_offset.saturating_sub(builder.start_offset),
                element: "Path".to_owned(),
            },
            attributes: builder.attributes,
        };
        self.push_entity(entity)
    }

    fn begin_glyph(
        &mut self,
        values: &BTreeMap<String, String>,
        offset: usize,
    ) -> Result<(), DwfError> {
        if self.path.is_some() || self.glyph.is_some() {
            return Err(self.invalid("nested Path/Glyphs visuals are not allowed"));
        }
        let canvas = self.current_canvas();
        let inherited_clips = self.canvas_clip_chain();
        let inherited_opacity_masks = self.canvas_opacity_mask_chain();
        let canvas_groups = self.canvas_group_chain();
        let local_transform = self.transform_attribute(values)?;
        let style = parse_style(values, self.document, &self.resource_scopes)?;
        self.warn_unsupported_brushes(&style, offset);
        let unicode_string = values
            .get("UnicodeString")
            .map(|value| value.strip_prefix("{}").unwrap_or(value).to_owned())
            .unwrap_or_default();
        let glyphs = XpsGlyphs {
            unicode_string,
            origin: XpsPoint {
                x: parse_required_f64(values, "OriginX", self.document, "Glyphs")?,
                y: parse_required_f64(values, "OriginY", self.document, "Glyphs")?,
            },
            font_uri: required(values, "FontUri", self.document, "Glyphs")?,
            font_resource_part: self.document.to_owned(),
            normalized_font_uri: None,
            font_rendering_em_size: parse_required_f64(
                values,
                "FontRenderingEmSize",
                self.document,
                "Glyphs",
            )?,
            indices: values.get("Indices").cloned(),
            style_simulations: values.get("StyleSimulations").cloned(),
            bidi_level: parse_optional(values, "BidiLevel", self.document, "Glyphs")?,
            sideways: parse_optional_bool(values, "IsSideways", self.document, "Glyphs")?
                .unwrap_or(false),
            font_part: None,
            font_content_type: None,
            font_obfuscated: false,
            outline: None,
        };
        if glyphs.font_rendering_em_size <= 0.0 {
            return Err(self.invalid("Glyphs.FontRenderingEmSize must be positive"));
        }
        validate_glyph_spec(&glyphs).map_err(|context| self.invalid(context))?;
        let clip = values
            .get("Clip")
            .map(|value| self.geometry_value(value))
            .transpose()?;
        let opacity_mask = values
            .get("OpacityMask")
            .map(|value| parse_brush_value(value, self.document, &self.resource_scopes))
            .transpose()?;
        self.glyph = Some(GlyphBuilder {
            start_offset: offset,
            entity: XpsEntity {
                name: values.get("Name").cloned(),
                canvas_name: canvas.name,
                navigate_uri: values.get("NavigateUri").cloned(),
                transform: canvas.transform.compose(local_transform),
                clip,
                clip_chain: Vec::new(),
                opacity_mask,
                opacity_mask_chain: Vec::new(),
                canvas_groups: canvas_groups.clone(),
                style,
                geometry: XpsGeometry::Glyphs {
                    glyphs: Box::new(glyphs),
                },
                source: XpsSourceSpan {
                    offset,
                    length: 0,
                    element: "Glyphs".to_owned(),
                },
                attributes: values.clone(),
            },
            inherited_clips,
            inherited_opacity_masks,
        });
        Ok(())
    }

    fn finish_glyph(&mut self, end_offset: usize) -> Result<(), DwfError> {
        let mut builder = self
            .glyph
            .take()
            .ok_or_else(|| self.invalid("Glyphs closed without matching start"))?;
        let mut clip_chain = builder.inherited_clips;
        if let Some(clip) = builder.entity.clip.clone() {
            clip_chain.push(XpsClip {
                geometry: Arc::new(clip),
                transform: builder.entity.transform,
            });
        }
        builder.entity.clip_chain = clip_chain;
        let mut opacity_mask_chain = builder.inherited_opacity_masks;
        if let Some(mask) = builder.entity.opacity_mask.clone() {
            opacity_mask_chain.push(XpsOpacityMask {
                brush: Arc::new(mask),
                transform: builder.entity.transform,
            });
        }
        builder.entity.opacity_mask_chain = opacity_mask_chain;
        builder.entity.source.length = end_offset.saturating_sub(builder.start_offset);
        self.push_entity(builder.entity)
    }

    fn begin_path_geometry(
        &mut self,
        values: &BTreeMap<String, String>,
        parent: Option<&str>,
        empty: bool,
    ) -> Result<(), DwfError> {
        if self.explicit_geometry.is_some() {
            return Err(self.invalid("nested PathGeometry elements are not allowed"));
        }
        let target = match parent {
            Some("Canvas.Clip") if !self.canvas.is_empty() => GeometryTarget::CanvasClip,
            Some("Path.Clip") if self.path.is_some() => GeometryTarget::PathClip,
            Some("Glyphs.Clip") if self.glyph.is_some() => GeometryTarget::GlyphClip,
            Some("Path.Data") if self.path.is_some() => GeometryTarget::PathData,
            _ if self.path.is_some() => GeometryTarget::PathData,
            _ => return Ok(()),
        };
        let mut geometry = if let Some(figures) = values.get("Figures") {
            self.parse_figures(figures)?
        } else {
            XpsPathGeometry {
                fill_rule: parse_fill_rule(values.get("FillRule"))?,
                figures: Vec::new(),
                data: None,
                transform: XpsMatrix::IDENTITY,
            }
        };
        if values.contains_key("FillRule") {
            geometry.fill_rule = parse_fill_rule(values.get("FillRule"))?;
        }
        geometry.transform = parse_resource_matrix(
            values.get("Transform").map(String::as_str),
            self.document,
            "PathGeometry",
            &self.resource_scopes,
        )?;
        if empty {
            self.apply_geometry(target, geometry)?;
        } else {
            self.explicit_geometry = Some(GeometryBuilder {
                target,
                geometry,
                explicit_figure: None,
            });
        }
        Ok(())
    }

    fn finish_explicit_geometry(&mut self) -> Result<(), DwfError> {
        let Some(builder) = self.explicit_geometry.take() else {
            return Ok(());
        };
        if builder.explicit_figure.is_some() {
            return Err(self.invalid("PathGeometry ended inside PathFigure"));
        }
        self.apply_geometry(builder.target, builder.geometry)
    }

    fn apply_geometry(
        &mut self,
        target: GeometryTarget,
        geometry: XpsPathGeometry,
    ) -> Result<(), DwfError> {
        match target {
            GeometryTarget::CanvasClip => {
                if self.canvas.is_empty() {
                    return Err(self.invalid("Canvas.Clip has no Canvas"));
                }
                if self.canvas.last().expect("checked").clip.is_some() {
                    return Err(self.invalid("Canvas specifies Clip more than once"));
                }
                self.canvas.last_mut().expect("checked").clip = Some(Arc::new(geometry));
            }
            GeometryTarget::PathClip => {
                if self.path.is_none() {
                    return Err(self.invalid("Path.Clip has no Path"));
                }
                if self.path.as_ref().expect("checked").clip.is_some() {
                    return Err(self.invalid("Path specifies Clip more than once"));
                }
                self.path.as_mut().expect("checked").clip = Some(geometry);
            }
            GeometryTarget::PathData => {
                if self.path.is_none() {
                    return Err(self.invalid("Path.Data has no Path"));
                }
                if self.path.as_ref().expect("checked").geometry.is_some() {
                    return Err(self.invalid("Path specifies Data more than once"));
                }
                self.path.as_mut().expect("checked").geometry = Some(geometry);
            }
            GeometryTarget::GlyphClip => {
                if self.glyph.is_none() {
                    return Err(self.invalid("Glyphs.Clip has no Glyphs"));
                }
                if self.glyph.as_ref().expect("checked").entity.clip.is_some() {
                    return Err(self.invalid("Glyphs specifies Clip more than once"));
                }
                self.glyph.as_mut().expect("checked").entity.clip = Some(geometry);
            }
        }
        Ok(())
    }

    fn begin_explicit_figure(&mut self, values: &BTreeMap<String, String>) -> Result<(), DwfError> {
        let start = parse_point(
            &required(values, "StartPoint", self.document, "PathFigure")?,
            self.document,
            "PathFigure.StartPoint",
        )?;
        if self
            .explicit_geometry
            .as_ref()
            .expect("checked")
            .geometry
            .data
            .is_some()
        {
            return Err(
                self.invalid("PathGeometry cannot combine Figures with child PathFigure elements")
            );
        }
        let geometry = self.explicit_geometry.as_mut().expect("checked");
        if geometry.explicit_figure.is_some() {
            return Err(self.invalid("nested PathFigure elements are not allowed"));
        }
        geometry.explicit_figure = Some(XpsPathFigure {
            start,
            segments: Vec::new(),
            closed: parse_optional_bool(values, "IsClosed", self.document, "PathFigure")?
                .unwrap_or(false),
            filled: parse_optional_bool(values, "IsFilled", self.document, "PathFigure")?
                .unwrap_or(true),
        });
        Ok(())
    }

    fn finish_explicit_figure(&mut self) -> Result<(), DwfError> {
        let Some(geometry) = self.explicit_geometry.as_mut() else {
            return Err(self.invalid("PathFigure appeared outside PathGeometry"));
        };
        if geometry.explicit_figure.is_none() {
            return Err(self.invalid("PathFigure closed without matching start"));
        }
        let figure = geometry.explicit_figure.take().expect("checked");
        geometry.geometry.figures.push(figure);
        Ok(())
    }

    fn explicit_segment(
        &mut self,
        name: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), DwfError> {
        let stroked =
            parse_optional_bool(values, "IsStroked", self.document, name)?.unwrap_or(true);
        let smooth_join =
            parse_optional_bool(values, "IsSmoothJoin", self.document, name)?.unwrap_or(false);
        let segments = match name {
            "PolyLineSegment" => parse_points(
                &required(values, "Points", self.document, name)?,
                self.document,
                "PolyLineSegment.Points",
            )?
            .into_iter()
            .map(|end| XpsPathSegment::Line {
                end,
                stroked,
                smooth_join,
            })
            .collect::<Vec<_>>(),
            "PolyBezierSegment" => {
                let points = parse_points(
                    &required(values, "Points", self.document, name)?,
                    self.document,
                    "PolyBezierSegment.Points",
                )?;
                if points.len() % 3 != 0 {
                    return Err(self.invalid(format!(
                        "PolyBezierSegment requires groups of 3 points, got {}",
                        points.len()
                    )));
                }
                points
                    .chunks_exact(3)
                    .map(|row| XpsPathSegment::CubicBezier {
                        control1: row[0],
                        control2: row[1],
                        end: row[2],
                        stroked,
                        smooth_join,
                    })
                    .collect()
            }
            "PolyQuadraticBezierSegment" => {
                let points = parse_points(
                    &required(values, "Points", self.document, name)?,
                    self.document,
                    "PolyQuadraticBezierSegment.Points",
                )?;
                if points.len() % 2 != 0 {
                    return Err(self.invalid(format!(
                        "PolyQuadraticBezierSegment requires groups of 2 points, got {}",
                        points.len()
                    )));
                }
                points
                    .chunks_exact(2)
                    .map(|row| XpsPathSegment::QuadraticBezier {
                        control: row[0],
                        end: row[1],
                        stroked,
                        smooth_join,
                    })
                    .collect()
            }
            "ArcSegment" => vec![XpsPathSegment::Arc {
                radius: parse_point(
                    &required(values, "Size", self.document, name)?,
                    self.document,
                    "ArcSegment.Size",
                )?,
                rotation_degrees: parse_optional(values, "RotationAngle", self.document, name)?
                    .unwrap_or(0.0),
                large_arc: parse_optional_bool(values, "IsLargeArc", self.document, name)?
                    .unwrap_or(false),
                sweep_clockwise: values
                    .get("SweepDirection")
                    .is_some_and(|value| value.eq_ignore_ascii_case("Clockwise")),
                end: parse_point(
                    &required(values, "Point", self.document, name)?,
                    self.document,
                    "ArcSegment.Point",
                )?,
                stroked,
                smooth_join,
            }],
            _ => unreachable!(),
        };
        if segments.len() > self.segments_remaining {
            return Err(DwfError::XpsPathSegmentLimitExceeded {
                page: self.document.to_owned(),
                limit: self.options.max_xps_path_segments,
            });
        }
        self.segments_remaining -= segments.len();
        if self
            .explicit_geometry
            .as_ref()
            .expect("checked")
            .explicit_figure
            .is_none()
        {
            return Err(self.invalid(format!("{name} appeared outside PathFigure")));
        }
        let figure = self
            .explicit_geometry
            .as_mut()
            .expect("checked")
            .explicit_figure
            .as_mut()
            .expect("checked");
        figure.segments.extend(segments);
        Ok(())
    }

    fn visual_brush(
        &mut self,
        name: &str,
        values: &BTreeMap<String, String>,
        parent: Option<&str>,
        offset: usize,
        empty: bool,
    ) -> Result<(), DwfError> {
        let brush = parse_brush_element(name, values, self.document, &self.resource_scopes)?;
        let target = match parent {
            Some("Canvas.OpacityMask") if !self.canvas.is_empty() => BrushTarget::CanvasOpacityMask,
            Some("Path.Fill") if self.path.is_some() => BrushTarget::PathFill,
            Some("Path.Stroke") if self.path.is_some() => BrushTarget::PathStroke,
            Some("Path.OpacityMask") if self.path.is_some() => BrushTarget::PathOpacityMask,
            Some("Glyphs.Fill") if self.glyph.is_some() => BrushTarget::GlyphFill,
            Some("Glyphs.OpacityMask") if self.glyph.is_some() => BrushTarget::GlyphOpacityMask,
            _ => return Ok(()),
        };
        if matches!(brush, XpsBrush::Unsupported { .. }) {
            self.warning(
                "unsupported_xps_brush",
                format!("{name} is retained in raw data but not rendered by the SVG preview"),
                offset,
            );
        }
        if empty {
            validate_brush(&brush, self.document, name)?;
            self.apply_brush(target, brush)?;
        } else {
            let resources = merged_active_resource_scopes(&self.resource_scopes);
            self.brush = Some(BrushBuilder {
                start_offset: offset,
                element: name.to_owned(),
                target,
                brush,
                resources,
            });
        }
        Ok(())
    }

    fn gradient_stop(&mut self, values: &BTreeMap<String, String>) -> Result<(), DwfError> {
        let stop = parse_gradient_stop_value(values, self.document)?;
        match &mut self.brush.as_mut().expect("checked").brush {
            XpsBrush::LinearGradient { gradient_stops, .. }
            | XpsBrush::RadialGradient { gradient_stops, .. } => gradient_stops.push(stop),
            _ => {
                return Err(self.invalid("GradientStop appeared outside a gradient brush"));
            }
        }
        Ok(())
    }

    fn brush_property_transform(
        &mut self,
        values: &BTreeMap<String, String>,
        parent: Option<&str>,
    ) -> Result<(), DwfError> {
        let matrix = parse_matrix(
            &required(values, "Matrix", self.document, "MatrixTransform")?,
            self.document,
            "brush MatrixTransform.Matrix",
        )?;
        let builder = self.brush.as_mut().expect("checked");
        if parent != Some(format!("{}.Transform", builder.element).as_str()) {
            return Ok(());
        }
        match &mut builder.brush {
            XpsBrush::Image { transform, .. }
            | XpsBrush::Visual { transform, .. }
            | XpsBrush::LinearGradient { transform, .. }
            | XpsBrush::RadialGradient { transform, .. } => *transform = matrix,
            _ => {}
        }
        Ok(())
    }

    fn finish_brush(&mut self, end_offset: usize) -> Result<(), DwfError> {
        let mut builder = self
            .brush
            .take()
            .ok_or_else(|| self.invalid("brush closed without matching start"))?;
        if let XpsBrush::Visual { visual, .. } = &mut builder.brush {
            let inline = parse_visual_brush_content(
                &self.xml[builder.start_offset..end_offset],
                self.document,
                self.archive,
                self.options,
                builder.resources,
                self.visual_depth,
            )?;
            if visual.is_some() && inline.is_some() {
                return Err(self
                    .invalid("VisualBrush cannot combine Visual with VisualBrush.Visual content"));
            }
            if visual.is_none() {
                if let Some(inline) = &inline {
                    self.charge_segments(visual_segment_count(inline))?;
                }
                *visual = inline.map(Arc::new);
            }
        }
        validate_brush(&builder.brush, self.document, &builder.element)?;
        self.apply_brush(builder.target, builder.brush)
    }

    fn apply_brush(&mut self, target: BrushTarget, brush: XpsBrush) -> Result<(), DwfError> {
        if matches!(target, BrushTarget::CanvasOpacityMask) {
            let canvas = self.canvas.last_mut().ok_or_else(|| DwfError::InvalidXps {
                part: self.document.to_owned(),
                context: "Canvas.OpacityMask has no Canvas".to_owned(),
            })?;
            if canvas.opacity_mask.replace(Arc::new(brush)).is_some() {
                return Err(self.invalid("brush property was specified more than once"));
            }
            return Ok(());
        }
        let document = self.document.to_owned();
        let slot = match target {
            BrushTarget::CanvasOpacityMask => unreachable!("handled above"),
            BrushTarget::PathFill => {
                &mut self
                    .path
                    .as_mut()
                    .ok_or_else(|| DwfError::InvalidXps {
                        part: document.clone(),
                        context: "Path.Fill has no Path".to_owned(),
                    })?
                    .style
                    .fill
            }
            BrushTarget::PathStroke => {
                &mut self
                    .path
                    .as_mut()
                    .ok_or_else(|| DwfError::InvalidXps {
                        part: document.clone(),
                        context: "Path.Stroke has no Path".to_owned(),
                    })?
                    .style
                    .stroke
            }
            BrushTarget::PathOpacityMask => {
                &mut self
                    .path
                    .as_mut()
                    .ok_or_else(|| DwfError::InvalidXps {
                        part: document.clone(),
                        context: "Path.OpacityMask has no Path".to_owned(),
                    })?
                    .opacity_mask
            }
            BrushTarget::GlyphFill => {
                &mut self
                    .glyph
                    .as_mut()
                    .ok_or_else(|| DwfError::InvalidXps {
                        part: document.clone(),
                        context: "Glyphs.Fill has no Glyphs".to_owned(),
                    })?
                    .entity
                    .style
                    .fill
            }
            BrushTarget::GlyphOpacityMask => {
                &mut self
                    .glyph
                    .as_mut()
                    .ok_or_else(|| DwfError::InvalidXps {
                        part: document,
                        context: "Glyphs.OpacityMask has no Glyphs".to_owned(),
                    })?
                    .entity
                    .opacity_mask
            }
        };
        if slot.replace(brush).is_some() {
            return Err(self.invalid("brush property was specified more than once"));
        }
        Ok(())
    }

    fn property_transform(
        &mut self,
        values: &BTreeMap<String, String>,
        parent: Option<&str>,
    ) -> Result<(), DwfError> {
        let matrix = parse_matrix(
            &required(values, "Matrix", self.document, "MatrixTransform")?,
            self.document,
            "MatrixTransform.Matrix",
        )?;
        match parent {
            Some("Canvas.RenderTransform") => {
                if self.canvas.is_empty() {
                    return Err(self.invalid("Canvas.RenderTransform has no Canvas"));
                }
                let canvas = self.canvas.last_mut().expect("checked");
                canvas.local_transform = matrix;
                canvas.transform = canvas.parent_transform.compose(matrix);
            }
            Some("Path.RenderTransform") => {
                if self.path.is_none() {
                    return Err(self.invalid("Path.RenderTransform has no Path"));
                }
                let path = self.path.as_mut().expect("checked");
                path.local_transform = matrix;
                path.transform = path.parent_transform.compose(matrix);
            }
            Some("Glyphs.RenderTransform") => {
                let canvas = self.current_canvas();
                if self.glyph.is_none() {
                    return Err(self.invalid("Glyphs.RenderTransform has no Glyphs"));
                }
                let glyph = self.glyph.as_mut().expect("checked");
                glyph.entity.transform = canvas.transform.compose(matrix);
            }
            Some("PathGeometry.Transform") => {
                let Some(geometry) = &mut self.explicit_geometry else {
                    return Err(self.invalid("PathGeometry.Transform has no PathGeometry"));
                };
                geometry.geometry.transform = matrix;
            }
            _ => {}
        }
        Ok(())
    }

    fn transform_attribute(
        &self,
        values: &BTreeMap<String, String>,
    ) -> Result<XpsMatrix, DwfError> {
        let Some(value) = values.get("RenderTransform") else {
            return Ok(XpsMatrix::IDENTITY);
        };
        if let Some(key) = static_resource_key(value) {
            return match find_resource(&self.resource_scopes, key) {
                Some(ResourceValue::Matrix(matrix)) => Ok(*matrix),
                Some(_) => Err(self.invalid(format!("resource {key:?} is not a MatrixTransform"))),
                None => Err(self.invalid(format!("unknown MatrixTransform resource {key:?}"))),
            };
        }
        parse_matrix(value, self.document, "RenderTransform")
    }

    fn geometry_value(&mut self, value: &str) -> Result<XpsPathGeometry, DwfError> {
        if let Some(key) = static_resource_key(value) {
            return match find_resource(&self.resource_scopes, key).cloned() {
                Some(ResourceValue::Geometry(geometry)) => {
                    self.charge_segments(geometry.segment_count())?;
                    Ok(geometry.as_ref().clone())
                }
                Some(_) => Err(self.invalid(format!("resource {key:?} is not PathGeometry"))),
                None => Err(self.invalid(format!("unknown PathGeometry resource {key:?}"))),
            };
        }
        self.parse_geometry(value)
    }

    fn parse_geometry(&mut self, value: &str) -> Result<XpsPathGeometry, DwfError> {
        parse_abbreviated_geometry(
            value,
            self.document,
            &mut self.segments_remaining,
            self.options.max_xps_path_segments,
            true,
        )
    }

    fn parse_figures(&mut self, value: &str) -> Result<XpsPathGeometry, DwfError> {
        parse_abbreviated_geometry(
            value,
            self.document,
            &mut self.segments_remaining,
            self.options.max_xps_path_segments,
            false,
        )
    }

    fn charge_segments(&mut self, count: usize) -> Result<(), DwfError> {
        if count > self.segments_remaining {
            return Err(DwfError::XpsPathSegmentLimitExceeded {
                page: self.document.to_owned(),
                limit: self.options.max_xps_path_segments,
            });
        }
        self.segments_remaining -= count;
        Ok(())
    }

    fn warn_unsupported_brushes(&mut self, style: &XpsStyle, offset: usize) {
        for brush in [style.fill.as_ref(), style.stroke.as_ref()]
            .into_iter()
            .flatten()
        {
            if let XpsBrush::Unsupported { brush_type, .. } = brush {
                self.warning(
                    "unsupported_xps_brush",
                    format!(
                        "{brush_type} is retained in raw data but has no resolved solid preview"
                    ),
                    offset,
                );
            }
        }
    }

    fn push_entity(&mut self, entity: XpsEntity) -> Result<(), DwfError> {
        let page = self.page.as_mut().expect("FixedPage parsed first");
        if page.entities.len() >= self.options.max_xps_visuals {
            return Err(DwfError::XpsVisualLimitExceeded {
                page: self.document.to_owned(),
                limit: self.options.max_xps_visuals,
            });
        }
        page.entities.push(entity);
        Ok(())
    }

    fn warning(&mut self, code: &str, message: String, offset: usize) {
        if let Some(page) = &mut self.page {
            page.diagnostics.push(Diagnostic {
                code: code.to_owned(),
                severity: DiagnosticSeverity::Warning,
                message,
                action: "preserved_raw".to_owned(),
                section: Some(page.name.clone()),
                resource: Some(self.document.to_owned()),
                offset: Some(offset),
                details: BTreeMap::new(),
            });
        }
    }

    fn invalid(&self, context: impl Into<String>) -> DwfError {
        DwfError::InvalidXps {
            part: self.document.to_owned(),
            context: context.into(),
        }
    }
}

fn find_resource<'a>(
    scopes: &'a [BTreeMap<String, ResourceValue>],
    key: &str,
) -> Option<&'a ResourceValue> {
    scopes.iter().rev().find_map(|scope| scope.get(key))
}

fn merged_active_resource_scopes(
    scopes: &[BTreeMap<String, ResourceValue>],
) -> BTreeMap<String, ResourceValue> {
    let mut output = BTreeMap::new();
    for scope in scopes {
        output.extend(
            scope
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    output
}

fn parse_style(
    values: &BTreeMap<String, String>,
    document: &str,
    resources: &[BTreeMap<String, ResourceValue>],
) -> Result<XpsStyle, DwfError> {
    let stroke_thickness =
        parse_optional(values, "StrokeThickness", document, "Path")?.unwrap_or(1.0);
    if stroke_thickness < 0.0 {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "Path.StrokeThickness must be non-negative".to_owned(),
        });
    }
    let stroke_dash_array = values
        .get("StrokeDashArray")
        .map(|value| parse_numbers(value, document, "Path.StrokeDashArray"))
        .transpose()?
        .unwrap_or_default();
    if stroke_dash_array.iter().any(|value| *value < 0.0) {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: "Path.StrokeDashArray values must be non-negative".to_owned(),
        });
    }
    Ok(XpsStyle {
        fill: values
            .get("Fill")
            .map(|value| parse_brush_value(value, document, resources))
            .transpose()?,
        stroke: values
            .get("Stroke")
            .map(|value| parse_brush_value(value, document, resources))
            .transpose()?,
        stroke_thickness,
        stroke_dash_array,
        stroke_dash_offset: parse_optional(values, "StrokeDashOffset", document, "Path")?
            .unwrap_or(0.0),
        stroke_start_line_cap: values.get("StrokeStartLineCap").cloned(),
        stroke_end_line_cap: values.get("StrokeEndLineCap").cloned(),
        stroke_dash_cap: values.get("StrokeDashCap").cloned(),
        stroke_line_join: values.get("StrokeLineJoin").cloned(),
        stroke_miter_limit: parse_optional(values, "StrokeMiterLimit", document, "Path")?,
        opacity: parse_optional_opacity(values, document, "visual")?.unwrap_or(1.0),
    })
}

fn parse_brush_value(
    value: &str,
    document: &str,
    resources: &[BTreeMap<String, ResourceValue>],
) -> Result<XpsBrush, DwfError> {
    if let Some(key) = static_resource_key(value) {
        return match find_resource(resources, key) {
            Some(ResourceValue::Brush(brush)) => Ok(brush.as_ref().clone()),
            Some(_) => Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("resource {key:?} is not a brush"),
            }),
            None => Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("unknown brush resource {key:?}"),
            }),
        };
    }
    if value.trim_start().starts_with("ContextColor ") {
        return Ok(XpsBrush::Unsupported {
            brush_type: "ContextColor".to_owned(),
            attributes: BTreeMap::from([("Color".to_owned(), value.to_owned())]),
        });
    }
    Ok(XpsBrush::Solid {
        color: parse_color(value, document)?,
        opacity: 1.0,
        attributes: BTreeMap::from([("Color".to_owned(), value.to_owned())]),
    })
}

fn parse_brush_element(
    name: &str,
    values: &BTreeMap<String, String>,
    document: &str,
    resources: &[BTreeMap<String, ResourceValue>],
) -> Result<XpsBrush, DwfError> {
    let opacity = parse_optional_opacity(values, document, name)?.unwrap_or(1.0);
    let transform = parse_resource_matrix(
        values.get("Transform").map(String::as_str),
        document,
        name,
        resources,
    )?;
    match name {
        "SolidColorBrush" => {
            let value = required(values, "Color", document, "SolidColorBrush")?;
            if value.trim_start().starts_with("ContextColor ") {
                Ok(XpsBrush::Unsupported {
                    brush_type: "SolidColorBrush.ContextColor".to_owned(),
                    attributes: values.clone(),
                })
            } else {
                Ok(XpsBrush::Solid {
                    color: parse_color(&value, document)?,
                    opacity,
                    attributes: values.clone(),
                })
            }
        }
        "ImageBrush" => Ok(XpsBrush::Image {
            source: image_source(&required(values, "ImageSource", document, "ImageBrush")?),
            resource_part: document.to_owned(),
            normalized_source: None,
            content_type: None,
            data: Vec::new(),
            image_metadata: None,
            viewbox: Some(parse_required_box(values, "Viewbox", document, name)?),
            viewport: Some(parse_required_box(values, "Viewport", document, name)?),
            viewbox_units: absolute_units(values, "ViewboxUnits", document, name)?,
            viewport_units: absolute_units(values, "ViewportUnits", document, name)?,
            tile_mode: parse_tile_mode(values, document, name)?,
            transform,
            opacity,
            attributes: values.clone(),
        }),
        "VisualBrush" => {
            let visual = if let Some(value) = values.get("Visual") {
                let key = static_resource_key(value).ok_or_else(|| DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: "VisualBrush.Visual must reference a static visual resource"
                        .to_owned(),
                })?;
                match find_resource(resources, key) {
                    Some(ResourceValue::Visual(visual)) => Some(visual.clone()),
                    Some(_) => {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: format!("resource {key:?} is not a visual"),
                        });
                    }
                    None => {
                        return Err(DwfError::InvalidXps {
                            part: document.to_owned(),
                            context: format!("unknown visual resource {key:?}"),
                        });
                    }
                }
            } else {
                None
            };
            Ok(XpsBrush::Visual {
                visual,
                viewbox: parse_required_box(values, "Viewbox", document, name)?,
                viewport: parse_required_box(values, "Viewport", document, name)?,
                viewbox_units: absolute_units(values, "ViewboxUnits", document, name)?,
                viewport_units: absolute_units(values, "ViewportUnits", document, name)?,
                tile_mode: parse_tile_mode(values, document, name)?,
                transform,
                opacity,
                attributes: values.clone(),
            })
        }
        "LinearGradientBrush" => {
            let mapping_mode = absolute_mapping_mode(values, document, name)?;
            Ok(XpsBrush::LinearGradient {
                start_point: parse_point(
                    &required(values, "StartPoint", document, name)?,
                    document,
                    "LinearGradientBrush.StartPoint",
                )?,
                end_point: parse_point(
                    &required(values, "EndPoint", document, name)?,
                    document,
                    "LinearGradientBrush.EndPoint",
                )?,
                spread_method: gradient_spread_method(values, document, name)?,
                mapping_mode,
                transform,
                gradient_stops: Vec::new(),
                opacity,
                attributes: values.clone(),
            })
        }
        "RadialGradientBrush" => {
            let radius_x = parse_required_f64(values, "RadiusX", document, name)?;
            let radius_y = parse_required_f64(values, "RadiusY", document, name)?;
            if radius_x < 0.0 || radius_y < 0.0 {
                return Err(DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: "RadialGradientBrush radii must be non-negative".to_owned(),
                });
            }
            Ok(XpsBrush::RadialGradient {
                center: parse_point(
                    &required(values, "Center", document, name)?,
                    document,
                    "RadialGradientBrush.Center",
                )?,
                gradient_origin: parse_point(
                    &required(values, "GradientOrigin", document, name)?,
                    document,
                    "RadialGradientBrush.GradientOrigin",
                )?,
                radius_x,
                radius_y,
                spread_method: gradient_spread_method(values, document, name)?,
                mapping_mode: absolute_mapping_mode(values, document, name)?,
                transform,
                gradient_stops: Vec::new(),
                opacity,
                attributes: values.clone(),
            })
        }
        _ => Ok(XpsBrush::Unsupported {
            brush_type: name.to_owned(),
            attributes: values.clone(),
        }),
    }
}

fn parse_resource_matrix(
    value: Option<&str>,
    document: &str,
    context: &str,
    resources: &[BTreeMap<String, ResourceValue>],
) -> Result<XpsMatrix, DwfError> {
    let Some(value) = value else {
        return Ok(XpsMatrix::IDENTITY);
    };
    if let Some(key) = static_resource_key(value) {
        return match find_resource(resources, key) {
            Some(ResourceValue::Matrix(matrix)) => Ok(*matrix),
            Some(_) => Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("resource {key:?} is not a MatrixTransform"),
            }),
            None => Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("unknown MatrixTransform resource {key:?}"),
            }),
        };
    }
    parse_matrix(value, document, &format!("{context}.Transform"))
}

fn absolute_units(
    values: &BTreeMap<String, String>,
    attribute: &str,
    document: &str,
    context: &str,
) -> Result<String, DwfError> {
    let value = values
        .get(attribute)
        .cloned()
        .unwrap_or_else(|| "Absolute".to_owned());
    if !value.eq_ignore_ascii_case("Absolute") {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{context}.{attribute} must be Absolute, got {value:?}"),
        });
    }
    Ok("Absolute".to_owned())
}

fn absolute_mapping_mode(
    values: &BTreeMap<String, String>,
    document: &str,
    context: &str,
) -> Result<String, DwfError> {
    absolute_units(values, "MappingMode", document, context)
}

fn gradient_spread_method(
    values: &BTreeMap<String, String>,
    document: &str,
    context: &str,
) -> Result<String, DwfError> {
    let value = values
        .get("SpreadMethod")
        .cloned()
        .unwrap_or_else(|| "Pad".to_owned());
    if !["pad", "reflect", "repeat"].contains(&value.to_ascii_lowercase().as_str()) {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{context}.SpreadMethod is invalid: {value:?}"),
        });
    }
    Ok(value)
}

fn parse_tile_mode(
    values: &BTreeMap<String, String>,
    document: &str,
    context: &str,
) -> Result<Option<String>, DwfError> {
    let Some(value) = values.get("TileMode") else {
        return Ok(None);
    };
    if !["none", "tile", "flipx", "flipy", "flipxy"].contains(&value.to_ascii_lowercase().as_str())
    {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{context}.TileMode is invalid: {value:?}"),
        });
    }
    Ok(Some(value.clone()))
}

fn hydrate_page_resources(
    page: &mut XpsPage,
    archive: &PackageArchive<'_>,
    content_types: &[OpcContentType],
    options: ParseOptions,
    diagnostics: &mut Vec<Diagnostic>,
    font_cache: &mut BTreeMap<String, PreparedFontResource>,
    resolve_glyph_outlines: bool,
) -> Result<(), DwfError> {
    let page_part = page.part_name.clone();
    let page_name = page.name.clone();
    for dictionary in &page.resource_dictionaries {
        warn_if_unrelated(
            diagnostics,
            &page_part,
            dictionary,
            &page.relationships,
            "remote resource dictionary",
        );
    }
    let context = HydrateContext {
        page_part: &page_part,
        page_name: &page_name,
        archive,
        content_types,
        options,
        relationships: &page.relationships,
        resolve_glyph_outlines,
    };
    let mut visual_cache = BTreeMap::new();
    let mut outline_segments_remaining = options.max_xps_path_segments;
    for entity in &mut page.entities {
        hydrate_entity_resources(
            entity,
            &context,
            diagnostics,
            font_cache,
            &mut visual_cache,
            &mut outline_segments_remaining,
        )?;
    }
    Ok(())
}

struct HydrateContext<'a, 'archive> {
    page_part: &'a str,
    page_name: &'a str,
    archive: &'a PackageArchive<'archive>,
    content_types: &'a [OpcContentType],
    options: ParseOptions,
    relationships: &'a [OpcRelationship],
    resolve_glyph_outlines: bool,
}

struct PreparedFontResource {
    data: Result<Arc<Vec<u8>>, String>,
    content_type: Option<String>,
    obfuscated: bool,
}

fn hydrate_entity_resources(
    entity: &mut XpsEntity,
    context: &HydrateContext<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
    font_cache: &mut BTreeMap<String, PreparedFontResource>,
    visual_cache: &mut BTreeMap<usize, Arc<XpsVisual>>,
    outline_segments_remaining: &mut usize,
) -> Result<(), DwfError> {
    if let XpsGeometry::Glyphs { glyphs } = &mut entity.geometry {
        let normalized =
            resolve_internal_target(Some(&glyphs.font_resource_part), &glyphs.font_uri)?;
        if context.archive.contains(&normalized) {
            warn_if_unrelated(
                diagnostics,
                context.page_part,
                &normalized,
                context.relationships,
                "font",
            );
            let content_type = content_type_for(&normalized, context.content_types);
            glyphs.font_part = Some(normalized.clone());
            glyphs.font_content_type = content_type.clone();
            glyphs.font_obfuscated = is_obfuscated_font(content_type.as_deref());
            if context.resolve_glyph_outlines && !font_cache.contains_key(&normalized) {
                let bytes = context
                    .archive
                    .read_entry(&normalized, context.options.max_entry_size)?;
                let obfuscated = is_obfuscated_font(content_type.as_deref());
                let data =
                    prepare_font_data(&bytes, &normalized, content_type.as_deref()).map(Arc::new);
                font_cache.insert(
                    normalized.clone(),
                    PreparedFontResource {
                        data,
                        content_type,
                        obfuscated,
                    },
                );
            }
            if context.resolve_glyph_outlines {
                let font = font_cache.get(&normalized).expect("inserted above");
                glyphs.font_content_type = font.content_type.clone();
                glyphs.font_obfuscated = font.obfuscated;
                match &font.data {
                    Ok(data) => match build_glyph_outline(glyphs, data) {
                        Ok(outline) => {
                            let segment_count =
                                outline.as_ref().map_or(0, XpsPathGeometry::segment_count);
                            if segment_count > *outline_segments_remaining {
                                return Err(DwfError::XpsPathSegmentLimitExceeded {
                                    page: context.page_part.to_owned(),
                                    limit: context.options.max_xps_path_segments,
                                });
                            }
                            *outline_segments_remaining -= segment_count;
                            glyphs.outline = outline;
                        }
                        Err(error) => push_glyph_outline_warning(
                            diagnostics,
                            context,
                            &entity.source,
                            &normalized,
                            &error,
                        ),
                    },
                    Err(error) => push_glyph_outline_warning(
                        diagnostics,
                        context,
                        &entity.source,
                        &normalized,
                        error,
                    ),
                }
            }
            glyphs.normalized_font_uri = Some(normalized);
        } else {
            diagnostics.push(resource_warning(
                context.page_part,
                context.page_name,
                &entity.source,
                "missing_xps_font",
                format!("font part {:?} is missing", glyphs.font_uri),
            ));
        }
    }
    hydrate_brush(
        &mut entity.style.fill,
        context,
        diagnostics,
        font_cache,
        visual_cache,
        outline_segments_remaining,
    )?;
    hydrate_brush(
        &mut entity.style.stroke,
        context,
        diagnostics,
        font_cache,
        visual_cache,
        outline_segments_remaining,
    )?;
    hydrate_brush(
        &mut entity.opacity_mask,
        context,
        diagnostics,
        font_cache,
        visual_cache,
        outline_segments_remaining,
    )?;
    for mask in &mut entity.opacity_mask_chain {
        hydrate_brush_value(
            Arc::make_mut(&mut mask.brush),
            context,
            diagnostics,
            font_cache,
            visual_cache,
            outline_segments_remaining,
        )?;
    }
    for group in &mut entity.canvas_groups {
        if let Some(mask) = &mut group.opacity_mask {
            hydrate_brush_value(
                Arc::make_mut(mask),
                context,
                diagnostics,
                font_cache,
                visual_cache,
                outline_segments_remaining,
            )?;
        }
    }
    Ok(())
}

fn push_glyph_outline_warning(
    diagnostics: &mut Vec<Diagnostic>,
    context: &HydrateContext<'_, '_>,
    source: &XpsSourceSpan,
    font_part: &str,
    error: &str,
) {
    let message = format!("packaged font {font_part:?} could not produce glyph outlines: {error}");
    if diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "xps_glyph_outline_failed"
            && diagnostic.resource.as_deref() == Some(context.page_part)
            && diagnostic.message == message
    }) {
        return;
    }
    let mut diagnostic = resource_warning(
        context.page_part,
        context.page_name,
        source,
        "xps_glyph_outline_failed",
        message,
    );
    diagnostic.action = "rendered_unicode_fallback".to_owned();
    diagnostics.push(diagnostic);
}

fn hydrate_brush(
    brush: &mut Option<XpsBrush>,
    context: &HydrateContext<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
    font_cache: &mut BTreeMap<String, PreparedFontResource>,
    visual_cache: &mut BTreeMap<usize, Arc<XpsVisual>>,
    outline_segments_remaining: &mut usize,
) -> Result<(), DwfError> {
    let Some(brush) = brush else {
        return Ok(());
    };
    hydrate_brush_value(
        brush,
        context,
        diagnostics,
        font_cache,
        visual_cache,
        outline_segments_remaining,
    )
}

fn hydrate_brush_value(
    brush: &mut XpsBrush,
    context: &HydrateContext<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
    font_cache: &mut BTreeMap<String, PreparedFontResource>,
    visual_cache: &mut BTreeMap<usize, Arc<XpsVisual>>,
    outline_segments_remaining: &mut usize,
) -> Result<(), DwfError> {
    match brush {
        XpsBrush::Image {
            source,
            resource_part,
            normalized_source,
            content_type,
            data,
            image_metadata: metadata,
            ..
        } => {
            let normalized = resolve_internal_target(Some(resource_part), source)?;
            require_part(context.archive, resource_part, source, &normalized)?;
            let bytes = context
                .archive
                .read_entry(&normalized, context.options.max_entry_size)?;
            *metadata = image_metadata(&bytes);
            *content_type = content_type_for(&normalized, context.content_types);
            *normalized_source = Some(normalized.clone());
            *data = bytes;
            warn_if_unrelated(
                diagnostics,
                context.page_part,
                &normalized,
                context.relationships,
                "image",
            );
        }
        XpsBrush::Visual {
            visual: Some(visual),
            ..
        } => {
            let key = Arc::as_ptr(visual) as usize;
            if let Some(hydrated) = visual_cache.get(&key) {
                *visual = hydrated.clone();
                return Ok(());
            }
            let mut hydrated = visual.as_ref().clone();
            for entity in &mut hydrated.entities {
                hydrate_entity_resources(
                    entity,
                    context,
                    diagnostics,
                    font_cache,
                    visual_cache,
                    outline_segments_remaining,
                )?;
            }
            let hydrated = Arc::new(hydrated);
            visual_cache.insert(key, hydrated.clone());
            *visual = hydrated;
        }
        _ => {}
    }
    Ok(())
}

fn content_type_for(part: &str, values: &[OpcContentType]) -> Option<String> {
    if let Some(value) = values
        .iter()
        .find(|value| value.part_name.as_deref() == Some(part))
    {
        return Some(value.content_type.clone());
    }
    let extension = part.rsplit_once('.')?.1.to_ascii_lowercase();
    values
        .iter()
        .find(|value| value.extension.as_deref() == Some(extension.as_str()))
        .map(|value| value.content_type.clone())
}

fn read_part_relationships(
    archive: &PackageArchive<'_>,
    source: &str,
    options: ParseOptions,
    all: &mut Vec<OpcRelationship>,
) -> Result<Vec<OpcRelationship>, DwfError> {
    let relationship_part = relationship_part_name(source)?;
    if !archive.contains(&relationship_part) {
        return Ok(Vec::new());
    }
    let xml = archive.read_entry(&relationship_part, options.max_xml_size)?;
    let relationships = parse_relationships(&xml, &relationship_part, Some(source), options)?;
    all.extend(relationships.iter().cloned());
    Ok(relationships)
}

fn relationship_part_name(source: &str) -> Result<String, DwfError> {
    let (directory, filename) = source.rsplit_once('/').unwrap_or(("", source));
    if filename.is_empty() {
        return Err(invalid_opc(source, "source part has no file name"));
    }
    Ok(if directory.is_empty() {
        format!("_rels/{filename}.rels")
    } else {
        format!("{directory}/_rels/{filename}.rels")
    })
}

fn resolve_internal_target(source: Option<&str>, target: &str) -> Result<String, DwfError> {
    if target.is_empty() {
        return Err(invalid_opc(
            source.unwrap_or("/"),
            "relationship target is empty",
        ));
    }
    if target.contains('\0') || target.contains('\\') {
        return Err(invalid_opc(
            source.unwrap_or("/"),
            "relationship target contains NUL or backslash",
        ));
    }
    let target = target.split('#').next().unwrap_or(target);
    if target.contains('?') {
        return Err(invalid_opc(
            source.unwrap_or("/"),
            "internal relationship target contains a query component",
        ));
    }
    if target
        .split('/')
        .next()
        .is_some_and(|component| component.contains(':'))
    {
        return Err(invalid_opc(
            source.unwrap_or("/"),
            "internal relationship target is an absolute URI",
        ));
    }
    let mut parts = Vec::new();
    if !target.starts_with('/') {
        if let Some(source) = source {
            if let Some((directory, _)) = source.rsplit_once('/') {
                parts.extend(directory.split('/').filter(|part| !part.is_empty()));
            }
        }
    }
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid_opc(
                        source.unwrap_or("/"),
                        format!("target {target:?} escapes the package root"),
                    ));
                }
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err(invalid_opc(
            source.unwrap_or("/"),
            "relationship target resolves to the package root",
        ));
    }
    Ok(parts.join("/"))
}

fn normalize_absolute_part_name(value: &str) -> Result<String, DwfError> {
    if !value.starts_with('/') {
        return Err(invalid_opc(
            CONTENT_TYPES_PART,
            format!("Override PartName must start with '/': {value:?}"),
        ));
    }
    resolve_internal_target(None, value)
}

fn require_part(
    archive: &PackageArchive<'_>,
    source: &str,
    target: &str,
    normalized: &str,
) -> Result<(), DwfError> {
    if !archive.contains(normalized) {
        return Err(DwfError::MissingOpcPart {
            source_part: source.to_owned(),
            target: target.to_owned(),
            normalized: normalized.to_owned(),
        });
    }
    Ok(())
}

fn warn_if_unrelated(
    diagnostics: &mut Vec<Diagnostic>,
    source: &str,
    target: &str,
    relationships: &[OpcRelationship],
    kind: &str,
) {
    if relationships.iter().any(|relationship| {
        relationship.target_mode.eq_ignore_ascii_case("Internal")
            && relationship
                .relationship_type
                .to_ascii_lowercase()
                .ends_with(REQUIRED_RESOURCE_RELATIONSHIP_SUFFIX)
            && relationship.normalized_target.as_deref() == Some(target)
    }) {
        return;
    }
    diagnostics.push(Diagnostic {
        code: "missing_xps_relationship".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "{kind} part {target:?} is referenced by markup but not by a relationship"
        ),
        action: "parsed_direct_reference".to_owned(),
        section: None,
        resource: Some(source.to_owned()),
        offset: None,
        details: BTreeMap::new(),
    });
}

fn resource_warning(
    page_part: &str,
    page_name: &str,
    source: &XpsSourceSpan,
    code: &str,
    message: String,
) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Warning,
        message,
        action: "preserved_reference".to_owned(),
        section: Some(page_name.to_owned()),
        resource: Some(page_part.to_owned()),
        offset: Some(source.offset),
        details: BTreeMap::new(),
    }
}

fn parse_matrix(value: &str, document: &str, context: &str) -> Result<XpsMatrix, DwfError> {
    let values = parse_numbers(value, document, context)?;
    let [m11, m12, m21, m22, offset_x, offset_y]: [f64; 6] =
        values
            .try_into()
            .map_err(|values: Vec<f64>| DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("{context} requires 6 numbers, got {}", values.len()),
            })?;
    Ok(XpsMatrix {
        m11,
        m12,
        m21,
        m22,
        offset_x,
        offset_y,
    })
}

fn parse_points(value: &str, document: &str, context: &str) -> Result<Vec<XpsPoint>, DwfError> {
    let numbers = parse_numbers(value, document, context)?;
    if numbers.len() % 2 != 0 {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!(
                "{context} requires coordinate pairs, got {} numbers",
                numbers.len()
            ),
        });
    }
    Ok(numbers
        .chunks_exact(2)
        .map(|pair| XpsPoint {
            x: pair[0],
            y: pair[1],
        })
        .collect())
}

fn parse_point(value: &str, document: &str, context: &str) -> Result<XpsPoint, DwfError> {
    let values = parse_points(value, document, context)?;
    let [point]: [XpsPoint; 1] =
        values
            .try_into()
            .map_err(|values: Vec<XpsPoint>| DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("{context} requires one point, got {}", values.len()),
            })?;
    Ok(point)
}

fn parse_numbers(value: &str, document: &str, context: &str) -> Result<Vec<f64>, DwfError> {
    value
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let value = part.parse::<f64>().map_err(|error| DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("invalid {context} number {part:?}: {error}"),
            })?;
            if !value.is_finite() {
                return Err(DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: format!("{context} contains a non-finite number"),
                });
            }
            Ok(value)
        })
        .collect()
}

fn parse_required_f64(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<f64, DwfError> {
    let value = required(values, key, document, element)?;
    parse_f64(&value, document, &format!("{element}.{key}"))
}

fn parse_f64(value: &str, document: &str, context: &str) -> Result<f64, DwfError> {
    let value = value.parse::<f64>().map_err(|error| DwfError::InvalidXps {
        part: document.to_owned(),
        context: format!("invalid {context} number: {error}"),
    })?;
    if !value.is_finite() {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{context} is not finite"),
        });
    }
    Ok(value)
}

fn parse_optional<T: std::str::FromStr>(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<Option<T>, DwfError>
where
    T::Err: std::fmt::Display,
{
    values
        .get(key)
        .map(|value| {
            value.parse::<T>().map_err(|error| DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("invalid {element}.{key} value {value:?}: {error}"),
            })
        })
        .transpose()
}

fn parse_optional_bool(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<Option<bool>, DwfError> {
    values
        .get(key)
        .map(|value| match value.as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(DwfError::InvalidXps {
                part: document.to_owned(),
                context: format!("invalid {element}.{key} boolean {value:?}"),
            }),
        })
        .transpose()
}

fn parse_optional_opacity(
    values: &BTreeMap<String, String>,
    document: &str,
    element: &str,
) -> Result<Option<f64>, DwfError> {
    let value = values
        .get("Opacity")
        .map(|value| parse_f64(value, document, &format!("{element}.Opacity")))
        .transpose()?;
    if value.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{element}.Opacity must be between 0 and 1"),
        });
    }
    Ok(value)
}

fn parse_optional_box(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<Option<[f64; 4]>, DwfError> {
    values
        .get(key)
        .map(|value| {
            let numbers = parse_numbers(value, document, &format!("{element}.{key}"))?;
            numbers
                .try_into()
                .map_err(|values: Vec<f64>| DwfError::InvalidXps {
                    part: document.to_owned(),
                    context: format!("{element}.{key} requires 4 numbers, got {}", values.len()),
                })
        })
        .transpose()
}

fn parse_required_box(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<[f64; 4], DwfError> {
    let value = required(values, key, document, element)?;
    let numbers = parse_numbers(&value, document, &format!("{element}.{key}"))?;
    let value: [f64; 4] = numbers
        .try_into()
        .map_err(|values: Vec<f64>| DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{element}.{key} requires 4 numbers, got {}", values.len()),
        })?;
    if value[2] < 0.0 || value[3] < 0.0 {
        return Err(DwfError::InvalidXps {
            part: document.to_owned(),
            context: format!("{element}.{key} width and height must be non-negative"),
        });
    }
    Ok(value)
}

fn parse_fill_rule(value: Option<&String>) -> Result<String, DwfError> {
    match value.map(String::as_str).unwrap_or("EvenOdd") {
        "EvenOdd" | "evenodd" | "even_odd" => Ok("even_odd".to_owned()),
        "NonZero" | "nonzero" => Ok("nonzero".to_owned()),
        value => Err(DwfError::InvalidXps {
            part: "<PathGeometry>".to_owned(),
            context: format!("invalid FillRule {value:?}"),
        }),
    }
}

fn parse_color(value: &str, document: &str) -> Result<[u8; 4], DwfError> {
    let value = value.trim();
    let color = if value.starts_with("sc#") {
        parse_scrgb_color(value, document)?
    } else if let Some(hex) = value.strip_prefix('#') {
        match hex.len() {
            3 => [
                expand_hex(hex.as_bytes()[0])?,
                expand_hex(hex.as_bytes()[1])?,
                expand_hex(hex.as_bytes()[2])?,
                255,
            ],
            4 => [
                expand_hex(hex.as_bytes()[1])?,
                expand_hex(hex.as_bytes()[2])?,
                expand_hex(hex.as_bytes()[3])?,
                expand_hex(hex.as_bytes()[0])?,
            ],
            6 => [
                hex_byte(&hex[0..2])?,
                hex_byte(&hex[2..4])?,
                hex_byte(&hex[4..6])?,
                255,
            ],
            8 => [
                hex_byte(&hex[2..4])?,
                hex_byte(&hex[4..6])?,
                hex_byte(&hex[6..8])?,
                hex_byte(&hex[0..2])?,
            ],
            _ => return Err(color_error(document, value)),
        }
    } else {
        match value.to_ascii_lowercase().as_str() {
            "black" => [0, 0, 0, 255],
            "white" => [255, 255, 255, 255],
            "red" => [255, 0, 0, 255],
            "green" => [0, 128, 0, 255],
            "blue" => [0, 0, 255, 255],
            "gray" | "grey" => [128, 128, 128, 255],
            "transparent" => [255, 255, 255, 0],
            _ => return Err(color_error(document, value)),
        }
    };
    Ok(color)
}

fn parse_scrgb_color(value: &str, document: &str) -> Result<[u8; 4], DwfError> {
    let channels = value
        .strip_prefix("sc#")
        .expect("prefix checked")
        .split(',')
        .map(str::trim)
        .map(|channel| {
            if channel.is_empty() || channel.contains(['e', 'E']) {
                return Err(color_error(document, value));
            }
            let channel = channel
                .parse::<f64>()
                .map_err(|_| color_error(document, value))?;
            if !channel.is_finite() {
                return Err(color_error(document, value));
            }
            Ok(channel)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (alpha, rgb) = match channels.as_slice() {
        [red, green, blue] => (1.0, [*red, *green, *blue]),
        [alpha, red, green, blue] => (*alpha, [*red, *green, *blue]),
        _ => return Err(color_error(document, value)),
    };
    Ok([
        scrgb_channel_to_srgb(rgb[0]),
        scrgb_channel_to_srgb(rgb[1]),
        scrgb_channel_to_srgb(rgb[2]),
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ])
}

fn scrgb_channel_to_srgb(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

fn expand_hex(value: u8) -> Result<u8, DwfError> {
    let digit = char::from(value)
        .to_digit(16)
        .ok_or_else(|| color_error("<color>", "hex"))?;
    Ok((digit as u8) * 17)
}

fn hex_byte(value: &str) -> Result<u8, DwfError> {
    u8::from_str_radix(value, 16).map_err(|_| color_error("<color>", value))
}

fn color_error(document: &str, value: &str) -> DwfError {
    DwfError::InvalidXps {
        part: document.to_owned(),
        context: format!("unsupported or invalid XPS color {value:?}"),
    }
}

fn static_resource_key(value: &str) -> Option<&str> {
    let value = value.trim();
    value
        .strip_prefix("{StaticResource")?
        .strip_suffix('}')
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

fn image_source(value: &str) -> String {
    let value = value.trim();
    if let Some(body) = value
        .strip_prefix("{ColorConvertedBitmap")
        .and_then(|body| body.strip_suffix('}'))
    {
        return body
            .split_ascii_whitespace()
            .next()
            .unwrap_or(body)
            .to_owned();
    }
    value.to_owned()
}

fn page_name(part: &str) -> String {
    part.rsplit_once('/')
        .map_or(part, |(_, name)| name)
        .trim_end_matches(".fpage")
        .to_owned()
}

fn check_xml_size(xml: &[u8], document: &str, options: ParseOptions) -> Result<(), DwfError> {
    if xml.len() > options.max_xml_size {
        return Err(DwfError::XmlSizeLimitExceeded {
            document: document.to_owned(),
            actual: xml.len(),
            limit: options.max_xml_size,
        });
    }
    Ok(())
}

fn check_depth(stack: &[String], document: &str, options: ParseOptions) -> Result<(), DwfError> {
    if stack.len() > options.max_xml_depth {
        return Err(DwfError::XmlDepthLimitExceeded {
            document: document.to_owned(),
            limit: options.max_xml_depth,
        });
    }
    Ok(())
}

fn pop_element(stack: &mut Vec<String>, name: &[u8], document: &str) -> Result<(), DwfError> {
    let name = local_name_string(name, document)?;
    let open = stack.pop().ok_or_else(|| DwfError::InvalidXml {
        document: document.to_owned(),
        context: format!("unexpected closing element {name:?}"),
    })?;
    if open != name {
        return Err(DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!("closing element {name:?} does not match {open:?}"),
        });
    }
    Ok(())
}

fn doctype_error(document: &str) -> DwfError {
    DwfError::InvalidXml {
        document: document.to_owned(),
        context: "DOCTYPE declarations are not allowed".to_owned(),
    }
}

fn invalid_opc(part: &str, context: impl Into<String>) -> DwfError {
    DwfError::InvalidOpc {
        part: part.to_owned(),
        context: context.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    fn fixture() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut output);
            let options = SimpleFileOptions::default();
            let entries: [(&str, &[u8]); 7] = [
                (CONTENT_TYPES_PART, br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="fdseq" ContentType="application/vnd.ms-package.xps-fixeddocumentsequence+xml"/><Default Extension="fdoc" ContentType="application/vnd.ms-package.xps-fixeddocument+xml"/><Default Extension="fpage" ContentType="application/vnd.ms-package.xps-fixedpage+xml"/></Types>"#),
                (ROOT_RELATIONSHIPS_PART, br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/Documents/1/FixedDocumentSequence.fdseq"/></Relationships>"#),
                ("Documents/1/FixedDocumentSequence.fdseq", br#"<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06"><DocumentReference Source="FixedDocument.fdoc"/></FixedDocumentSequence>"#),
                ("Documents/1/_rels/FixedDocumentSequence.fdseq.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="d" Type="http://schemas.microsoft.com/xps/2005/06/fixeddocument" Target="FixedDocument.fdoc"/></Relationships>"#),
                ("Documents/1/FixedDocument.fdoc", br#"<FixedDocument xmlns="http://schemas.microsoft.com/xps/2005/06"><PageContent Source="Pages/1.fpage"/></FixedDocument>"#),
                ("Documents/1/_rels/FixedDocument.fdoc.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="p" Type="http://schemas.microsoft.com/xps/2005/06/fixedpage" Target="Pages/1.fpage"/></Relationships>"#),
                ("Documents/1/Pages/1.fpage", br##"<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="100" Height="50"><Canvas RenderTransform="1,0,0,1,2,3"><Path Data="M 0,0 L 10,0 10,10 Z" Fill="#ff112233" Stroke="#445566"/><Glyphs FontUri="../Resources/font.odttf" FontRenderingEmSize="12" OriginX="4" OriginY="20" UnicodeString="test" Fill="#000000"/></Canvas></FixedPage>"##),
            ];
            for (name, data) in entries {
                archive.start_file(name, options).unwrap();
                archive.write_all(data).unwrap();
            }
            archive.finish().unwrap();
        }
        output.into_inner()
    }

    #[test]
    fn resolves_opc_graph_and_fixed_page() {
        let package = inspect_dwfx(&fixture(), ParseOptions::default()).unwrap();
        assert_eq!(
            package.document_sequence,
            "Documents/1/FixedDocumentSequence.fdseq"
        );
        assert_eq!(package.sheet_count(), 1);
        assert_eq!(package.entity_count(), 2);
        let page = package.pages().next().unwrap();
        assert_eq!((page.width, page.height), (100.0, 50.0));
        assert_eq!(page.entities[0].transform.offset_x, 2.0);
        assert!(matches!(
            page.entities[0].geometry,
            XpsGeometry::Path { .. }
        ));
    }

    #[test]
    fn rejects_relationship_escape() {
        assert!(resolve_internal_target(Some("Documents/1/a.fdoc"), "../../../x").is_err());
    }

    #[test]
    fn parses_scrgb_and_preserves_context_color() {
        assert_eq!(
            parse_color("sc#0.5,1.0,0.0", "p").unwrap(),
            [188, 255, 0, 255]
        );
        assert_eq!(
            parse_color("sc#0.25,0.0,0.0,0.0", "p").unwrap(),
            [0, 0, 0, 64]
        );
        assert!(parse_color("sc#1e0,0,0", "p").is_err());

        let brush =
            parse_brush_value("ContextColor /profile.icc 1.0,0.2,0.3,0.4", "p", &[]).unwrap();
        assert!(matches!(
            brush,
            XpsBrush::Unsupported { ref brush_type, .. } if brush_type == "ContextColor"
        ));
    }

    #[test]
    fn rejects_content_outside_opc_roots_and_duplicate_content_types() {
        let options = ParseOptions::default();
        assert!(parse_relationships(
            b"<Relationship Id='x' Type='t' Target='a'/><Relationships/>",
            "r",
            None,
            options,
        )
        .is_err());
        assert!(parse_content_types(
            b"<Types><Default Extension='PNG' ContentType='image/png'/><Default Extension='png' ContentType='image/png'/></Types>",
            "c",
            options,
        )
        .is_err());
    }
}
