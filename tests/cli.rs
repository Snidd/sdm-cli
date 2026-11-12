//! End-to-end tests: the real binary against a one-shot local HTTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

/// Serves exactly one request with the given status and JSON body, and hands
/// back the raw request (head and body) that was received.
fn serve_once(status: u16, body: &'static str) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = channel();
    thread::spawn(move || {
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
    });
    (base_url, receiver)
}

fn sdm(base_url: &str, args: &[&str]) -> Output {
    let home = std::env::temp_dir().join(format!("sdm-cli-test-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_sdm"))
        .args(args)
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
