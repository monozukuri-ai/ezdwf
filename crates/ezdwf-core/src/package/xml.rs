use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str;

use quick_xml::encoding::Decoder;
use quick_xml::events::BytesStart;
use quick_xml::XmlVersion;

use crate::DwfError;

pub(crate) fn normalize_xml_encoding<'a>(
    xml: &'a [u8],
    document: &str,
    max_size: usize,
) -> Result<Cow<'a, [u8]>, DwfError> {
    if xml.len() > max_size {
        return Err(DwfError::XmlSizeLimitExceeded {
            document: document.to_owned(),
            actual: xml.len(),
            limit: max_size,
        });
    }
    if xml.starts_with(&[0x00, 0x00, 0xfe, 0xff]) || xml.starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
        return Err(DwfError::InvalidXml {
            document: document.to_owned(),
            context: "UTF-32 XML is not supported".to_owned(),
        });
    }
    if let Some(bytes) = xml.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return Ok(Cow::Borrowed(bytes));
    }

    let (bytes, little_endian) = if let Some(bytes) = xml.strip_prefix(&[0xff, 0xfe]) {
        (bytes, true)
    } else if let Some(bytes) = xml.strip_prefix(&[0xfe, 0xff]) {
        (bytes, false)
    } else if xml.starts_with(&[b'<', 0]) {
        (xml, true)
    } else if xml.starts_with(&[0, b'<']) {
        (xml, false)
    } else {
        return Ok(Cow::Borrowed(xml));
    };
    if bytes.len() % 2 != 0 {
        return Err(DwfError::InvalidXml {
            document: document.to_owned(),
            context: "UTF-16 XML has an odd byte length".to_owned(),
        });
    }

    let code_units = bytes.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    let mut text = char::decode_utf16(code_units)
        .collect::<Result<String, _>>()
        .map_err(|error| DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!("invalid UTF-16 XML: {error}"),
        })?;
    rewrite_xml_declaration_encoding(&mut text);
    let decoded = text.into_bytes();
    if decoded.len() > max_size {
        return Err(DwfError::XmlSizeLimitExceeded {
            document: document.to_owned(),
            actual: decoded.len(),
            limit: max_size,
        });
    }
    Ok(Cow::Owned(decoded))
}

fn rewrite_xml_declaration_encoding(text: &mut String) {
    if !text.starts_with("<?xml") {
        return;
    }
    let Some(declaration_end) = text.find("?>") else {
        return;
    };
    let declaration = &text[..declaration_end];
    let lower = declaration.to_ascii_lowercase();
    let Some(encoding_offset) = lower.find("encoding") else {
        return;
    };
    let Some(equals_offset) = declaration[encoding_offset + "encoding".len()..].find('=') else {
        return;
    };
    let value_offset = encoding_offset + "encoding".len() + equals_offset + 1;
    let Some(quote_offset) = declaration[value_offset..].find(['\'', '"']) else {
        return;
    };
    let quote_start = value_offset + quote_offset;
    let quote = declaration.as_bytes()[quote_start];
    let Some(quote_end_offset) = declaration.as_bytes()[quote_start + 1..]
        .iter()
        .position(|byte| *byte == quote)
    else {
        return;
    };
    let value_start = quote_start + 1;
    let value_end = value_start + quote_end_offset;
    text.replace_range(value_start..value_end, "UTF-8");
}

pub(crate) fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

pub(crate) fn local_name_string(name: &[u8], document: &str) -> Result<String, DwfError> {
    str::from_utf8(local_name(name))
        .map(str::to_owned)
        .map_err(|error| DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!("element name is not UTF-8: {error}"),
        })
}

pub(crate) fn attributes(
    start: &BytesStart<'_>,
    decoder: Decoder,
    document: &str,
) -> Result<BTreeMap<String, String>, DwfError> {
    let mut values = BTreeMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!("invalid attribute: {error}"),
        })?;
        let key = str::from_utf8(local_name(attribute.key.as_ref())).map_err(|error| {
            DwfError::InvalidXml {
                document: document.to_owned(),
                context: format!("attribute name is not UTF-8: {error}"),
            }
        })?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| DwfError::InvalidXml {
                document: document.to_owned(),
                context: format!("invalid value for attribute {key:?}: {error}"),
            })?
            .into_owned();
        values.insert(key.to_owned(), value);
    }
    Ok(values)
}

pub(crate) fn required(
    values: &BTreeMap<String, String>,
    key: &str,
    document: &str,
    element: &str,
) -> Result<String, DwfError> {
    values
        .get(key)
        .cloned()
        .ok_or_else(|| DwfError::InvalidXml {
            document: document.to_owned(),
            context: format!("{element} is missing required {key:?} attribute"),
        })
}

pub(crate) fn xml_error(document: &str, position: u64, error: quick_xml::Error) -> DwfError {
    DwfError::InvalidXml {
        document: document.to_owned(),
        context: format!("{error} at byte offset {position}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_xml(text: &str, little_endian: bool) -> Vec<u8> {
        let mut output = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            output.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        output
    }

    #[test]
    fn normalizes_utf16_and_rewrites_the_xml_declaration() {
        for little_endian in [true, false] {
            let source = utf16_xml(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?><Root value=\"日本語\"/>",
                little_endian,
            );
            for candidate in [&source[..], &source[2..]] {
                let normalized = normalize_xml_encoding(candidate, "fixture.xml", 1024).unwrap();
                assert_eq!(
                    str::from_utf8(normalized.as_ref()).unwrap(),
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Root value=\"日本語\"/>"
                );
            }
        }
    }

    #[test]
    fn rejects_malformed_utf16() {
        let error = normalize_xml_encoding(&[0xff, 0xfe, b'<'], "fixture.xml", 1024)
            .expect_err("odd UTF-16 byte count must fail");
        assert!(error.to_string().contains("odd byte length"));
    }
}
