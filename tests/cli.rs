//! End-to-end tests: the real binary against a one-shot local HTTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

/// Serves exactly one request with the given status and JSON body, and hands
/// back the raw request (head and body) that was received.
fn serve_once(status: u16, body: &'static str) -> (String, Receiver<String>) {
    serve(vec![(status, body)])
}

/// Serves one request per answer, in order, and hands back each raw request.
fn serve(answers: Vec<(u16, &'static str)>) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = channel();
    thread::spawn(move || {
        for (status, body) in answers {
            answer(&listener, &sender, status, body);
        }
    });
    (base_url, receiver)
}

fn answer(
    listener: &TcpListener,
    sender: &std::sync::mpsc::Sender<String>,
    status: u16,
    body: &str,
) {
    {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            if line == "\r\n" || line.is_empty() {
                break;
            }
            head.push_str(&line);
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).unwrap();
        sender
            .send(format!("{head}\n{}", String::from_utf8_lossy(&payload)))
            .unwrap();
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        reader.get_mut().write_all(response.as_bytes()).unwrap();
    }
}

fn sdm(base_url: &str, args: &[&str]) -> Output {
    // A directory with no scope file above it.
    sdm_in(&std::env::temp_dir(), base_url, args)
}

fn sdm_in(dir: &Path, base_url: &str, args: &[&str]) -> Output {
    let home = std::env::temp_dir().join(format!("sdm-cli-test-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_sdm"))
        .args(args)
        .current_dir(dir)
        .env("SDM_BASE_URL", base_url)
        .env("SDM_TOKEN", "secret-key-1234")
        // Keep the developer's real credentials and cached spec out of it.
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn patch_sends_method_path_token_and_untouched_body() {
    let (base_url, request) = serve_once(200, r#"{"id":3}"#);

    let output = sdm(
        &base_url,
        &[
            "projects",
            "patch",
            "--id",
            "3",
            "--data",
            r#"{"steps":[{"type":"mapping"}]}"#,
            "--field",
            "name=demo",
            "--query",
            "extra=1",
        ],
    );

    let request = request.recv().unwrap();
    assert!(
        request.starts_with("PATCH /api/v1/projects/3/?extra=1 HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_lowercase()
            .contains("authorization: token secret-key-1234")
    );
    assert!(
        request.ends_with(r#"{"steps":[{"type":"mapping"}],"name":"demo"}"#),
        "{request}"
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        r#"{"id":3}"#
    );
}

#[test]
fn file_field_is_uploaded_as_multipart() {
    let (base_url, request) = serve_once(201, r#"{"id":9}"#);
    let file = std::env::temp_dir().join(format!("sdm-cli-upload-{}.csv", std::process::id()));
    std::fs::write(&file, "sku;name\n1;a\n").unwrap();

    let output = sdm(
        &base_url,
        &[
            "api",
            "POST",
            "/files/",
            "--field",
            &format!("file=@{}", file.display()),
            "--field",
            "project:=7",
        ],
    );

    let request = request.recv().unwrap();
    assert!(
        request.starts_with("POST /api/v1/files/ HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_lowercase()
            .contains("content-type: multipart/form-data")
    );
    assert!(request.contains("sku;name"));
    assert!(request.contains("name=\"project\""));
    assert_eq!(output.status.code(), Some(0));
    std::fs::remove_file(file).unwrap();
}

#[test]
fn api_errors_go_to_stderr_with_exit_code_4() {
    let (base_url, _request) = serve_once(404, r#"{"detail":"Not found."}"#);

    let output = sdm(&base_url, &["projects", "get", "--id", "1"]);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        r#"{"detail":"Not found."}"#
    );
}

#[test]
fn rejected_key_exits_with_code_3() {
    let (base_url, _request) = serve_once(401, r#"{"detail":"Invalid token."}"#);

    let output = sdm(&base_url, &["whoami"]);

    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn delete_without_yes_is_refused_and_sends_nothing() {
    // Port 9 is never contacted: the refusal happens before any request.
    let output = sdm("http://127.0.0.1:9", &["projects", "delete", "--id", "1"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("pass --yes"));
}

#[test]
fn dry_run_prints_the_request_and_masks_the_key() {
    let output = sdm(
        "http://127.0.0.1:9",
        &[
            "steps",
            "hooks",
            "post",
            "--action-id",
            "5",
            "--data",
            r#"{"a":1}"#,
            "--dry-run",
        ],
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains(r#""url":"http://127.0.0.1:9/api/v1/steps/5/hooks/""#),
        "{stdout}"
    );
    assert!(stdout.contains("Token ****1234"));
    assert!(!stdout.contains("secret-key"));
}

/// Never contacted: a refusal happens before any request.
const UNREACHABLE: &str = "http://127.0.0.1:9";

/// A fresh directory scoped to project 7 on `base_url`: every group at
/// `write`, organization-wide data closed except for reading files.
fn scoped_dir(name: &str, base_url: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sdm-cli-scope-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    let scope = format!(
        r#"{{"version":1,"base_url":"{base_url}","projects":[{{"id":7,"name":"Demo","access":{{
            "project":"write","steps":"write","jobs":"write","rules":"write","hooks":"write",
            "options_collections":"write","portals":"write","outputs":"write",
            "connectors":"write","metrics":"write"}}}}],
            "organization":{{"files":"read"}}}}"#
    );
    std::fs::write(dir.join(".sdm-scope.json"), scope).unwrap();
    dir
}

fn refusal(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(6), "{output:?}");
    assert!(output.stdout.is_empty());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_scoped_project_is_reachable_from_the_directory_and_below() {
    let (base_url, request) = serve_once(200, r#"{"id":7}"#);
    let dir = scoped_dir("inside", &base_url);

    let output = sdm_in(
        &dir.join("nested"),
        &base_url,
        &["projects", "patch", "--id", "7", "--field", "name=x"],
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        request
            .recv()
            .unwrap()
            .starts_with("PATCH /api/v1/projects/7/ ")
    );
}

#[test]
fn another_project_is_refused_and_nothing_is_sent() {
    let dir = scoped_dir("other", UNREACHABLE);

    let by_command = sdm_in(&dir, UNREACHABLE, &["projects", "get", "--id", "8"]);
    let by_raw_path = sdm_in(&dir, UNREACHABLE, &["api", "GET", "/projects/7/../8/"]);
    let by_filter = sdm_in(&dir, UNREACHABLE, &["jobs", "get", "--project", "8"]);
    let by_body = sdm_in(
        &dir,
        UNREACHABLE,
        &["jobs", "post", "--field", "project:=8", "--dry-run"],
    );

    for output in [by_command, by_raw_path, by_filter, by_body] {
        let message = refusal(&output);
        assert!(
            message.contains("project 8 is not in the scope"),
            "{message}"
        );
    }
}

#[test]
fn deleting_is_refused_at_the_write_level() {
    let dir = scoped_dir("delete", UNREACHABLE);

    let delete = sdm_in(
        &dir,
        UNREACHABLE,
        &["projects", "delete", "--id", "7", "--yes"],
    );
    let rule = sdm_in(
        &dir,
        UNREACHABLE,
        &[
            "projects",
            "rules",
            "delete",
            "--project-pk",
            "7",
            "--rule-pk",
            "1",
            "--yes",
        ],
    );

    assert!(refusal(&delete).contains("needs 'delete' on project of project 7"));
    assert!(refusal(&rule).contains("needs 'delete' on rules of project 7"));
}

#[test]
fn requests_that_cannot_be_placed_in_a_project_are_refused() {
    let dir = scoped_dir("unplaced", UNREACHABLE);
    let refused: [&[&str]; 7] = [
        &["projects", "get"],
        &["jobs", "get"],
        &["bases", "get"],
        &["files", "post", "--field", "name=x"],
        &["api", "GET", "/api/v2/projects/7/"],
        &["api", "GET", "/something/new/"],
        &[
            "projects",
            "get",
            "--id",
            "7",
            "--base-url",
            "http://127.0.0.1:10",
        ],
    ];

    for args in refused {
        let output = sdm_in(&dir, UNREACHABLE, args);
        refusal(&output);
    }
}

#[test]
fn opened_organization_data_is_reachable_at_its_level() {
    let (base_url, request) = serve_once(200, r#"{"results":[]}"#);
    let dir = scoped_dir("org", &base_url);

    let output = sdm_in(&dir, &base_url, &["files", "get"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(request.recv().unwrap().starts_with("GET /api/v1/files/ "));
}

#[test]
fn a_step_is_placed_by_reading_the_scoped_project() {
    let (base_url, request) = serve(vec![
        (200, r#"{"id":7,"steps":[{"id":70},{"id":71}]}"#),
        (200, r#"{"ok":true}"#),
    ]);
    let dir = scoped_dir("step", &base_url);

    let output = sdm_in(
        &dir,
        &base_url,
        &["mapping", "patch", "--action-id", "71", "--field", "a=b"],
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        request
            .recv()
            .unwrap()
            .starts_with("GET /api/v1/projects/7/ ")
    );
    assert!(
        request
            .recv()
            .unwrap()
            .starts_with("PATCH /api/v1/mapping/71/ ")
    );
}

#[test]
fn a_step_of_another_project_is_refused() {
    let (base_url, _request) = serve_once(200, r#"{"id":7,"steps":[{"id":70}]}"#);
    let dir = scoped_dir("foreign-step", &base_url);

    let output = sdm_in(&dir, &base_url, &["mapping", "get", "--action-id", "99"]);

    assert!(refusal(&output).contains("step 99 is not a step of a scoped project"));
}

#[test]
fn a_job_is_placed_by_reading_it() {
    let (base_url, _request) = serve_once(200, r#"{"id":"j1","project":8}"#);
    let dir = scoped_dir("job", &base_url);

    let output = sdm_in(&dir, &base_url, &["jobs", "get", "--id", "j1"]);

    assert!(refusal(&output).contains("job j1 belongs elsewhere"));
}

#[test]
fn a_damaged_scope_file_refuses_everything() {
    let dir = scoped_dir("damaged", UNREACHABLE);
    std::fs::write(dir.join(".sdm-scope.json"), r#"{"version":1"#).unwrap();

    let output = sdm_in(&dir, UNREACHABLE, &["projects", "get", "--id", "7"]);

    assert!(refusal(&output).contains("cannot use scope file"));
}

#[test]
fn the_scope_can_be_read_but_not_changed_without_a_terminal() {
    let dir = scoped_dir("show", UNREACHABLE);

    let show = sdm_in(&dir, UNREACHABLE, &["scope", "show"]);
    let change = sdm_in(&dir, UNREACHABLE, &["scope"]);
    let clear = sdm_in(&dir, UNREACHABLE, &["scope", "clear", "--yes"]);

    let shown = String::from_utf8_lossy(&show.stdout);
    assert_eq!(show.status.code(), Some(0));
    assert!(shown.contains(r#""id":7"#), "{shown}");
    assert!(shown.contains(".sdm-scope.json"));
    assert_eq!(change.status.code(), Some(2));
    assert_eq!(clear.status.code(), Some(2));
    assert!(dir.join(".sdm-scope.json").exists());
}

#[test]
fn sent_requests_are_recorded_in_the_history_directory() {
    let (base_url, _request) = serve(vec![
        (200, r#"{"id":7}"#),
        (200, r#"{"id":7}"#),
        (404, r#"{"detail":"x"}"#),
    ]);
    let dir = scoped_dir("history", &base_url);

    sdm_in(
        &dir.join("nested"),
        &base_url,
        &["projects", "patch", "--id", "7", "--field", "name=x"],
    );
    // Not recorded: a read, two refusals and a preview.
    sdm_in(&dir, &base_url, &["projects", "get", "--id", "7"]);
    sdm_in(&dir, &base_url, &["projects", "patch", "--id", "8"]);
    sdm_in(
        &dir,
        &base_url,
        &["projects", "delete", "--id", "7", "--yes"],
    );
    sdm_in(
        &dir,
        &base_url,
        &["projects", "patch", "--id", "7", "--dry-run"],
    );
    sdm_in(
        &dir,
        &base_url,
        &["projects", "rules", "post", "--project-pk", "7"],
    );

    let files: Vec<PathBuf> = std::fs::read_dir(dir.join(".sdmhistory"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let history = std::fs::read_to_string(&files[0]).unwrap();
    let lines: Vec<&str> = history.lines().collect();
    assert_eq!(lines.len(), 2, "{history}");
    assert!(lines[0].contains(r#""command":["projects","patch","--id","7","--field","name=x"]"#));
    assert!(lines[0].contains(r#""method":"PATCH""#));
    assert!(lines[0].contains(r#"/api/v1/projects/7/","body":{"name":"x"},"status":200"#));
    assert!(lines[1].contains(r#""method":"POST""#) && lines[1].contains(r#""status":404"#));
    assert!(!history.contains("secret-key"));
}

#[test]
fn history_can_be_moved_turned_off_and_include_reads() {
    let (base_url, _request) = serve(vec![(200, "{}"), (200, "{}")]);
    let dir = scoped_dir("history-off", &base_url);
    let file = dir.join(".sdm-scope.json");
    let scope = std::fs::read_to_string(&file).unwrap();
    let with_history = |value: &str| scope.replacen("{", &format!(r#"{{"history":{value},"#), 1);

    std::fs::write(&file, with_history(r#""log/sdm","history_reads":true"#)).unwrap();
    sdm_in(&dir, &base_url, &["projects", "get", "--id", "7"]);
    std::fs::write(&file, with_history("null")).unwrap();
    sdm_in(&dir, &base_url, &["projects", "patch", "--id", "7"]);

    assert_eq!(std::fs::read_dir(dir.join("log/sdm")).unwrap().count(), 1);
    assert!(!dir.join(".sdmhistory").exists());
}
