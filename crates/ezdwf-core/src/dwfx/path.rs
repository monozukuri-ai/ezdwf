use crate::{DwfError, XpsMatrix, XpsPathFigure, XpsPathGeometry, XpsPathSegment, XpsPoint};

pub(super) fn parse_abbreviated_geometry(
    data: &str,
    part: &str,
    segment_budget: &mut usize,
    segment_limit: usize,
    allow_fill_rule: bool,
) -> Result<XpsPathGeometry, DwfError> {
    let tokens = tokenize(data, part)?;
    let mut parser = GeometryParser {
        tokens: &tokens,
        cursor: 0,
        part,
        segment_budget,
        segment_limit,
        current: XpsPoint::default(),
        current_figure: None,
        figures: Vec::new(),
        previous_cubic_control: None,
        command: None,
    };
    let mut fill_rule = "even_odd".to_owned();
    if parser.peek_command().is_some_and(|command| command == 'F') {
        if !allow_fill_rule {
            return Err(
                parser.error("FillRule command is not allowed in PathGeometry.Figures".to_owned())
            );
        }
        parser.cursor += 1;
        let rule = parser.number("fill rule")?;
        fill_rule = match rule {
            0.0 => "even_odd".to_owned(),
            1.0 => "nonzero".to_owned(),
            _ => return Err(parser.error(format!("fill rule must be 0 or 1, got {rule}"))),
        };
    }
    parser.parse()?;
    Ok(XpsPathGeometry {
        fill_rule,
        figures: parser.figures,
        data: Some(data.to_owned()),
        transform: XpsMatrix::IDENTITY,
    })
}

#[derive(Debug, Clone, Copy)]
enum Token {
    Command(char),
    Number(f64),
}

fn tokenize(data: &str, part: &str) -> Result<Vec<Token>, DwfError> {
    let bytes = data.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b',')
        {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let byte = bytes[cursor];
        if byte.is_ascii_alphabetic() {
            let command = char::from(byte);
            if !matches!(
                command,
                'F' | 'M'
                    | 'm'
                    | 'L'
                    | 'l'
                    | 'H'
                    | 'h'
                    | 'V'
                    | 'v'
                    | 'C'
                    | 'c'
                    | 'Q'
                    | 'q'
                    | 'S'
                    | 's'
                    | 'A'
                    | 'a'
                    | 'Z'
                    | 'z'
            ) {
                return Err(invalid(
                    part,
                    format!(
                        "unsupported abbreviated geometry command {command:?} at byte {cursor}"
                    ),
                ));
            }
            tokens.push(Token::Command(command));
            cursor += 1;
            continue;
        }
        let start = cursor;
        if matches!(bytes[cursor], b'+' | b'-') {
            cursor += 1;
        }
        let mut digits = 0;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
            digits += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b'.' {
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return Err(invalid(
                part,
                format!("expected geometry number at byte {start}"),
            ));
        }
        if cursor < bytes.len() && matches!(bytes[cursor], b'e' | b'E') {
            cursor += 1;
            if cursor < bytes.len() && matches!(bytes[cursor], b'+' | b'-') {
                cursor += 1;
            }
            let exponent_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if exponent_start == cursor {
                return Err(invalid(
                    part,
                    format!("invalid geometry exponent at byte {start}"),
                ));
            }
        }
        let text = &data[start..cursor];
        let value = text
            .parse::<f64>()
            .map_err(|error| invalid(part, format!("invalid geometry number {text:?}: {error}")))?;
        if !value.is_finite() {
            return Err(invalid(
                part,
                format!("geometry number is not finite: {text:?}"),
            ));
        }
        tokens.push(Token::Number(value));
    }
    Ok(tokens)
}

struct GeometryParser<'a> {
    tokens: &'a [Token],
    cursor: usize,
    part: &'a str,
    segment_budget: &'a mut usize,
    segment_limit: usize,
    current: XpsPoint,
    current_figure: Option<XpsPathFigure>,
    figures: Vec<XpsPathFigure>,
    previous_cubic_control: Option<XpsPoint>,
    command: Option<char>,
}

