//! Per-directory scope: which projects, and what inside them, the CLI may
//! touch when run from a directory.
//!
//! `sdm scope` writes [`FILE_NAME`] into the current directory. Every later
//! run from that directory or below is checked against it before anything is
//! sent ([`Scope::check`]). The check works on the final URL, so generated
//! commands and `sdm api` are treated alike. Whatever cannot be tied to a
//! scoped project is refused.

use std::collections::BTreeMap;
use std::fmt;
use std::io::IsTerminal;
use std::path::PathBuf;

use inquire::{Confirm, InquireError, MultiSelect, Select};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{CliError, Result};
use crate::output;
use crate::request::{self, Body, Call, Context};
use crate::spec::{API_PREFIX, flag_name};

/// Name of the scope file.
pub const FILE_NAME: &str = ".sdm-scope.json";
const VERSION: u32 = 1;

/// What may be done to a group. Each level includes the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    None,
    Read,
    Write,
    Delete,
}

impl Level {
    const ALL: [Self; 4] = [Self::None, Self::Read, Self::Write, Self::Delete];

    fn label(self) -> &'static str {
        match self {
            Self::None => "no access",
            Self::Read => "read",
            Self::Write => "read and write, no delete",
            Self::Delete => "read, write and delete",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        })
    }
}

/// The things inside a project that can be allowed separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Project,
    Steps,
    Jobs,
    Rules,
    Hooks,
    OptionsCollections,
    Portals,
    Outputs,
    Connectors,
    Metrics,
}

impl Group {
    const ALL: [Self; 10] = [
        Self::Project,
        Self::Steps,
        Self::Jobs,
        Self::Rules,
        Self::Hooks,
        Self::OptionsCollections,
        Self::Portals,
        Self::Outputs,
        Self::Connectors,
        Self::Metrics,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Project => "project (the project itself and its fields)",
            Self::Steps => "steps (step settings and the job data of each step)",
            Self::Jobs => "jobs (and their media)",
            Self::Rules => "rules",
            Self::Hooks => "hooks",
            Self::OptionsCollections => "options_collections (and their options)",
            Self::Portals => "portals",
            Self::Outputs => "outputs",
            Self::Connectors => "connectors",
            Self::Metrics => "metrics",
        }
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.serialize(f)
    }
}

/// Organization-wide endpoints, which no project owns. Closed unless opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgGroup {
    Files,
    Bases,
    InputFormats,
    Users,
    Organization,
    Tasks,
}

impl OrgGroup {
    const ALL: [Self; 6] = [
        Self::Files,
        Self::Bases,
        Self::InputFormats,
        Self::Users,
        Self::Organization,
        Self::Tasks,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Files => "files (uploads; needed to create jobs)",
            Self::Bases => "bases (reference bases)",
            Self::InputFormats => "input_formats",
            Self::Users => "users (and invitations)",
            Self::Organization => "organization (settings, management access, feature flags)",
            Self::Tasks => "tasks (task and operation status)",
        }
    }
}

impl fmt::Display for OrgGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.serialize(f)
    }
}

/// One project the directory may touch. A group that is not listed is closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectScope {
    pub id: u64,
    /// For the reader only; never used by the check.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub access: BTreeMap<Group, Level>,
}

/// Contents of the scope file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeFile {
    pub version: u32,
    /// Host the project ids belong to.
    pub base_url: String,
    pub projects: Vec<ProjectScope>,
    #[serde(default)]
    pub organization: BTreeMap<OrgGroup, Level>,
}

/// The scope in force, and the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub file: PathBuf,
    pub data: ScopeFile,
}

/// Nearest scope file, looking in the current directory and then its parents.
///
/// # Errors
/// Returns a scope error when the current directory cannot be determined.
pub fn find() -> Result<Option<PathBuf>> {
    let dir = current_dir()?;
    Ok(dir
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        // A file that cannot be examined counts as present, so reading it fails.
        .find(|file| file.try_exists().unwrap_or(true)))
}

