---
name: sdm-cli
description: Use the installed `sdm` CLI to work with the Akeneo Supplier Data Manager (SDM) API - projects, steps, hooks, extensions, jobs, files, mappings and every other SDM endpoint. Use when the user mentions Akeneo SDM, Supplier Data Manager, sdm.akeneo.cloud, SDM projects, steps, hooks or extension steps, or asks to read or change anything in SDM.
---

# Akeneo SDM CLI

`sdm` is on PATH. Its commands are generated from the SDM API spec: path segments are nested commands, the HTTP method is the verb, and path parameters are flags (`GET /api/v1/projects/{id}/` is `sdm projects get --id 3`).

Read [AGENTS.md](AGENTS.md) in this folder before running commands. It covers the naming rule, finding commands with `sdm describe`, login, request bodies, output and exit codes, safety flags (`--dry-run`, `--yes`), and what is known about steps and hooks.

Quick start:

```sh
sdm whoami                     # which account and organization the key belongs to
sdm describe                   # one line per command
sdm describe projects patch    # parameters and JSON schemas for one command
```
