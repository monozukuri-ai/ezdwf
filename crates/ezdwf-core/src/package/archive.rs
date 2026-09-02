use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::path::normalize_entry_name;
use crate::{ArchiveEntry, DwfError, ParseOptions};

const CENTRAL_DIRECTORY_FILE_HEADER: &[u8; 4] = b"PK\x01\x02";
const CENTRAL_DIRECTORY_FILE_HEADER_SIZE: usize = 46;

pub(crate) struct PackageArchive<'a> {
    data: &'a [u8],
    prefix_len: usize,
    entries: Vec<ArchiveEntry>,
    entry_indices: BTreeMap<String, usize>,
}

impl<'a> PackageArchive<'a> {
    pub(crate) fn open(
        data: &'a [u8],
        prefix_len: usize,
        options: ParseOptions,
    ) -> Result<Self, DwfError> {
        if prefix_len > data.len() {
            return Err(DwfError::InvalidArchive {
                context: format!(
                    "package prefix is {prefix_len} bytes but input is only {} bytes",
                    data.len()
                ),
            });
        }
        let mut archive = open_zip(data, prefix_len)?;
        let unique_entry_count = archive.len();
        let central_entry_count =
            count_central_directory_entries(data, archive.central_directory_start())?;
        if central_entry_count > unique_entry_count {
            return Err(DwfError::DuplicateArchiveEntryNames {
                actual: central_entry_count,
                unique: unique_entry_count,
            });
        }
        if central_entry_count != unique_entry_count {
            return Err(DwfError::InvalidArchive {
                context: format!(
                    "central directory contains {central_entry_count} file headers but the ZIP reader exposed {unique_entry_count} entries"
                ),
            });
        }
        if central_entry_count > options.max_archive_entries {
            return Err(DwfError::ArchiveEntryLimitExceeded {
                actual: central_entry_count,
                limit: options.max_archive_entries,
            });
        }

        let mut entries: Vec<ArchiveEntry> = Vec::with_capacity(archive.len());
        let mut entry_indices: BTreeMap<String, usize> = BTreeMap::new();
        let mut total_uncompressed = 0_u64;
        for index in 0..archive.len() {
            let file = archive.by_index(index).map_err(zip_error)?;
            let original_name = file.name().to_owned();
            let normalized_name = normalize_entry_name(&original_name)?;
            let uncompressed_size = file.size();
            let compressed_size = file.compressed_size();

            if uncompressed_size > options.max_entry_size as u64 {
                return Err(DwfError::EntrySizeLimitExceeded {
                    name: original_name,
                    actual: uncompressed_size,
                    limit: options.max_entry_size,
                });
            }
            total_uncompressed = total_uncompressed.checked_add(uncompressed_size).ok_or(
                DwfError::TotalUncompressedSizeLimitExceeded {
                    actual: u64::MAX,
                    limit: options.max_total_uncompressed_size,
                },
            )?;
            if total_uncompressed > options.max_total_uncompressed_size as u64 {
                return Err(DwfError::TotalUncompressedSizeLimitExceeded {
                    actual: total_uncompressed,
                    limit: options.max_total_uncompressed_size,
                });
            }

            let ratio = if uncompressed_size == 0 {
                0
            } else {
                uncompressed_size / compressed_size.max(1)
            };
            if ratio > options.max_compression_ratio as u64 {
                return Err(DwfError::CompressionRatioLimitExceeded {
                    name: original_name,
                    actual: ratio,
                    limit: options.max_compression_ratio,
                });
            }

            if let Some(previous_index) = entry_indices.insert(normalized_name.clone(), index) {
                let previous = &entries[previous_index];
                return Err(DwfError::DuplicateEntryName {
                    normalized: normalized_name,
                    first: previous.original_name.clone(),
                    second: original_name,
                });
            }

            entries.push(ArchiveEntry {
                original_name,
                normalized_name,
                compressed_size,
                uncompressed_size,
                compression_method: format!("{:?}", file.compression()),
                is_directory: file.is_dir(),
                encrypted: file.encrypted(),
            });
        }

        Ok(Self {
            data,
            prefix_len,
            entries,
            entry_indices,
        })
    }

    pub(crate) fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    pub(crate) fn contains(&self, normalized_name: &str) -> bool {
        self.entry_indices.contains_key(normalized_name)
    }

    pub(crate) fn read_entry(
        &self,
        normalized_name: &str,
        limit: usize,
    ) -> Result<Vec<u8>, DwfError> {
        let index =
            *self
                .entry_indices
                .get(normalized_name)
                .ok_or_else(|| DwfError::MissingEntry {
                    name: normalized_name.to_owned(),
                })?;
        let entry = &self.entries[index];
        if entry.encrypted {
            return Err(DwfError::EncryptedEntry {
                name: entry.original_name.clone(),
            });
        }
        if entry.uncompressed_size > limit as u64 {
            return Err(DwfError::XmlSizeLimitExceeded {
                document: normalized_name.to_owned(),
                actual: usize::try_from(entry.uncompressed_size).unwrap_or(usize::MAX),
                limit,
            });
        }

        let mut archive = open_zip(self.data, self.prefix_len)?;
        let mut file = archive.by_index(index).map_err(zip_error)?;
        let mut output = Vec::with_capacity(
            usize::try_from(entry.uncompressed_size)
                .unwrap_or(limit)
                .min(limit),
        );
        file.by_ref()
            .take(limit.saturating_add(1) as u64)
            .read_to_end(&mut output)
            .map_err(|error| DwfError::InvalidArchive {
                context: format!("failed to read {normalized_name:?}: {error}"),
            })?;
        if output.len() > limit {
            return Err(DwfError::XmlSizeLimitExceeded {
                document: normalized_name.to_owned(),
                actual: output.len(),
                limit,
            });
        }
        if output.len() as u64 != entry.uncompressed_size {
            return Err(DwfError::InvalidArchive {
                context: format!(
                    "entry {normalized_name:?} declared {} bytes but decoded to {} bytes",
                    entry.uncompressed_size,
                    output.len()
                ),
            });
        }
        Ok(output)
    }

