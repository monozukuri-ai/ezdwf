use std::collections::BTreeMap;
use std::io::Cursor;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::path::normalize_entry_name;
use super::xml::{attributes, local_name_string, normalize_xml_encoding, required, xml_error};
use crate::{DwfError, DwfProperty, EPlotPage, EPlotPaper, EPlotResource, ParseOptions};

pub(crate) fn parse_eplot(
    xml: &[u8],
    document: &str,
    section: &str,
    options: ParseOptions,
) -> Result<EPlotPage, DwfError> {
    let xml = normalize_xml_encoding(xml, document, options.max_xml_size)?;
    let mut reader = Reader::from_reader(Cursor::new(xml.as_ref()));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut page: Option<EPlotPage> = None;

    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(document, reader.buffer_position(), error))?;
        match event {
            Event::Start(start) => {
                let name = local_name_string(start.name().as_ref(), document)?;
                let values = attributes(&start, reader.decoder(), document)?;
                handle_element(&name, &values, document, section, &mut page)?;
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
                handle_element(&name, &values, document, section, &mut page)?;
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
    page.ok_or_else(|| DwfError::InvalidEPlot {
        section: section.to_owned(),
        context: "Page root element is missing".to_owned(),
    })
}

fn handle_element(
    name: &str,
    values: &BTreeMap<String, String>,
    document: &str,
    section: &str,
    page: &mut Option<EPlotPage>,
) -> Result<(), DwfError> {
    match name {
        "Page" => {
            if page.is_some() {
                return Err(DwfError::InvalidEPlot {
                    section: section.to_owned(),
                    context: "Page must be the unique descriptor root".to_owned(),
                });
            }
            *page = Some(EPlotPage {
                version: required(values, "version", document, "Page")?,
                name: required(values, "name", document, "Page")?,
                object_id: values.get("objectId").cloned(),
                plot_order: parse_optional(values, "plotOrder", document, "Page")?,
                color: parse_optional_color(values, "color", document, "Page")?,
                paper: None,
                properties: Vec::new(),
                resources: Vec::new(),
            });
        }
        "Property" => {
            let page = require_page(page, section)?;
            page.properties.push(DwfProperty {
                name: required(values, "name", document, "Property")?,
                category: values.get("category").cloned(),
                value: required(values, "value", document, "Property")?,
                value_type: values.get("type").cloned(),
            });
        }
        "Paper" => {
            let parsed = EPlotPaper {
                show: parse_optional_bool(values, "show", document, "Paper")?,
                units: values.get("units").cloned(),
                width: parse_optional(values, "width", document, "Paper")?,
                height: parse_optional(values, "height", document, "Paper")?,
                clip: parse_optional_numbers(values, "clip", 4, document, "Paper")?,
                color: parse_optional_color(values, "color", document, "Paper")?,
            };
            let page = require_page(page, section)?;
            if page.paper.replace(parsed).is_some() {
                return Err(DwfError::InvalidEPlot {
                    section: section.to_owned(),
                    context: "Page contains more than one Paper element".to_owned(),
                });
            }
        }
        resource_kind if resource_kind.ends_with("Resource") => {
            let href = required(values, "href", document, resource_kind)?;
            let normalized_href = normalize_entry_name(&href)?;
            let resource = EPlotResource {
                kind: resource_kind.to_owned(),
                role: required(values, "role", document, resource_kind)?,
                mime: required(values, "mime", document, resource_kind)?,
                href,
                normalized_href,
                title: values.get("title").cloned(),
                size: parse_optional(values, "size", document, resource_kind)?,
                object_id: values.get("objectId").cloned(),
                parent_object_id: values.get("parentObjectId").cloned(),
                transform: parse_optional_numbers(
                    values,
                    "transform",
                    16,
                    document,
                    resource_kind,
                )?,
                clip: parse_optional_numbers(values, "clip", 4, document, resource_kind)?,
                extents: parse_optional_numbers(values, "extents", 4, document, resource_kind)?,
                attributes: values.clone(),
            };
            require_page(page, section)?.resources.push(resource);
        }
        _ => {}
    }
    Ok(())
}

fn require_page<'a>(
    page: &'a mut Option<EPlotPage>,
    section: &str,
) -> Result<&'a mut EPlotPage, DwfError> {
    page.as_mut().ok_or_else(|| DwfError::InvalidEPlot {
        section: section.to_owned(),
        context: "descriptor content appeared before the Page root".to_owned(),
    })
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
            value.parse::<T>().map_err(|error| DwfError::InvalidXml {
                document: document.to_owned(),
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
            _ => Err(DwfError::InvalidXml {
                document: document.to_owned(),
                context: format!("invalid {element}.{key} boolean {value:?}"),
            }),
        })
        .transpose()
}

fn parse_optional_numbers(
    values: &BTreeMap<String, String>,
    key: &str,
    expected: usize,
    document: &str,
    element: &str,
) -> Result<Option<Vec<f64>>, DwfError> {
    values
        .get(key)
        .map(|value| {
            let numbers = value
                .split_ascii_whitespace()
                .map(|part| {
                    part.parse::<f64>().map_err(|error| DwfError::InvalidXml {
                        document: document.to_owned(),
                        context: format!("invalid {element}.{key} number {part:?}: {error}"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if numbers.len() != expected {
                return Err(DwfError::InvalidXml {
                    document: document.to_owned(),
                    context: format!(
                        "{element}.{key} requires {expected} numbers, got {}",
                        numbers.len()
                    ),
                });
            }
            Ok(numbers)
        })
        .transpose()
}

fn parse_optional_color(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<Option<[u8; 3]>, DwfError> {
    let Some(value) = values.get(key) else {
        return Ok(None);
    };
    let channels = value
        .split_ascii_whitespace()
        .map(|part| {
            part.parse::<u8>().map_err(|error| DwfError::InvalidXml {
                document: document.to_owned(),
                context: format!("invalid {element}.{key} channel {part:?}: {error}"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let color: [u8; 3] = channels
        .try_into()
        .map_err(|channels: Vec<u8>| DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!(
                "{element}.{key} requires 3 channels, got {}",
                channels.len()
            ),
        })?;
    Ok(Some(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_paper_properties_and_resources() {
        let xml = br#"<ePlot:Page xmlns:ePlot="DWF-ePlot:1.2" version="1.2" plotOrder="1" name="Sheet" color="128 128 128">
          <ePlot:Properties><ePlot:Property name="Creator" value="test"/></ePlot:Properties>
          <ePlot:Paper show="true" units="mm" width="297" height="210" clip="0 0 297 210" color="255 255 255"/>
          <ePlot:Resources><ePlot:GraphicResource role="2d streaming graphics" mime="application/x-w2d" href="sheet\\main.w2d" size="12" transform="1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1"/></ePlot:Resources>
        </ePlot:Page>"#;
        let page = parse_eplot(
            xml,
            "sheet/descriptor.xml",
            "sheet",
            ParseOptions::default(),
        )
        .unwrap();
        assert_eq!(page.name, "Sheet");
        assert_eq!(page.paper.unwrap().units.as_deref(), Some("mm"));
        assert_eq!(page.resources[0].normalized_href, "sheet/main.w2d");
        assert_eq!(page.resources[0].transform.as_ref().unwrap().len(), 16);
    }
}
