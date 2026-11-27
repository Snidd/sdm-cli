//! Command-line definition (built at runtime from the spec) and dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::credentials::{self, Credentials};
use crate::error::{CliError, Result};
use crate::request::{self, Body, Call, Context};
use crate::spec::{self, API_PREFIX, Node, Operation, Spec, SpecSource, flag_name};
use crate::{describe, output, scope, skill};

const DEFAULT_BASE_URL: &str = "https://sdm.akeneo.cloud";
const GLOBAL_HEADING: &str = "Global options";
const BUILTINS: [&str; 9] = [
    "login", "logout", "whoami", "version", "scope", "spec", "skill", "describe", "api",
];

/// Parses the process arguments and runs the selected command.
///
/// # Errors
/// Returns the [`CliError`] of the command that ran. Malformed command lines
/// are reported by clap, which exits with status 2.
pub fn run() -> Result<()> {
    let spec = Spec::load();
    let tree = spec.tree();
    let matches = build(&spec, &tree).get_matches();
    dispatch(&spec, &tree, &matches)
}

/// Builds the full clap command: built-ins plus one command per API path.
#[must_use]
pub fn build(spec: &Spec, tree: &Node) -> Command {
    Command::new("sdm")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Command-line client for the Akeneo Supplier Data Manager (SDM) API")
        .long_about(
            "Command-line client for the Akeneo Supplier Data Manager (SDM) API.\n\n\
             Commands mirror the API: the fixed parts of a path are nested commands, the HTTP \
             method is the verb, and path parameters are flags.\n\n  \
             GET   /api/v1/projects/                        sdm projects get\n  \
             GET   /api/v1/projects/{id}/                   sdm projects get --id 3\n  \
             PATCH /api/v1/projects/{project_pk}/steps/{id}/  sdm projects steps patch --project-pk 1 --id 2\n\n\
             Output is the API's JSON on stdout; errors are JSON on stderr. Exit codes: 0 ok, \
             2 usage, 3 authentication, 4 API error, 5 network, 6 outside the directory's scope, \
             or no scope file (see `sdm scope --help`).",
        )
        .after_help(
            "Run `sdm describe` for an index of every command, and `sdm describe <command...>` \
             for the parameters and JSON schemas of one command.",
        )
        .subcommand_required(true)
        .arg_required_else_help(true)
        .disable_help_subcommand(true)
        .args(global_args())
        .subcommands(builtin_commands())
        .subcommands(
            api_commands(spec, tree, "sdm")
                .into_iter()
                .filter(|command| !BUILTINS.contains(&command.get_name())),
        )
}

fn global_args() -> Vec<Arg> {
    let global = |arg: Arg| arg.global(true).help_heading(GLOBAL_HEADING);
    vec![
        global(
            Arg::new("base-url")
                .long("base-url")
                .env("SDM_BASE_URL")
                .value_name("URL")
                .help(format!("SDM host [default: {DEFAULT_BASE_URL}]")),
        ),
        global(
            Arg::new("token")
                .long("token")
                .env("SDM_TOKEN")
                .hide_env_values(true)
                .value_name("KEY")
                .help("API key to use instead of the one stored by `sdm login`"),
        ),
        global(
            Arg::new("auth-scheme")
                .long("auth-scheme")
                .value_name("SCHEME")
                .default_value("Token")
                .help("Authorization header prefix (extension job tokens use Bearer)"),
        ),
        global(
            Arg::new("dry-run")
                .long("dry-run")
                .action(ArgAction::SetTrue)
                .help("Print the request that would be sent, without sending it"),
        ),
        global(
            Arg::new("no-scope")
                .long("no-scope")
                .action(ArgAction::SetTrue)
                .help("Send the request although the directory has no scope file"),
        ),
        global(
            Arg::new("yes")
                .long("yes")
                .short('y')
                .action(ArgAction::SetTrue)
                .help("Skip the confirmation that DELETE requests ask for"),
        ),
        global(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_name("FILE")
                .help("Write the response body to FILE (`-` for stdout)"),
        ),
        global(
            Arg::new("query")
                .long("query")
                .short('q')
                .value_name("KEY=VALUE")
                .action(ArgAction::Append)
                .help("Extra query parameter, for ones the spec does not list (repeatable)"),
        ),
    ]
}

