use std::io::{self, BufReader, BufWriter, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use agentcloud::backup::{spawn as spawn_backups, BackupConfig};
use agentcloud::db::Db;
use agentcloud::env::{load_env_file, read_config_from_os};
use agentcloud::http::{json_response, read_request_with, write_response, ReadError};
use agentcloud::json::{stringify, Value};
use agentcloud::server::{handle, AppState};
use agentcloud::time::now_iso;

/// Empty the trash of items deleted more than `TRASH_DAYS` ago, at boot and
/// every 6 hours.
fn spawn_trash_purge(db: Arc<std::sync::Mutex<Db>>) {
    std::thread::spawn(move || loop {
        let cutoff = agentcloud::time::days_ago_iso(agentcloud::sync::TRASH_DAYS);
        let result = match db.lock() {
            Ok(db) => db.purge_trash(&cutoff),
            Err(poisoned) => poisoned.into_inner().purge_trash(&cutoff),
        };
        match result {
            Ok(0) => {}
            Ok(n) => println!("{{\"t\":\"{}\",\"msg\":\"trash purged\",\"rows\":{n}}}", now_iso()),
            Err(e) => eprintln!("trash purge: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cmd) = args.first() {
        if matches!(cmd.as_str(), "seal" | "open" | "auth-token" | "seal-file" | "open-file") {
            match agentcloud::seal::run(cmd, &args[1..]) {
                Ok(out) => {
                    if !out.is_empty() {
                        println!("{out}");
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
    }
    if args.first().map(String::as_str) == Some("stats") {
        // `agentcloud stats --db PATH`: row counts of a (possibly encrypted)
        // database, for the restore test. `DB_KEY` from the environment.
        let path = match args.get(1..3) {
            Some([flag, path]) if flag == "--db" => path.clone(),
            _ => {
                eprintln!("usage: agentcloud stats --db PATH");
                std::process::exit(2);
            }
        };
        if !Path::new(&path).exists() {
            eprintln!("no such file: {path}");
            std::process::exit(1);
        }
        let key = std::env::var("DB_KEY").ok();
        let result = Db::open_with(&path, key.as_deref()).and_then(|db| db.stats());
        match result {
            Ok(counts) => {
                let pairs: Vec<(&str, Value)> =
                    counts.into_iter().map(|(k, n)| (k, Value::Number(n))).collect();
                println!("{}", stringify(&Value::object(&pairs)));
                return;
            }
            Err(e) => {
                eprintln!("stats: {e}");
                std::process::exit(1);
            }
        }
    }
    let env_path = Path::new(".env");
    if let Err(e) = load_env_file(env_path) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    // yagni: rustls for TLS_KEY/TLS_CERT; add if loopback HTTPS comes back
    let cfg = read_config_from_os();
    let db_key = cfg.db_key.as_str();
    let db_key = (!db_key.is_empty()).then_some(db_key);
    let db = Db::open_with(&cfg.db_path, db_key).unwrap_or_else(|e| {
        eprintln!("db: {e}");
        std::process::exit(1);
    });
    if !cfg.feed_token.is_empty() && !cfg.agent_key.is_empty() {
        match db.ensure_home_calendar(&cfg.feed_token, &cfg.agent_key, &cfg.cal_name) {
            Ok((_, changed)) if !changed.is_empty() => println!(
                "{}",
                stringify(&Value::object(&[
                    ("t", Value::String(now_iso())),
                    ("msg", Value::String("home".into())),
                    (
                        "changed",
                        Value::Array(changed.into_iter().map(|c| Value::String(c.into())).collect()),
                    ),
                ]))
            ),
            Ok(_) => {}
            Err(e) => {
                eprintln!("home calendar: {e}");
                std::process::exit(1);
            }
        }
    }
    // One-shot: drop the last plaintext copy of every write key. Off unless asked,
    // because it ends the option of rolling back to a build older than 0.8.0.
    if matches!(
        std::env::var("SCRUB_LEGACY_KEYS").as_deref(),
        Ok("1" | "true" | "yes")
    ) {
        match db.scrub_legacy_keys(Path::new(&cfg.db_path)) {
            Ok(n) => println!(
                "{}",
                stringify(&Value::object(&[
                    ("t", Value::String(now_iso())),
                    ("msg", Value::String("scrub".into())),
                    ("calendars", Value::Number(n as i64)),
                ]))
            ),
            Err(e) => {
                eprintln!("scrub: {e}");
                std::process::exit(1);
            }
        }
    }
    // Copies of the database on a timer; BACKUP_INTERVAL_HOURS=0 turns it off.
    spawn_backups(
        Path::new(&cfg.db_path).to_path_buf(),
        BackupConfig::from_env(&cfg.db_path, |k| std::env::var(k).ok()),
        db_key.map(str::to_string),
    );
    let public_base = std::env::var("PUBLIC_BASE").unwrap_or_default();
    let mut state = AppState::new(db, public_base);
    spawn_trash_purge(state.db.clone());
    // TRUSTED_PROXY_HOPS sets which X-Forwarded-For entry counts as the
    // client; the rest tune the caps. See `limits::Limits`.
    state.limits = state.limits.from_env(|k| std::env::var(k).ok());
    let binds = listen_hosts(&cfg.host);
    let mut listeners = Vec::new();
    for host in &binds {
        let addr = parse_bind(host, cfg.port).unwrap_or_else(|| {
            eprintln!("bad bind {host}:{}", cfg.port);
            std::process::exit(1);
        });
        let listener = TcpListener::bind(addr).unwrap_or_else(|e| {
            eprintln!("listen {addr}: {e}");
            std::process::exit(1);
        });
        listeners.push(listener);
    }
    println!(
        "{}",
        stringify(&Value::object(&[
            ("t", Value::String(now_iso())),
            ("msg", Value::String("listen".into())),
            ("host", Value::String(cfg.host.clone())),
            (
                "binds",
                Value::Array(binds.iter().cloned().map(Value::String).collect()),
            ),
            ("port", Value::Number(i64::from(cfg.port))),
            ("scheme", Value::String("http".into())),
        ]))
    );
    let mut joins = Vec::new();
    for listener in listeners {
        let state = state.clone();
        joins.push(thread::spawn(move || accept_loop(listener, state)));
    }
    for join in joins {
        let _ = join.join();
    }
}

/// Longest a client may go silent between reads.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Total time a client gets to deliver a whole request, headers and body.
///
/// Every response sends `connection: close` and one request is served per
/// connection, so no legitimate client holds a socket open waiting. `READ_TIMEOUT`
/// alone is per `read()`: a client sending one byte every nine seconds never
/// trips it and pins its thread. This budget does not reset.
const REQUEST_DEADLINE: Duration = Duration::from_secs(15);

/// A reader whose timeout shrinks toward a fixed deadline.
struct DeadlineReader {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left.min(READ_TIMEOUT)))?;
        self.stream.read(buf)
    }
}

/// Give up on a client that will not accept its response.
///
/// Generous compared to the read side: an ICS feed can be large and the client
/// on a slow mobile link.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Refuse new connections past this many in flight.
///
/// One thread per connection, so this bounds thread and fd use under a flood.
/// Roomy for a personal calendar sitting behind a proxy.
const MAX_CONNECTIONS: usize = 512;

/// Decrements the live-connection count when a worker finishes.
///
/// A guard rather than a bare decrement so the slot is released on panic too --
/// `read_request` and `handle` both run inside the worker.
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: TcpListener, state: AppState) {
    let live = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };

        // Reserve the slot before spawning, so a burst cannot overshoot.
        let taken = live.fetch_add(1, Ordering::SeqCst) + 1;
        if taken > MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::SeqCst);
            eprintln!("agentcloud: refusing connection, {MAX_CONNECTIONS} already in flight");
            drop(stream);
            continue;
        }

        let guard = ConnectionGuard(live.clone());
        let state = state.clone();
        thread::spawn(move || {
            let _guard = guard;
            serve(stream, &state);
        });
    }
}

