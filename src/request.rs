//! Turning a resolved command into an HTTP call and running it.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder, multipart};
use serde_json::{Map, Value, json};

use crate::error::{CliError, Result};
use crate::output;
use crate::scope::Scope;

/// Settings shared by every call, taken from flags, environment and the
/// credentials file.
#[derive(Debug, Clone)]
pub struct Context {
    pub base_url: String,
    pub token: Option<String>,
    pub auth_scheme: String,
    pub dry_run: bool,
    pub yes: bool,
    pub output: Option<String>,
    /// Extra `--query key=value` pairs.
    pub extra_query: Vec<(String, String)>,
    /// Limits of the current directory, when it has a scope file.
    pub scope: Option<Scope>,
}

/// Request payload.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    None,
    Json(Value),
    Multipart {
        fields: Vec<(String, String)>,
        files: Vec<(String, PathBuf)>,
    },
}

/// One HTTP call, ready to send.
#[derive(Debug, Clone)]
pub struct Call {
    /// Uppercase HTTP method.
    pub method: String,
    /// Path below the host, starting with `/`; may already carry a query.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Body,
    /// Follow `next` links and return one combined array.
    pub all: bool,
}

/// Builds the body from `--data` and `--field`.
///
/// `--data` is JSON given inline, as `@file`, or `-` for stdin. Each `--field`
/// sets a top-level key: `key=value` (string), `key:=<json>` (any JSON value)
/// or `key=@path` (file upload, which switches the request to multipart).
/// Nothing is validated against the spec, so undocumented fields pass through.
///
/// # Errors
/// Returns a usage error for unreadable input, invalid JSON, or fields
/// combined with a non-object `--data`.
pub fn build_body(data: Option<&str>, fields: &[String]) -> Result<Body> {
    let data = data.map(read_data).transpose()?;
    if fields.is_empty() {
        return Ok(data.map_or(Body::None, Body::Json));
    }

    let mut object = match data {
        None => Map::new(),
        Some(Value::Object(object)) => object,
        Some(_) => {
            return Err(CliError::usage(
                "--field needs --data to be a JSON object (or absent)",
            ));
        }
    };
    let mut files = Vec::new();
    for field in fields {
        match parse_field(field)? {
            Field::Value(key, value) => {
                object.insert(key, value);
            }
            Field::File(key, path) => files.push((key, path)),
        }
    }
    if files.is_empty() {
        return Ok(Body::Json(Value::Object(object)));
    }
    let fields = object
        .into_iter()
        .map(|(key, value)| match value {
            Value::String(text) => (key, text),
            other => (key, other.to_string()),
        })
        .collect();
    Ok(Body::Multipart { fields, files })
}

enum Field {
    Value(String, Value),
    File(String, PathBuf),
}

fn parse_field(field: &str) -> Result<Field> {
    let (key, value) = field
        .split_once('=')
        .ok_or_else(|| CliError::usage(format!("--field `{field}` is not key=value")))?;
    if let Some(key) = key.strip_suffix(':') {
        let value = serde_json::from_str(value).map_err(|err| {
            CliError::usage(format!("--field `{key}:=` is not valid JSON: {err}"))
        })?;
        return Ok(Field::Value(key.to_owned(), value));
    }
    if key.is_empty() {
        return Err(CliError::usage(format!("--field `{field}` has no key")));
    }
    Ok(match value.strip_prefix('@') {
        Some(path) => Field::File(key.to_owned(), PathBuf::from(path)),
        None => Field::Value(key.to_owned(), Value::String(value.to_owned())),
    })
}

fn read_data(data: &str) -> Result<Value> {
    let text = if data == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| CliError::usage(format!("cannot read --data from stdin: {err}")))?;
        text
    } else if let Some(path) = data.strip_prefix('@') {
        std::fs::read_to_string(path)
            .map_err(|err| CliError::usage(format!("cannot read --data file {path}: {err}")))?
    } else {
        data.to_owned()
    };
    serde_json::from_str(&text)
        .map_err(|err| CliError::usage(format!("--data is not valid JSON: {err}")))
}