fn body_args() -> [Arg; 2] {
    [
        Arg::new("data")
            .long("data")
            .short('d')
            .value_name("JSON|@FILE|-")
            .help("Request body as JSON: inline, @file, or - for stdin. Sent as given"),
        Arg::new("field")
            .long("field")
            .short('f')
            .value_name("KEY=VALUE")
            .action(ArgAction::Append)
            .help(
                "Set a top-level body key: key=text, key:=<json>, or key=@path to upload a \
                 file as multipart (repeatable)",
            ),
    ]
}

fn all_arg() -> Arg {
    Arg::new("all")
        .long("all")
        .action(ArgAction::SetTrue)
        .help("Follow pagination and print every result as one JSON array")
}

fn builtin_commands() -> Vec<Command> {
    vec![
        Command::new("login")
            .about("Create an API key and store it in the credentials file")
            .long_about(
                "Create an API key with POST /api/v1/auth/token/ and store it (mode 0600).\n\n\
                 The email comes from --email or SDM_EMAIL, the password from SDM_PASSWORD or a \
                 prompt. The password is never stored.",
            )
            .arg(
                Arg::new("email")
                    .long("email")
                    .env("SDM_EMAIL")
                    .value_name("EMAIL"),
            )
            .arg(
                Arg::new("token-name")
                    .long("token-name")
                    .value_name("NAME")
                    .help("Name of the key in SDM [default: sdm-cli-<hostname>]"),
            ),
        Command::new("logout")
            .about("Delete the stored API key (the key itself stays valid until revoked in SDM)"),
        Command::new("whoami").about("Show the logged-in user (GET /api/v1/users/me/)"),
        Command::new("version").about("Show the version of sdm as JSON"),
        Command::new("scope")
            .about("Limit this directory to chosen projects (interactive)")
            .long_about(
                "Choose which projects, and what inside them, the CLI may touch when it is run \
                 from this directory or below. The answer is saved as .sdm-scope.json in the \
                 current directory; later commands that fall outside it are refused with exit \
                 code 6 before anything is sent.\n\n\
                 Each group of a project (project, steps, jobs, rules, hooks, \
                 options_collections, portals, outputs, connectors, metrics) gets a level: none, \
                 read, write or delete, each including the ones before. The default is write \
                 everywhere, so nothing can be deleted. Organization-wide data (files, bases, \
                 input_formats, users, organization, tasks) is closed unless opened here.\n\n\
                 Every write sent from a scoped directory (and reads, when asked for) is recorded in a history \
                 directory (.sdmhistory next to the scope file unless changed or turned off \
                 here), one JSON line per request, for committing.\n\n\
                 A scope file is required: without one every API command is refused with exit \
                 code 6, unless --no-scope is passed. `whoami` runs either way.\n\n\
                 Changing or clearing the scope needs a terminal. No flag or environment \
                 variable overrides a scope; --no-scope does nothing where a scope file exists.",
            )
            .disable_help_subcommand(true)
            .subcommand(Command::new("show").about("Print the scope in force and its file as JSON"))
            .subcommand(Command::new("clear").about("Remove the scope file in force (asks first)")),
        Command::new("spec")
            .about("Manage the API spec that commands are generated from")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .disable_help_subcommand(true)
            .subcommand(Command::new("info").about("Show which spec is active"))
            .subcommand(
                Command::new("update")
                    .about("Download the current spec from the host and use it from now on"),
            )
            .subcommand(Command::new("reset").about("Go back to the spec built into the binary")),
        Command::new("skill")
            .about("Install the Claude Code skill that teaches agents to use sdm")
            .long_about(
                "Install, check or remove the Claude Code skill carried inside this binary: \
                 SKILL.md and the agent guide (AGENTS.md), written as plain files.\n\n\
                 --user installs for every project of this user (~/.claude/skills/sdm-cli, or \
                 under CLAUDE_CONFIG_DIR); --project installs into .claude/skills/sdm-cli in the \
                 current directory, to be committed. Without either, a terminal is asked.\n\n\
                 Run `sdm skill install` again after upgrading sdm to refresh the files.",
            )
            .subcommand_required(true)
            .arg_required_else_help(true)
            .disable_help_subcommand(true)
            .subcommand(
                Command::new("install")
                    .about("Write the skill's files (again, to update them)")
                    .args(skill_location_args())
                    .arg(
                        Arg::new("force")
                            .long("force")
                            .action(ArgAction::SetTrue)
                            .help("Replace a symbolic link found in the way with a copy"),
                    ),
            )
            .subcommand(
                Command::new("status")
                    .about("Show where the skill is installed and whether it is current"),
            )
            .subcommand(
                Command::new("uninstall")
                    .about("Remove the skill's files")
                    .args(skill_location_args()),
            ),
        Command::new("describe")
            .about("Describe commands as JSON: parameters, body and response schemas")
            .long_about(
                "With no arguments, or with a group, print one line per command.\n\
                 With a full command (e.g. `sdm describe projects patch`), print JSON with \
                 every path variant, its parameters, request body schema and response schema.",
            )
            .arg(Arg::new("command").num_args(0..).value_name("COMMAND"))
            .arg(
                Arg::new("depth")
                    .long("depth")
                    .value_name("N")
                    .default_value("3")
                    .value_parser(clap::value_parser!(usize))
                    .help("How many levels of schema references to inline"),
            )
            .arg(
                Arg::new("examples")
                    .long("examples")
                    .action(ArgAction::SetTrue)
                    .help("Include the spec's example response"),
            ),
        Command::new("api")
            .about("Call any path directly, including ones the spec does not list")
            .arg(
                Arg::new("method")
                    .required(true)
                    .value_name("METHOD")
                    .help("HTTP method, e.g. GET"),
            )
            .arg(
                Arg::new("path").required(true).value_name("PATH").help(
                    "Path such as /projects/4704/ (the /api/v1 prefix is added when missing)",
                ),
            )
            .args(body_args())
            .arg(all_arg()),
    ]
}

