//! Remote dependencies: fetched by an explicit step, pinned by hash, and never
//! reached for at run time.
//!
//! The server here is a few lines of TcpListener rather than a mock: the thing
//! under test is what happens over a real socket, including what happens when
//! the bytes at a URL change after they were pinned.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Serves the given path → body map until dropped. `body` is a Mutex so a test
/// can change what a URL serves — which is the whole point of pinning.
struct Server {
    port: u16,
    files: Arc<Mutex<Vec<(String, String)>>>,
    stop: mpsc::Sender<()>,
}

impl Server {
    fn start(files: Vec<(String, String)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let files = Arc::new(Mutex::new(files));
        let (stop, stopped) = mpsc::channel::<()>();
        let served = files.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.try_recv().is_ok() {
                    return;
                }
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 2048];
                let Ok(n) = s.read(&mut buf) else { continue };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = served
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| b.clone());
                let resp = match body {
                    Some(b) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
                        b.len(),
                        b
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string(),
                };
                let _ = s.write_all(resp.as_bytes());
            }
        });
        Server { port, files, stop }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// Change what a URL serves, as a compromised host would.
    fn replace(&self, path: &str, body: &str) {
        let mut files = self.files.lock().unwrap();
        for (p, b) in files.iter_mut() {
            if p == path {
                *b = body.to_string();
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        // Unblock the accept loop.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

const INDEX: &str = r#"import { square } from "./util.mersey";

export function sumOfSquares(xs: int32[]): int32 {
    let total: int32 = 0;
    for (const x of xs) { total += square(x); }
    return total;
}
"#;

const UTIL: &str = "export function square(n: int32): int32 { return n * n; }\n";

fn mersey(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mersey"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run mersey");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// A project that imports a package by URL.
fn project(dir: &std::path::Path, server: &Server) {
    std::fs::create_dir_all(dir).unwrap();
    let app = format!(
        "import {{ console }} from \"std:console\";\nimport {{ sumOfSquares }} from \"{}\";\n\nconsole.log(sumOfSquares([1, 2, 3, 4]));\n",
        server.url("/mathkit/index.mersey")
    );
    std::fs::write(dir.join("app.mersey"), app).unwrap();
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mersey-fetch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fetch_then_run_offline() {
    let server = Server::start(vec![
        ("/mathkit/index.mersey".into(), INDEX.into()),
        ("/mathkit/util.mersey".into(), UTIL.into()),
    ]);
    let dir = tmpdir("offline");
    project(&dir, &server);

    // Running code has no authority to reach the network (§5.4): an unfetched
    // dependency is an error, not a download.
    let (ok, out) = mersey(&dir, &["run", "app.mersey"]);
    assert!(!ok, "should refuse to run: {out}");
    assert!(out.contains("not in the local cache"), "{out}");

    // Fetching is the one step that talks to the network, and it follows the
    // package's own imports — `./util.mersey` resolves against the package URL,
    // not the project's disk.
    let (ok, out) = mersey(&dir, &["fetch", "app.mersey"]);
    assert!(ok, "fetch failed: {out}");
    assert!(out.contains("fetched 2 module"), "{out}");

    let (ok, out) = mersey(&dir, &["run", "app.mersey"]);
    assert!(ok, "run failed: {out}");
    assert_eq!(out.trim(), "30", "1+4+9+16");
}

#[test]
fn a_package_that_changes_under_its_pin_is_refused() {
    let server = Server::start(vec![
        ("/mathkit/index.mersey".into(), INDEX.into()),
        ("/mathkit/util.mersey".into(), UTIL.into()),
    ]);
    let dir = tmpdir("pinned");
    project(&dir, &server);

    assert!(mersey(&dir, &["fetch", "app.mersey"]).0);
    let (ok, out) = mersey(&dir, &["lock", "app.mersey"]);
    assert!(ok, "lock failed: {out}");

    // The host serves something else now. A fresh clone (empty cache) must not
    // silently pick it up.
    server.replace(
        "/mathkit/util.mersey",
        "export function square(n: int32): int32 { return n * n + 1000; }\n",
    );
    std::fs::remove_dir_all(dir.join(".mersey")).unwrap();

    let (ok, out) = mersey(&dir, &["fetch", "app.mersey"]);
    assert!(!ok, "a changed package must be refused, not fetched: {out}");
    assert!(out.contains("does not match mersey.lock"), "{out}");

    // And the bytes it served never reached the disk.
    let (ok, out) = mersey(&dir, &["run", "app.mersey"]);
    assert!(!ok, "{out}");
    assert!(out.contains("not in the local cache"), "{out}");
}
