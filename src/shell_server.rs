//! `smarthistory shell-server`: a long-running local process that runs
//! the zsh integration's hot-path CLI calls (`search` on every
//! keystroke for the live dropdown, `add` on every prompt, `config get`
//! at shell startup, ...) without exec'ing the `smarthistory` binary
//! each time.
//!
//! Why: exec'ing a 17 MB binary costs a few milliseconds from a local
//! disk, but far more when the binary lives on a network filesystem
//! (SMB/NFS) — every exec revalidates the file and pages it in over
//! the network, which shows up as visible keystroke echo lag. The
//! server is started once (by `smarthistory init zsh`, see
//! [`ensure_running`]) and listens on a Unix socket on local disk; the
//! zsh snippet talks to it via the `zsh/net/socket` builtin, so the
//! hot path never execs anything.
//!
//! Design: the listening process is single-threaded and does nothing
//! but `accept()`. Each connection is handled by a forked child
//! (`handle_connection`), which in turn forks a worker that runs the
//! request through the exact same CLI dispatch as a normal exec
//! ([`crate::run_cli`]) with the client's cwd and environment, stdout
//! and stderr captured into pipes. Forking (rather than calling the
//! handlers in-process) keeps exact CLI semantics — every `println!`,
//! `std::process::exit`, and per-call `Config::load()`/DB open behaves
//! as it does for an exec — while the forked children share the
//! server's already-resident pages, so nothing is re-read from the
//! (possibly remote) binary. The listener never opens the DB or loads
//! config, so no SQLite handle is ever carried across `fork()`.
//!
//! Wire protocol (version tag [`PROTOCOL`]):
//!
//! - Request: NUL-terminated fields, ended by an empty field (so the
//!   request ends in `\0\0`): `SH1`, a verb (`RUN`/`PING`/`QUIT`), and
//!   for `RUN`: `C<cwd>`, any number of `E<KEY>=<VALUE>`, then one
//!   `A<arg>` per argument. The one-letter prefixes keep every field
//!   non-empty, so the first `\0\0` unambiguously ends the request.
//! - Response: `SH1 <exit> <stdout-len> <stderr-len>\n`, followed by
//!   the stdout bytes, then the stderr bytes.
//!
//! See `docs/shell-server.md`.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Protocol version tag — the first request field and the first
/// response-header token.
pub(crate) const PROTOCOL: &str = "SH1";

/// Requests larger than this are rejected (an env + argv for a single
/// CLI call is a few KB; this only guards against a runaway client).
const MAX_REQUEST: usize = 1 << 20;

/// The only subcommands the server will run. Everything the zsh
/// snippet calls non-interactively; anything needing a tty (`tui`,
/// `ask`, `create-note`) or running forever (`serve`, `daemon`) must
/// keep exec'ing the binary directly.
pub(crate) const ALLOWED_COMMANDS: &[&str] = &[
    "search",
    "next",
    "expand",
    "add",
    "capture-tmux",
    "capture-herdr",
    "config",
    "project",
];

/// How often the idle listener re-checks that its socket file is still
/// its own, and that its background monitor is still running (see
/// `run_server`).
const SOCKET_CHECK_INTERVAL_MS: i32 = 10_000;

/// The monitor is restarted at most this often, so one that exits
/// immediately (e.g. on a broken config) can't spin.
const MONITOR_RESPAWN_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    Ping,
    Quit,
    Run {
        cwd: Vec<u8>,
        env: Vec<(Vec<u8>, Vec<u8>)>,
        args: Vec<Vec<u8>>,
    },
}

/// Serialize a request (see the module docs for the format).
pub(crate) fn encode_request(req: &Request) -> Vec<u8> {
    let mut fields: Vec<Vec<u8>> = vec![PROTOCOL.as_bytes().to_vec()];
    match req {
        Request::Ping => fields.push(b"PING".to_vec()),
        Request::Quit => fields.push(b"QUIT".to_vec()),
        Request::Run { cwd, env, args } => {
            fields.push(b"RUN".to_vec());
            fields.push([b"C".as_slice(), cwd].concat());
            for (k, v) in env {
                fields.push([b"E".as_slice(), k, b"=", v].concat());
            }
            for a in args {
                fields.push([b"A".as_slice(), a].concat());
            }
        }
    }
    let mut out = Vec::new();
    for f in fields {
        out.extend_from_slice(&f);
        out.push(0);
    }
    out.push(0);
    out
}