/// Clap id of an API parameter; namespaced so it cannot clash with the CLI's
/// own arguments.
fn skill_location_args() -> [Arg; 2] {
    [
        Arg::new("user")
            .long("user")
            .action(ArgAction::SetTrue)
            .conflicts_with("project")
            .help("The user's Claude Code folder: every project"),
        Arg::new("project")
            .long("project")
            .action(ArgAction::SetTrue)
            .help("The current directory's .claude folder: this project"),
    ]
}

fn skill_location(matches: &ArgMatches) -> Option<skill::Location> {
    if matches.get_flag("user") {
        Some(skill::Location::User)
    } else if matches.get_flag("project") {
        Some(skill::Location::Project)
    } else {
        None
    }
}

fn param_id(name: &str) -> String {
    format!("param:{name}")
}

/// One clap command per child of `node`. A name that is both a verb and a
/// group (`.../product-models/delete/`) becomes a single command that takes
/// the verb's flags and the group's subcommands.
fn api_commands(spec: &Spec, node: &Node, prefix: &str) -> Vec<Command> {
    let names: BTreeSet<&String> = node.groups.keys().chain(node.verbs.keys()).collect();
    names
        .into_iter()
        .map(|name| {
            let full = format!("{prefix} {name}");
            let mut command = Command::new(name.clone()).disable_help_subcommand(true);
            if let Some(variants) = node.verbs.get(name) {
                command = verb_command(spec, command, &full, variants);
            }
            if let Some(group) = node.groups.get(name) {
                command = command.subcommands(api_commands(spec, group, &full));
                if !node.verbs.contains_key(name) {
                    command = command
                        .about(group_about(group))
                        .subcommand_required(true)
                        .arg_required_else_help(true);
                }
            }
            command
        })
        .collect()
}

