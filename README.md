# sdm-cli

`sdm` is a command-line client for the Akeneo Supplier Data Manager (SDM) API,
made for people and for LLM agents.

- **Every endpoint is a command.** Commands are generated at runtime from the
  API's OpenAPI spec, so they mirror the API one to one:
  `GET /api/v1/projects/{id}/` is `sdm projects get --id 3`.
- **Plain JSON in and out.** stdout is the API's answer, unchanged; errors are
  JSON on stderr with a distinct exit code per kind of failure.
- **Self-describing.** `sdm describe` lists every command and prints the
  parameters and JSON schemas of any one of them.
- **Scoped.** API commands only run from a directory with a scope file, which
  limits them to the projects and the level of access you chose. Requests
  outside it are refused before anything is sent.
- **Recorded.** Every write sent from a scoped directory is appended to a
  history folder that can be committed.
- **Agent-ready.** The binary carries a Claude Code skill and the agent guide
  ([AGENTS.md](AGENTS.md)).

This is an independent project, not an Akeneo product.

## Install

```sh
cargo install sdm-cli
```

This needs a [Rust toolchain](https://rustup.rs) (1.88 or later) and installs
the `sdm` binary. From a checkout, use `cargo install --path .`.

## Log in

```sh
sdm login      # asks for email and password; or set SDM_EMAIL and SDM_PASSWORD
sdm whoami     # the user and organization the key belongs to
sdm logout
```

`login` creates an API key and stores it in `~/.config/sdm/credentials.json`
(mode 0600); the password is not stored. `SDM_TOKEN` or `--token` overrides the
stored key. The host defaults to `https://sdm.akeneo.cloud`; change it with
`--base-url` or `SDM_BASE_URL`.

## Set a scope

A scope says which projects the CLI may touch from a directory, and how far it
may go. It is required: without a scope file, API commands are refused.

```sh
cd my-sdm-work
sdm scope          # choose projects and access; writes .sdm-scope.json here
sdm scope show     # the scope in force, as JSON
sdm scope clear    # remove it
```

`sdm scope` asks, at a terminal:

- which **projects** to allow;
- the **access** inside those projects, as a level for each group (`project`, `steps`, `jobs`,
  `rules`, `hooks`, `options_collections`, `portals`, `outputs`, `connectors`,
  `metrics`): `none`, `read`, `write` or `delete`, each including the ones
  before. The default is `write` everywhere, so nothing can be deleted;
- whether to open **organization-wide data** (`files`, `bases`,
  `input_formats`, `users`, `organization`, `tasks`), which is closed otherwise;
- where to keep the **history**.

The scope applies to the directory and everything below it. A request that
falls outside it exits with code `6` before anything is sent, and the message
says what was needed and what the scope allows. No flag or environment variable
overrides a scope, and only a person at a terminal can change or clear one, so
an agent working in the directory cannot widen it.

Every write sent from a scoped directory is appended to `.sdmhistory/` next to
the scope file, one JSON line per request (the command, method, URL, body and
status). It is meant to be committed.

To run a command from a directory that has no scope file, pass `--no-scope`:

```sh
sdm projects get --all --no-scope
```

`login`, `logout`, `whoami`, `describe`, `spec` and `skill` need no scope.

## Use

### From an API path to a command

- The fixed parts of a path are nested commands.
- The HTTP method is the verb: `get`, `post`, `put`, `patch`, `delete`.
- Path and query parameters are flags, in kebab-case.

| API call | Command |
|---|---|
| `GET /api/v1/projects/` | `sdm projects get` |
| `GET /api/v1/projects/{id}/` | `sdm projects get --id 3` |
| `PATCH /api/v1/projects/{project_pk}/steps/{id}/` | `sdm projects steps patch --project-pk 1 --id 2` |
| `POST /api/v1/steps/{action_id}/hooks/` | `sdm steps hooks post --action-id 5` |

### Finding a command

```sh
sdm describe                   # one line per command
sdm describe projects          # the commands of one group
sdm describe projects patch    # parameters, body and response schemas, as JSON
sdm projects patch --help      # the same, for people
```

### Reading

```sh
sdm projects get --id 4717
sdm jobs get --project 4717 --all          # every page, as one JSON array
sdm jobs get --project 4717 --page 2 --page-size 50
sdm projects get --id 4717 | jq .steps
```

### Writing

```sh
sdm projects patch --id 4717 --field name="New name"            # one string key
sdm projects patch --id 4717 --field 'available_locales:=["en-US"]'   # one JSON key
sdm projects patch --id 4717 --data '{"name": "New name"}'      # inline JSON
sdm projects patch --id 4717 --data @project.json               # from a file, or - for stdin
sdm files post --field data=@catalog.xlsx --field skiprows:=1   # upload
sdm projects patch --id 4717 --field name=x --dry-run           # print the request, send nothing
```

The body is sent as given; nothing is checked against the spec. `delete`
commands ask for confirmation, or need `--yes` without a terminal.

### Anything the spec does not list

```sh
sdm api GET /projects/4717/
sdm api POST /some/new/path/ --data @body.json
```

### Output and exit codes

stdout is the API's JSON (compact when piped). Errors are JSON on stderr.

| Code | Meaning |
|---|---|
| `0` | ok |
| `2` | usage |
| `3` | authentication |
| `4` | the API answered with an error |
| `5` | network |
| `6` | outside the directory's scope, or no scope file |

### Keeping commands current

```sh
sdm spec info      # which spec is active
sdm spec update    # download the host's current spec and use it from now on
sdm spec reset     # go back to the spec built into the binary
```

## Use from an agent

```sh
sdm skill install --user       # ~/.claude/skills/sdm-cli: every project of yours
sdm skill install --project    # ./.claude/skills/sdm-cli: this project, to be committed
sdm skill status               # whether the installed files match this binary
```

`cargo install` cannot install a skill by itself, so the skill is carried in
the binary. Run `sdm skill install` again after an upgrade. For other agents,
[AGENTS.md](AGENTS.md) is the full guide: naming, bodies, output, exit codes,
safety flags, scopes, history, and what is known about steps and hooks.

Give an agent a scoped directory to work in: it can then only reach what the
scope allows, and what it changed is in the history.

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
  `src/request.rs` the HTTP calls, `src/scope.rs` the per-directory scope
  (`.sdm-scope.json`) and its check, `src/history.rs` the history,
  `src/skill.rs` the skill installer.
- `skill/SKILL.md` and `AGENTS.md` are the skill; both are embedded in the binary.
- [CONTRIBUTING.md](CONTRIBUTING.md) says when to raise the version (shown by
  `sdm version`) and how to publish to crates.io. Read it before changing
  anything.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
