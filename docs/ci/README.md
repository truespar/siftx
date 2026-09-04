# Continuous integration

The workflows live in [`.github/workflows/`](../../.github/workflows/):
`ci.yml` for the Rust tree, `bindings.yml` for the four language bindings.
This file records what they cover, what they cannot, and what they cost.

They were kept inert here for a while, on the assumption that Actions minutes
would be billed. They are not: this repository is public, and public
repositories run on standard runners for free. The per-minute multipliers
below still apply to a private fork.

## Running the same checks locally, for free

`scripts/check.sh` (or `scripts/check.ps1` on Windows) runs everything the
`rust` job would, and prints a pass/fail line per check:

```bash
./scripts/check.sh              # Rust only
./scripts/check.sh --bindings   # also C#, Java, Python, Node.js
```

It needs no network. The binding checks are skipped where the toolchain is
absent, so it is useful on a machine that only has Rust.

## What runs, and when

`push` to `main`, every pull request, and manual dispatch. A second push to a
pull request supersedes the one before it: both workflows set a `concurrency`
group keyed on workflow, event and ref, and cancel in progress *only* for
`pull_request`. Push and dispatch runs are deliberately left to finish - a push
to `main` must not cancel a manual `cross-platform` run on the same ref, and a
completed run per commit is what makes a break easy to attribute.

Worth knowing before you open a pull request, because it is the job most likely
to fail for a reason unrelated to your change: **`licences`** runs on every push
and every pull request, like the rest. It runs `cargo deny check`, then
regenerates `THIRD-PARTY-NOTICES.md` with a pinned `cargo-about` and fails if
the committed file differs. MIT and Apache-2.0 require their notices to ship
with the binary, so a stale file is a compliance gap rather than a cosmetic one
- regenerate it with the command in `about.toml` when you change a dependency.

One job is deliberately not on that trigger:

- **`cross-platform`** (macOS, Windows) is `workflow_dispatch` only, and
  advisory (`continue-on-error`). The library claims to be cross-platform but
  has only ever been exercised on Linux; promote it to a required check once it
  has been green for a while. On a private repository macOS bills at 10x and
  Windows at 2x the Linux rate, which is the other reason it is manual.

`Swatinem/rust-cache` writes a multi-gigabyte cache per job. The Actions cache
allowance is 10 GB per repository whatever its visibility, so this is worth
watching as jobs are added, public or not. The `concurrency` group above keeps
superseded *pull-request* runs from filling it, and `cargo-about` is cached by
version rather than rebuilt from source on every run. Each job that builds
outside the root workspace - `fuzz`, and the Python and Node.js bindings, all
of which are separate crate roots - names its own `workspaces` for
`rust-cache`, without which their `target` directories are not cached at all.

Both workflows declare `permissions: contents: read`. Nothing here writes to the
repository, and the jobs run third-party actions and build scripts from a pull
request's own head, so the token they are handed should not carry write scope.

## What CI can and cannot check here

The integration suites read corpora from `testdata/`, which is about 2 GB and
not committed. They skip rather than fail when it is absent, so CI proves the
tree builds, the unit tests pass, and the skip paths work - it does not
reproduce the accuracy figures in the README. Those need the corpora and the
reference tools, and are run locally. See [../testing.md](../testing.md).

## Dependency updates

[`.github/dependabot.yml`](../../.github/dependabot.yml) batches version
updates into one grouped pull request per ecosystem per month, across the five
ecosystems the workflows build: cargo (the root plus `fuzz/` and the two native
bindings, which are separate crates with their own lockfiles), npm, maven,
nuget and the actions themselves.

Every ecosystem that supports a cooldown gets seven days, so a version yanked
shortly after publication never reaches a lockfile. `github-actions` is the
exception - it has no cooldown option - which is worth knowing precisely
because it is the ecosystem most likely to rot unattended. Security updates are
a separate mechanism, are not shaped by that schedule or by the groups, and are
enabled in the repository's security settings rather than in the file.

`rust-toolchain.toml` is not covered. Dependabot can update a toolchain file,
but only when the channel names a version (`1.90.0`) or a dated nightly; this
one says `stable`, which has nothing to bump. Pinning it would make the
toolchain reviewable and Dependabot-updatable, at the cost of no longer picking
up a new stable automatically - a trade worth making deliberately, not as a
side effect of turning CI on.