/// Parse a complete request (everything up to and including the
/// terminating empty field).
pub(crate) fn decode_request(buf: &[u8]) -> Result<Request> {
    let end = find_terminator(buf).context("request is not terminated")?;
    let fields: Vec<&[u8]> = buf[..end].split(|b| *b == 0).collect();
    if fields.first().copied() != Some(PROTOCOL.as_bytes()) {
        bail!("unsupported protocol");
    }
    match fields.get(1).copied() {
        Some(b"PING") => Ok(Request::Ping),
        Some(b"QUIT") => Ok(Request::Quit),
        Some(b"RUN") => {
            let mut cwd = Vec::new();
            let mut env = Vec::new();
            let mut args = Vec::new();
            for f in &fields[2..] {
                let (tag, rest) = f.split_first().context("empty field")?;
                match tag {
                    b'C' => cwd = rest.to_vec(),
                    b'E' => {
                        let eq = rest.iter().position(|b| *b == b'=').context("env field without '='")?;
                        env.push((rest[..eq].to_vec(), rest[eq + 1..].to_vec()));
                    }
                    b'A' => args.push(rest.to_vec()),
                    _ => bail!("unknown field tag"),
                }
            }
            Ok(Request::Run { cwd, env, args })
        }
        _ => bail!("unknown verb"),
    }
}

/// Index of the empty field ending the request, i.e. the position of
/// the first `\0` that directly follows another `\0`, exclusive of
/// that trailing pair (so `buf[..idx]` is the NUL-joined field list).
fn find_terminator(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == [0, 0])
}

pub(crate) fn encode_response(code: i32, out: &[u8], err: &[u8]) -> Vec<u8> {
    let mut resp = format!("{PROTOCOL} {code} {} {}\n", out.len(), err.len()).into_bytes();
    resp.extend_from_slice(out);
    resp.extend_from_slice(err);
    resp
}

/// Split a response into `(exit code, stdout, stderr)`.
pub(crate) fn decode_response(buf: &[u8]) -> Option<(i32, &[u8], &[u8])> {
    let nl = buf.iter().position(|b| *b == b'\n')?;
    let header = std::str::from_utf8(&buf[..nl]).ok()?;
    let mut parts = header.split(' ');
    if parts.next()? != PROTOCOL {
        return None;
    }
    let code: i32 = parts.next()?.parse().ok()?;
    let out_len: usize = parts.next()?.parse().ok()?;
    let err_len: usize = parts.next()?.parse().ok()?;
    let body = &buf[nl + 1..];
    if body.len() != out_len + err_len {
        return None;
    }
    Some((code, &body[..out_len], &body[out_len..]))
}

fn uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Where the server listens: `SMARTHISTORY_SERVER_SOCKET` if set,
/// otherwise `<temp dir>/smarthistory-<uid>/shell.sock`. The temp dir
/// (`$TMPDIR` on macOS, `/tmp` on Linux) is local disk even when the
/// home directory or the binary is on a network share — Unix sockets
/// don't work on SMB mounts at all.
pub(crate) fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SMARTHISTORY_SERVER_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let dir = format!("smarthistory-{}", uid());
    let preferred = std::env::temp_dir().join(&dir).join("shell.sock");
    // `sun_path` holds only ~104 bytes (macOS) / 108 (Linux); an
    // unusually long `$TMPDIR` would make `bind` fail outright.
    if preferred.as_os_str().len() < MAX_SOCKET_PATH {
        preferred
    } else {
        PathBuf::from("/tmp").join(dir).join("shell.sock")
    }
}

/// Longest socket path `bind` accepts on every supported platform
/// (macOS's `sun_path` is 104 bytes including the trailing NUL).
const MAX_SOCKET_PATH: usize = 100;

/// Create the socket's parent directory (mode 0700) if needed and
/// refuse to use one that another user owns or that others can access
/// — anyone who can connect can run allowed CLI commands as this user.
pub(crate) fn prepare_socket_dir(socket: &Path) -> Result<()> {
    let dir = socket.parent().context("socket path has no parent directory")?;
    if !dir.exists() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    check_socket_dir(dir)
}

