//! The Celesto runner against a hand-rolled HTTP/1.1 mock of the Computers API.
#![cfg(feature = "celesto")]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitbots_actions::{
    CelestoConfig, CelestoRunner, Job, JobContext, RunStatus, Runner, Source, Step,
};
use gitbots_core::id::{RunId, Ulid};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// (method, path, lowercased head, body)
type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;
type Route = fn(&str, &str) -> (u16, String);

async fn serve(route: Route) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Seen::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed mid-request");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse().unwrap());
            while buf.len() < head_end + len {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned();
            let mut first = head.split_whitespace();
            let (method, path) =
                (first.next().unwrap().to_ascii_uppercase(), first.next().unwrap().to_owned());
            let (code, resp) = route(&method, &path);
            log.lock().unwrap().push((method, path, head.clone(), body));
            let msg = format!(
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                resp.len()
            );
            sock.write_all(msg.as_bytes()).await.unwrap();
        }
    });
    (url, seen)
}

fn api(method: &str, path: &str) -> (u16, String) {
    let body = match (method, path) {
        ("POST", "/v1/computers") => r#"{"id":"cmp_1","status":"creating"}"#,
        ("GET", "/v1/computers/cmp_1") => r#"{"id":"cmp_1","status":"running"}"#,
        ("POST", "/v1/computers/cmp_1/exec") => {
            r#"{"exit_code":0,"stdout":"ok\n","stderr":"via https://x-access-token:tok-123@github.com\n","duration_ms":5,"timed_out":false,"command_id":"c"}"#
        }
        ("DELETE", "/v1/computers/cmp_1") => "{}",
        _ => return (404, "{}".into()),
    };
    (200, body.into())
}

fn exec_fails(method: &str, path: &str) -> (u16, String) {
    if path.ends_with("/exec") { (500, r#"{"error":"boom"}"#.into()) } else { api(method, path) }
}

async fn run(route: Route) -> (gitbots_actions::JobOutcome, String, Seen) {
    let (url, seen) = serve(route).await;
    let mut config = CelestoConfig::new("test-key");
    config.api_url = url;
    config.git_token = Some("tok-123".into());
    config.poll_interval = Duration::from_millis(10);
    let runner = CelestoRunner::new(config).unwrap();
    let step = Step {
        name: Some("test".into()),
        run: "cargo test".into(),
        env: BTreeMap::new(),
        workdir: None,
        continue_on_error: false,
    };
    let job = Job {
        runs_on: "celesto".into(),
        needs: vec![],
        env: BTreeMap::new(),
        timeout_secs: Some(60),
        steps: vec![step],
    };
    let source =
        Source { clone_url: "https://github.com/acme/app.git".into(), commit: "abc123".into() };
    let run = RunId::from_ulid(Ulid::from_parts(1, 1));
    let env = [("CI".to_owned(), "true".to_owned())].into();
    let ctx = JobContext {
        run: &run,
        workflow: "w",
        name: "j",
        job: &job,
        workdir: Path::new("."),
        env,
        source: Some(&source),
    };
    let out = runner.run_job(ctx).await.unwrap();
    let log = String::from_utf8_lossy(&out.log).into_owned();
    (out, log, seen)
}

#[tokio::test]
async fn runs_a_job_and_always_deletes_the_computer() {
    let (out, log, seen) = run(api).await;
    assert_eq!(out.status, RunStatus::Success, "{log}");
    let seen = seen.lock().unwrap();
    let calls: Vec<String> = seen.iter().map(|(m, p, _, _)| format!("{m} {p}")).collect();
    assert_eq!(
        calls,
        [
            "POST /v1/computers",
            "GET /v1/computers/cmp_1",
            "POST /v1/computers/cmp_1/exec",
            "POST /v1/computers/cmp_1/exec",
            "DELETE /v1/computers/cmp_1"
        ]
    );
    assert!(seen.iter().all(|(_, _, head, _)| head.contains("authorization: bearer test-key")));
    let create: serde_json::Value = serde_json::from_str(&seen[0].3).unwrap();
    assert_eq!(create["disk_size_mb"], 10240);
    assert_eq!(create["network_policy"]["mode"], "open");
    let clone: serde_json::Value = serde_json::from_str(&seen[2].3).unwrap();
    assert!(
        clone["command"]
            .as_str()
            .unwrap()
            .contains("https://x-access-token:tok-123@github.com/acme/app.git")
    );
    let step: serde_json::Value = serde_json::from_str(&seen[3].3).unwrap();
    assert_eq!(step["command"], "cd '/work/repo' && export CI='true' && sh -c 'cargo test'");
    assert!((55..=60).contains(&step["timeout"].as_u64().unwrap()));
    assert!(!log.contains("tok-123"), "{log}");
    assert!(log.contains("$ cargo test\nok\n") && log.contains("deleted computer cmp_1"), "{log}");
}

#[tokio::test]
async fn deletes_the_computer_when_exec_errors() {
    let (out, log, seen) = run(exec_fails).await;
    assert_eq!(out.status, RunStatus::Failure, "{log}");
    assert_eq!(out.failed_step.as_deref(), Some("checkout"));
    assert!(log.contains("HTTP 500") && log.contains("deleted computer cmp_1"), "{log}");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.last().map(|(m, p, _, _)| format!("{m} {p}")).as_deref(),
        Some("DELETE /v1/computers/cmp_1")
    );
}