fn serve(stream: TcpStream, state: &AppState) {
    if let Err(e) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
        eprintln!("agentcloud: set_write_timeout failed: {e}");
        return;
    }
    let peer = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();

    // Was `.expect("clone")`, which panicked the worker on fd exhaustion --
    // exactly the state a connection flood reaches.
    let Ok(read_half) = stream.try_clone() else {
        eprintln!("agentcloud: could not clone stream, dropping connection");
        return;
    };

    let mut reader = BufReader::new(DeadlineReader {
        stream: read_half,
        deadline: Instant::now() + REQUEST_DEADLINE,
    });
    let mut go_ahead = || {
        let _ = (&stream).write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    };
    let res = match read_request_with(&mut reader, &mut go_ahead) {
        Ok(mut req) => {
            req.peer = peer;
            // A panic in a handler answers 500 instead of dropping the socket
            // with no reply. The DB lock recovers from poisoning, so the next
            // request is fine.
            catch_unwind(AssertUnwindSafe(|| handle(state, &req))).unwrap_or_else(|_| {
                eprintln!("agentcloud: handler panicked: {} {}", req.method, req.path);
                json_response(500, r#"{"error":"internal error"}"#)
            })
        }
        Err(ReadError::Reject(status, msg)) => {
            eprintln!("agentcloud: rejecting request from {peer}: {status} {msg}");
            json_response(status, &format!(r#"{{"error":"{msg}"}}"#))
        }
        Err(ReadError::Closed(why)) => {
            // Nothing logged this before: a stalled connection produced no
            // output at all, so a flood looked like a silent, idle server.
            eprintln!("agentcloud: dropping connection from {peer}: {why}");
            return;
        }
    };
    let mut writer = BufWriter::new(stream);
    let _ = write_response(&mut writer, &res);
}

/// Loopback hostnames must listen on v4 and v6. `localhost` prefers ::1.
fn listen_hosts(host: &str) -> Vec<String> {
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        vec!["127.0.0.1".into(), "::1".into()]
    } else {
        vec![host.to_string()]
    }
}

fn parse_bind(host: &str, port: u16) -> Option<SocketAddr> {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}").parse().ok()
    } else {
        format!("{host}:{port}").parse().ok()
    }
}