fn group_about(group: &Node) -> String {
    let verbs: Vec<&str> = group.verbs.keys().map(String::as_str).collect();
    let groups: Vec<&str> = group.groups.keys().map(String::as_str).collect();
    [verbs, groups].concat().join(", ")
}

fn verb_command(spec: &Spec, mut command: Command, full: &str, variants: &[Operation]) -> Command {
    let mut summaries: Vec<&str> = Vec::new();
    for op in variants {
        if !op.summary.is_empty() && !summaries.contains(&op.summary.as_str()) {
            summaries.push(&op.summary);
        }
    }
    command = command.about(summaries.join(" / "));

    let mut after = String::from("Variants:\n");
    for op in variants {
        after.push_str(&format!(
            "  {}\n      {} {}  {}\n",
            spec::usage_line(full, op),
            op.method.to_uppercase(),
            op.path,
            op.summary
        ));
        if let Some(notes) = spec
            .overlay_for(op)
            .and_then(|overlay| overlay.get("notes"))
            .and_then(Value::as_str)
        {
            after.push_str(&format!("      Note: {notes}\n"));
        }
    }
    let described = full.strip_prefix("sdm ").unwrap_or(full);
    after.push_str(&format!(
        "\nRun `sdm describe {described}` for body and response schemas."
    ));
    command = command.after_help(after);

    // name -> (is path param somewhere, is query param somewhere, array, help)
    let mut params: BTreeMap<&str, (bool, bool, bool, &str)> = BTreeMap::new();
    for op in variants {
        for param in &op.path_params {
            let entry = params.entry(&param.name).or_default();
            entry.0 = true;
            if entry.3.is_empty() {
                entry.3 = &param.description;
            }
        }
        for param in &op.query_params {
            let entry = params.entry(&param.name).or_default();
            entry.1 = true;
            entry.2 |= param.is_array;
            if entry.3.is_empty() {
                entry.3 = &param.description;
            }
        }
    }
    for (name, (in_path, in_query, is_array, description)) in params {
        let kind = match (in_path, in_query) {
            (true, true) => "path/query",
            (true, false) => "path",
            _ => "query",
        };
        let flag = flag_name(name);
        let mut arg = Arg::new(param_id(name))
            .long(flag.clone())
            .value_name(name.to_uppercase())
            .help(format!("[{kind}] {description}"))
            .help_heading(if in_path {
                "Path parameters"
            } else {
                "Query parameters"
            });
        if flag != name {
            // Accept the API's own spelling too (`--page_size`).
            arg = arg.alias(name.to_owned());
        }
        if is_array && !in_path {
            arg = arg.action(ArgAction::Append);
        }
        command = command.arg(arg);
    }

    if variants.iter().any(Operation::takes_body) {
        command = command.args(body_args());
    }
    if variants.iter().any(|op| op.method == "get") {
        command = command.arg(all_arg());
    }
    command
}

fn dispatch(spec: &Spec, tree: &Node, matches: &ArgMatches) -> Result<()> {
    let Some((name, sub)) = matches.subcommand() else {
        return Err(CliError::usage("no command given; run `sdm --help`"));
    };
    let result = match name {
        "login" => login(sub),
        "logout" => logout(),
        "whoami" => identity_context(sub)?.execute(&Call {
            method: "GET".to_owned(),
            path: format!("{API_PREFIX}/users/me/"),
            query: Vec::new(),
            body: Body::None,
            all: false,
        }),
        "version" => {
            output::print_json(&json!({
                "name": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
            }));
            Ok(())
        }
        "scope" => match sub.subcommand() {
            Some(("show", _)) => scope::show(),
            Some(("clear", _)) => scope::clear(),
            _ => scope::configure(&unscoped_context(sub)?),
        },
        "spec" => spec_command(spec, sub),
        "skill" => {
            return match sub.subcommand() {
                Some(("install", sub)) => {
                    skill::install(skill_location(sub), sub.get_flag("force"))
                }
                Some(("uninstall", sub)) => skill::uninstall(skill_location(sub)),
                _ => skill::status(),
            };
        }
        "describe" => {
            let tokens: Vec<String> = sub
                .get_many::<String>("command")
                .map(|values| values.cloned().collect())
                .unwrap_or_default();
            let depth = sub.get_one::<usize>("depth").copied().unwrap_or(3);
            describe::run(spec, tree, &tokens, depth, sub.get_flag("examples"))
        }
        "api" => api(sub),
        _ => dispatch_api(tree, matches, "sdm"),
    };
    if result.is_ok() {
        skill::hint();
    }
    result
}