    pub(crate) fn looks_like_dwfx(&self, options: ParseOptions) -> Result<bool, DwfError> {
        if !self.contains("[Content_Types].xml") || !self.contains("_rels/.rels") {
            return Ok(false);
        }
        let entry_extension = |suffix: &str| {
            self.entries
                .iter()
                .any(|entry| entry.normalized_name.to_ascii_lowercase().ends_with(suffix))
        };
        // XPS viewers need a FixedDocumentSequence (.fdseq); DWFx writers that
        // skip the XPS half (3D eModel exports among them) still carry the DWF
        // document sequence (.dwfseq).
        let has_xps_sequence = entry_extension(".fdseq");
        let has_dwf_sequence = entry_extension(".dwfseq");
        if !has_xps_sequence && !has_dwf_sequence {
            return Ok(false);
        }
        if has_dwf_sequence && !has_xps_sequence {
            return Ok(true);
        }
        let content_types = self.read_entry("[Content_Types].xml", options.max_xml_size)?;
        let content_types = String::from_utf8_lossy(&content_types).to_ascii_lowercase();
        Ok(content_types.contains("fixeddocumentsequence"))
    }

    /// Normalized entry names, for content classification.
    pub(crate) fn entry_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .map(|entry| entry.normalized_name.as_str())
    }
}

fn open_zip(data: &[u8], prefix_len: usize) -> Result<ZipArchive<Cursor<&[u8]>>, DwfError> {
    debug_assert!(prefix_len <= data.len());
    // DWF 6 packages place a 12-byte DWF signature before the first local ZIP
    // header, but files in the wild do not agree on whether central-directory
    // offsets include that signature. Let `zip` detect the actual archive
    // offset from the EOCD/CDFH positions while keeping the complete byte
    // stream available to it.
    ZipArchive::new(Cursor::new(data)).map_err(zip_error)
}

fn count_central_directory_entries(data: &[u8], start: u64) -> Result<usize, DwfError> {
    let mut offset = usize::try_from(start).map_err(|_| DwfError::InvalidArchive {
        context: format!("central directory offset {start} does not fit in memory"),
    })?;
    if offset > data.len() {
        return Err(DwfError::InvalidArchive {
            context: format!(
                "central directory starts at byte {offset}, beyond input size {}",
                data.len()
            ),
        });
    }

    let mut count = 0usize;
    while data.get(offset..offset.saturating_add(4)) == Some(CENTRAL_DIRECTORY_FILE_HEADER) {
        let header_end = offset
            .checked_add(CENTRAL_DIRECTORY_FILE_HEADER_SIZE)
            .ok_or_else(|| invalid_central_directory(offset))?;
        let header = data
            .get(offset..header_end)
            .ok_or_else(|| invalid_central_directory(offset))?;
        let name_length = usize::from(u16::from_le_bytes([header[28], header[29]]));
        let extra_length = usize::from(u16::from_le_bytes([header[30], header[31]]));
        let comment_length = usize::from(u16::from_le_bytes([header[32], header[33]]));
        let record_length = CENTRAL_DIRECTORY_FILE_HEADER_SIZE
            .checked_add(name_length)
            .and_then(|value| value.checked_add(extra_length))
            .and_then(|value| value.checked_add(comment_length))
            .ok_or_else(|| invalid_central_directory(offset))?;
        offset = offset
            .checked_add(record_length)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| invalid_central_directory(offset))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid_central_directory(offset))?;
    }
    Ok(count)
}

fn invalid_central_directory(offset: usize) -> DwfError {
    DwfError::InvalidArchive {
        context: format!("truncated or overflowing central directory entry at byte {offset}"),
    }
}

fn zip_error(error: zip::result::ZipError) -> DwfError {
    DwfError::InvalidArchive {
        context: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn central_directory_record(name: &[u8], extra: &[u8], comment: &[u8]) -> Vec<u8> {
        let mut record = vec![0; CENTRAL_DIRECTORY_FILE_HEADER_SIZE];
        record[..4].copy_from_slice(CENTRAL_DIRECTORY_FILE_HEADER);
        record[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        record[30..32].copy_from_slice(&(extra.len() as u16).to_le_bytes());
        record[32..34].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        record.extend_from_slice(name);
        record.extend_from_slice(extra);
        record.extend_from_slice(comment);
        record
    }

    #[test]
    fn counts_variable_length_central_directory_records() {
        let mut data = b"prefix".to_vec();
        let start = data.len() as u64;
        data.extend(central_directory_record(b"a", b"extra", b"comment"));
        data.extend(central_directory_record(b"longer/name", b"", b""));
        data.extend_from_slice(b"PK\x05\x06");
        assert_eq!(count_central_directory_entries(&data, start).unwrap(), 2);
    }

    #[test]
    fn rejects_truncated_central_directory_record() {
        let error = count_central_directory_entries(CENTRAL_DIRECTORY_FILE_HEADER, 0)
            .expect_err("truncated header");
        assert!(matches!(error, DwfError::InvalidArchive { .. }));
    }
}
