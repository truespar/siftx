# Testing

All test data lives in `testdata/` (gitignored - not committed to the repo).
Total size: ~2 GB across 12 sources.

## On using these corpora

SiftX **does not redistribute any of this data**. `testdata/` is gitignored, no
corpus file is committed, and every test fixture in the source tree is
constructed in code rather than copied from a corpus. What this repository
contains is a list of upstream URLs and a script that clones them - naming a
public repository is not distribution, and needs no permission from its author.

That distinction is what keeps the licences below from mattering to a consumer
of SiftX. Reading a GPL-licensed test image to check that a parser produces the
right tag values does not make the parser a derivative work; the GPL governs
copying and distributing the work, not what you point a program at. What would
create an obligation is copying corpus files, or large verbatim extracts of
ExifTool's `.out` baselines, into this repository - so don't.

The licence notes below are what each project states upstream, recorded as a
convenience. They are not legal advice, and they are not a substitute for
checking the upstream terms if you intend to redistribute any of this data.
Individual files inside these corpora may also carry their own copyright (many
are real photographs), independent of the repository's licence.

## Directory Layout

```
testdata/
|-- exiftool-images/     -> exiftool/exiftool t/images/ (193 files, 1.2 MB)
|-- exiftool-tests/      -> exiftool/exiftool t/ (113 test scripts + 448 .out baselines)
|-- exif-samples/        -> github.com/ianare/exif-samples (99 images, 106 MB)
|-- exif-orientation/    -> github.com/recurser/exif-orientation-examples (18 images, 11 MB)
|-- pdfjs-pdfs/          -> mozilla/pdf.js test/pdfs (904 PDFs, 118 MB)
|-- poppler-test/        -> gitlab.freedesktop.org/poppler/test (80 PDFs + 45 images, 14 MB)
|-- verapdf-corpus/      -> github.com/veraPDF/veraPDF-corpus (2907 PDFs, 246 MB)
|-- pdfa-testsuite/      -> github.com/bfocom/pdfa-testsuite (33 PDFs, 4.2 MB)
|-- pdfbox-testfiles/    -> github.com/apache/pdfbox-testfiles (JBIG2 data, 2.8 MB)
|-- format-corpus/       -> github.com/openpreserve/format-corpus (194 PDFs + 26 images, 698 MB)
|-- fuzzing-seeds/       -> github.com/ForAllSecure/starter-testsuites (1277 images + 202 PDFs, 785 MB)
|-- markitdown-tests/    -> microsoft/markitdown packages/markitdown/tests/test_files (7 PDFs + 25 others, 5 MB)
`-- pdf-corpora-index/   -> github.com/pdf-association/pdf-corpora (index/reference only)
```

## Corpus Details

### Image / EXIF Corpora

#### ExifTool Test Images
- **Path:** `testdata/exiftool-images/`
- **Source:** `github.com/exiftool/exiftool` (`t/images/` and `t/`)
- **Contents:** 193 files covering JPEG (41), TIFF (2), PNG, GIF, BMP, WebP, PSD, RAW (CR2, CR3, NEF, RAF, RW2, DNG, etc.), audio/video, documents
- **Baselines:** `testdata/exiftool-tests/*.out` - 448 expected output files for validation
- **License:** Same terms as Perl - Artistic License or GPL, at your option
  (reference only; SiftX is a clean-room implementation and copies no ExifTool code)
- **Use:** Primary EXIF validation source. Compare our output against ExifTool's `.out` baselines.

#### ianare/exif-samples
- **Path:** `testdata/exif-samples/`
- **Source:** `github.com/ianare/exif-samples`
- **Contents:** 99 JPEG/TIFF images from various cameras and devices
- **License:** No licence stated upstream; the repository is archived
- **Use:** Broad camera coverage for EXIF tag extraction testing.

#### recurser/exif-orientation-examples
- **Path:** `testdata/exif-orientation/`
- **Source:** `github.com/recurser/exif-orientation-examples`
- **Contents:** 18 images - all 8 EXIF orientation flag values in landscape + portrait
- **License:** See upstream repository
- **Use:** Validate EXIF orientation tag handling (tag 0x0112).

### PDF Corpora

#### Mozilla pdf.js Test PDFs (highest value)
- **Path:** `testdata/pdfjs-pdfs/`
- **Source:** `github.com/mozilla/pdf.js` (sparse checkout of `test/pdfs/`)
- **Contents:** 904 actual PDF files + 447 `.link` files referencing external PDFs = 1351 total entries
- **License:** Apache 2.0 (code); individual PDFs have varied origins
- **Use:** Best edge-case corpus. These are real-world PDFs that exposed parsing bugs. Covers broken structures, unusual fonts, complex forms, encryption, linearized PDFs, etc.
- **Note:** `.link` files contain URLs to additional test PDFs that can be downloaded.

#### Poppler Test Data
- **Path:** `testdata/poppler-test/`
- **Source:** `gitlab.freedesktop.org/poppler/test`
- **Contents:** 80 PDFs + 45 images + expected output files
- **License:** Mixed; see upstream repository
- **Use:** Official test suite for the tool we are replacing. Has expected text extraction output for comparison.

#### veraPDF Corpus
- **Path:** `testdata/verapdf-corpus/`
- **Source:** `github.com/veraPDF/veraPDF-corpus`
- **Contents:** 2907 PDFs testing PDF/A-1 (a/b), PDF/A-2 (a/b/u), PDF/A-3 (a/b/u), PDF/A-4, PDF/UA-1, PDF/UA-2
- **License:** See upstream repository (PDF Association TWG)
- **Use:** Systematic conformance testing. Each file targets a specific spec requirement with pass/fail classification.

#### BFO PDF/A Test Suite
- **Path:** `testdata/pdfa-testsuite/`
- **Source:** `github.com/bfocom/pdfa-testsuite`
- **Contents:** 33 PDFs with pass/fail cases and explanations
- **License:** See upstream repository
- **Use:** PDF/A conformance validation with documented reasoning.

#### OPF Format Corpus
- **Path:** `testdata/format-corpus/`
- **Source:** `github.com/openpreserve/format-corpus`
- **Contents:** 194 PDFs + 26 images + other formats (646 files total)
- **License:** CC0 (public domain) unless noted per file
- **Use:** Multi-format testing. CC0 license makes these safe to use freely. Good for file-type detection layer.

#### MarkItDown Test Files
- **Path:** `testdata/markitdown-tests/`
- **Source:** `github.com/microsoft/markitdown` (`packages/markitdown/tests/test_files/`)
- **Contents:** 32 files, 5 MB - 7 PDFs plus DOCX, PPTX, XLSX/XLS, HTML, JPEG, MP3/WAV and an Outlook `.msg`
- **License:** MIT for the repository. The test files carry their own provenance;
  `packages/markitdown/ThirdPartyNotices.md` is the upstream record, and is worth
  reading before reusing any individual file for anything but local testing.
- **Use:** Small, deliberately-shaped layout cases rather than a broad corpus, and
  the PDFs are aimed at what our text extraction is weakest at: a borderless table
  (`SPARSE-2024-INV-1234`), a multipage invoice (`REPAIR-2022-INV-001`), partial list
  numbering (`masterformat_partial_numbering`), a scan with no text layer
  (`MEDRPT-2024-PAT-3847`), and two receipt/booking layouts. Their suite asserts
  substrings present and absent rather than byte-exact output, which is the right
  shape for reading-order checks - see the note below.

**On reusing their assertions.** MarkItDown's `_test_vectors.py` describes each file
with a `must_include` / `must_not_include` pair and never compares whole documents.
That pattern is worth adopting for layout tests here, and an independent
implementation of it is just a good idea rather than a derived work. Their test code
itself is Python and MIT-only; SiftX is `MIT OR Apache-2.0`, so transcribing it would
narrow the licence on whatever file it landed in for no practical gain. Point tests at
the corpus, do not copy it or the code that reads it.

#### PDFBox Test Files
- **Path:** `testdata/pdfbox-testfiles/`
- **Source:** `github.com/apache/pdfbox-testfiles`
- **Contents:** JBIG2 image data only (main PDF tests are in the PDFBox repo itself)
- **License:** Apache 2.0
- **Use:** JBIG2 image stream decoding tests.

#### PDF Association Corpora Index
- **Path:** `testdata/pdf-corpora-index/`
- **Source:** `github.com/pdf-association/pdf-corpora`
- **Contents:** Meta-index of PDF test corpora (no actual PDFs). References datasets including the CC-MAIN 8M PDF web crawl.
- **Use:** Reference for finding additional test PDFs as needed.

### Fuzzing Corpora

#### ForAllSecure Starter Test Suites
- **Path:** `testdata/fuzzing-seeds/`
- **Source:** `github.com/ForAllSecure/starter-testsuites`
- **Contents:** 1277 images + 202 PDFs + many other format seeds (69320 files total)
- **License:** See upstream repository
- **Use:** Minimal seed files for AFL/libFuzzer-style fuzzing. Start fuzzing from these seeds.

## Mapping Corpora to Build Layers

| Build Layer | Primary Corpus | Secondary Corpus |
|---|---|---|
| Binary primitives / file detection | `format-corpus` (CC0, multi-format) | `fuzzing-seeds` |
| JPEG segment parser | `exiftool-images` (41 JPEGs) | `exif-samples` |
| TIFF/IFD parser | `exiftool-images` (2 TIFFs) | `exif-samples` |
| EXIF decoder | `exiftool-images` + `.out` baselines | `exif-samples` |
| EXIF orientation | `exif-orientation` (all 8 values) | - |
| PNG metadata | `exiftool-images` (1 PNG) | `format-corpus` |
| WebP / HEIF | `exiftool-images` | - |
| XMP parser | `exiftool-images` (10 XMP files) | `exif-samples` |
| IPTC decoder | `exiftool-images` | - |
| ICC profiles | `exiftool-images` (1 ICC) | - |
| Maker notes | `exiftool-images` (per-camera RAW) | - |
| PDF tokenizer / objects | `pdfjs-pdfs` (904 PDFs) | `poppler-test` |
| XRef parsing | `pdfjs-pdfs` | `poppler-test` |
| XRef reconstruction | `pdfjs-pdfs` (damaged files) | `fuzzing-seeds` |
| PDF metadata | `poppler-test` | `pdfjs-pdfs` |
| Stream decompression | `pdfjs-pdfs` | `poppler-test` |
| Font/encoding | `pdfjs-pdfs` | `poppler-test` |
| Text extraction | `poppler-test` (has expected output) | `pdfjs-pdfs` |
| Text layout | `poppler-test` | `format-corpus` |
| Tables and reading order | `markitdown-tests` | `poppler-test` |
| Image extraction | `poppler-test` | `pdfjs-pdfs` |
| PDF/A conformance | `verapdf-corpus` + `pdfa-testsuite` | - |
| JBIG2 streams | `pdfbox-testfiles` | - |
| Robustness / fuzzing | `fuzzing-seeds` | all corpora |

## Re-downloading

If testdata is lost, run from the project root:

```bash
mkdir -p testdata && cd testdata

# ExifTool test images and their expected-output baselines
git clone --depth 1 https://github.com/exiftool/exiftool.git exiftool-src
ln -s exiftool-src/t/images exiftool-images
ln -s exiftool-src/t exiftool-tests

# Image/EXIF
git clone --depth 1 https://github.com/ianare/exif-samples.git exif-samples
git clone --depth 1 https://github.com/recurser/exif-orientation-examples.git exif-orientation

# PDF
git clone --depth 1 https://gitlab.freedesktop.org/poppler/test.git poppler-test
git clone --depth 1 --filter=blob:none --sparse https://github.com/mozilla/pdf.js.git pdfjs-temp \
  && cd pdfjs-temp && git sparse-checkout set test/pdfs && cd .. \
  && mv pdfjs-temp/test/pdfs pdfjs-pdfs && rm -rf pdfjs-temp
git clone --depth 1 https://github.com/veraPDF/veraPDF-corpus.git verapdf-corpus
git clone --depth 1 https://github.com/bfocom/pdfa-testsuite.git pdfa-testsuite
git clone --depth 1 https://github.com/apache/pdfbox-testfiles.git pdfbox-testfiles
git clone --depth 1 https://github.com/pdf-association/pdf-corpora.git pdf-corpora-index

# Multi-format
git clone --depth 1 https://github.com/openpreserve/format-corpus.git format-corpus
git clone --depth 1 --filter=blob:none --sparse https://github.com/microsoft/markitdown.git markitdown-temp \
  && cd markitdown-temp && git sparse-checkout set packages/markitdown/tests/test_files && cd .. \
  && mv markitdown-temp/packages/markitdown/tests/test_files markitdown-tests && rm -rf markitdown-temp

# Fuzzing
git clone --depth 1 https://github.com/ForAllSecure/starter-testsuites.git fuzzing-seeds
```

## File Counts Summary

| Corpus | Images | PDFs | Other | Total | Size |
|---|---|---|---|---|---|
| exiftool-images | 41+ | 2 | 150 | 193 | 1.2 MB |
| exif-samples | 99 | 0 | 45 | 144 | 106 MB |
| exif-orientation | 18 | 0 | 36 | 54 | 11 MB |
| pdfjs-pdfs | 0 | 904 | 447 | 1351 | 118 MB |
| poppler-test | 45 | 80 | 90 | 215 | 14 MB |
| verapdf-corpus | 0 | 2907 | 30 | 2937 | 246 MB |
| pdfa-testsuite | 0 | 33 | 32 | 65 | 4.2 MB |
| pdfbox-testfiles | 0 | 0 | 31 | 31 | 2.8 MB |
| format-corpus | 26 | 194 | 426 | 646 | 698 MB |
| fuzzing-seeds | 1277 | 202 | 67841 | 69320 | 785 MB |
| markitdown-tests | 2 | 7 | 23 | 32 | 5.0 MB |
| **TOTAL** | **~1508** | **~4329** | | **~74988** | **~2 GB** |

## Running the tests

The unit tests need no corpora:

```bash
cargo test --lib
```

The integration tests read from `testdata/` and **skip** rather than fail when a
corpus is absent, so a fresh clone is green without downloading 2 GB:

```bash
cargo test --all-features
```

Two suites also compare SiftX's output against the reference tools, and
skip unless those tools are present:

| Suite | Needs | How it is found |
|---|---|---|
| `tests/exif_real_files.rs` | ExifTool | `$EXIFTOOL`, else `exiftool` on `PATH` |
| `tests/pdf_real_files.rs` | Poppler's `pdftotext`, `pdfinfo`, `pdfimages` | `PATH` |

Neither tool is a build dependency; they are only used to validate that SiftX
reproduces their output.

### Language bindings

```bash
cargo build --release                       # build the native library first

cd bindings/python  && maturin develop && python -m pytest
cd bindings/nodejs  && npm install && npm run build && npm test
cd bindings/csharp  && dotnet test tests/SiftX.Tests/SiftX.Tests.csproj
cd bindings/java    && mvn test              # bundles the library cargo just built into the JAR
```
