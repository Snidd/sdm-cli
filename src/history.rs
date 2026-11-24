//! The record of the requests sent from a scoped directory: writes, and
//! reads when the scope file asks for them.
//!
//! One JSON object per line, in one file per UTC day (`2026-11-24.jsonl`)
//! inside the directory the scope file names, so the record can be committed
//! and read back as the history of what was done to the scoped projects.
//! Responses are not kept: only what was sent and how the API answered.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde_json::{Map, Value, json};

use crate::error::{CliError, Result};
use crate::request::{Body, Call};

const HIDDEN: &str = "********";

/// Appends one request and its outcome to the history in `dir`. Failures
/// that happened before anything was sent are not recorded.
///
/// The request has already been sent, so a history that cannot be written is
/// a warning, not an error.
pub fn record<T>(dir: &Path, call: &Call, url: &Url, outcome: &Result<(u16, T)>) {
    let mut entry = Map::new();
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (date, time) = utc(seconds);
    entry.insert("time".to_owned(), json!(format!("{date}T{time}Z")));
    entry.insert(
        "command".to_owned(),
        json!(mask_arguments(std::env::args().skip(1))),
    );
    entry.insert("method".to_owned(), json!(call.method));
    entry.insert("url".to_owned(), json!(url.as_str()));
    if let Some(body) = body(&call.body) {
        entry.insert("body".to_owned(), body);
    }
    match outcome {
        Ok((status, _)) => entry.insert("status".to_owned(), json!(status)),
        Err(CliError::Api { status, .. }) => entry.insert("status".to_owned(), json!(status)),
        Err(CliError::Auth { status, .. }) => entry.insert("status".to_owned(), json!(status)),
        Err(CliError::Network(message)) => entry.insert("error".to_owned(), json!(message)),
        Err(CliError::Usage(_) | CliError::Scope(_)) => return,
    };

    let file = dir.join(format!("{date}.jsonl"));
    let line = format!("{}\n", Value::Object(entry));
    let written = std::fs::create_dir_all(dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)?
            .write_all(line.as_bytes())
    });
    if let Err(err) = written {
        eprintln!(
            "warning: the request was sent but not recorded in {}: {err}",
            file.display()
        );
    }
}

/// The body as sent, with passwords hidden. Uploads are recorded by path.
fn body(body: &Body) -> Option<Value> {
    match body {
        Body::None => None,
        Body::Json(value) => {
            let mut value = value.clone();
            if let Some(password) = value.get_mut("password") {
                *password = json!(HIDDEN);
            }
            Some(value)
        }
        Body::Multipart { fields, files } => {
            let fields: Map<String, Value> = fields
                .iter()
                .map(|(key, value)| (key.clone(), json!(value)))
                .collect();
            let files: Map<String, Value> = files
                .iter()
                .map(|(key, path)| (key.clone(), json!(path.display().to_string())))
                .collect();
            Some(json!({ "fields": fields, "files": files }))
        }
    }
}

/// The command line without the API key.
fn mask_arguments(arguments: impl Iterator<Item = String>) -> Vec<String> {
    let mut masked = Vec::new();
    let mut hide_next = false;
    for argument in arguments {
        if hide_next {
            masked.push(HIDDEN.to_owned());
            hide_next = false;
        } else if argument.starts_with("--token=") {
            masked.push(format!("--token={HIDDEN}"));
        } else {
            hide_next = argument == "--token";
            masked.push(argument);
        }
    }
    masked
}

/// UTC date (`2026-11-24`) and time (`09:15:02`) of a Unix timestamp.
fn utc(seconds: u64) -> (String, String) {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!(
            "{:02}:{:02}:{:02}",
            rest / 3_600,
            rest % 3_600 / 60,
            rest % 60
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(utc(0), ("1970-01-01".to_owned(), "00:00:00".to_owned()));
        assert_eq!(
            utc(1_795_511_702),
            ("2026-11-24".to_owned(), "09:15:02".to_owned())
        );
        assert_eq!(utc(1_709_251_199).0, "2024-02-29");
        assert_eq!(utc(1_709_251_200).0, "2024-03-01");
    }

    #[test]
    fn the_key_is_not_recorded() {
        let arguments = [
            "projects",
            "get",
            "--token",
            "secret",
            "--token=secret",
            "--id",
            "3",
        ];

        let masked = mask_arguments(arguments.iter().map(|argument| (*argument).to_owned()));

        assert_eq!(
            masked,
            [
                "projects",
                "get",
                "--token",
                HIDDEN,
                "--token=********",
                "--id",
                "3"
            ]
        );
    }

    #[test]
    fn passwords_are_not_recorded() {
        let sent = Body::Json(json!({"email": "a@b.c", "password": "hunter2"}));

        assert_eq!(
            body(&sent),
            Some(json!({"email": "a@b.c", "password": HIDDEN}))
        );
        assert_eq!(body(&Body::None), None);
    }
}