/// Parses a `key=value` pair for `--query`.
///
/// # Errors
/// Returns a usage error when there is no `=`.
pub fn parse_pair(pair: &str) -> Result<(String, String)> {
    pair.split_once('=')
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| CliError::usage(format!("`{pair}` is not key=value")))
}

/// Percent-encodes a value for use as one path segment.
#[must_use]
pub fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// HTTP client used for every call.
///
/// # Errors
/// Returns a network error when the TLS backend cannot be initialised.
pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("sdm-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| CliError::Network(format!("cannot create HTTP client: {err}")))
}

impl Context {
    /// Absolute URL for `call`, including its query parameters.
    ///
    /// # Errors
    /// Returns a usage error when base URL and path do not form a URL.
    pub fn url(&self, call: &Call) -> Result<Url> {
        let joined = format!("{}{}", self.base_url.trim_end_matches('/'), call.path);
        let mut url = Url::parse(&joined)
            .map_err(|err| CliError::usage(format!("invalid URL {joined}: {err}")))?;
        let pairs = call.query.iter().chain(&self.extra_query);
        if pairs.clone().next().is_some() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
        Ok(url)
    }

    fn authorization(&self) -> Option<String> {
        self.token
            .as_ref()
            .map(|token| format!("{} {token}", self.auth_scheme))
    }

    /// The `Authorization` value with all but the last four characters hidden.
    fn masked_authorization(&self) -> Option<String> {
        self.token.as_ref().map(|token| {
            let tail: String = token
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let tail = if token.chars().count() > 8 {
                tail
            } else {
                String::new()
            };
            format!("{} ****{tail}", self.auth_scheme)
        })
    }

    /// Refuses a call that falls outside the directory's scope. Runs before
    /// `--dry-run` too, so a preview shows the refusal.
    fn check_scope(&self, call: &Call, url: &Url) -> Result<()> {
        match &self.scope {
            Some(scope) => scope.check(self, call, url),
            None => Ok(()),
        }
    }

    /// Runs `call` and prints the result, honouring the scope, `--dry-run`,
    /// delete confirmation, `--all` and `--output`.
    ///
    /// # Errors
    /// Returns the matching [`CliError`] for usage, scope, auth, API and
    /// network failures.
    pub fn execute(&self, call: &Call) -> Result<()> {
        let url = self.url(call)?;
        self.check_scope(call, &url)?;
        if self.dry_run {
            output::print_json(&self.describe_call(call, &url));
            return Ok(());
        }
        if call.method == "DELETE" {
            self.confirm_delete(&url)?;
        }

        let client = client()?;
        let (body, content_type) = self.send(&client, call, url)?;
        if !call.all {
            return output::emit_body(&body, &content_type, self.output.as_deref());
        }

        // `--all`: keep requesting while the answer is a page with a `next` link.
        let Some((results, next)) = split_page(&body) else {
            return output::emit_body(&body, &content_type, self.output.as_deref());
        };
        let results = self.following_pages(&client, call, results, next)?;
        let combined = serde_json::to_vec(&Value::Array(results)).expect("JSON serializes");
        output::emit_body(&combined, "application/json", self.output.as_deref())
    }

    /// Adds the results of every page after the first to `results`.
    fn following_pages(
        &self,
        client: &Client,
        call: &Call,
        mut results: Vec<Value>,
        mut next: Option<String>,
    ) -> Result<Vec<Value>> {
        while let Some(link) = next {
            let page_url = self.next_url(call, &link)?;
            let (body, _) = self.send(client, call, page_url)?;
            let Some((more, following)) = split_page(&body) else {
                return Err(CliError::Network(
                    "a page of an --all request was not a paginated answer".to_owned(),
                ));
            };
            results.extend(more);
            next = following;
        }
        Ok(results)
    }

    /// Every result of a paginated list, without printing anything.
    ///
    /// # Errors
    /// Returns the matching [`CliError`] for scope, auth, API and network
    /// failures, and a network error when the answer is not a list.
    pub fn list_all(&self, call: &Call) -> Result<Vec<Value>> {
        let url = self.url(call)?;
        self.check_scope(call, &url)?;
        let client = client()?;
        let (body, _) = self.send(&client, call, url)?;
        if let Some((results, next)) = split_page(&body) {
            return self.following_pages(&client, call, results, next);
        }
        match serde_json::from_slice(&body) {
            Ok(Value::Array(results)) => Ok(results),
            _ => Err(CliError::Network(format!(
                "{} did not answer with a list",
                call.path
            ))),
        }
    }

