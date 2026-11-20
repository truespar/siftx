<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/wordmark-dark.svg">
  <img alt="SiftX" src="docs/assets/wordmark-light.svg" width="204">
</picture>

# SiftX

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

Read metadata out of images, and text, images and metadata out of PDFs - from
one native library, in five languages, under MIT or Apache-2.0.

SiftX covers the read side of [ExifTool](https://exiftool.org/) and the
[Poppler](https://poppler.freedesktop.org/) command-line tools (`pdftotext`,
`pdfimages`, `pdfinfo`), as one embeddable library instead of various runtimes
and a set of binaries installed alongside your application. It does not write
metadata back, and it does not render.

## Why SiftX

**The licence.** Poppler is GPL-2.0/3.0, exiv2 is GPL-2.0-or-later with no
commercial option since its dual licensing was withdrawn, and PyMuPDF is
AGPL-3.0 unless you buy a licence from Artifex. SiftX is MIT OR Apache-2.0,
written from the published format specifications rather than ported from an
existing tool, so it links into anything.

**Both halves in one library.** Image metadata and PDF content are normally two
dependencies with two licences and two APIs. SiftX reads both, through one API,
from Rust, Python, Node.js, C#, Java and C.

## What it does

**From images** - EXIF, IPTC, XMP, ICC profiles, and manufacturer maker notes,
as a flat list of tags or as typed accessors:

- Camera, lens, exposure, ISO, orientation, timestamps
- GPS position as decimal degrees, with altitude and timestamp
- The embedded JPEG thumbnail, extracted as bytes
- Maker notes for Canon, Nikon, Sony, Olympus, Pentax, Panasonic, Fujifilm,
  Kodak, Ricoh, Samsung and others (Canon `CameraSettings`/`ColorData`, Nikon
  custom settings, Pentax `AEInfo`)

**From PDFs**:

- Text, either with layout preserved or in content-stream order
- Embedded images, passed through untranscoded where possible - a JPEG stored
  in the PDF comes back as the original JPEG bytes, not a re-encode
- Document metadata: title, author, dates, page count, page geometry,
  encryption state, PDF version, tagged status
- AcroForm fields, all 28 annotation types, and the tagged-PDF structure tree
- Encrypted documents (RC4 and AES-128/256, revisions 2-6) once authenticated

**From movies** (QuickTime, MP4, M4A) - brands, dates, tracks and codecs, and the
length in seconds (`duration()`: the movie header's, a fragmented file's, or the
longest track's), read from a mapped file without loading the media

**Formats read:** JPEG, TIFF, PNG, WebP, HEIC/HEIF, GIF, BMP, PDF,
QuickTime/MP4, standalone ICC profiles, and camera RAW (CR2, CR3, NEF, ARW,
DNG, ORF, PEF, RW2, RAF, SRW).

Each format is a Cargo feature, so a build that only needs JPEG and PDF does
not carry the rest.

SiftX only reads. It does not write metadata back, and it does not render.

## Who it's for

**Software that ingests files from other people** - document pipelines, photo
libraries, archives, anything that accepts an upload.

**Code that hands files to a model.** A vision model cannot see what is not
pixels: the GPS coordinate, the capture time, the lens, the author, the form
field. SiftX pulls those out as typed values that go straight into a prompt,
and separates a PDF's text from its embedded images so each reaches the model
in the form it handles best - text as text, images as images, without
re-encoding a page into a screenshot.

Because it is in-process, an agent does not need a tool installed or a
subprocess spawned. The CLI exists mainly for local inspection and testing.

## Install

| Ecosystem | Install | Import |
|---|---|---|
| Rust | `cargo add truespar-siftx` | `use siftx::...` |
| Python | `pip install truespar-siftx` (3.10+) | `import siftx` |
| Node.js | `npm install @truespar/siftx` (20+) | `require("@truespar/siftx")` |
| C# / .NET | `dotnet add package Truespar.SiftX` (net8.0, net10.0) | `using SiftX;` |
| Java | `com.truespar:siftx:0.1.0` (Java 25+) | `com.truespar.siftx` |
| C / C++ | build with `--features ffi`, link, `include/siftx.h` | `#include <siftx.h>` |

```xml
<dependency>
  <groupId>com.truespar</groupId>
  <artifactId>siftx</artifactId>
  <version>0.1.0</version>
</dependency>
```

Packages are published under the `truespar` namespace because `sift` and
`siftx` are both taken on some registries by unrelated projects. What you write
in code is unaffected: `use siftx::`, `import siftx` and `using SiftX;` read as
they always did.

Python, Node.js, C# and Java ship the native library inside the package, so
there is nothing to install separately. Prebuilt Node.js binaries cover macOS
(arm64, x64), Linux (arm64 and x64, gnu and musl) and Windows (x64).

## Quick start

**Rust**

```rust
// One call, when you just want the tags:
for tag in siftx::tags("photo.jpg")? {
    println!("[{}] {} = {}", tag.group, tag.name, tag.value);
}

// Or open once and ask several questions. The file is memory-mapped and the
// document borrows from it, so nothing is copied until you ask for bytes.
let file = siftx::open("scan.pdf")?;
let doc = file.parse()?;

for (n, page) in doc.text_pages()?.iter().enumerate() {
    println!("--- page {} ---\n{page}", n + 1);
}
for image in doc.images()? {
    std::fs::write(format!("img{}.{}", image.page, image.extension()), image.bytes())?;
}
```

**Python**

```python
import siftx

for tag in siftx.tags("photo.jpg"):
    print(f"[{tag.group}] {tag.name} = {tag.value}")

with siftx.SiftFile.open("scan.pdf") as f:
    doc = f.parse()
    text = doc.text_pages()
    images = doc.images()
```

**Node.js**

```js
const { tags, SiftFile } = require("@truespar/siftx")

for (const tag of tags("photo.jpg")) {
  console.log(`[${tag.group}] ${tag.name} = ${tag.value}`)
}

const doc = SiftFile.open("scan.pdf").parse()
const text = doc.textPages()
doc.close()
```

**C#**

```csharp
using SiftX;

foreach (var tag in SiftLib.Tags("photo.jpg"))
    Console.WriteLine($"[{tag.Group}] {tag.Name} = {tag.Value}");

using var doc = SiftLib.Read(File.ReadAllBytes("scan.pdf"));
var text = doc.TextPages();

// Or typed, without string parsing:
Console.WriteLine(doc.Exif.Model);                        // "Canon EOS 5D"
Console.WriteLine(doc.GpsInfo.Coordinates?.Latitude);     // 48.8583 (decimal degrees)
```

**Java**

```java
import com.truespar.siftx.*;

for (Tag tag : SiftX.tags("photo.jpg"))
    System.out.printf("[%s] %s = %s%n", tag.group(), tag.name(), tag.value());

try (var file = SiftFile.open("scan.pdf"); var doc = file.parse()) {
    List<String> text = doc.textPages();
}
```

## API

| Call | Returns |
|---|---|
| `tags()` | every tag, flat, display-formatted |
| `exif_tags()` / `xmp_tags()` / `iptc_tags()` | one group only |
| `gps()` | decimal degrees, altitude, timestamp |
| `thumbnail()` | embedded JPEG thumbnail bytes |
| `text_pages()` / `text_pages_raw()` | text per page, with or without layout |
| `images()` | embedded images with geometry and bytes |
| `acro_form()` | form fields and document-level flags |
| `all_annotations()` | every annotation with rect, contents, destination |
| `struct_tree()` | tagged-PDF structure tree |
| `authenticate(password)` | unlock an encrypted PDF |
| `file_type()` | detected format |

Tags carry a `typed_value` alongside the display string.

## Command line

```bash
siftx tags photo.jpg       # metadata tags  (--exif / --xmp / --iptc / --json)
siftx thumbnail photo.jpg  # embedded EXIF thumbnail
siftx text document.pdf    # extracted text (--raw for no layout)
siftx images document.pdf  # embedded images
siftx info document.pdf    # document metadata
siftx forms fillable.pdf   # AcroForm fields
siftx annots annotated.pdf # annotations
siftx struct tagged.pdf    # tagged-PDF structure tree
```

`siftx --help` lists every option. Encrypted PDFs take `--password`.

## Accuracy

These are test output. Reproduce them with `cargo test --all-features` and the
reference tools installed; the test data and the procedure are in
[docs/testing.md](docs/testing.md).

What is being measured is agreement with the reference tool on the formats
SiftX supports. ExifTool recognises far more tags across far more file types
than SiftX does, and Poppler does a great deal that SiftX does not. This is
overlap accuracy, not feature parity - contributions that close the gap are
welcome.

**Metadata** - 1334 files, 8579 tags that both tools emit:

| Group | Coverage | Value match |
|---|---|---|
| EXIF (IFD0, ExifIFD, GPS, Interop) | 6930/6999 (99.0%) | 6930/6930 (100%) |
| XMP | 1388/1388 (100%) | 1387/1388 (99.9%) |
| IPTC | 192/192 (100%) | 192/192 (100%) |
| Maker notes | 3804/4164 (91.4%) | 3792/3804 (99.7%) |

**PDF** - against Poppler:

| Comparison | Result |
|---|---|
| `pdftotext` word overlap | 82669/87692 (94.3%) over 3891 PDFs; 1226 byte-identical (81.1%) |
| `pdfinfo` fields | 4050/4118 PDFs; 9 of 16 fields match exactly, the other 7 at 99% |
| `pdfimages` counts | 414/422 PDFs agree on image count (98.1%) |
| `pdfimages` fields | type 99.2%, bpc 98.7%, components 98.6%, colour 97.5%, **width 83.2%, height 83.4%** |

One known divergence is larger than the rest: reported image width and height,
at 83%. On a focused 59-PDF subset the same fields agree on all 281 images, so
it is specific to cases the wider sweep reaches.

## Untrusted input

Every byte SiftX reads comes from a file somebody else produced, and the
parsers are written on that assumption: bounds-checked throughout, with cycle
guards on every recursive structure. Thirteen fuzz targets cover the format
parsers - JPEG, TIFF, PNG, WebP, HEIF, GIF, BMP, XMP, IPTC, ICC, QuickTime,
PDF, and the auto-detecting entry point.

A truncated or malformed file yields the tags that are readable rather than an
error.

Suspected vulnerabilities go through [SECURITY.md](SECURITY.md).

## Design

Two interop surfaces: a C ABI (`siftx.h`) used by C#, Java and C/C++, and
direct native bindings for Python (PyO3) and Node.js (napi-rs).

## Building

```bash
cargo build --release
cargo test
```

`./scripts/check.sh` runs everything at once - formatting, tests, docs, every
feature combination, clippy, licence policy and the fuzz targets - and
`--bindings` adds the four language suites. `scripts/check.ps1` is the Windows
equivalent.

See [docs/testing.md](docs/testing.md) for the test data and how to run each
binding's suite.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). One rule matters more than the rest:
SiftX is written **from specifications only**, and no code may be copied or
transcribed from ExifTool, Poppler, or any other existing implementation.

Participation is governed by our [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option. MIT is shorter and is compatible with GPLv2; Apache-2.0
additionally grants an explicit patent licence. Take whichever suits you - you
do not have to satisfy both.

Contributions are accepted under the same dual licence unless you say otherwise.

Third-party crate licences are reproduced in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
