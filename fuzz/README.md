# DWF parser fuzz targets

Install `cargo-fuzz` and a nightly Rust toolchain, then run:

```console
cargo +nightly fuzz build
cargo +nightly fuzz run decode_w2d fuzz/corpus/decode_w2d
cargo +nightly fuzz run inspect_dwfx fuzz/corpus/inspect_dwfx
```

libFuzzer writes coverage-increasing inputs back into the supplied corpus
directory. Review and minimize those generated files before committing them;
crash artifacts remain ignored under `fuzz/artifacts/`.

The targets use deliberately smaller parse limits than production so malformed
archives, OPC relationships, XML, path data, compression streams, counts,
strings, and nesting fail quickly and predictably. The committed corpora are
generated data only; third-party DWF/DWFx files are not redistributed here.

`inspect_dwfx` also reaches the Phase 6 UTF-16 XML normalizer and explicit clip
geometry builder plus the Phase 7 resource pre-scan, remote dictionary,
gradient, opacity-mask, brush-transform, and transformed-geometry paths when
mutations retain a valid OPC envelope. The 7.3 MB `ECMA-388.xps`
interoperability file remains an optional hash-verified integration sample
rather than a committed fuzz seed.

`fuzz/corpus/inspect_dwfx/visual-image-font.dwfx` is a generated, redistributable
package that keeps a valid OPC envelope around nested Canvas groups, inline and
static VisualBrush execution, a 192-DPI PNG viewbox crop, and a GUID-named
obfuscated-font resource. The font payload is deliberately synthetic: it
exercises bounded deobfuscation and invalid-font fallback without committing a
third-party typeface; successful packaged-font outlining is covered by the
hash-verified ECMA integration test.
