# sdm-cli

`sdm` is a command-line client for the Akeneo Supplier Data Manager (SDM) API,
for people and for LLM agents. Its commands are generated at runtime from the
API's OpenAPI spec, so they mirror the API one to one.

## Install

```sh
cargo install --path .
```

## Use

```sh
sdm login                      # or set SDM_EMAIL / SDM_PASSWORD
sdm projects get --all
sdm projects get --id 4717
sdm describe                   # index of every command
sdm describe projects patch    # parameters and schemas as JSON
```

[AGENTS.md](AGENTS.md) is the full guide: the naming rule, bodies, output, exit
codes, safety flags, and what is known about steps and hooks.

## Development

```sh
cargo test                     # unit tests and end-to-end tests against a local server
cargo clippy --all-targets
```

- `spec/sdm-openapi.json` is the embedded snapshot of
  `https://sdm.akeneo.cloud/api/v1/schema/?format=json`. Replace the file to
  refresh it.
- `spec/overlay.json` holds notes for calls the spec documents badly. Keys are
  `"METHOD /path"`; `notes` is shown in `--help` and `sdm describe`.
- `src/spec.rs` builds the command tree, `src/cli.rs` the command line,
  `src/request.rs` the HTTP calls.
