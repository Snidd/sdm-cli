# Using `sdm` (guide for agents)

`sdm` is a command-line client for the Akeneo Supplier Data Manager (SDM) API.
Every command is generated from the API's OpenAPI spec, so a command can be
worked out from an API path.

## Naming rule

- The fixed parts of a path are nested commands.
- The HTTP method is the verb: `get`, `post`, `put`, `patch`, `delete`.
- Path parameters are flags, in kebab-case (`action_id` is `--action-id`).
- Query parameters are flags too (`page_size` is `--page-size`).

| API call | Command |
|---|---|
| `GET /api/v1/projects/` | `sdm projects get` |
| `GET /api/v1/projects/{id}/` | `sdm projects get --id 3` |
| `PATCH /api/v1/projects/{project_pk}/steps/{id}/` | `sdm projects steps patch --project-pk 1 --id 2` |
| `POST /api/v1/steps/{action_id}/hooks/` | `sdm steps hooks post --action-id 5` |
| `GET /api/v1/extraction/{action_id}/{job_id}/` | `sdm extraction get --action-id 5 --job-id 9` |

One command can serve several paths; the path is chosen by which path flags
are passed. Path segments keep the API's spelling (`options_collections`,
`flags-admin`).

## Finding a command

```sh
sdm describe                  # one line per command, all of them
sdm describe projects         # one line per command in a group
sdm describe projects patch   # JSON: variants, parameters, body and response schemas
sdm describe projects get --examples   # also include the spec's example response
sdm projects patch --help     # the same, for humans
```

`describe` inlines schema references three levels deep; raise it with
`--depth N` when a schema still shows `{"$ref": ...}`. Where
`request_body.documented` is `false`, the API publishes no body schema: read an
existing object with the matching `get` and mirror its fields.

## Authentication

```sh
sdm login            # prompts; or set SDM_EMAIL and SDM_PASSWORD
sdm whoami
sdm logout
```

The key is stored in `~/.config/sdm/credentials.json` (mode 0600). `SDM_TOKEN`
or `--token` overrides it. The host defaults to `https://sdm.akeneo.cloud`;
change it with `--base-url` or `SDM_BASE_URL`. The stored key is only sent to
the host it was issued by. Extension job endpoints use a separate job token:
pass `--token <job token> --auth-scheme Bearer`.

## Sending a body

```sh
sdm projects patch --id 3 --data '{"name": "New name"}'      # inline JSON
sdm projects patch --id 3 --data @project.json               # from a file
sdm projects get --id 3 | jq '.name = "x" | {name}' | sdm projects patch --id 3 --data -
sdm projects patch --id 3 --field name="New name"            # one string key
sdm projects patch --id 3 --field 'available_locales:=["en-US"]'   # one JSON key
sdm files post --field data=@catalog.xlsx --field skiprows:=1      # upload (multipart)
```

The body is sent as given. Nothing is checked against the spec, and unknown
fields are not removed.

## Output and exit codes

- stdout is the API's JSON, unchanged (compact when piped). Filter with `jq`.
- Errors are JSON on stderr: the API's own body, or
  `{"status_code", "message", "errors"}` for failures inside the CLI.
- Exit codes: `0` ok, `2` usage, `3` authentication, `4` API error, `5` network,
  `6` outside the directory's scope.
- Lists are paginated: `--page`, `--page-size`, or `--all` to get every result
  as one JSON array.
- Binary downloads need `--output <file>`.

## Safety

- `--dry-run` on any command prints the method, URL and body without sending.
  Use it before a write you are unsure of.
- `delete` commands refuse to run without a terminal unless `--yes` is passed.
- Other writes run immediately. Some `post` commands are destructive
  (`jobs bulk-delete post`, `bases delete-rows post`, `mapping memory bulk-delete post`,
  `hooks rotate-secret post`) and do not ask.
- `sdm whoami` shows which organization the key belongs to.
- A directory can be limited to chosen projects; see Scope below.

## Scope

A directory with a `.sdm-scope.json` file (in it or in a parent) is scoped:
the CLI refuses, with exit code `6` and before sending anything, every request
that is not allowed by the file. Refusals apply to `--dry-run` and `sdm api`
too. No flag or environment variable overrides a scope.

