//! What reaches stdout and stderr: JSON as the API returned it.

use std::io::{IsTerminal, Write};

use serde_json::{Value, json};

use crate::error::{CliError, Result};

/// Prints a JSON value to stdout: indented on a terminal, compact when piped.
pub fn print_json(value: &Value) {
    let text = render(value, std::io::stdout().is_terminal());
    // A closed pipe (`sdm ... | head`) is not an error worth reporting.
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn render(value: &Value, pretty: bool) -> String {
    let rendered = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    rendered.expect("a JSON value always serializes")
}

/// Emits a successful response body.
///
/// With `output` set to a path the raw bytes go to that file and a short JSON
/// receipt goes to stdout. Otherwise JSON is printed as JSON, text as text,
/// and binary data only when stdout is not a terminal (or `--output -`).
///
/// # Errors
/// Returns a usage error when the file cannot be written, or when a binary
/// body would be dumped on a terminal.
pub fn emit_body(body: &[u8], content_type: &str, output: Option<&str>) -> Result<()> {
    if let Some(path) = output.filter(|path| *path != "-") {
        std::fs::write(path, body)
            .map_err(|err| CliError::usage(format!("cannot write {path}: {err}")))?;
        print_json(&json!({ "saved": path, "bytes": body.len() }));
        return Ok(());
    }
    if body.is_empty() {
        return Ok(());
    }
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        print_json(&value);
        return Ok(());
    }
    let is_text = content_type.starts_with("text/") || std::str::from_utf8(body).is_ok();
    if output == Some("-") || is_text || !std::io::stdout().is_terminal() {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(body);
        let _ = stdout.flush();
        return Ok(());
    }
    Err(CliError::usage(format!(
        "the response is binary ({content_type}, {} bytes); pass --output <file>",
        body.len()
    )))
}

/// Reports a failure on stderr as JSON in the API's error shape.
///
/// API answers are passed through unchanged; failures that originate in the
/// CLI use the same `{status_code, message, errors}` layout.
pub fn print_error(err: &CliError) {
    let pretty = std::io::stderr().is_terminal();
    let text = match err {
        CliError::Api { body, .. } => passthrough(body, pretty),
        CliError::Auth {
            message,
            status,
            body,
        } => {
            let mut value = json!({ "status_code": status, "message": message, "errors": [] });
            if !body.is_empty() {
                value["response"] = serde_json::from_slice(body)
                    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
            }
            render(&value, pretty)
        }
        CliError::Usage(message) | CliError::Network(message) => render(
            &json!({ "status_code": null, "message": message, "errors": [] }),
            pretty,
        ),
    };
    let _ = writeln!(std::io::stderr(), "{text}");
}

fn passthrough(body: &[u8], pretty: bool) -> String {
    match serde_json::from_slice::<Value>(body) {
        Ok(value) => render(&value, pretty),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}