fn dispatch_api(node: &Node, matches: &ArgMatches, prefix: &str) -> Result<()> {
    let Some((name, sub)) = matches.subcommand() else {
        return Err(CliError::usage(format!(
            "`{prefix}` needs a subcommand; run `{prefix} --help`"
        )));
    };
    let full = format!("{prefix} {name}");
    if let Some(group) = node.groups.get(name)
        && sub.subcommand().is_some()
    {
        return dispatch_api(group, sub, &full);
    }
    match node.verbs.get(name) {
        Some(variants) => run_verb(&full, variants, sub),
        None => Err(CliError::usage(format!(
            "`{full}` needs a subcommand; run `{full} --help`"
        ))),
    }
}

fn run_verb(command: &str, variants: &[Operation], matches: &ArgMatches) -> Result<()> {
    let names: BTreeSet<&str> = variants
        .iter()
        .flat_map(|op| op.path_params.iter().chain(&op.query_params))
        .map(|param| param.name.as_str())
        .collect();
    let supplied: Vec<(&str, Vec<String>)> = names
        .into_iter()
        .filter_map(|name| {
            let values = matches.try_get_many::<String>(&param_id(name)).ok()??;
            Some((name, values.cloned().collect()))
        })
        .collect();
    let supplied_names: Vec<&str> = supplied.iter().map(|(name, _)| *name).collect();
    let op = spec::resolve_variant(command, variants, &supplied_names)?;

    let mut path = op.path.clone();
    let mut query = Vec::new();
    for (name, values) in &supplied {
        if op.has_path_param(name) {
            let value = values.first().map(String::as_str).unwrap_or_default();
            path = path.replace(&format!("{{{name}}}"), &request::encode_segment(value));
        } else {
            query.extend(
                values
                    .iter()
                    .map(|value| ((*name).to_owned(), value.clone())),
            );
        }
    }

    let body = if op.takes_body() {
        body_from(matches)?
    } else {
        Body::None
    };
    context(matches)?.execute(&Call {
        method: op.method.to_uppercase(),
        path,
        query,
        body,
        all: op.method == "get" && flag(matches, "all"),
    })
}

fn api(matches: &ArgMatches) -> Result<()> {
    let method = required(matches, "method")?.to_uppercase();
    let path = required(matches, "path")?;
    if path.contains("://") {
        return Err(CliError::usage(
            "give a path, not a URL; the host comes from --base-url / SDM_BASE_URL",
        ));
    }
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let path = if path.starts_with("/api/") {
        path
    } else {
        format!("{API_PREFIX}{path}")
    };
    let all = method == "GET" && flag(matches, "all");
    context(matches)?.execute(&Call {
        method,
        path,
        query: Vec::new(),
        body: body_from(matches)?,
        all,
    })
}

fn body_from(matches: &ArgMatches) -> Result<Body> {
    let data = matches.try_get_one::<String>("data").ok().flatten();
    let fields: Vec<String> = matches
        .try_get_many::<String>("field")
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    request::build_body(data.map(String::as_str), &fields)
}