pub(crate) fn check_socket_dir(dir: &Path) -> Result<()> {
    let meta = std::fs::metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    if meta.uid() != uid() {
        bail!("{} is owned by another user", dir.display());
    }
    if meta.mode() & 0o077 != 0 {
        bail!("{} is accessible by other users (needs mode 0700)", dir.display());
    }
    Ok(())
}

/// Identifies the binary a server runs: version plus the executable's
/// mtime and size. `ensure_running` restarts a server whose identity
/// differs from the `init` caller's (i.e. the binary was upgraded).
pub(crate) fn identity() -> String {
    let meta = std::env::current_exe().and_then(std::fs::metadata);
    let (mtime, size) = meta.map(|m| (m.mtime(), m.size())).unwrap_or((0, 0));
    format!("{} {} {}", env!("CARGO_PKG_VERSION"), mtime, size)
}

/// Send one request and return the decoded `(code, stdout, stderr)`.
pub(crate) fn send(socket: &Path, req: &Request, timeout: Duration) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(&encode_request(req))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let (code, out, err) = decode_response(&buf).context("malformed response")?;
    Ok((code, out.to_vec(), err.to_vec()))
}

/// The running server's identity, or `None` if nothing answers.
pub(crate) fn ping(socket: &Path, timeout: Duration) -> Option<String> {
    let (code, out, _) = send(socket, &Request::Ping, timeout).ok()?;
    (code == 0).then(|| String::from_utf8_lossy(&out).into_owned())
}

/// Make sure a server for THIS binary is listening, starting one if
/// needed. Best effort: every failure is swallowed (the zsh client
/// falls back to exec'ing the binary when it can't connect), so
/// `smarthistory init zsh` never fails because of the server. Returns
/// the socket path the zsh snippet should use.
pub(crate) fn ensure_running() -> PathBuf {
    let socket = socket_path();
    let me = identity();
    match ping(&socket, Duration::from_millis(200)) {
        Some(id) if id == me => return socket,
        Some(_) => {
            // Stale server from an older binary: stop it, then start
            // ours below.
            let _ = send(&socket, &Request::Quit, Duration::from_millis(200));
            wait_until(Duration::from_millis(500), || !socket.exists());
        }
        None => {}
    }
    if spawn_detached().is_ok() {
        wait_until(Duration::from_secs(1), || UnixStream::connect(&socket).is_ok());
    }
    socket
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() && start.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Start `<this binary> shell-server` in its own session, detached
/// from the calling shell's terminal.
fn spawn_detached() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("shell-server")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and only affects the
    // freshly forked child.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

/// `smarthistory shell-server [--stop|--status]`.
pub(crate) fn cli(stop: bool, status: bool) -> Result<()> {
    let socket = socket_path();
    if status {
        match ping(&socket, Duration::from_millis(500)) {
            Some(id) => {
                println!("running at {} ({id})", socket.display());
                return Ok(());
            }
            None => {
                println!("not running ({})", socket.display());
                std::process::exit(1);
            }
        }
    }
    if stop {
        if send(&socket, &Request::Quit, Duration::from_millis(500)).is_ok() {
            println!("stopped");
        } else {
            println!("not running ({})", socket.display());
        }
        return Ok(());
    }
    run_server(&socket)
}

/// The listener loop. Single-threaded on purpose: every connection is
/// handled in a forked child, and `fork()` is only safe to use like
/// this from a single-threaded process.
fn run_server(socket: &Path) -> Result<()> {
    prepare_socket_dir(socket)?;
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            // Another server already owns the socket.
            return Ok(());
        }
        std::fs::remove_file(socket).ok();
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let our_ino = std::fs::metadata(socket)?.ino();
    let identity = identity();
    // SAFETY: plain signal-disposition changes, no handlers installed.
    // SIG_IGN on SIGCHLD makes the kernel reap the per-connection
    // children, so the listener never has to `waitpid`.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    // The background multiplexer monitor (see `crate::mux_monitor`).
    // Forked here, while still single-threaded, and re-forked if it
    // exits (config change, crash) — at most once per
    // MONITOR_RESPAWN_INTERVAL.
    let state_dir = socket.parent().map(Path::to_path_buf);
    let spawn_monitor = || {
        state_dir
            .clone()
            .and_then(|dir| crate::mux_monitor::spawn(dir, listener.as_raw_fd()))
    };
    let mut monitor = spawn_monitor();
    let mut monitor_started = Instant::now();
    loop {
        // SAFETY: `kill(pid, 0)` only probes for existence. With
        // SIGCHLD ignored, an exited monitor leaves no zombie, so the
        // probe fails as soon as it is gone.
        let monitor_alive = monitor.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
        if !monitor_alive && monitor_started.elapsed() >= MONITOR_RESPAWN_INTERVAL {
            monitor = spawn_monitor();
            monitor_started = Instant::now();
        }
        let mut pfd = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid, initialized pollfd array of length 1.
        let ready = unsafe { libc::poll(&mut pfd, 1, SOCKET_CHECK_INTERVAL_MS) };
        if ready == 0 {
            // Idle: exit if our socket file was removed or replaced
            // (`--stop`, a newer server, a cleaned temp dir) — no
            // client could ever reach this process again.
            match std::fs::metadata(socket) {
                Ok(m) if m.ino() == our_ino => continue,
                _ => return Ok(()),
            }
        }
        if ready < 0 {
            continue;
        }
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        // SAFETY: the listener is single-threaded (see above).
        match unsafe { libc::fork() } {
            0 => {
                drop(listener);
                handle_connection(stream, socket, &identity);
                // SAFETY: terminate the handler without running the
                // parent's exit-time cleanup a second time.
                unsafe { libc::_exit(0) }
            }
            _ => drop(stream),
        }
    }
}

