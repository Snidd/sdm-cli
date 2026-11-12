//! OpenAPI spec loading and the command tree derived from its paths.
//!
//! Naming rule: the static segments of a path become nested command groups,
//! the HTTP method is the verb, and path parameters are flags. Several paths
//! can share one command (`/projects/` and `/projects/{id}/` are both
//! `projects get`); the variant is picked from the flags that were passed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;

use crate::error::{CliError, Result};

const EMBEDDED_SPEC: &str = include_str!("../spec/sdm-openapi.json");
const EMBEDDED_OVERLAY: &str = include_str!("../spec/overlay.json");

/// Prefix shared by every documented path; dropped from command names.
pub const API_PREFIX: &str = "/api/v1";
/// HTTP methods that become verbs, in display order.
pub const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// Where the active spec came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecSource {
    Embedded,
    Cache(PathBuf),
}

/// A query or path parameter of one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Name exactly as the API spells it (`page_size`).
    pub name: String,
    pub ty: String,
    pub is_array: bool,
    pub description: String,
}

/// One method on one path.
#[derive(Debug, Clone)]
pub struct Operation {
    /// Lowercase HTTP method.
    pub method: String,
    /// Path as written in the spec, including the API prefix.
    pub path: String,
    /// Static path segments, i.e. the command groups.
    pub segments: Vec<String>,
    pub path_params: Vec<Param>,
    pub query_params: Vec<Param>,
    pub summary: String,
}

impl Operation {
    /// Path without the API prefix, as shown in help.
    #[must_use]
    pub fn short_path(&self) -> &str {
        self.path.strip_prefix(API_PREFIX).unwrap_or(&self.path)
    }

    /// Whether the method can carry a request body.
    #[must_use]
    pub fn takes_body(&self) -> bool {
        matches!(self.method.as_str(), "post" | "put" | "patch")
    }

    /// Key used by the overlay file, e.g. `PATCH /projects/{id}/`.
    #[must_use]
    pub fn overlay_key(&self) -> String {
        format!("{} {}", self.method.to_uppercase(), self.short_path())
    }

    #[must_use]
    pub fn has_path_param(&self, name: &str) -> bool {
        self.path_params.iter().any(|p| p.name == name)
    }

    #[must_use]
    pub fn has_query_param(&self, name: &str) -> bool {
        self.query_params.iter().any(|p| p.name == name)
    }
}

/// A level of the command tree.
#[derive(Debug, Default)]
pub struct Node {
    pub groups: BTreeMap<String, Node>,
    /// Verb (HTTP method) to the path variants it serves.
    pub verbs: BTreeMap<String, Vec<Operation>>,
}

/// The parsed spec plus the hand-maintained overlay.
#[derive(Debug)]
pub struct Spec {
    pub root: Value,
    pub overlay: Value,
    pub source: SpecSource,
}

impl Spec {
    /// Loads the cached spec if `sdm spec update` stored one, otherwise the
    /// snapshot embedded in the binary. A corrupt cache falls back to the
    /// snapshot with a warning.
    #[must_use]
    pub fn load() -> Self {
        if let Some(path) = cache_path()
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match Self::parse(&text, SpecSource::Cache(path.clone())) {
                Ok(spec) => return spec,
                Err(err) => eprintln!(
                    "warning: ignoring cached spec {}: {err}; run `sdm spec reset`",
                    path.display()
                ),
            }
        }
        Self::embedded()
    }

    /// The snapshot compiled into the binary.
    ///
    /// # Panics
    /// Panics if the embedded snapshot is not valid JSON, which is a build bug.
    #[must_use]
    pub fn embedded() -> Self {
        Self::parse(EMBEDDED_SPEC, SpecSource::Embedded).expect("embedded spec is valid")
    }

    /// Parses spec text and checks it has a `paths` object.
    ///
    /// # Errors
    /// Returns a usage error when the text is not an OpenAPI document.
    pub fn parse(text: &str, source: SpecSource) -> Result<Self> {
        let root: Value = serde_json::from_str(text)
            .map_err(|err| CliError::usage(format!("spec is not valid JSON: {err}")))?;
        if !root.get("paths").is_some_and(Value::is_object) {
            return Err(CliError::usage("spec has no `paths` object"));
        }
        let overlay = serde_json::from_str(EMBEDDED_OVERLAY).unwrap_or(Value::Null);
        Ok(Self {
            root,
            overlay,
            source,
        })
    }

    /// Every operation in the spec, in spec order.
    #[must_use]
    pub fn operations(&self) -> Vec<Operation> {
        let Some(paths) = self.root.get("paths").and_then(Value::as_object) else {
            return Vec::new();
        };
        let mut operations = Vec::new();
        for (path, item) in paths {
            for method in METHODS {
                if let Some(op) = item.get(method) {
                    operations.push(build_operation(path, method, item, op));
                }
            }
        }
        operations
    }

    /// Builds the command tree.
    #[must_use]
    pub fn tree(&self) -> Node {
        let mut root = Node::default();
        for op in self.operations() {
            if op.segments.is_empty() {
                continue;
            }
            let mut node = &mut root;
            for segment in &op.segments {
                node = node.groups.entry(segment.clone()).or_default();
            }
            // The verb hangs off the node of the last static segment.
            node.verbs.entry(op.method.clone()).or_default().push(op);
        }
        root
    }

    /// The raw operation object for `op`.
    #[must_use]
    pub fn operation_value(&self, op: &Operation) -> &Value {
        &self.root["paths"][&op.path][&op.method]
    }

    /// Overlay entry (notes, example) for `op`, if any.
    #[must_use]
    pub fn overlay_for(&self, op: &Operation) -> Option<&Value> {
        self.overlay.get(op.overlay_key())
    }

    /// Follows a local `#/...` reference.
    #[must_use]
    pub fn resolve_ref(&self, reference: &str) -> Option<&Value> {
        self.root.pointer(reference.strip_prefix('#')?)
    }
}