fn flag(matches: &ArgMatches, id: &str) -> bool {
    matches
        .try_get_one::<bool>(id)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

fn text(matches: &ArgMatches, id: &str) -> Option<String> {
    matches
        .try_get_one::<String>(id)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
        .cloned()
}

fn required<'a>(matches: &'a ArgMatches, id: &str) -> Result<&'a str> {
    matches
        .get_one::<String>(id)
        .map(String::as_str)
        .ok_or_else(|| CliError::usage(format!("missing <{id}>")))
}

/// The context every command runs in: host, key and the directory's scope.
fn context(matches: &ArgMatches) -> Result<Context> {
    let scope = scope::load()?;
    if scope.is_none() && !flag(matches, "no-scope") {
        return Err(CliError::Scope(format!(
            "this directory has no scope file ({}); run `sdm scope` in a terminal to choose \
             what may be touched, or pass --no-scope to run without one",
            scope::FILE_NAME
        )));
    }
    Ok(Context {
        scope,
        ..unscoped_context(matches)?
    })
}

/// Context for the calls that say nothing about a project (`login`, `whoami`,
/// the spec download): a scope applies when there is one, and none is required.
fn identity_context(matches: &ArgMatches) -> Result<Context> {
    Ok(Context {
        scope: scope::load()?,
        ..unscoped_context(matches)?
    })
}

/// Resolves host and key. Order: flag, environment, credentials file, default.
/// The stored key is only used for the host it was issued by.
fn unscoped_context(matches: &ArgMatches) -> Result<Context> {
    let stored = credentials::load();
    let base_url = text(matches, "base-url")
        .or_else(|| stored.as_ref().map(|c| c.base_url.clone()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let base_url = base_url.trim_end_matches('/').to_owned();
    let token = text(matches, "token").or_else(|| {
        stored
            .filter(|c| c.base_url.trim_end_matches('/') == base_url)
            .map(|c| c.key)
    });
    let extra_query = matches
        .try_get_many::<String>("query")
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .map(|pair| request::parse_pair(pair))
        .collect::<Result<_>>()?;
    Ok(Context {
        base_url,
        token,
        auth_scheme: text(matches, "auth-scheme").unwrap_or_else(|| "Token".to_owned()),
        dry_run: flag(matches, "dry-run"),
        yes: flag(matches, "yes"),
        output: text(matches, "output"),
        extra_query,
        scope: None,
    })
}

fn login(matches: &ArgMatches) -> Result<()> {
    let context = Context {
        token: None,
        extra_query: Vec::new(),
        ..identity_context(matches)?
    };
    let interactive = std::io::stdin().is_terminal();
    let email = match text(matches, "email") {
        Some(email) => email,
        None if interactive => prompt_line("Email: ")?,
        None => return Err(CliError::usage("no email: pass --email or set SDM_EMAIL")),
    };
    let password = match std::env::var("SDM_PASSWORD").ok().filter(|p| !p.is_empty()) {
        Some(password) => password,
        None if interactive => rpassword::prompt_password("Password: ")
            .map_err(|err| CliError::usage(format!("cannot read password: {err}")))?,
        None => return Err(CliError::usage("no password: set SDM_PASSWORD")),
    };
    let token_name =
        text(matches, "token-name").unwrap_or_else(|| format!("sdm-cli-{}", hostname()));

    let mut call = Call {
        method: "POST".to_owned(),
        path: format!("{API_PREFIX}/auth/token/"),
        query: Vec::new(),
        body: Body::Json(
            json!({ "email": email, "password": "********", "token_name": token_name }),
        ),
        all: false,
    };
    if context.dry_run {
        return context.execute(&call);
    }
    call.body =
        Body::Json(json!({ "email": email, "password": password, "token_name": token_name }));

    let (body, _) = context.request(&call).map_err(|err| match err {
        CliError::Auth { status, body, .. } => CliError::Auth {
            message: "login failed".to_owned(),
            status,
            body,
        },
        other => other,
    })?;
    let key = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.get("key")?.as_str().map(str::to_owned))
        .ok_or_else(|| CliError::Network("the login answer did not contain a key".to_owned()))?;
    let file = credentials::save(&Credentials {
        base_url: context.base_url.clone(),
        email: email.clone(),
        token_name: token_name.clone(),
        key,
    })?;
    output::print_json(&json!({
        "logged_in": true,
        "email": email,
        "token_name": token_name,
        "base_url": context.base_url,
        "credentials_file": file.display().to_string(),
    }));
    Ok(())
}

fn logout() -> Result<()> {
    let removed = credentials::delete()?;
    output::print_json(&json!({
        "logged_out": removed,
        "credentials_file": credentials::path()?.display().to_string(),
        "note": "the key itself stays valid until it is revoked in SDM",
    }));
    Ok(())
}

fn spec_command(spec: &Spec, matches: &ArgMatches) -> Result<()> {
    let cache = spec::cache_path()
        .ok_or_else(|| CliError::usage("cannot locate a cache directory for the spec"))?;
    match matches.subcommand() {
        Some(("update", sub)) => {
            let context = Context {
                token: None,
                extra_query: Vec::new(),
                ..identity_context(sub)?
            };
            let call = Call {
                method: "GET".to_owned(),
                path: format!("{API_PREFIX}/schema/"),
                query: vec![("format".to_owned(), "json".to_owned())],
                body: Body::None,
                all: false,
            };
            if context.dry_run {
                return context.execute(&call);
            }
            let (body, _) = context.request(&call)?;
            let text = String::from_utf8(body)
                .map_err(|_| CliError::Network("the downloaded spec is not UTF-8".to_owned()))?;
            let fresh = Spec::parse(&text, SpecSource::Cache(cache.clone()))?;
            let io_err = |err: std::io::Error| {
                CliError::usage(format!("cannot write {}: {err}", cache.display()))
            };
            if let Some(dir) = cache.parent() {
                std::fs::create_dir_all(dir).map_err(io_err)?;
            }
            std::fs::write(&cache, text).map_err(io_err)?;
            output::print_json(&spec_info(&fresh));
            Ok(())
        }
        Some(("reset", _)) => {
            match std::fs::remove_file(&cache) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CliError::usage(format!(
                        "cannot remove {}: {err}",
                        cache.display()
                    )));
                }
            }
            output::print_json(&spec_info(&Spec::embedded()));
            Ok(())
        }
        _ => {
            output::print_json(&spec_info(spec));
            Ok(())
        }
    }
}