    /// GETs `path` as JSON for the scope check itself, which is why the
    /// request is not checked against the scope.
    pub(crate) fn lookup(&self, path: &str) -> Result<Value> {
        let call = Call {
            method: "GET".to_owned(),
            path: path.to_owned(),
            query: Vec::new(),
            body: Body::None,
            all: false,
        };
        let url = Self {
            extra_query: Vec::new(),
            ..self.clone()
        }
        .url(&call)?;
        let (body, _) = self.send(&client()?, &call, url)?;
        serde_json::from_slice(&body)
            .map_err(|err| CliError::Network(format!("{path} did not answer with JSON: {err}")))
    }

    /// The `next` link points wherever the server thinks it lives; only its
    /// query is trusted, so the key is never sent to another host.
    fn next_url(&self, call: &Call, link: &str) -> Result<Url> {
        let next = Url::parse(link)
            .map_err(|err| CliError::Network(format!("invalid `next` link {link}: {err}")))?;
        let bare = Call {
            query: Vec::new(),
            ..call.clone()
        };
        let mut url = Self {
            extra_query: Vec::new(),
            ..self.clone()
        }
        .url(&bare)?;
        url.set_query(next.query());
        Ok(url)
    }

    /// Sends `call` once and returns the raw body and its content type,
    /// without printing anything.
    ///
    /// # Errors
    /// Returns the matching [`CliError`] for scope, auth, API and network
    /// failures.
    pub fn request(&self, call: &Call) -> Result<(Vec<u8>, String)> {
        let url = self.url(call)?;
        self.check_scope(call, &url)?;
        self.send(&client()?, call, url)
    }

    fn send(&self, client: &Client, call: &Call, url: Url) -> Result<(Vec<u8>, String)> {
        let method = reqwest::Method::from_bytes(call.method.as_bytes())
            .map_err(|_| CliError::usage(format!("unknown HTTP method {}", call.method)))?;
        let mut request = client.request(method, url);
        if let Some(authorization) = self.authorization() {
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }
        request = attach_body(request, &call.body)?;

        let response = request
            .send()
            .map_err(|err| CliError::Network(describe_transport_error(&err)))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response
            .bytes()
            .map_err(|err| CliError::Network(describe_transport_error(&err)))?
            .to_vec();

        match status {
            401 | 403 => Err(CliError::Auth {
                message: if self.token.is_some() {
                    format!("the API rejected the credentials (status {status})")
                } else {
                    "not logged in: run `sdm login` or set SDM_TOKEN".to_owned()
                },
                status: Some(status),
                body,
            }),
            400.. => Err(CliError::Api { status, body }),
            _ => Ok((body, content_type)),
        }
    }

    fn describe_call(&self, call: &Call, url: &Url) -> Value {
        let mut headers = Map::new();
        if let Some(authorization) = self.masked_authorization() {
            headers.insert("Authorization".to_owned(), Value::String(authorization));
        }
        let body = match &call.body {
            Body::None => Value::Null,
            Body::Json(value) => {
                headers.insert("Content-Type".to_owned(), json!("application/json"));
                value.clone()
            }
            Body::Multipart { fields, files } => {
                headers.insert("Content-Type".to_owned(), json!("multipart/form-data"));
                let fields: Map<String, Value> = fields
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect();
                let files: Map<String, Value> = files
                    .iter()
                    .map(|(key, path)| (key.clone(), json!(path.display().to_string())))
                    .collect();
                json!({ "fields": fields, "files": files })
            }
        };
        json!({
            "dry_run": true,
            "method": call.method,
            "url": url.as_str(),
            "headers": headers,
            "body": body,
        })
    }

    fn confirm_delete(&self, url: &Url) -> Result<()> {
        if self.yes {
            return Ok(());
        }
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Err(CliError::usage(format!(
                "refusing to DELETE {url} without confirmation; pass --yes (or --dry-run to preview)"
            )));
        }
        eprint!("DELETE {url}\nThis cannot be undone. Continue? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|err| CliError::usage(format!("cannot read confirmation: {err}")))?;
        if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            Ok(())
        } else {
            Err(CliError::usage("delete cancelled"))
        }
    }
}