/// Runs in the per-connection child: read one request, answer it.
fn handle_connection(mut stream: UnixStream, socket: &Path, identity: &str) {
    // SAFETY: restore default SIGCHLD so `waitpid` on the worker works.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let response = match read_request(&mut stream).and_then(|buf| decode_request(&buf)) {
        Err(e) => encode_response(2, b"", format!("smarthistory shell-server: {e}\n").as_bytes()),
        Ok(Request::Ping) => encode_response(0, identity.as_bytes(), b""),
        Ok(Request::Quit) => {
            let _ = std::fs::remove_file(socket);
            // SAFETY: signalling our own parent, the listener.
            unsafe {
                libc::kill(libc::getppid(), libc::SIGTERM);
            }
            encode_response(0, b"", b"")
        }
        Ok(Request::Run { cwd, env, args }) => run_request(cwd, env, args),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if find_terminator(&buf).is_some() {
            return Ok(buf);
        }
        if buf.len() > MAX_REQUEST {
            bail!("request too large");
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("connection closed before the request was complete");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// True if `args` names a subcommand the server may run.
pub(crate) fn is_allowed(args: &[Vec<u8>]) -> bool {
    args.first()
        .is_some_and(|a| ALLOWED_COMMANDS.iter().any(|c| c.as_bytes() == a.as_slice()))
}

/// Fork a worker that runs the CLI with the request's cwd, env and
/// args; collect its stdout/stderr and exit status.
fn run_request(cwd: Vec<u8>, env: Vec<(Vec<u8>, Vec<u8>)>, args: Vec<Vec<u8>>) -> Vec<u8> {
    if !is_allowed(&args) {
        let name = args.first().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
        return encode_response(
            2,
            b"",
            format!("smarthistory shell-server: `{name}` is not served; run the binary directly\n").as_bytes(),
        );
    }
    let (out_r, out_w) = match pipe() {
        Ok(p) => p,
        Err(e) => return encode_response(1, b"", format!("pipe: {e}\n").as_bytes()),
    };
    let (err_r, err_w) = match pipe() {
        Ok(p) => p,
        Err(e) => return encode_response(1, b"", format!("pipe: {e}\n").as_bytes()),
    };
    // SAFETY: the handler process is single-threaded at this point.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: fd juggling in the freshly forked worker.
        unsafe {
            libc::close(out_r);
            libc::close(err_r);
            libc::dup2(out_w, 1);
            libc::dup2(err_w, 2);
            libc::close(out_w);
            libc::close(err_w);
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
            if devnull >= 0 {
                libc::dup2(devnull, 0);
                libc::close(devnull);
            }
        }
        let code = run_worker(cwd, env, args);
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // SAFETY: end the worker immediately with the CLI's exit code.
        unsafe { libc::_exit(code) }
    }
    // SAFETY: closing our copies of the write ends so reads see EOF.
    unsafe {
        libc::close(out_w);
        libc::close(err_w);
    }
    if pid < 0 {
        // SAFETY: closing the read ends we still own.
        unsafe {
            libc::close(out_r);
            libc::close(err_r);
        }
        return encode_response(1, b"", b"smarthistory shell-server: fork failed\n");
    }
    // SAFETY: we own these fds; `File` closes them on drop.
    let mut out_file = unsafe { std::fs::File::from_raw_fd(out_r) };
    let mut err_file = unsafe { std::fs::File::from_raw_fd(err_r) };
    let err_reader = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = err_file.read_to_end(&mut err);
        err
    });
    let mut out = Vec::new();
    let _ = out_file.read_to_end(&mut out);
    let err = err_reader.join().unwrap_or_default();
    let mut status = 0;
    // SAFETY: waiting for our own child.
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    };
    encode_response(code, &out, &err)
}

