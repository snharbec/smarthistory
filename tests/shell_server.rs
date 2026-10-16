//! End-to-end test of `smarthistory shell-server`: start a real server
//! against an isolated `HOME` + socket, drive it over the wire protocol
//! (see `src/shell_server.rs`), and check it answers exactly like a
//! direct exec of the binary.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_smarthistory");

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Kept short: `sun_path` is only ~104 bytes on macOS.
        let root = PathBuf::from("/tmp").join(format!("sh-it-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let sock_dir = root.join("s");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&sock_dir).unwrap();
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Fixture { socket: sock_dir.join("shell.sock"), root, home }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("HOME", &self.home)
            .env("SMARTHISTORY_SERVER_SOCKET", &self.socket)
            .env_remove("SMART_HISTORY_SESSION");
        cmd
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.home.display().to_string()),
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("SMART_HISTORY_SESSION".into(), "it-session".into()),
        ]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn request(verb: &str, cwd: &Path, env: &[(String, String)], args: &[&str]) -> Vec<u8> {
    let mut fields: Vec<String> = vec!["SH1".into(), verb.into()];
    if verb == "RUN" {
        fields.push(format!("C{}", cwd.display()));
        fields.extend(env.iter().map(|(k, v)| format!("E{k}={v}")));
        fields.extend(args.iter().map(|a| format!("A{a}")));
    }
    let mut out = Vec::new();
    for f in fields {
        out.extend_from_slice(f.as_bytes());
        out.push(0);
    }
    out.push(0);
    out
}

fn send(socket: &Path, req: &[u8]) -> (i32, String, String) {
    let mut s = UnixStream::connect(socket).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let nl = buf.iter().position(|b| *b == b'\n').expect("header");
    let header = String::from_utf8(buf[..nl].to_vec()).unwrap();
    let parts: Vec<&str> = header.split(' ').collect();
    assert_eq!(parts[0], "SH1");
    let code: i32 = parts[1].parse().unwrap();
    let out_len: usize = parts[2].parse().unwrap();
    let body = &buf[nl + 1..];
    (
        code,
        String::from_utf8_lossy(&body[..out_len]).into_owned(),
        String::from_utf8_lossy(&body[out_len..]).into_owned(),
    )
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

#[test]
fn shell_server_answers_like_a_direct_exec() {
    let fx = Fixture::new();
    let mut server = fx.command().arg("shell-server").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || UnixStream::connect(&fx.socket).is_ok()),
        "server never started listening"
    );

    // PING answers with the binary's identity (version first).
    let (code, identity, _) = send(&fx.socket, &request("PING", Path::new("/"), &[], &[]));
    assert_eq!(code, 0);
    assert!(identity.starts_with(env!("CARGO_PKG_VERSION")), "{identity}");

    // A second server against a live socket leaves the first alone.
    let second = fx.command().arg("shell-server").status().unwrap();
    assert!(second.success());

    // `add` through the server, with an argument containing a newline
    // and non-ASCII text, then `search` both ways.
    let cwd = fx.root.clone();
    let env = fx.env();
    let (code, _, err) = send(
        &fx.socket,
        &request("RUN", &cwd, &env, &["add", "echo 'a\nb' ünï", "--exit-code", "3"]),
    );
    assert_eq!(code, 0, "{err}");
    let search_args = ["search", "ünï", "--fields", "directory,exit_code,command", "--ansi=off"];
    let (code, via_server, _) = send(&fx.socket, &request("RUN", &cwd, &env, &search_args));
    assert_eq!(code, 0);
    let direct = fx
        .command()
        .current_dir(&cwd)
        .env("SMART_HISTORY_SESSION", "it-session")
        .args(search_args)
        .output()
        .unwrap();
    assert_eq!(via_server, String::from_utf8_lossy(&direct.stdout));
    // `search` prints an embedded newline as a literal `\n`.
    assert!(via_server.contains("echo 'a\\nb' ünï"), "{via_server}");
    assert!(via_server.contains(&cwd.canonicalize().unwrap().display().to_string()), "{via_server}");

    // Errors keep their exit code and stderr.
    let (code, _, err) = send(&fx.socket, &request("RUN", &cwd, &env, &["config", "get", "no.such.key"]));
    assert_eq!(code, 1);
    assert!(err.contains("no.such.key"), "{err}");

    // Interactive commands are refused, not run.
    let (code, _, err) = send(&fx.socket, &request("RUN", &cwd, &env, &["tui"]));
    assert_eq!(code, 2);
    assert!(err.contains("not served"), "{err}");

    // QUIT removes the socket and ends the server.
    let (code, _, _) = send(&fx.socket, &request("QUIT", Path::new("/"), &[], &[]));
    assert_eq!(code, 0);
    assert!(wait_for(Duration::from_secs(5), || matches!(server.try_wait(), Ok(Some(_)))));
    assert!(!fx.socket.exists());
}
