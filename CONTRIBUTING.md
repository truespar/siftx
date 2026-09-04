# Contributing to SiftX

Thanks for your interest. This document covers the development workflow and the
conventions the codebase follows.

## The one rule that is not negotiable

**SiftX is a clean-room implementation. Do not copy, translate, or closely
transcribe code from ExifTool, Poppler, or any other GPL-licensed project.**

This is what lets SiftX be `MIT OR Apache-2.0` while doing the same job, and a
single pasted function would compromise that for every downstream consumer.
Concretely:

- Write from specifications and from observed behaviour, not from another
  project's source.
- Describing *what the output is* is fine and is the point of the project -
  "matches ExifTool's `1/60` formatting for exposures under 0.25s" tells a
  reader why a line exists. Quoting *how another project computes it* is not:
  no Perl fragments, no transcribed C++, no "ported from `Nikon.pm` line 4102".
- If you needed to look at GPL source to answer a question, say so in the pull
  request so it can be reviewed properly. That is far better than it being
  discovered later.

Specifications, format documentation, and your own experiments against real
files are all fair game.

## Getting set up

A stable Rust toolchain is the only requirement for the core library:

```bash
cargo build --release
cargo test
```

The language bindings each need their own toolchain - .NET 10 SDK, JDK 25,
Python 3.10+, or Node.js 20+ - but only for the binding you are working on.

## Build and check

```bash
cargo check --all-features --all-targets   # fastest full-tree check
cargo build --release                      # optimised library and CLI
cargo test --all-features                  # everything
cargo clippy --all-features --all-targets  # lints
cargo fmt --all                            # formatting
cargo deny check                           # licences and advisories
```

The compiler build is warning-free and `cargo fmt --all -- --check` passes.
Nothing enforces that automatically yet, so it is on you and on review.

`scripts/check.sh` runs all of it in one go - formatting, build, tests, docs
under `-D warnings`, every feature combination, clippy, cargo-deny and the fuzz
targets - and reports pass or fail per check without stopping at the first
failure. `--bindings` adds the four language suites. `scripts/check.ps1` is the
Windows equivalent. The same checks are defined as CI workflows, which run on
pushes to `main` and on pull requests once Actions is enabled for the
repository - it is not yet, so today `scripts/check.sh` is the check. See
[docs/ci/](docs/ci/) for what those workflows do and do not cover.

**Clippy has no errors.** Its deny-level lints - the ones that flag a probable
defect rather than a style preference - are at zero, and should stay there.
Roughly 380 style warnings remain; don't feel obliged to fix that backlog in an
unrelated pull request, but don't add to it either.

**Every `extern "C"` function has a `# Safety` section** describing what the
caller must guarantee. If you add one, write its contract too: for the C ABI
that section *is* the API, and a caller in another language has nothing else to
go on.

## Testing

Unit tests need nothing. Integration tests read corpora from `testdata/` and
**skip rather than fail** when a corpus is absent, so a fresh clone is green
without downloading 2 GB. See [docs/testing.md](docs/testing.md) for the
inventory, the download script, and how to run each binding's suite.

Two suites compare SiftX's output against the reference tools and skip unless
those are installed: `tests/exif_real_files.rs` needs ExifTool (`$EXIFTOOL`, or
on `PATH`) and `tests/pdf_real_files.rs` needs Poppler's `pdftotext`,
`pdfinfo`, and `pdfimages`. Neither is a build dependency.

Add tests for behaviour changes. For a parser fix, the useful test is usually
the smallest hand-constructed file that reproduces it - build fixtures in code
rather than committing a sample, both to keep the repository small and because
corpus files are not ours to redistribute.

### Fuzzing

`fuzz/` has a libFuzzer target per format family. Every parser change is worth
a few minutes of fuzzing:

```bash
cargo +nightly fuzz run fuzz_pdf
```

## Conventions

**Errors.** One `Error` enum in `src/core/error.rs`, propagated everywhere. Add
a variant rather than stringly-typing a new failure mode.

**Never panic on input.** Every byte SiftX reads is attacker-controlled. Use the
bounds-checked helpers in `src/core/reader.rs`; do not index a slice directly
with a value that came from the file, and do not allocate based on a length
field without checking it against the data actually present.

**Recursion.** Anything that can nest - IFDs pointing at IFDs, PDF objects
referencing each other, Form XObjects - goes through `RecursionGuard`. Malformed
files with reference cycles are common, not hypothetical.

**Partial results beat failure.** A truncated or slightly malformed file should
yield the tags that *are* readable rather than an error. Consumers are usually
processing a directory and would rather have nine tags than an exception.

**Feature flags.** Each format is behind a cargo feature. A new format gets its
own, added to `default`.

**The public API is small on purpose.** Parser modules are `pub(crate)`; what
is public is the re-exports in `lib.rs` plus `core` and `ffi`. The non-default
`internals` feature re-exposes the parsers so the integration tests can reach
them, and the crate's own dev-dependency turns it on, so `cargo test` needs no
flag. `internals` has no stability guarantee.

If a test needs something that is not public, prefer `internals` over widening
the API. If a *user* needs it, that is a missing method on `SiftDocument`, not
a module to publish - every item made public is one we owe semver on.

**Native library packaging.** Neither the Java nor the Node.js binding asks a
user to install anything. Java stages the library into the JAR under
`/native/<os>-<arch>/` and extracts it at runtime; Node.js publishes one
package per platform and lets npm pick by `os`/`cpu`/`libc`. Both fall back to
a locally built library for development - Java via `SIFTX_NATIVE_LIB_PATH`,
Node via the `.node` file left in the binding directory - so neither has to be
published for you to work on it.

**FFI struct changes.** If you add a field to a struct in `include/siftx.h`,
every binding that describes that layout must be updated in the same commit -
the C# struct in `Native.cs` and the FFM layout in `Native.java`. A short layout
is not a truncated read, it is a heap overflow; `NativeLayoutTest` guards the
sizes.

## Licences and third-party notices

Two separate things:

- **Policy** - `deny.toml` lists which licences may appear in the dependency
  graph. `cargo deny check` fails on anything else, so a new dependency with an
  unexpected licence is a deliberate decision rather than a surprise.
- **Attribution** - MIT and BSD require their copyright notices to ship with a
  binary. `THIRD-PARTY-NOTICES.md` is generated from the resolved graph.
  Regenerate it whenever dependencies change:

```bash
cargo about generate --fail about.hbs | tr -d '\r' > THIRD-PARTY-NOTICES.md
```

SiftX is dual-licensed `MIT OR Apache-2.0`; contributions are accepted under the
same terms unless you say otherwise.

## Pull requests

- Keep commits focused, and explain *why* in the message rather than restating
  the diff.
- `cargo fmt --all -- --check` and `cargo deny check` must pass. Clippy must
  report no errors; its style warnings are advisory - see above.
- Add tests for behaviour changes.
- Update the README or `docs/` when you change setup or behaviour.
- If a change affects a tag's output value, say which files you checked it
  against - accuracy against real files is the whole point of this library.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).