fn attach_body(request: RequestBuilder, body: &Body) -> Result<RequestBuilder> {
    Ok(match body {
        Body::None => request,
        Body::Json(value) => request.json(value),
        Body::Multipart { fields, files } => {
            let mut form = multipart::Form::new();
            for (key, value) in fields {
                form = form.text(key.clone(), value.clone());
            }
            for (key, path) in files {
                form = form.file(key.clone(), path).map_err(|err| {
                    CliError::usage(format!("cannot read {}: {err}", path.display()))
                })?;
            }
            request.multipart(form)
        }
    })
}

/// Splits a paginated answer into its results and `next` link.
fn split_page(body: &[u8]) -> Option<(Vec<Value>, Option<String>)> {
    let Value::Object(mut page) = serde_json::from_slice(body).ok()? else {
        return None;
    };
    let next = page.get("next")?.as_str().map(str::to_owned);
    let Value::Array(results) = page.remove("results")? else {
        return None;
    };
    Some((results, next))
}

fn describe_transport_error(err: &reqwest::Error) -> String {
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn data_alone_is_sent_untouched() {
        let body = build_body(Some(r#"{"steps":[{"name":"x"}],"unknown":1}"#), &[]).unwrap();

        assert_eq!(
            body,
            Body::Json(json!({"steps": [{"name": "x"}], "unknown": 1}))
        );
    }

    #[test]
    fn fields_override_data_and_parse_json_values() {
        let body = build_body(
            Some(r#"{"name":"old","keep":true}"#),
            &fields(&["name=new", "count:=3", "tags:=[\"a\"]"]),
        )
        .unwrap();

        assert_eq!(
            body,
            Body::Json(json!({"name": "new", "keep": true, "count": 3, "tags": ["a"]}))
        );
    }

    #[test]
    fn file_field_switches_to_multipart() {
        let body = build_body(None, &fields(&["file=@/tmp/a.csv", "project:=7"])).unwrap();

        assert_eq!(
            body,
            Body::Multipart {
                fields: vec![("project".to_owned(), "7".to_owned())],
                files: vec![("file".to_owned(), PathBuf::from("/tmp/a.csv"))],
            }
        );
    }

    #[test]
    fn fields_reject_non_object_data() {
        assert!(build_body(Some("[1]"), &fields(&["a=b"])).is_err());
    }

    #[test]
    fn no_flags_means_no_body() {
        assert_eq!(build_body(None, &[]).unwrap(), Body::None);
    }

    #[test]
    fn path_values_are_percent_encoded() {
        assert_eq!(encode_segment("a/b c"), "a%2Fb%20c");
        assert_eq!(encode_segment("4704"), "4704");
    }

    #[test]
    fn page_is_split_into_results_and_next() {
        let (results, next) =
            split_page(br#"{"count":2,"next":"http://x/?page=2","results":[1]}"#).unwrap();

        assert_eq!(results, vec![json!(1)]);
        assert_eq!(next.as_deref(), Some("http://x/?page=2"));
        assert!(split_page(br#"{"id":1}"#).is_none());
    }

    #[test]
    fn dry_run_masks_the_key() {
        let context = Context {
            base_url: "https://sdm.akeneo.cloud".to_owned(),
            token: Some("0123456789abcdef".to_owned()),
            auth_scheme: "Token".to_owned(),
            dry_run: true,
            yes: false,
            output: None,
            extra_query: vec![("x".to_owned(), "1".to_owned())],
            scope: None,
        };
        let call = Call {
            method: "GET".to_owned(),
            path: "/api/v1/projects/".to_owned(),
            query: vec![("page_size".to_owned(), "5".to_owned())],
            body: Body::None,
            all: false,
        };

        let url = context.url(&call).unwrap();
        let described = context.describe_call(&call, &url);

        assert_eq!(
            described["url"],
            "https://sdm.akeneo.cloud/api/v1/projects/?page_size=5&x=1"
        );
        assert_eq!(described["headers"]["Authorization"], "Token ****cdef");
    }
}
