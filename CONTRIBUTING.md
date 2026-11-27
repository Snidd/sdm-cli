# Contributing to sdm-cli

For people and agents who change this repository. [AGENTS.md](AGENTS.md) is
the guide to *using* `sdm`; [README.md](README.md#development) describes the
layout of the code.

## Versioning

The version is the `version` field of `Cargo.toml`. `sdm version`,
`sdm --version`, the `User-Agent` header, `sdm skill status` and the skill hint
all read it, so a change released without a new version is invisible to
installed copies.

**Every change to what the crate does or ships raises the version, in the same
commit.** That covers the code in `src/`, `spec/`, `skill/SKILL.md` and
`AGENTS.md` (the last two are embedded in the binary).

1. Edit `version` in `Cargo.toml`.
2. Run `cargo build` so `Cargo.lock` follows, and commit both files.

Which number to raise, while the version is below 1.0:

| Change | Raise | Example |
|---|---|---|
| Bug fix, refreshed spec, text of the skill or of `AGENTS.md` | patch | `0.2.0` to `0.2.1` |
| New command or flag; anything that changes or removes existing behaviour, output or exit codes | minor | `0.2.0` to `0.3.0` |

No new version is needed for changes that reach no user of the binary: tests,
`README.md`, this file.

If the version in `Cargo.toml` is already higher than the newest one on
crates.io (`cargo search sdm-cli`), an unreleased change is pending: raise it
further only when your change needs a bigger step (a minor instead of a patch).

## Releasing to crates.io

A published version is permanent: it can be yanked but never replaced or
reused. Agents: do not run `cargo publish`, create tags or push unless the user
asks for exactly that.

From a clean, up-to-date `master` whose version is not yet on crates.io:

```sh
cargo test
cargo clippy --all-targets
cargo package --list           # the files that will be uploaded
cargo publish --dry-run        # builds the package as crates.io would

git tag v0.2.0                 # the version in Cargo.toml
git push && git push --tags

cargo publish                  # needs `cargo login` once, with a crates.io API token
```

Then check the result:

```sh
cargo install sdm-cli
sdm version                    # the version just published
sdm skill install --user       # refresh an installed skill
```

`.sdm-scope.json` and `.sdmhistory/` are kept out of the package by `exclude`
in `Cargo.toml`. Look at the output of `cargo package --list` before publishing
and add anything else that should not be public.