/// Loads the scope in force; `None` when no scope file is found.
///
/// # Errors
/// Returns a scope error when a scope file exists but cannot be used, so that
/// a damaged file never leaves the directory unscoped.
pub fn load() -> Result<Option<Scope>> {
    let Some(file) = find()? else {
        return Ok(None);
    };
    let unusable = |reason: String| {
        CliError::Scope(format!(
            "cannot use scope file {}: {reason}; fix it or run `sdm scope` in a terminal",
            file.display()
        ))
    };
    let text = std::fs::read_to_string(&file).map_err(|err| unusable(err.to_string()))?;
    let data = parse(&text).map_err(unusable)?;
    Ok(Some(Scope { file, data }))
}

fn parse(text: &str) -> std::result::Result<ScopeFile, String> {
    let data: ScopeFile = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if data.version != VERSION {
        return Err(format!("version {} is not supported", data.version));
    }
    Ok(data)
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|err| {
        CliError::Scope(format!(
            "cannot determine the current directory to look for {FILE_NAME}: {err}"
        ))
    })
}

/// Step types; each is a path group addressed by step id (`action_id`).
const STEP_TYPES: [&str; 9] = [
    "extraction",
    "mapping",
    "valuemapping",
    "normalisation",
    "categorisation",
    "composition",
    "matchhistory",
    "extension",
    "variant",
];

const UNCOVERED: &str = "this path is not covered by any scope group";
const NO_PROJECT: &str = "this path cannot be tied to a project";

