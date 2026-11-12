//! `sdm describe`: a machine-readable view of commands, for agents.

use serde_json::{Map, Value, json};

use crate::error::{CliError, Result};
use crate::spec::{Node, Operation, Param, Spec, flag_name, usage_line};

/// Prints either an index of commands (for a group, or everything) or the
/// full JSON description of one command.
///
/// # Errors
/// Returns a usage error when `tokens` does not name a group or command.
pub fn run(
    spec: &Spec,
    tree: &Node,
    tokens: &[String],
    depth: usize,
    examples: bool,
) -> Result<()> {
    let mut node = tree;
    let mut prefix = vec!["sdm".to_owned()];
    for (index, token) in tokens.iter().enumerate() {
        let is_last = index + 1 == tokens.len();
        if is_last && let Some(variants) = node.verbs.get(token) {
            prefix.push(token.clone());
            let detail = describe_command(spec, &prefix.join(" "), variants, depth, examples);
            crate::output::print_json(&detail);
            return Ok(());
        }
        node = node.groups.get(token).ok_or_else(|| {
            CliError::usage(format!(
                "`{} {token}` is not a command; run `{} --help`",
                prefix.join(" "),
                prefix.join(" ")
            ))
        })?;
        prefix.push(token.clone());
    }

    let mut lines = Vec::new();
    index(node, &prefix.join(" "), &mut lines);
    let width = lines
        .iter()
        .map(|(usage, _)| usage.len())
        .max()
        .unwrap_or(0);
    for (usage, summary) in lines {
        println!("{usage:<width$}  # {summary}");
    }
    Ok(())
}

fn index(node: &Node, prefix: &str, lines: &mut Vec<(String, String)>) {
    for (verb, variants) in &node.verbs {
        let command = format!("{prefix} {verb}");
        for op in variants {
            lines.push((usage_line(&command, op), op.summary.clone()));
        }
    }
    for (name, group) in &node.groups {
        index(group, &format!("{prefix} {name}"), lines);
    }
}

fn describe_command(
    spec: &Spec,
    command: &str,
    variants: &[Operation],
    depth: usize,
    examples: bool,
) -> Value {
    let variants: Vec<Value> = variants
        .iter()
        .map(|op| describe_variant(spec, command, op, depth, examples))
        .collect();
    json!({ "command": command, "variants": variants })
}

fn describe_variant(
    spec: &Spec,
    command: &str,
    op: &Operation,
    depth: usize,
    examples: bool,
) -> Value {
    let raw = spec.operation_value(op);
    let mut out = Map::new();
    out.insert("usage".to_owned(), json!(usage_line(command, op)));
    out.insert("method".to_owned(), json!(op.method.to_uppercase()));
    out.insert("path".to_owned(), json!(op.path));
    out.insert("summary".to_owned(), json!(op.summary));
    if let Some(description) = raw.get("description").and_then(Value::as_str)
        && description.trim() != op.summary
    {
        out.insert("description".to_owned(), json!(description.trim()));
    }
    out.insert("path_params".to_owned(), params(&op.path_params));
    out.insert("query_params".to_owned(), params(&op.query_params));

    if op.takes_body() {
        out.insert("request_body".to_owned(), request_body(spec, raw, depth));
    }
    if let Some(response) = response(spec, raw, depth, examples) {
        out.insert("response".to_owned(), response);
    }
    if let Some(overlay) = spec.overlay_for(op).and_then(Value::as_object) {
        for (key, value) in overlay {
            out.insert(key.clone(), value.clone());
        }
    }
    Value::Object(out)
}

fn params(params: &[Param]) -> Value {
    params
        .iter()
        .map(|param| {
            let mut entry = Map::new();
            entry.insert("name".to_owned(), json!(param.name));
            entry.insert(
                "flag".to_owned(),
                json!(format!("--{}", flag_name(&param.name))),
            );
            entry.insert("type".to_owned(), json!(param.ty));
            if param.is_array {
                entry.insert("repeatable".to_owned(), json!(true));
            }
            if !param.description.is_empty() {
                entry.insert("description".to_owned(), json!(param.description));
            }
            Value::Object(entry)
        })
        .collect()
}

fn request_body(spec: &Spec, raw: &Value, depth: usize) -> Value {
    let Some(content) = raw
        .pointer("/requestBody/content")
        .and_then(Value::as_object)
    else {
        return json!({
            "documented": false,
            "hint": "the API does not publish a body schema for this call; pass JSON with --data",
        });
    };
    let schema = content
        .get("application/json")
        .or_else(|| content.values().next())
        .and_then(|media| media.get("schema"))
        .map_or(Value::Null, |schema| resolve(spec, schema, depth));
    json!({
        "documented": true,
        "required": raw.pointer("/requestBody/required").and_then(Value::as_bool).unwrap_or(false),
        "content_types": content.keys().collect::<Vec<_>>(),
        "schema": schema,
    })
}

fn response(spec: &Spec, raw: &Value, depth: usize, examples: bool) -> Option<Value> {
    let responses = raw.get("responses")?.as_object()?;
    let (status, success) = responses
        .iter()
        .find(|(status, _)| status.starts_with('2'))?;
    let mut out = Map::new();
    out.insert("status".to_owned(), json!(status));
    if let Some(description) = success.get("description").and_then(Value::as_str) {
        out.insert("description".to_owned(), json!(description));
    }
    if let Some(content) = success.get("content").and_then(Value::as_object) {
        let media = content.get("application/json");
        match media {
            Some(media) => {
                if let Some(schema) = media.get("schema") {
                    out.insert("schema".to_owned(), resolve(spec, schema, depth));
                }
                if examples
                    && let Some(example) = media
                        .get("examples")
                        .and_then(Value::as_object)
                        .and_then(|all| all.values().next())
                        .and_then(|example| example.get("value"))
                {
                    out.insert("example".to_owned(), example.clone());
                }
            }
            None => {
                out.insert(
                    "content_types".to_owned(),
                    json!(content.keys().collect::<Vec<_>>()),
                );
                out.insert(
                    "hint".to_owned(),
                    json!("binary download; pass --output <file>"),
                );
            }
        }
    }
    Some(Value::Object(out))
}

/// Inlines `$ref`s up to `depth` levels; deeper references are left as
/// `{"$ref": ...}` so that recursive and very large schemas stay bounded.
fn resolve(spec: &Spec, value: &Value, depth: usize) -> Value {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                return match (depth, spec.resolve_ref(reference)) {
                    (0, _) | (_, None) => value.clone(),
                    (_, Some(target)) => resolve(spec, target, depth - 1),
                };
            }
            Value::Object(
                object
                    .iter()
                    .map(|(key, child)| (key.clone(), resolve(spec, child, depth)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve(spec, item, depth))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_inlined_up_to_the_depth() {
        let spec = Spec::embedded();
        let schema = json!({"$ref": "#/components/schemas/ObtainTokenRequest"});

        let shallow = resolve(&spec, &schema, 0);
        let inlined = resolve(&spec, &schema, 1);

        assert_eq!(shallow, schema);
        assert!(inlined["properties"]["email"].is_object());
    }

    #[test]
    fn undocumented_bodies_are_flagged() {
        let spec = Spec::embedded();
        let tree = spec.tree();
        let variants = &tree.groups["steps"].groups["hooks"].verbs["post"];

        let detail = describe_command(&spec, "sdm steps hooks post", variants, 2, false);

        assert_eq!(
            detail["variants"][0]["usage"],
            "sdm steps hooks post --action-id <action_id>"
        );
        assert_eq!(detail["variants"][0]["request_body"]["documented"], false);
    }
}
