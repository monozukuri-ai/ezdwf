use std::io::Cursor;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::path::normalize_entry_name;
use super::xml::{attributes, local_name_string, normalize_xml_encoding, required, xml_error};
use crate::{
    DwfError, DwfInterface, DwfManifest, DwfProperty, DwfResource, DwfSection, DwfSource,
    ParseOptions,
};

pub(crate) fn parse_manifest(
    xml: &[u8],
    document: &str,
    options: ParseOptions,
) -> Result<DwfManifest, DwfError> {
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut root_seen = false;
    let mut version = None;
    let mut object_id = None;
    let mut properties = Vec::new();
    let mut interfaces = Vec::new();
    let mut sections = Vec::new();
    let mut current_section: Option<DwfSection> = None;

    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(start) => {
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(&start, reader.decoder(), document)?;
                let parent = stack.last().map(String::as_str);
                handle_element(
                    &name,
                    parent,
                    &values,
                    document,
                    &mut root_seen,
                    &mut version,
                    &mut object_id,
                    &mut properties,
                    &mut interfaces,
                    &mut current_section,
                )?;
                stack.push(name);
                if stack.len() > options.max_xml_depth {
                    return Err(DwfError::XmlDepthLimitExceeded {
                        document: document.to_owned(),
                        limit: options.max_xml_depth,
                    });
                }
            }
            Event::Empty(start) => {
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(&start, reader.decoder(), document)?;
                let parent = stack.last().map(String::as_str);
                handle_element(
                    &name,
                    parent,
                    &values,
                    document,
                    &mut root_seen,
                    &mut version,
                    &mut object_id,
                    &mut properties,
                    &mut interfaces,
                    &mut current_section,
                )?;
                if name == "Section" {
                    finish_section(&mut current_section, &mut sections)?;
                }
            }
            Event::End(end) => {
                let name = local_name_string(end.name().as_ref(), document)?;
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
                if name == "Section" {
                    finish_section(&mut current_section, &mut sections)?;
                }
            }
            Event::DocType(_) => {
                return Err(DwfError::InvalidXml {
                    document: document.to_owned(),
                    context: "DOCTYPE declarations are not allowed".to_owned(),
                });
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if !stack.is_empty() {
        return Err(DwfError::InvalidXml {
            document: document.to_owned(),
            context: "physical EOF occurred before all elements closed".to_owned(),
        });
    }
    if current_section.is_some() {
        return Err(DwfError::InvalidManifest {
            context: "physical EOF occurred inside a Section".to_owned(),
        });
    }
    if !root_seen {
        return Err(DwfError::InvalidManifest {
            context: "Manifest root element is missing".to_owned(),
        });
    }
    if sections.is_empty() {
        return Err(DwfError::InvalidManifest {
            context: "manifest contains no Section elements".to_owned(),
        });
    }

    Ok(DwfManifest {
        version: version.ok_or_else(|| DwfError::InvalidManifest {
            context: "Manifest version attribute is missing".to_owned(),
        })?,
        object_id,
        properties,
        interfaces,
        sections,
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_element(
    name: &str,
    parent: Option<&str>,
    values: &std::collections::BTreeMap<String, String>,
    document: &str,
    root_seen: &mut bool,
    version: &mut Option<String>,
    object_id: &mut Option<String>,
    properties: &mut Vec<DwfProperty>,
    interfaces: &mut Vec<DwfInterface>,
    current_section: &mut Option<DwfSection>,
) -> Result<(), DwfError> {
    match name {
        "Manifest" => {
            if *root_seen || parent.is_some() {
                return Err(DwfError::InvalidManifest {
                    context: "Manifest must be the unique document root".to_owned(),
                });
            }
            *root_seen = true;
            *version = Some(required(values, "version", document, "Manifest")?);
            *object_id = values.get("objectId").cloned();
        }
        "Interface" if parent == Some("Interfaces") => {
            interfaces.push(DwfInterface {
                object_id: values.get("objectId").cloned(),
                name: required(values, "name", document, "Interface")?,
                href: values.get("href").cloned(),
            });
        }
        "Property" if parent == Some("Properties") && current_section.is_none() => {
            properties.push(DwfProperty {
                name: required(values, "name", document, "Property")?,
                category: values.get("category").cloned(),
                value: required(values, "value", document, "Property")?,
                value_type: values.get("type").cloned(),
            });
        }
        "Section" => {
            if current_section.is_some() {
                return Err(DwfError::InvalidManifest {
                    context: "nested Section elements are not allowed".to_owned(),
                });
            }
            *current_section = Some(DwfSection {
                section_type: required(values, "type", document, "Section")?,
                name: required(values, "name", document, "Section")?,
                title: values.get("title").cloned(),
                source: None,
                resources: Vec::new(),
                page: None,
                w2d_streams: Vec::new(),
            });
        }
        "Source" => {
            if let Some(section) = current_section {
                section.source = Some(DwfSource {
                    provider: values.get("provider").cloned(),
                    href: values.get("href").cloned(),
                });
            }
        }
        "Resource" => {
            if let Some(section) = current_section {
                let href = required(values, "href", document, "Resource")?;
                let normalized_href = normalize_entry_name(&href)?;
                section.resources.push(DwfResource {
                    role: required(values, "role", document, "Resource")?,
                    mime: required(values, "mime", document, "Resource")?,
                    href,
                    normalized_href,
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn finish_section(
    current: &mut Option<DwfSection>,
    sections: &mut Vec<DwfSection>,
) -> Result<(), DwfError> {
    let section = current.take().ok_or_else(|| DwfError::InvalidManifest {
        context: "Section closed without a matching open element".to_owned(),
    })?;
    if section.resources.is_empty() {
        return Err(DwfError::InvalidManifest {
            context: format!("section {:?} has no resources", section.name),
        });
    }
    sections.push(section);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixed_manifest_names() {
        let xml = br#"<dwf:Manifest xmlns:dwf="DWF-Manifest:6.0" version="6.0">
          <dwf:Properties><dwf:Property name="Creator" value="test"/></dwf:Properties>
          <dwf:Interfaces><dwf:Interface name="ePlot"/></dwf:Interfaces>
          <dwf:Sections><dwf:Section type="com.autodesk.dwf.ePlot" name="sheet" title="One">
            <dwf:Toc><dwf:Resource dwf:role="descriptor" dwf:mime="text/xml" dwf:href="sheet\\descriptor.xml"/></dwf:Toc>
          </dwf:Section></dwf:Sections>
        </dwf:Manifest>"#;
        let manifest = parse_manifest(xml, "manifest.xml", ParseOptions::default()).unwrap();
        assert_eq!(manifest.version, "6.0");
        assert_eq!(manifest.properties[0].value, "test");
        assert_eq!(
            manifest.sections[0].resources[0].normalized_href,
            "sheet/descriptor.xml"
        );
    }

    #[test]
    fn rejects_doctype() {
        let xml = br#"<!DOCTYPE Manifest><Manifest version="6.0"><Sections/></Manifest>"#;
        assert!(matches!(
            parse_manifest(xml, "manifest.xml", ParseOptions::default()),
            Err(DwfError::InvalidXml { .. })
        ));
    }
}