/// How a request is judged.
#[derive(Debug, PartialEq, Eq)]
enum Rule {
    /// Needed by the CLI itself (login, whoami, spec); always allowed.
    Open,
    Refused(&'static str),
    Org(OrgGroup),
    /// Belongs to the project found by following `Via` from the `Source` value.
    Project(Group, Via, Source),
}

/// What the value identifies.
#[derive(Debug, PartialEq, Eq)]
enum Via {
    Project,
    Step,
    Hook,
    Job,
    /// An object of this path group whose `project` field names the project.
    Object(&'static str),
}

/// Where the identifying value is found in the request.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    Path(String),
    Query(&'static str),
    Body(&'static str),
    BodyList(&'static str),
}

fn is_read(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
}

/// Level a request needs. Some writes delete without being a DELETE.
fn required_level(method: &str, segments: &[&str]) -> Level {
    if is_read(method) {
        return Level::Read;
    }
    let destructive = segments.iter().any(|segment| {
        matches!(
            *segment,
            "bulk-delete" | "clear-all" | "delete-rows" | "rotate-secret"
        )
    }) || segments.last() == Some(&"delete");
    if method == "DELETE" || destructive {
        Level::Delete
    } else {
        Level::Write
    }
}

/// The rule for a path, given as its segments below `/api/v1/`.
fn classify(method: &str, segments: &[&str]) -> Rule {
    use Rule::{Open, Org, Refused};
    let read = is_read(method);
    let path = |value: &str| Source::Path(value.to_owned());
    match segments {
        ["check" | "live" | "schema"] | ["users", "me"] if read => Open,
        ["auth", "token"] => Open,

        ["projects"] if read => Refused(
            "listing projects reaches outside the scope; read one with `sdm projects get --id <id>`",
        ),
        ["projects"] => Refused("creating a project is outside any scope"),
        ["projects", id] | ["projects", id, "contributor-access" | "fields"] => {
            Rule::Project(Group::Project, Via::Project, path(id))
        }
        ["projects", id, "steps", _] => Rule::Project(Group::Steps, Via::Project, path(id)),
        ["projects", id, "rules"] | ["projects", id, "rules", _] => {
            Rule::Project(Group::Rules, Via::Project, path(id))
        }

        [kind, step, ..] if STEP_TYPES.contains(kind) => {
            Rule::Project(Group::Steps, Via::Step, path(step))
        }
        ["steps", step, "hooks"] => Rule::Project(Group::Hooks, Via::Step, path(step)),
        ["hooks", hook, ..] => Rule::Project(Group::Hooks, Via::Hook, path(hook)),

        ["jobs"] if read => Rule::Project(Group::Jobs, Via::Project, Source::Query("project")),
        ["jobs"] => Rule::Project(Group::Jobs, Via::Project, Source::Body("project")),
        ["jobs", "bulk-delete"] => {
            Rule::Project(Group::Jobs, Via::Job, Source::BodyList("job_ids"))
        }
        ["jobs", job, ..] => Rule::Project(Group::Jobs, Via::Job, path(job)),
        ["media"] if read => {
            Rule::Project(Group::Jobs, Via::Job, Source::Query("uploaded_for_job"))
        }
        ["media"] => Rule::Project(Group::Jobs, Via::Job, Source::Body("uploaded_for_job")),

        ["options_collections"] if read => Rule::Project(
            Group::OptionsCollections,
            Via::Project,
            Source::Query("project"),
        ),
        ["options_collections"] => Rule::Project(
            Group::OptionsCollections,
            Via::Project,
            Source::Body("project"),
        ),
        ["options_collections", id, ..] => Rule::Project(
            Group::OptionsCollections,
            Via::Object("options_collections"),
            path(id),
        ),
        ["options"] if read => Rule::Project(
            Group::OptionsCollections,
            Via::Object("options_collections"),
            Source::Query("collection"),
        ),
        ["options", ..] => Refused(NO_PROJECT),

        ["outputs"] if read => {
            Rule::Project(Group::Outputs, Via::Project, Source::Query("project"))
        }
        ["outputs", id, ..] => Rule::Project(Group::Outputs, Via::Object("outputs"), path(id)),

        ["portals"] if read => {
            Rule::Project(Group::Portals, Via::Project, Source::Query("project"))
        }
        ["portals"] => Rule::Project(Group::Portals, Via::Project, Source::Body("project_id")),
        ["portals", "bulk-create" | "bulk-template" | "me", ..] => Refused(NO_PROJECT),
        ["portals", id, ..] => Rule::Project(Group::Portals, Via::Object("portals"), path(id)),

        ["connectors"] => Refused(
            "the connector list has no project filter; use `sdm connectors status get --project-id <id>`",
        ),
        ["connectors", "status" | "connection-operation-logs"] if read => {
            Rule::Project(Group::Connectors, Via::Project, Source::Query("project_id"))
        }
        ["connectors", "akeneo", ..] => Refused(NO_PROJECT),
        ["connectors", id, ..] => {
            Rule::Project(Group::Connectors, Via::Object("connectors"), path(id))
        }

        ["metrics"] if read => {
            Rule::Project(Group::Metrics, Via::Project, Source::Query("project"))
        }
        ["metrics", ..] => Refused(NO_PROJECT),

        ["files", ..] => Org(OrgGroup::Files),
        ["bases", ..] => Org(OrgGroup::Bases),
        ["input-formats", ..] => Org(OrgGroup::InputFormats),
        ["users" | "invitations", ..] => Org(OrgGroup::Users),
        ["organization" | "flags-admin", ..] => Org(OrgGroup::Organization),
        ["tasks" | "operations", ..] => Org(OrgGroup::Tasks),

        [
            "documents" | "portal_documents" | "step-completion" | "job-media-archive"
            | "password-resets",
            ..,
        ] => Refused(NO_PROJECT),
        _ => Refused(UNCOVERED),
    }
}

/// Segments of `path` below `/api/v1/`. `None` for any other path, and for
/// segments that could mean something else to the server than to the check.
fn api_segments(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix(API_PREFIX)?.strip_prefix('/')?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let segments: Vec<&str> = rest.split('/').collect();
    segments
        .iter()
        .all(|segment| !segment.is_empty() && !segment.contains('%'))
        .then_some(segments)
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn body_field(body: &Body, name: &str) -> Option<Value> {
    match body {
        Body::None => None,
        Body::Json(value) => value.get(name).cloned(),
        Body::Multipart { fields, .. } => fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| Value::String(value.clone())),
    }
}

/// The values in the request that say what it is about.
fn values(source: &Source, call: &Call, url: &Url) -> std::result::Result<Vec<String>, String> {
    match source {
        Source::Path(value) => Ok(vec![value.clone()]),
        Source::Query(name) => {
            let found: Vec<String> = url
                .query_pairs()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .collect();
            if found.is_empty() {
                Err(format!(
                    "pass --{} so the request stays inside the scope",
                    flag_name(name)
                ))
            } else {
                Ok(found)
            }
        }
        Source::Body(name) => body_field(&call.body, name)
            .as_ref()
            .and_then(scalar)
            .map(|value| vec![value])
            .ok_or_else(|| format!("the body must carry `{name}` so the request can be checked")),
        Source::BodyList(name) => body_field(&call.body, name)
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|items| items.iter().map(scalar).collect::<Option<Vec<_>>>())
            .filter(|items| !items.is_empty())
            .ok_or_else(|| {
                format!("the body must carry a `{name}` list so the request can be checked")
            }),
    }
}

/// The project an object belongs to: `project` as an id, or as an object.
fn project_of(object: &Value) -> Option<String> {
    let project = object.get("project")?;
    scalar(project).or_else(|| scalar(project.get("id")?))
}

/// The step a hook belongs to, under whichever name the API uses.
fn step_of(hook: &Value) -> Option<String> {
    ["step", "step_id", "action", "action_id"]
        .iter()
        .filter_map(|key| hook.get(key))
        .find_map(|step| scalar(step).or_else(|| scalar(step.get("id")?)))
}

fn lookup(context: &Context, path: &str, what: &str) -> std::result::Result<Value, String> {
    context
        .lookup(&format!("{API_PREFIX}{path}"))
        .map_err(|err| format!("cannot check which project {what} belongs to: {err}"))
}

impl Scope {
    /// Refuses `call` unless the scope allows it. `url` is the URL that would
    /// be requested. May read from the API to find the project a step, job or
    /// other object belongs to.
    ///
    /// # Errors
    /// Returns a scope error when the request falls outside the scope or
    /// cannot be shown to fall inside it.
    pub fn check(&self, context: &Context, call: &Call, url: &Url) -> Result<()> {
        self.judge(context, call, url).map_err(|reason| {
            CliError::Scope(format!(
                "outside scope ({}): {} {}: {reason}",
                self.file.display(),
                call.method,
                url.path()
            ))
        })
    }

    fn judge(&self, context: &Context, call: &Call, url: &Url) -> std::result::Result<(), String> {
        let segments = api_segments(url.path())
            .ok_or_else(|| format!("only plain {API_PREFIX} paths can be checked"))?;
        let rule = classify(&call.method, &segments);
        if rule == Rule::Open {
            return Ok(());
        }
        let scoped_host = self.data.base_url.trim_end_matches('/');
        if context.base_url.trim_end_matches('/') != scoped_host {
            return Err(format!("the scope is for {scoped_host}"));
        }

        let need = required_level(&call.method, &segments);
        match rule {
            Rule::Open => Ok(()),
            Rule::Refused(reason) => Err(reason.to_owned()),
            Rule::Org(group) => {
                let have = self.data.organization.get(&group).copied();
                allowed(need, have.unwrap_or(Level::None), &format!("{group}"))
            }
            Rule::Project(group, via, source) => {
                for value in values(&source, call, url)? {
                    let project = self.locate(context, &via, &value)?;
                    let have = project.access.get(&group).copied();
                    allowed(
                        need,
                        have.unwrap_or(Level::None),
                        &format!("{group} of project {}", project.id),
                    )?;
                }
                Ok(())
            }
        }
    }

    /// The scoped project that `value` leads to.
    fn locate(
        &self,
        context: &Context,
        via: &Via,
        value: &str,
    ) -> std::result::Result<&ProjectScope, String> {
        let encoded = request::encode_segment(value);
        match via {
            Via::Project => self.project(value),
            Via::Step => self.project_of_step(context, value),
            Via::Job => {
                let what = format!("job {value}");
                let job = lookup(context, &format!("/jobs/{encoded}/"), &what)?;
                self.owner(&job, &what)
            }
            Via::Object(group) => {
                let what = format!("{group} {value}");
                let object = lookup(context, &format!("/{group}/{encoded}/"), &what)?;
                self.owner(&object, &what)
            }
            Via::Hook => {
                let what = format!("hook {value}");
                let hook = lookup(context, &format!("/hooks/{encoded}/"), &what)?;
                match step_of(&hook) {
                    Some(step) => self.project_of_step(context, &step),
                    None => self.project_listing_hook(context, value),
                }
            }
        }
    }

    fn project(&self, id: &str) -> std::result::Result<&ProjectScope, String> {
        id.parse::<u64>()
            .ok()
            .and_then(|id| self.data.projects.iter().find(|project| project.id == id))
            .ok_or_else(|| {
                let scoped: Vec<String> = self
                    .data
                    .projects
                    .iter()
                    .map(|project| project.id.to_string())
                    .collect();
                format!(
                    "project {id} is not in the scope (scoped: {})",
                    scoped.join(", ")
                )
            })
    }

    fn owner(&self, object: &Value, what: &str) -> std::result::Result<&ProjectScope, String> {
        let id = project_of(object)
            .ok_or_else(|| format!("cannot tell which project {what} belongs to"))?;
        self.project(&id)
            .map_err(|reason| format!("{what} belongs elsewhere: {reason}"))
    }

    /// Ids of a scoped project's steps, read from the API.
    fn steps(
        context: &Context,
        project: &ProjectScope,
    ) -> std::result::Result<Vec<String>, String> {
        let what = format!("project {}", project.id);
        let detail = lookup(context, &format!("/projects/{}/", project.id), &what)?;
        Ok(detail
            .get("steps")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| scalar(step.get("id")?))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn project_of_step(
        &self,
        context: &Context,
        step: &str,
    ) -> std::result::Result<&ProjectScope, String> {
        for project in &self.data.projects {
            if Self::steps(context, project)?.iter().any(|id| id == step) {
                return Ok(project);
            }
        }
        Err(format!("step {step} is not a step of a scoped project"))
    }

    /// For hooks that do not name their step: the scoped project with a step
    /// that lists the hook.
    fn project_listing_hook(
        &self,
        context: &Context,
        hook: &str,
    ) -> std::result::Result<&ProjectScope, String> {
        for project in &self.data.projects {
            for step in Self::steps(context, project)? {
                let what = format!("hook {hook}");
                let listed = lookup(context, &format!("/steps/{step}/hooks/"), &what)?;
                let hooks = listed.get("results").unwrap_or(&listed);
                let found = hooks.as_array().is_some_and(|hooks| {
                    hooks
                        .iter()
                        .filter_map(|item| scalar(item.get("id")?))
                        .any(|id| id == hook)
                });
                if found {
                    return Ok(project);
                }
            }
        }
        Err(format!("hook {hook} is not on a step of a scoped project"))
    }
}

fn allowed(need: Level, have: Level, what: &str) -> std::result::Result<(), String> {
    if have >= need {
        Ok(())
    } else {
        Err(format!(
            "needs '{need}' on {what}; the scope allows '{have}'"
        ))
    }
}

/// `sdm scope show`: prints the scope in force.
///
/// # Errors
/// Returns a scope error when a scope file exists but cannot be used.
pub fn show() -> Result<()> {
    let scope = load()?;
    output::print_json(&match scope {
        Some(scope) => report(&scope.file, &scope.data),
        None => json!({ "scope_file": null, "scope": null }),
    });
    Ok(())
}

fn report(file: &std::path::Path, data: &ScopeFile) -> Value {
    json!({ "scope_file": file.display().to_string(), "scope": data })
}

/// Changing the scope takes a person at a terminal, so that a script or agent
/// working inside a scope cannot widen it through the CLI.
fn require_terminal(command: &str) -> Result<()> {
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "`{command}` changes the scope and only runs in a terminal"
        )))
    }
}