/// Location of the spec written by `sdm spec update`.
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("sdm").join("spec.json"))
}

fn build_operation(path: &str, method: &str, item: &Value, op: &Value) -> Operation {
    let short = path.strip_prefix(API_PREFIX).unwrap_or(path);
    let mut segments = Vec::new();
    let mut template_params = Vec::new();
    for part in short.split('/').filter(|part| !part.is_empty()) {
        match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            Some(name) => template_params.push(name.to_owned()),
            None => segments.push(part.to_owned()),
        }
    }

    let declared: Vec<&Value> = [op, item]
        .iter()
        .filter_map(|holder| holder.get("parameters").and_then(Value::as_array))
        .flatten()
        .collect();
    let find = |location: &str, name: &str| {
        declared.iter().copied().find(|p| {
            p.get("in").and_then(Value::as_str) == Some(location)
                && p.get("name").and_then(Value::as_str) == Some(name)
        })
    };

    // The path template is the truth for path params: the spec declares a few
    // that the URL does not contain.
    let path_params = template_params
        .iter()
        .map(|name| match find("path", name) {
            Some(value) => param_from(value),
            None => Param {
                name: name.clone(),
                ty: "string".to_owned(),
                is_array: false,
                description: String::new(),
            },
        })
        .collect();

    let mut seen = BTreeSet::new();
    let query_params = declared
        .iter()
        .filter(|p| p.get("in").and_then(Value::as_str) == Some("query"))
        .map(|p| param_from(p))
        .filter(|p| seen.insert(p.name.clone()))
        .collect();

    Operation {
        method: method.to_owned(),
        path: path.to_owned(),
        segments,
        path_params,
        query_params,
        summary: text_field(op, "summary")
            .or_else(|| text_field(op, "description"))
            .unwrap_or_default(),
    }
}

fn param_from(value: &Value) -> Param {
    let schema = value.get("schema");
    let ty = schema
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("string");
    Param {
        name: text_field(value, "name").unwrap_or_default(),
        ty: ty.to_owned(),
        is_array: ty == "array",
        description: text_field(value, "description").unwrap_or_default(),
    }
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?.trim();
    // Keep help output to one line per item.
    let first_line = text.lines().next()?.trim();
    (!first_line.is_empty()).then(|| first_line.to_owned())
}

/// Picks the variant of a command from the parameter flags that were passed.
///
/// A variant matches when all of its path parameters were supplied and every
/// other supplied flag is one of its query parameters. When several match
/// (`users get --id` is both a filter on the list and the detail path), the
/// one that uses more path parameters wins.
///
/// # Errors
/// Returns a usage error listing the variants when none matches.
pub fn resolve_variant<'a>(
    command: &str,
    variants: &'a [Operation],
    supplied: &[&str],
) -> Result<&'a Operation> {
    variants
        .iter()
        .filter(|op| {
            op.path_params
                .iter()
                .all(|p| supplied.contains(&p.name.as_str()))
                && supplied
                    .iter()
                    .all(|name| op.has_path_param(name) || op.has_query_param(name))
        })
        .max_by_key(|op| op.path_params.len())
        .ok_or_else(|| {
            let mut message = format!("no variant of `{command}` matches the flags given; use:");
            for op in variants {
                message.push_str("\n  ");
                message.push_str(&usage_line(command, op));
            }
            CliError::Usage(message)
        })
}