fn spec_info(spec: &Spec) -> Value {
    let operations = spec.operations();
    let paths: BTreeSet<&str> = operations.iter().map(|op| op.path.as_str()).collect();
    let (source, file) = match &spec.source {
        SpecSource::Embedded => ("embedded", Value::Null),
        SpecSource::Cache(path) => ("cache", json!(path.display().to_string())),
    };
    json!({
        "source": source,
        "file": file,
        "openapi": spec.root.get("openapi"),
        "version": spec.root.pointer("/info/version"),
        "paths": paths.len(),
        "operations": operations.len(),
    })
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| CliError::usage(format!("cannot read input: {err}")))?;
    Ok(line.trim().to_owned())
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "cli".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_command_line_is_valid() {
        let spec = Spec::embedded();
        let tree = spec.tree();

        // Panics on duplicate flags, clashing ids and similar definition bugs.
        build(&spec, &tree).debug_assert();
    }

    #[test]
    fn api_flags_use_kebab_case_and_accept_the_api_spelling() {
        let spec = Spec::embedded();
        let tree = spec.tree();

        for flag in ["--page-size", "--page_size"] {
            let matches = build(&spec, &tree)
                .try_get_matches_from(["sdm", "projects", "get", flag, "5"])
                .unwrap();
            let (_, projects) = matches.subcommand().unwrap();
            let (_, get) = projects.subcommand().unwrap();
            assert_eq!(get.get_one::<String>("param:page_size").unwrap(), "5");
        }
    }
}
