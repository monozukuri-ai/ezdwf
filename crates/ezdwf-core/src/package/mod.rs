pub(crate) mod archive;
mod eplot;
mod manifest;
pub(crate) mod path;
pub(crate) mod xml;

use archive::PackageArchive;
use eplot::parse_eplot;
use manifest::parse_manifest;

use crate::{decode_w2d, detect_format, DwfError, DwfFormat, DwfPackage, ParseOptions};

/// Inspect a DWF 6 package and resolve its manifest and ePlot descriptors.
pub fn inspect_package(data: &[u8], options: ParseOptions) -> Result<DwfPackage, DwfError> {
    let format = detect_format(data, options)?;
    let prefix_len = match &format {
        DwfFormat::DwfPackage { .. } => format.package_prefix_len(),
        _ => {
            return Err(DwfError::UnsupportedFormat { format });
        }
    };

    let archive = PackageArchive::open(data, prefix_len, options)?;
    let manifest_xml = archive.read_entry("manifest.xml", options.max_xml_size)?;
    let mut manifest = parse_manifest(&manifest_xml, "manifest.xml", options)?;
    let mut diagnostics = Vec::new();

    for section in &mut manifest.sections {
        for resource in &section.resources {
            if !archive.contains(&resource.normalized_href) {
                return Err(DwfError::MissingResource {
                    section: section.name.clone(),
                    href: resource.href.clone(),
                    normalized: resource.normalized_href.clone(),
                });
            }
        }

        if !section.is_eplot_sheet() {
            continue;
        }
        let descriptor = section
            .resources
            .iter()
            .find(|resource| resource.role.eq_ignore_ascii_case("descriptor"))
            .ok_or_else(|| DwfError::InvalidManifest {
                context: format!(
                    "ePlot section {:?} has no descriptor resource",
                    section.name
                ),
            })?;
        let descriptor_xml =
            archive.read_entry(&descriptor.normalized_href, options.max_xml_size)?;
        let page = parse_eplot(
            &descriptor_xml,
            &descriptor.normalized_href,
            &section.name,
            options,
        )?;
        for resource in &page.resources {
            if !archive.contains(&resource.normalized_href) {
                return Err(DwfError::MissingResource {
                    section: section.name.clone(),
                    href: resource.href.clone(),
                    normalized: resource.normalized_href.clone(),
                });
            }
        }
        section.page = Some(page);

        let w2d_resources = section
            .resources
            .iter()
            .filter(|resource| resource.mime.eq_ignore_ascii_case("application/x-w2d"))
            .cloned()
            .collect::<Vec<_>>();
        for resource in w2d_resources {
            let bytes = archive.read_entry(&resource.normalized_href, options.max_entry_size)?;
            let mut stream = decode_w2d(&bytes, &resource.normalized_href, options)?;
            stream.href = resource.href.clone();
            stream.role = resource.role.clone();
            stream.mime = resource.mime.clone();
            if let Some(page_resource) = section.page.as_ref().and_then(|page| {
                page.resources
                    .iter()
                    .find(|candidate| candidate.normalized_href == resource.normalized_href)
            }) {
                stream.transform = page_resource.transform.clone();
                stream.clip = page_resource.clip.clone();
            }
            for diagnostic in &mut stream.diagnostics {
                diagnostic.section = Some(section.name.clone());
            }
            diagnostics.extend(stream.diagnostics.iter().cloned());
            section.w2d_streams.push(stream);
        }
    }

    Ok(DwfPackage {
        format,
        entries: archive.entries().to_vec(),
        manifest,
        diagnostics,
    })
}