fn prompt_error(err: InquireError) -> CliError {
    match err {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            CliError::usage("cancelled; the scope is unchanged")
        }
        other => CliError::usage(format!("cannot prompt: {other}")),
    }
}

/// `sdm scope clear`: removes the scope file in force, after confirmation.
///
/// # Errors
/// Returns a usage error without a terminal, when declined, or when the file
/// cannot be removed.
pub fn clear() -> Result<()> {
    require_terminal("sdm scope clear")?;
    let Some(file) = find()? else {
        output::print_json(&json!({ "removed": false, "scope_file": null }));
        return Ok(());
    };
    let confirmed = Confirm::new(&format!("Remove {}?", file.display()))
        .with_default(false)
        .prompt()
        .map_err(prompt_error)?;
    if !confirmed {
        return Err(CliError::usage("cancelled; the scope is unchanged"));
    }
    std::fs::remove_file(&file)
        .map_err(|err| CliError::usage(format!("cannot remove {}: {err}", file.display())))?;
    output::print_json(&json!({ "removed": true, "scope_file": file.display().to_string() }));
    Ok(())
}

struct ProjectChoice {
    id: u64,
    name: String,
}

impl fmt::Display for ProjectChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (id {})", self.name, self.id)
    }
}

/// `sdm scope`: asks which projects and groups to allow and writes the scope
/// file into the current directory. `context` must be unscoped.
///
/// # Errors
/// Returns a usage error without a terminal or when cancelled, and the usual
/// errors when the project list cannot be fetched.
pub fn configure(context: &Context) -> Result<()> {
    require_terminal("sdm scope")?;
    let file = current_dir()?.join(FILE_NAME);
    // Earlier answers become the defaults; a damaged file is simply replaced.
    let previous = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| parse(&text).ok());

    let listed = context.list_all(&Call {
        method: "GET".to_owned(),
        path: format!("{API_PREFIX}/projects/"),
        query: vec![("page_size".to_owned(), "100".to_owned())],
        body: Body::None,
        all: true,
    })?;
    let mut choices: Vec<ProjectChoice> = listed
        .iter()
        .filter_map(|project| {
            Some(ProjectChoice {
                id: project.get("id")?.as_u64()?,
                name: project.get("name")?.as_str()?.to_owned(),
            })
        })
        .collect();
    if choices.is_empty() {
        return Err(CliError::usage(format!(
            "no projects are visible on {}; nothing to scope",
            context.base_url
        )));
    }
    choices.sort_by_key(|choice| choice.name.to_lowercase());
    let was_scoped = |id: u64| {
        previous
            .as_ref()
            .is_some_and(|scope| scope.projects.iter().any(|project| project.id == id))
    };
    let defaults: Vec<usize> = (0..choices.len())
        .filter(|index| was_scoped(choices[*index].id))
        .collect();

    let picked = MultiSelect::new("Projects this directory may touch:", choices)
        .with_default(&defaults)
        .with_page_size(15)
        .prompt()
        .map_err(prompt_error)?;
    if picked.is_empty() {
        return Err(CliError::usage(
            "no project selected; the scope is unchanged",
        ));
    }

    let access = ask_access()?;
    let organization = ask_organization(previous.as_ref())?;
    let data = ScopeFile {
        version: VERSION,
        base_url: context.base_url.clone(),
        projects: picked
            .into_iter()
            .map(|choice| ProjectScope {
                id: choice.id,
                name: choice.name,
                access: access.clone(),
            })
            .collect(),
        organization,
    };

    let mut text = serde_json::to_string_pretty(&data).expect("a scope serializes");
    text.push('\n');
    std::fs::write(&file, text)
        .map_err(|err| CliError::usage(format!("cannot write {}: {err}", file.display())))?;
    output::print_json(&report(&file, &data));
    Ok(())
}