```sh
sdm scope show    # the scope in force and its file, as JSON; null when unscoped
sdm scope         # choose projects and access (a person at a terminal only)
sdm scope clear   # remove the scope file (a person at a terminal only)
```

The file lists projects, and a level for each group inside a project:
`project`, `steps`, `jobs`, `rules`, `hooks`, `options_collections`,
`portals`, `outputs`, `connectors`, `metrics`. Levels are `none`, `read`,
`write`, `delete`; each includes the ones before. `sdm scope` sets `write`
everywhere unless told otherwise, so nothing can be deleted. `delete` is
needed for `DELETE` and for the destructive `post`/`patch` commands
(`bulk-delete`, `clear-all`, `delete-rows`, `rotate-secret`,
`product-models delete`).

Inside a scope:

- Lists need their project filter: `sdm jobs get --project 4717`, likewise
  `options_collections`, `outputs`, `portals`, `metrics`;
  `sdm connectors status get --project-id 4717`; `sdm options get --collection <id>`;
  `sdm media get --uploaded-for-job <job id>`. `sdm projects get` without
  `--id` is refused.
- A body that creates something must name a scoped project (`project`, or
  `project_id` for portals).
- Commands addressed by a step, job, hook or other object id cost one or more
  extra `GET`s: the CLI reads the object to find its project first.
- Organization-wide data (`files`, `bases`, `input_formats`, `users`,
  `organization`, `tasks`) is refused unless the file opens it under
  `organization`. Creating a job needs a file, so `files` must be opened at
  `write` to upload one.
- Paths the check cannot tie to a project are refused: other API versions,
  paths the spec does not list, `portals bulk-create`, `connectors akeneo`,
  `metrics xlsx`, `options get --id`, `documents`, `step-completion`,
  `job-media-archive`.
- `login`, `logout`, `whoami`, `describe` and `spec` work as usual.

The error message says what was needed and what the scope allows. Do not edit
or remove the scope file to get past a refusal; ask the user.

## Steps

A project's steps are in the `steps` array of `sdm projects get --id <id>`.
Each entry has `id`, `name`, `type`, `number`, `run_mode` and `next_step`.

```sh
# rename a step, change its run mode, or point it at another next step
sdm projects steps patch --project-pk 4717 --id 16469 --field run_mode=fast
sdm projects steps patch --project-pk 4717 --id 16469 --field next_step_id:=16473

# configure a step: use the command named after its type, with the step id
sdm mapping get --action-id 16469
sdm mapping patch --action-id 16469 --data @mapping.json
sdm extension patch --action-id 16480 --field iframe_url=https://example.com/app

# remove a step (soft delete)
sdm projects steps delete --project-pk 4717 --id 16469 --yes
```

**Adding a step is not possible through this API.** `projects post` and
`projects patch` accept a `steps` array and answer 200, but the server marks
the field read-only and ignores it; the response still shows the old steps.
Always check the returned `steps` rather than the exit code. Verified on
2026-11-12 against both `/api/v1` and `/api/v2`. Steps have to be added in the
SDM web application.

## Hooks

```sh
sdm steps hooks get --action-id 16469         # hooks on a step
sdm steps hooks post --action-id 16469 --data @hook.json
sdm hooks patch --hook-id <id> --data @hook.json
sdm hooks deliveries get --hook-id <id>
sdm hooks rotate-secret post --hook-id <id>   # replaces the secret immediately
```

The API publishes no body schema for hooks, and the body has not been verified
with this CLI. Read an existing hook first and mirror its fields; use
`--dry-run` to check the request.

## Anything the spec does not list

```sh
sdm api GET /projects/4717/            # /api/v1 is added when missing
sdm api POST /some/new/path/ --data @body.json
sdm api GET /api/v2/projects/          # other API versions: give the full path
sdm projects get --query some_filter=value   # query parameters the spec omits
```

## Keeping commands current

```sh
sdm spec info     # which spec is active
sdm spec update   # download the host's current spec and use it from now on
sdm spec reset    # go back to the spec built into the binary
```