impl GeometryParser<'_> {
    fn parse(&mut self) -> Result<(), DwfError> {
        while self.cursor < self.tokens.len() {
            if let Some(command) = self.peek_command() {
                self.cursor += 1;
                self.command = Some(command);
            }
            let command = self
                .command
                .ok_or_else(|| self.error("geometry must begin with a Move command".to_owned()))?;
            let relative = command.is_ascii_lowercase();
            match command.to_ascii_uppercase() {
                'M' => {
                    let point = self.point(relative, self.current, "Move endpoint")?;
                    self.finish_figure();
                    self.current = point;
                    self.current_figure = Some(XpsPathFigure {
                        start: point,
                        segments: Vec::new(),
                        closed: false,
                        filled: true,
                    });
                    self.previous_cubic_control = None;
                    // Point pairs after Move are implicit Line commands.
                    self.command = Some(if relative { 'l' } else { 'L' });
                }
                'L' => {
                    self.require_figure()?;
                    let end = self.point(relative, self.current, "Line endpoint")?;
                    self.push(XpsPathSegment::Line {
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                }
                'H' => {
                    self.require_figure()?;
                    let x = self.number("Horizontal Line coordinate")?;
                    let end = XpsPoint {
                        x: if relative { self.current.x + x } else { x },
                        y: self.current.y,
                    };
                    self.push(XpsPathSegment::Line {
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                }
                'V' => {
                    self.require_figure()?;
                    let y = self.number("Vertical Line coordinate")?;
                    let end = XpsPoint {
                        x: self.current.x,
                        y: if relative { self.current.y + y } else { y },
                    };
                    self.push(XpsPathSegment::Line {
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                }
                'C' => {
                    self.require_figure()?;
                    let base = self.current;
                    let control1 = self.point(relative, base, "Cubic control point 1")?;
                    let control2 = self.point(relative, base, "Cubic control point 2")?;
                    let end = self.point(relative, base, "Cubic endpoint")?;
                    self.push(XpsPathSegment::CubicBezier {
                        control1,
                        control2,
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                    self.previous_cubic_control = Some(control2);
                }
                'Q' => {
                    self.require_figure()?;
                    let base = self.current;
                    let control = self.point(relative, base, "Quadratic control point")?;
                    let end = self.point(relative, base, "Quadratic endpoint")?;
                    self.push(XpsPathSegment::QuadraticBezier {
                        control,
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                }
                'S' => {
                    self.require_figure()?;
                    let base = self.current;
                    let control1 = self
                        .previous_cubic_control
                        .map_or(base, |previous| XpsPoint {
                            x: 2.0 * base.x - previous.x,
                            y: 2.0 * base.y - previous.y,
                        });
                    let control2 = self.point(relative, base, "Smooth Cubic control point")?;
                    let end = self.point(relative, base, "Smooth Cubic endpoint")?;
                    self.push(XpsPathSegment::CubicBezier {
                        control1,
                        control2,
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                    self.previous_cubic_control = Some(control2);
                }
                'A' => {
                    self.require_figure()?;
                    let base = self.current;
                    let radius = self.point(false, XpsPoint::default(), "Arc radius")?;
                    let rotation_degrees = self.number("Arc rotation")?;
                    let large_arc = self.flag("Arc large-arc flag")?;
                    let sweep_clockwise = self.flag("Arc sweep flag")?;
                    let end = self.point(relative, base, "Arc endpoint")?;
                    self.push(XpsPathSegment::Arc {
                        radius: XpsPoint {
                            x: radius.x.abs(),
                            y: radius.y.abs(),
                        },
                        rotation_degrees,
                        large_arc,
                        sweep_clockwise,
                        end,
                        stroked: true,
                        smooth_join: false,
                    })?;
                }
                'Z' => {
                    if self.current_figure.is_none() {
                        return Err(self.error("Close command appeared before a Move".to_owned()));
                    }
                    let figure = self.current_figure.as_mut().expect("checked");
                    figure.closed = true;
                    self.current = figure.start;
                    self.previous_cubic_control = None;
                    self.command = None;
                }
                _ => unreachable!(),
            }
            if !matches!(command.to_ascii_uppercase(), 'C' | 'S') {
                self.previous_cubic_control = None;
            }
        }
        self.finish_figure();
        if self.figures.is_empty() {
            return Err(self.error("geometry contains no figures".to_owned()));
        }
        Ok(())
    }

    fn peek_command(&self) -> Option<char> {
        match self.tokens.get(self.cursor) {
            Some(Token::Command(command)) => Some(*command),
            _ => None,
        }
    }

    fn number(&mut self, context: &str) -> Result<f64, DwfError> {
        match self.tokens.get(self.cursor) {
            Some(Token::Number(value)) => {
                self.cursor += 1;
                Ok(*value)
            }
            Some(Token::Command(command)) => {
                Err(self.error(format!("{context} is missing before command {command:?}")))
            }
            None => Err(self.error(format!("{context} is truncated"))),
        }
    }

    fn flag(&mut self, context: &str) -> Result<bool, DwfError> {
        match self.number(context)? {
            0.0 => Ok(false),
            1.0 => Ok(true),
            value => Err(self.error(format!("{context} must be 0 or 1, got {value}"))),
        }
    }

    fn point(
        &mut self,
        relative: bool,
        base: XpsPoint,
        context: &str,
    ) -> Result<XpsPoint, DwfError> {
        let x = self.number(context)?;
        let y = self.number(context)?;
        Ok(if relative {
            XpsPoint {
                x: base.x + x,
                y: base.y + y,
            }
        } else {
            XpsPoint { x, y }
        })
    }

    fn push(&mut self, segment: XpsPathSegment) -> Result<(), DwfError> {
        if *self.segment_budget == 0 {
            return Err(DwfError::XpsPathSegmentLimitExceeded {
                page: self.part.to_owned(),
                limit: self.segment_limit,
            });
        }
        *self.segment_budget -= 1;
        self.current = segment.end();
        self.current_figure
            .as_mut()
            .expect("figure checked before segment")
            .segments
            .push(segment);
        Ok(())
    }

    fn require_figure(&self) -> Result<(), DwfError> {
        if self.current_figure.is_none() {
            return Err(self.error("drawing command appeared before a Move".to_owned()));
        }
        Ok(())
    }

    fn finish_figure(&mut self) {
        if let Some(figure) = self.current_figure.take() {
            self.figures.push(figure);
        }
    }

    fn error(&self, context: String) -> DwfError {
        invalid(self.part, context)
    }
}

fn invalid(part: &str, context: String) -> DwfError {
    DwfError::InvalidXps {
        part: part.to_owned(),
        context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_relative_curves_and_arc() {
        let mut budget = 20;
        let geometry = parse_abbreviated_geometry(
            "F 1 M 1,2 l 3,4 H 9 v -2 C 1,2 3,4 5,6 s 2,3 4,5 Q 1,1 2,2 A 4,5 30 0 1 20,30 Z",
            "page.fpage",
            &mut budget,
            32,
            true,
        )
        .unwrap();
        assert_eq!(geometry.fill_rule, "nonzero");
        assert_eq!(geometry.figures.len(), 1);
        assert!(geometry.figures[0].closed);
        assert_eq!(geometry.figures[0].segments.len(), 7);
        assert_eq!(
            geometry.figures[0].segments[0].end(),
            XpsPoint { x: 4.0, y: 6.0 }
        );
        assert!(matches!(
            geometry.figures[0].segments[6],
            XpsPathSegment::Arc { .. }
        ));
    }

    #[test]
    fn rejects_non_finite_and_invalid_flags() {
        let mut budget = 20;
        assert!(parse_abbreviated_geometry("M 0,0 L 1e999,0", "p", &mut budget, 32, true).is_err());
        assert!(
            parse_abbreviated_geometry("M 0,0 A 1,1 0 2 0 1,1", "p", &mut budget, 32, true)
                .is_err()
        );
    }

    #[test]
    fn move_tail_is_implicit_line_and_figures_reject_fill_command() {
        let mut budget = 10;
        let geometry =
            parse_abbreviated_geometry("M 0,0 1,1 2,3", "p", &mut budget, 10, true).unwrap();
        assert_eq!(geometry.figures.len(), 1);
        assert_eq!(geometry.figures[0].segments.len(), 2);
        assert_eq!(
            geometry.figures[0].segments[1].end(),
            XpsPoint { x: 2.0, y: 3.0 }
        );

        let mut budget = 10;
        assert!(
            parse_abbreviated_geometry("F 1 M 0,0 L 1,1", "p", &mut budget, 10, false).is_err()
        );
    }
}
