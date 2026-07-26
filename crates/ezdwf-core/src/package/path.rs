use crate::DwfError;

/// Normalize the slash variants found in historical DWF archives without
/// allowing the resulting name to escape a logical package root.
pub(crate) fn normalize_entry_name(name: &str) -> Result<String, DwfError> {
    if name.is_empty() {
        return Err(invalid(name, "the name is empty"));
    }
    if name.contains('\0') {
        return Err(invalid(name, "the name contains NUL"));
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(invalid(name, "absolute paths are not allowed"));
    }

    let mut parts = Vec::new();
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(invalid(name, "parent traversal is not allowed"));
        }
        if parts.is_empty() && is_drive_prefix(part) {
            return Err(invalid(name, "drive-prefixed paths are not allowed"));
        }
        parts.push(part);
    }

    if parts.is_empty() {
        return Err(invalid(name, "the normalized name is empty"));
    }
    Ok(parts.join("/"))
}

fn is_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn invalid(name: &str, reason: &str) -> DwfError {
    DwfError::InvalidEntryName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_historical_backslash_names() {
        assert_eq!(
            normalize_entry_name(r"section\graphics.w2d").unwrap(),
            "section/graphics.w2d"
        );
        assert_eq!(
            normalize_entry_name("section//./graphics.w2d").unwrap(),
            "section/graphics.w2d"
        );
    }

    #[test]
    fn rejects_escaping_names() {
        for name in [
            "../manifest.xml",
            "/manifest.xml",
            r"C:\manifest.xml",
            "a\0b",
        ] {
            assert!(normalize_entry_name(name).is_err(), "accepted {name:?}");
        }
    }
}