/// `sdm projects get --id <id>` style usage for one variant.
#[must_use]
pub fn usage_line(command: &str, op: &Operation) -> String {
    let mut line = command.to_owned();
    for param in &op.path_params {
        line.push_str(&format!(" --{} <{}>", flag_name(&param.name), param.name));
    }
    line
}

/// Flags the CLI defines itself; an API parameter with one of these names is
/// exposed as `--param-<name>` instead.
const RESERVED_FLAGS: [&str; 13] = [
    "data",
    "field",
    "query",
    "all",
    "yes",
    "dry-run",
    "output",
    "base-url",
    "token",
    "auth-scheme",
    "help",
    "depth",
    "examples",
];

/// Command-line spelling of an API parameter: `page_size` becomes `page-size`.
#[must_use]
pub fn flag_name(param: &str) -> String {
    let kebab = param.replace('_', "-");
    if RESERVED_FLAGS.contains(&kebab.as_str()) {
        format!("param-{kebab}")
    } else {
        kebab
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Node {
        Spec::embedded().tree()
    }

    fn variants<'a>(node: &'a Node, groups: &[&str], verb: &str) -> &'a [Operation] {
        let mut node = node;
        for group in groups {
            node = &node.groups[*group];
        }
        &node.verbs[verb]
    }

    #[test]
    fn list_and_detail_share_one_command() {
        let tree = tree();
        let ops = variants(&tree, &["projects"], "get");

        let list = resolve_variant("sdm projects get", ops, &[]).unwrap();
        let detail = resolve_variant("sdm projects get", ops, &["id"]).unwrap();

        assert_eq!(list.short_path(), "/projects/");
        assert_eq!(detail.short_path(), "/projects/{id}/");
    }

    #[test]
    fn nested_static_segments_become_nested_groups() {
        let tree = tree();
        let ops = variants(&tree, &["projects", "steps"], "patch");

        let op = resolve_variant("sdm projects steps patch", ops, &["project_pk", "id"]).unwrap();

        assert_eq!(op.short_path(), "/projects/{project_pk}/steps/{id}/");
    }

    #[test]
    fn step_type_variants_are_picked_by_path_params() {
        let tree = tree();
        let ops = variants(&tree, &["extraction"], "get");

        let op = resolve_variant("sdm extraction get", ops, &["action_id", "job_id"]).unwrap();

        assert_eq!(op.short_path(), "/extraction/{action_id}/{job_id}/");
    }

    #[test]
    fn query_flags_do_not_change_the_variant() {
        let tree = tree();
        let ops = variants(&tree, &["projects"], "get");

        let op = resolve_variant("sdm projects get", ops, &["page_size", "name"]).unwrap();

        assert_eq!(op.short_path(), "/projects/");
    }

    #[test]
    fn path_param_wins_over_same_named_query_param() {
        let tree = tree();
        let ops = variants(&tree, &["users"], "get");

        let op = resolve_variant("sdm users get", ops, &["id"]).unwrap();

        assert_eq!(op.short_path(), "/users/{id}/");
    }

    #[test]
    fn unmatched_flags_list_the_variants() {
        let tree = tree();
        let ops = variants(&tree, &["projects", "steps"], "patch");

        let err = resolve_variant("sdm projects steps patch", ops, &["id"]).unwrap_err();

        assert!(
            err.to_string()
                .contains("--project-pk <project_pk> --id <id>")
        );
    }

    #[test]
    fn every_command_resolves_each_of_its_variants() {
        fn walk(node: &Node) {
            for ops in node.verbs.values() {
                for op in ops {
                    let supplied: Vec<&str> =
                        op.path_params.iter().map(|p| p.name.as_str()).collect();
                    let found = resolve_variant("x", ops, &supplied).unwrap();
                    assert_eq!(found.path, op.path, "ambiguous variant for {}", op.path);
                }
            }
            node.groups.values().for_each(walk);
        }
        walk(&tree());
    }

    #[test]
    fn reserved_names_get_a_prefix() {
        assert_eq!(flag_name("page_size"), "page-size");
        assert_eq!(flag_name("token"), "param-token");
    }
}