fn ask_level(message: &str, levels: &[Level], start: Level) -> Result<Level> {
    let labels: Vec<&str> = levels.iter().map(|level| level.label()).collect();
    let cursor = levels.iter().position(|level| *level == start).unwrap_or(0);
    let answer = Select::new(message, labels)
        .with_starting_cursor(cursor)
        .raw_prompt()
        .map_err(prompt_error)?;
    Ok(levels[answer.index])
}

fn ask_access() -> Result<BTreeMap<Group, Level>> {
    let presets = vec![
        "Everything: read and write, no delete",
        "Everything: read only",
        "Everything: read, write and delete",
        "Choose per group",
    ];
    let preset = Select::new("Access inside those projects:", presets)
        .raw_prompt()
        .map_err(prompt_error)?
        .index;
    let uniform = |level| Group::ALL.iter().map(|group| (*group, level)).collect();
    Ok(match preset {
        0 => uniform(Level::Write),
        1 => uniform(Level::Read),
        2 => uniform(Level::Delete),
        _ => {
            let mut access = BTreeMap::new();
            for group in Group::ALL {
                let level = ask_level(group.label(), &Level::ALL, Level::Write)?;
                access.insert(group, level);
            }
            access
        }
    })
}

fn ask_organization(previous: Option<&ScopeFile>) -> Result<BTreeMap<OrgGroup, Level>> {
    let before = |group: OrgGroup| {
        previous
            .and_then(|scope| scope.organization.get(&group).copied())
            .unwrap_or(Level::None)
    };
    let labels: Vec<&str> = OrgGroup::ALL.iter().map(|group| group.label()).collect();
    let defaults: Vec<usize> = (0..OrgGroup::ALL.len())
        .filter(|index| before(OrgGroup::ALL[*index]) > Level::None)
        .collect();
    let opened: Vec<usize> = MultiSelect::new(
        "Organization-wide data to open (shared by all projects; none by default):",
        labels,
    )
    .with_default(&defaults)
    .raw_prompt()
    .map_err(prompt_error)?
    .into_iter()
    .map(|option| option.index)
    .collect();

    let mut organization = BTreeMap::new();
    for (index, group) in OrgGroup::ALL.into_iter().enumerate() {
        let level = if opened.contains(&index) {
            let start = before(group).max(Level::Read);
            ask_level(group.label(), &Level::ALL[1..], start)?
        } else {
            Level::None
        };
        organization.insert(group, level);
    }
    Ok(organization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Spec;

    /// Segments of a spec path, with every parameter filled in as `1`.
    fn filled(path: &str) -> Vec<String> {
        path.strip_prefix(API_PREFIX)
            .unwrap()
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                if part.starts_with('{') {
                    "1".to_owned()
                } else {
                    part.to_owned()
                }
            })
            .collect()
    }

    fn rule(method: &str, path: &str) -> Rule {
        classify(method, &api_segments(path).unwrap())
    }

    #[test]
    fn every_operation_in_the_spec_has_an_explicit_rule() {
        for op in Spec::embedded().operations() {
            let segments = filled(&op.path);
            let segments: Vec<&str> = segments.iter().map(String::as_str).collect();

            let rule = classify(&op.method.to_uppercase(), &segments);

            assert_ne!(rule, Rule::Refused(UNCOVERED), "{} {}", op.method, op.path);
        }
    }

    #[test]
    fn paths_resolve_to_their_group_and_owner() {
        let project =
            |group, via, value: &str| Rule::Project(group, via, Source::Path(value.to_owned()));

        assert_eq!(
            rule("PATCH", "/api/v1/projects/7/"),
            project(Group::Project, Via::Project, "7")
        );
        assert_eq!(
            rule("DELETE", "/api/v1/projects/7/steps/9/"),
            project(Group::Steps, Via::Project, "7")
        );
        assert_eq!(
            rule("GET", "/api/v1/projects/7/rules/2/"),
            project(Group::Rules, Via::Project, "7")
        );
        assert_eq!(
            rule("PATCH", "/api/v1/mapping/9/memory/clear-all/"),
            project(Group::Steps, Via::Step, "9")
        );
        assert_eq!(
            rule("POST", "/api/v1/steps/9/hooks/"),
            project(Group::Hooks, Via::Step, "9")
        );
        assert_eq!(
            rule("GET", "/api/v1/hooks/abc/deliveries/"),
            project(Group::Hooks, Via::Hook, "abc")
        );
        assert_eq!(
            rule("GET", "/api/v1/jobs/j1/data/"),
            project(Group::Jobs, Via::Job, "j1")
        );
        assert_eq!(
            rule("GET", "/api/v1/jobs/"),
            Rule::Project(Group::Jobs, Via::Project, Source::Query("project"))
        );
        assert_eq!(
            rule("POST", "/api/v1/jobs/bulk-delete/"),
            Rule::Project(Group::Jobs, Via::Job, Source::BodyList("job_ids"))
        );
        assert_eq!(
            rule("POST", "/api/v1/portals/"),
            Rule::Project(Group::Portals, Via::Project, Source::Body("project_id"))
        );
        assert_eq!(
            rule("GET", "/api/v1/files/3/download/"),
            Rule::Org(OrgGroup::Files)
        );
        assert_eq!(rule("GET", "/api/v1/users/me/"), Rule::Open);
        assert_eq!(
            rule("PATCH", "/api/v1/users/me/settings/"),
            Rule::Org(OrgGroup::Users)
        );
        assert!(matches!(rule("GET", "/api/v1/projects/"), Rule::Refused(_)));
        assert!(matches!(
            rule("POST", "/api/v1/projects/"),
            Rule::Refused(_)
        ));
        assert_eq!(rule("GET", "/api/v1/brand/new/"), Rule::Refused(UNCOVERED));
    }

    #[test]
    fn only_plain_v1_paths_can_be_checked() {
        assert_eq!(
            api_segments("/api/v1/projects/3/"),
            Some(vec!["projects", "3"])
        );
        assert_eq!(api_segments("/api/v2/projects/3/"), None);
        assert_eq!(api_segments("/api/v1/projects//3/"), None);
        assert_eq!(api_segments("/api/v1/projects/3%2F..%2F4/"), None);
        assert_eq!(api_segments("/api/v1"), None);
    }

    #[test]
    fn writes_that_delete_need_the_delete_level() {
        let level = |method, path| required_level(method, &api_segments(path).unwrap());

        assert_eq!(level("GET", "/api/v1/jobs/j1/"), Level::Read);
        assert_eq!(level("PATCH", "/api/v1/projects/7/"), Level::Write);
        assert_eq!(level("DELETE", "/api/v1/jobs/j1/"), Level::Delete);
        assert_eq!(level("POST", "/api/v1/jobs/bulk-delete/"), Level::Delete);
        assert_eq!(level("POST", "/api/v1/bases/2/delete-rows/"), Level::Delete);
        assert_eq!(
            level("PATCH", "/api/v1/mapping/9/memory/clear-all/"),
            Level::Delete
        );
        assert_eq!(
            level("POST", "/api/v1/hooks/h/rotate-secret/"),
            Level::Delete
        );
        assert_eq!(
            level("POST", "/api/v1/variant/9/j1/product-models/delete/"),
            Level::Delete
        );
        assert_eq!(
            level("GET", "/api/v1/mapping/9/memory/bulk-delete/"),
            Level::Read
        );
    }

    #[test]
    fn levels_include_the_ones_before() {
        assert!(allowed(Level::Read, Level::Write, "x").is_ok());
        assert!(allowed(Level::Write, Level::Write, "x").is_ok());
        assert!(allowed(Level::Delete, Level::Write, "x").is_err());
        assert!(allowed(Level::Read, Level::None, "x").is_err());
    }

    #[test]
    fn a_scope_file_that_is_not_understood_is_rejected() {
        let good = r#"{"version":1,"base_url":"https://h","projects":[{"id":7,"access":{"jobs":"write"}}]}"#;

        let scope = parse(good).unwrap();

        assert_eq!(scope.projects[0].access[&Group::Jobs], Level::Write);
        assert!(scope.organization.is_empty());
        assert!(parse(&good.replace(r#""version":1"#, r#""version":2"#)).is_err());
        assert!(parse(&good.replace("jobs", "everything")).is_err());
        assert!(parse(&good.replace(r#""write""#, r#""admin""#)).is_err());
        assert!(parse(&good.replace(r#""id":7"#, r#""id":7,"extra":1"#)).is_err());
        assert!(parse("{").is_err());
    }

    #[test]
    fn the_project_is_read_from_query_and_body() {
        let url = Url::parse("https://h/api/v1/jobs/?project=7&project=8&page=2").unwrap();
        let call = |body| Call {
            method: "POST".to_owned(),
            path: String::new(),
            query: Vec::new(),
            body,
            all: false,
        };
        let json_body = call(Body::Json(json!({"project": 7, "job_ids": ["a", "b"]})));
        let multipart = call(Body::Multipart {
            fields: vec![("uploaded_for_job".to_owned(), "j1".to_owned())],
            files: Vec::new(),
        });

        assert_eq!(
            values(&Source::Query("project"), &json_body, &url).unwrap(),
            ["7", "8"]
        );
        assert!(values(&Source::Query("step"), &json_body, &url).is_err());
        assert_eq!(
            values(&Source::Body("project"), &json_body, &url).unwrap(),
            ["7"]
        );
        assert_eq!(
            values(&Source::BodyList("job_ids"), &json_body, &url).unwrap(),
            ["a", "b"]
        );
        assert_eq!(
            values(&Source::Body("uploaded_for_job"), &multipart, &url).unwrap(),
            ["j1"]
        );
        assert!(values(&Source::Body("project"), &call(Body::None), &url).is_err());
        assert!(values(&Source::BodyList("project"), &json_body, &url).is_err());
    }

    #[test]
    fn owners_are_read_from_id_or_nested_object() {
        assert_eq!(project_of(&json!({"project": 7})).as_deref(), Some("7"));
        assert_eq!(
            project_of(&json!({"project": {"id": 7}})).as_deref(),
            Some("7")
        );
        assert_eq!(project_of(&json!({"name": "x"})), None);
        assert_eq!(step_of(&json!({"step": {"id": 9}})).as_deref(), Some("9"));
        assert_eq!(step_of(&json!({"action_id": 9})).as_deref(), Some("9"));
        assert_eq!(step_of(&json!({"url": "x"})), None);
    }
}
