use std::fmt;

use serde::Serialize;

use crate::package::archive::PackageArchive;
use crate::{DwfError, ParseOptions};

pub const DWF_PACKAGE_HEADER_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DwfVersion {
    pub major: u8,
    pub minor: u8,
}

impl fmt::Display for DwfVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:02}.{:02}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DwfFormat {
    LegacyDwf { version: DwfVersion },
    DwfPackage { version: DwfVersion },
    Dwfx,
}

impl DwfFormat {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LegacyDwf { .. } => "legacy_dwf",
            Self::DwfPackage { .. } => "dwf_package",
            Self::Dwfx => "dwfx",
        }
    }

    #[must_use]
    pub const fn version(&self) -> Option<DwfVersion> {
        match self {
            Self::LegacyDwf { version } | Self::DwfPackage { version } => Some(*version),
            Self::Dwfx => None,
        }
    }

    #[must_use]
    pub const fn package_prefix_len(&self) -> usize {
        match self {
            Self::DwfPackage { .. } => DWF_PACKAGE_HEADER_LEN,
            Self::LegacyDwf { .. } | Self::Dwfx => 0,
        }
    }
}

impl fmt::Display for DwfFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyDwf { version } => write!(formatter, "legacy DWF {version}"),
            Self::DwfPackage { version } => write!(formatter, "DWF package {version}"),
            Self::Dwfx => formatter.write_str("DWFx OPC/XPS package"),
        }
    }
}

/// Detect a DWF family from bytes, without relying on the filename extension.
pub fn detect_format(data: &[u8], options: ParseOptions) -> Result<DwfFormat, DwfError> {
    check_file_size(data, options)?;

    if data.starts_with(b"(DWF") {
        let version = parse_dwf_header(data)?;
        return if version.major >= 6 {
            Ok(DwfFormat::DwfPackage { version })
        } else {
            Ok(DwfFormat::LegacyDwf { version })
        };
    }

    if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
        let archive = PackageArchive::open(data, 0, options)?;
        if archive.looks_like_dwfx(options)? {
            return Ok(DwfFormat::Dwfx);
        }
    }

    if data.len() < 4 {
        return Err(DwfError::InputTooShort {
            needed: 4,
            actual: data.len(),
        });
    }
    Err(DwfError::UnrecognizedFormat {
        signature: signature_hex(data),
    })
}

pub(crate) fn check_file_size(data: &[u8], options: ParseOptions) -> Result<(), DwfError> {
    if data.len() > options.max_file_size {
        return Err(DwfError::FileSizeLimitExceeded {
            actual: data.len(),
            limit: options.max_file_size,
        });
    }
    Ok(())
}

fn parse_dwf_header(data: &[u8]) -> Result<DwfVersion, DwfError> {
    if data.len() < DWF_PACKAGE_HEADER_LEN {
        return Err(DwfError::InputTooShort {
            needed: DWF_PACKAGE_HEADER_LEN,
            actual: data.len(),
        });
    }
    let header = &data[..DWF_PACKAGE_HEADER_LEN];
    if &header[..6] != b"(DWF V" || header[8] != b'.' || header[11] != b')' {
        return Err(DwfError::InvalidDwfHeader {
            context: format!(
                "expected (DWF V00.00), got {:?}",
                String::from_utf8_lossy(header)
            ),
        });
    }
    let major = parse_two_digits(&header[6..8]).ok_or_else(|| DwfError::InvalidDwfHeader {
        context: "major version is not two decimal digits".to_owned(),
    })?;
    let minor = parse_two_digits(&header[9..11]).ok_or_else(|| DwfError::InvalidDwfHeader {
        context: "minor version is not two decimal digits".to_owned(),
    })?;
    Ok(DwfVersion { major, minor })
}

fn parse_two_digits(bytes: &[u8]) -> Option<u8> {
    if bytes.len() != 2 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some((bytes[0] - b'0') * 10 + bytes[1] - b'0')
}

fn signature_hex(data: &[u8]) -> String {
    data.iter()
        .take(16)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_legacy_and_package_headers() {
        assert_eq!(
            detect_format(b"(DWF V00.55)payload", ParseOptions::default()).unwrap(),
            DwfFormat::LegacyDwf {
                version: DwfVersion {
                    major: 0,
                    minor: 55
                }
            }
        );
        assert_eq!(
            detect_format(b"(DWF V06.00)payload", ParseOptions::default()).unwrap(),
            DwfFormat::DwfPackage {
                version: DwfVersion { major: 6, minor: 0 }
            }
        );
    }

    #[test]
    fn rejects_malformed_dwf_header() {
        let error = detect_format(b"(DWF V0X.00)payload", ParseOptions::default()).unwrap_err();
        assert!(matches!(error, DwfError::InvalidDwfHeader { .. }));
    }
}
