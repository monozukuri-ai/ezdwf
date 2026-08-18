use crate::{DwfError, ParseOptions};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Atom(String),
    String(String),
    List(Vec<Node>),
}

impl Node {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Atom(value) | Self::String(value) => Some(value),
            Self::List(_) => None,
        }
    }

    pub(crate) fn list(&self) -> Option<&[Node]> {
        match self {
            Self::List(values) => Some(values),
            Self::Atom(_) | Self::String(_) => None,
        }
    }

    pub(crate) fn numbers(&self, output: &mut Vec<f64>) {
        match self {
            Self::Atom(value) => {
                if let Ok(value) = value.parse::<f64>() {
                    output.push(value);
                }
            }
            Self::String(_) => {}
            Self::List(values) => {
                for value in values {
                    value.numbers(output);
                }
            }
        }
    }
}

pub(crate) fn parse_record(
    data: &[u8],
    resource: &str,
    source_offset: usize,
    options: ParseOptions,
) -> Result<Node, DwfError> {
    let mut parser = Parser {
        data,
        position: 0,
        resource,
        source_offset,
        options,
    };
    parser.skip_separators();
    let value = parser.parse_value(0)?;
    parser.skip_separators();
    if parser.position != data.len() {
        return Err(parser.error("unexpected bytes after extended ASCII record"));
    }
    Ok(value)
}

struct Parser<'a> {
    data: &'a [u8],
    position: usize,
    resource: &'a str,
    source_offset: usize,
    options: ParseOptions,
}