fn pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid 2-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// Runs in the worker: adopt the client's cwd and environment, then
/// dispatch exactly like a normal `smarthistory <args>` exec.
fn run_worker(cwd: Vec<u8>, env: Vec<(Vec<u8>, Vec<u8>)>, args: Vec<Vec<u8>>) -> i32 {
    if !cwd.is_empty() {
        let _ = std::env::set_current_dir(std::ffi::OsStr::from_bytes(&cwd));
    }
    let current: Vec<OsString> = std::env::vars_os().map(|(k, _)| k).collect();
    // SAFETY: the worker is single-threaded (freshly forked from the
    // single-threaded handler before it spawned its reader thread).
    unsafe {
        for k in current {
            std::env::remove_var(k);
        }
        for (k, v) in env {
            if k.is_empty() || k.contains(&0) || k.contains(&b'=') {
                continue;
            }
            std::env::set_var(OsString::from_vec(k), OsString::from_vec(v));
        }
    }
    let argv = std::iter::once(OsString::from("smarthistory")).chain(args.into_iter().map(OsString::from_vec));
    crate::run_cli(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request::Run {
            cwd: "/tmp/dir with space".into(),
            env: vec![(b"HOME".to_vec(), b"/h".to_vec()), (b"EMPTY".to_vec(), Vec::new())],
            args: vec![
                b"search".to_vec(),
                Vec::new(),
                "multi\nline ünïcode".as_bytes().to_vec(),
                b"a=b".to_vec(),
            ],
        };
        let bytes = encode_request(&req);
        assert!(bytes.ends_with(&[0, 0]));
        assert_eq!(decode_request(&bytes).unwrap(), req);
        assert_eq!(decode_request(&encode_request(&Request::Ping)).unwrap(), Request::Ping);
        assert_eq!(decode_request(&encode_request(&Request::Quit)).unwrap(), Request::Quit);
    }

    #[test]
    fn request_rejects_bad_input() {
        assert!(decode_request(b"SH1\0RUN\0").is_err(), "unterminated");
        assert!(decode_request(b"SH9\0PING\0\0").is_err(), "wrong protocol");
        assert!(decode_request(b"SH1\0NOPE\0\0").is_err(), "unknown verb");
        assert!(decode_request(b"SH1\0RUN\0Xfoo\0\0").is_err(), "unknown tag");
    }

    #[test]
    fn response_round_trips() {
        let resp = encode_response(3, b"out\nput", b"err");
        assert_eq!(decode_response(&resp), Some((3, b"out\nput".as_slice(), b"err".as_slice())));
        assert_eq!(decode_response(b"SH1 0 5 0\nabc"), None, "truncated body");
        assert_eq!(decode_response(b"XX 0 0 0\n"), None);
    }

    #[test]
    fn allowlist_blocks_interactive_commands() {
        assert!(is_allowed(&[b"search".to_vec()]));
        assert!(is_allowed(&[b"add".to_vec(), b"ls".to_vec()]));
        assert!(!is_allowed(&[b"tui".to_vec()]));
        assert!(!is_allowed(&[b"shell-server".to_vec()]));
        assert!(!is_allowed(&[]));
    }

    #[test]
    fn socket_dir_must_be_private() {
        let dir = std::env::temp_dir().join(format!("smarthistory-sockdir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let socket = dir.join("shell.sock");
        prepare_socket_dir(&socket).expect("fresh dir is created private");
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_socket_dir(&dir).is_err(), "group/world access must be refused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