impl Parser<'_> {
    fn parse_value(&mut self, depth: usize) -> Result<Node, DwfError> {
        if depth > self.options.max_w2d_nesting_depth {
            return Err(DwfError::W2dNestingLimitExceeded {
                resource: self.resource.to_owned(),
                offset: self.source_offset.saturating_add(self.position),
                limit: self.options.max_w2d_nesting_depth,
            });
        }
        let byte = *self
            .data
            .get(self.position)
            .ok_or_else(|| self.error("unexpected end of extended ASCII record"))?;
        match byte {
            b'(' => self.parse_list(depth),
            b'\'' => self.parse_quoted_string(b'\''),
            b'"' => self.parse_hex_string(),
            b')' => Err(self.error("unexpected closing parenthesis")),
            b'{' => self.parse_binary_string(),
            b'}' => Err(self.error("unexpected closing brace in decoded ASCII operand")),
            _ => self.parse_atom(),
        }
    }

    fn parse_list(&mut self, depth: usize) -> Result<Node, DwfError> {
        self.position += 1;
        let mut values = Vec::new();
        loop {
            self.skip_separators();
            match self.data.get(self.position).copied() {
                Some(b')') => {
                    self.position += 1;
                    return Ok(Node::List(values));
                }
                Some(_) => values.push(self.parse_value(depth + 1)?),
                None => return Err(self.error("unterminated extended ASCII record")),
            }
        }
    }

    fn parse_quoted_string(&mut self, quote: u8) -> Result<Node, DwfError> {
        let start = self.position;
        self.position += 1;
        let mut output = Vec::new();
        let mut escaped = false;
        while let Some(byte) = self.data.get(self.position).copied() {
            self.position += 1;
            if escaped {
                output.push(byte);
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                return Ok(Node::String(String::from_utf8_lossy(&output).into_owned()));
            } else {
                output.push(byte);
            }
            if output.len() > self.options.max_w2d_string_size {
                return Err(DwfError::W2dStringLimitExceeded {
                    resource: self.resource.to_owned(),
                    offset: self.source_offset.saturating_add(start),
                    limit: self.options.max_w2d_string_size,
                });
            }
        }
        Err(self.error("unterminated quoted string"))
    }

    /// WHIP! binary Unicode string operand: `{` + int32 character count +
    /// UTF-16LE code units + `}` (used e.g. by `(FontExtension {..} {..})` and
    /// `(Title {..})` in ePlot streams).
    fn parse_binary_string(&mut self) -> Result<Node, DwfError> {
        let start = self.position;
        self.position += 1;
        let count_bytes = self
            .data
            .get(self.position..self.position + 4)
            .ok_or_else(|| self.error("truncated binary Unicode string length"))?;
        let count = i32::from_le_bytes([
            count_bytes[0],
            count_bytes[1],
            count_bytes[2],
            count_bytes[3],
        ]);
        let count = usize::try_from(count)
            .map_err(|_| self.error("binary Unicode string length cannot be negative"))?;
        self.position += 4;
        let byte_count = count
            .checked_mul(2)
            .ok_or_else(|| self.error("binary Unicode string length overflow"))?;
        if byte_count > self.options.max_w2d_string_size {
            return Err(DwfError::W2dStringLimitExceeded {
                resource: self.resource.to_owned(),
                offset: self.source_offset.saturating_add(start),
                limit: self.options.max_w2d_string_size,
            });
        }
        let payload = self
            .data
            .get(self.position..self.position + byte_count)
            .ok_or_else(|| self.error("truncated binary Unicode string"))?;
        let units = payload
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        self.position += byte_count;
        if self.data.get(self.position) != Some(&b'}') {
            return Err(self.error("binary Unicode string has no closing brace"));
        }
        self.position += 1;
        Ok(Node::String(String::from_utf16_lossy(&units)))
    }

    fn parse_hex_string(&mut self) -> Result<Node, DwfError> {
        let start = self.position;
        self.position += 1;
        let digits_start = self.position;
        while self.data.get(self.position).copied() != Some(b'"') {
            let byte = *self
                .data
                .get(self.position)
                .ok_or_else(|| self.error("unterminated Unicode hex string"))?;
            if !byte.is_ascii_hexdigit() {
                return Err(self.error("Unicode string contains a non-hex digit"));
            }
            self.position += 1;
            if self.position - digits_start > self.options.max_w2d_string_size.saturating_mul(4) {
                return Err(DwfError::W2dStringLimitExceeded {
                    resource: self.resource.to_owned(),
                    offset: self.source_offset.saturating_add(start),
                    limit: self.options.max_w2d_string_size,
                });
            }
        }
        let digits = &self.data[digits_start..self.position];
        self.position += 1;
        if digits.len() % 4 != 0 {
            return Err(self.error("Unicode hex string length is not divisible by four"));
        }
        let units = digits
            .chunks_exact(4)
            .map(|digits| {
                let digits = std::str::from_utf8(digits)
                    .map_err(|_| self.error("Unicode string is not ASCII hex"))?;
                u16::from_str_radix(digits, 16)
                    .map_err(|_| self.error("Unicode string contains invalid hex"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Node::String(String::from_utf16_lossy(&units)))
    }

    fn parse_atom(&mut self) -> Result<Node, DwfError> {
        let start = self.position;
        while let Some(byte) = self.data.get(self.position).copied() {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b'(' | b')') {
                break;
            }
            self.position += 1;
            if self.position - start > self.options.max_w2d_string_size {
                return Err(DwfError::W2dStringLimitExceeded {
                    resource: self.resource.to_owned(),
                    offset: self.source_offset.saturating_add(start),
                    limit: self.options.max_w2d_string_size,
                });
            }
        }
        if self.position == start {
            return Err(self.error("expected an ASCII operand"));
        }
        Ok(Node::Atom(
            String::from_utf8_lossy(&self.data[start..self.position]).into_owned(),
        ))
    }

    fn skip_separators(&mut self) {
        while self
            .data
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            self.position += 1;
        }
    }

    fn error(&self, context: impl Into<String>) -> DwfError {
        DwfError::InvalidW2d {
            resource: self.resource.to_owned(),
            offset: self.source_offset.saturating_add(self.position),
            context: context.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_records_and_escaped_strings() {
        let node = parse_record(
            br#"(Viewport 'A\'B' (Contour 1 3 0,0 10,0 10,10))"#,
            "sheet.w2d",
            10,
            ParseOptions::default(),
        )
        .unwrap();
        let values = node.list().unwrap();
        assert_eq!(values[0].text(), Some("Viewport"));
        assert_eq!(values[1].text(), Some("A'B"));
        assert_eq!(values[2].list().unwrap()[0].text(), Some("Contour"));
    }
}
