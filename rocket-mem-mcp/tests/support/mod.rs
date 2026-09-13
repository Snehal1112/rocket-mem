use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

fn rocket_mem_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ROCKET_MEM_BIN") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/rocket-mem")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Path to the shared self-signed TLS test fixture certificate (CN/SAN `localhost`/
/// `127.0.0.1`, the same one `crates/server/tests/tls.rs` uses) — the CA path a test passes to
/// `Pool::connect` to verify a spawned instance's TLS listener.
#[allow(dead_code)]
pub fn tls_fixture_cert_path() -> PathBuf {
    fixture("test-cert.pem")
}

/// Owns a spawned `rocket-mem` child process and guarantees it is killed — whether the test
/// finishes normally, returns early, or panics mid-assertion. Without this, a panicking test
/// (e.g. a failed `assert!`) would skip its own `child.kill()` call and leak the process.
pub struct RocketMemGuard(Option<Child>);

impl RocketMemGuard {
    /// Kills the child immediately and reaps it. Idempotent — safe to call explicitly (e.g. to
    /// simulate a lost connection mid-test) and again implicitly via `Drop` at scope end.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RocketMemGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Spawns the real compiled `rocket-mem` binary, applies `configure` to its `Command` before
/// spawning (env vars, `current_dir`, ...), then reads its startup banner on stdout until every
/// token in `wanted_tokens` (e.g. `"RESP"`, `"RESP+TLS"`) has had its bound address captured.
/// Matching the exact first whitespace-separated token (not a prefix) is what lets `"RESP"` and
/// `"RESP+TLS"` be told apart — see `crates/server/tests/kill_and_recover.rs`'s `spawn_server`,
/// which this generalizes to support more than one listener row per spawn.
fn spawn_and_parse(
    configure: impl FnOnce(&mut Command),
    wanted_tokens: &[&str],
) -> (RocketMemGuard, HashMap<String, String>) {
    let bin = rocket_mem_bin();
    let mut cmd = Command::new(&bin);
    cmd.stdout(Stdio::piped());
    configure(&mut cmd);
    let mut child = cmd.spawn().unwrap_or_else(|e| {
        panic!(
            "failed to spawn {bin:?}: {e} — build it first with `cargo build -p rocket-mem` \
             from the repo root, or set ROCKET_MEM_BIN"
        )
    });

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    let mut found: HashMap<String, String> = HashMap::new();
    for _ in 0..40 {
        if found.len() == wanted_tokens.len() {
            break;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().trim_matches(|c| c == '│' || c == ' ');
                let mut parts = trimmed.split_whitespace();
                if let Some(token) = parts.next() {
                    if wanted_tokens.contains(&token) {
                        if let Some(addr) = parts.next() {
                            found.insert(token.to_string(), addr.to_string());
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }
    for token in wanted_tokens {
        assert!(
            found.contains_key(*token),
            "rocket-mem never printed a {token} listener line on stdout"
        );
    }
    (RocketMemGuard(Some(child)), found)
}

/// Spawns the real compiled `rocket-mem` binary bound to an OS-assigned port, reading its
/// startup banner on stdout to discover which port it actually got. Returns a guard that kills
/// the process on drop (so it's cleaned up even on panic — see `RocketMemGuard`) and the bound
/// `"host:port"` string.
pub fn spawn_rocket_mem(aof_path: &std::path::Path) -> (RocketMemGuard, String) {
    let aof_path = aof_path.to_path_buf();
    let (guard, mut found) = spawn_and_parse(
        move |cmd| {
            cmd.env("ROCKET_MEM_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_AOF_PATH", &aof_path);
        },
        &["RESP"],
    );
    (guard, found.remove("RESP").unwrap())
}

/// Like `spawn_rocket_mem`, but also binds a TLS RESP listener using the shared self-signed
/// test fixture (`tls_fixture_cert_path()`, CN/SAN `localhost`/`127.0.0.1` — matches the
/// `127.0.0.1` address every spawned instance binds to). Returns the plaintext and TLS
/// `"host:port"` strings separately; both listeners are live simultaneously, matching how a
/// real deployment runs them side by side (see `rocket-mem.toml`'s own TLS section).
#[allow(dead_code)]
pub fn spawn_rocket_mem_with_tls(aof_path: &std::path::Path) -> (RocketMemGuard, String, String) {
    let aof_path = aof_path.to_path_buf();
    let cert = tls_fixture_cert_path();
    let key = fixture("test-key.pem");
    let (guard, mut found) = spawn_and_parse(
        move |cmd| {
            cmd.env("ROCKET_MEM_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_AOF_PATH", &aof_path)
                .env("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_TLS_CERT_PATH", &cert)
                .env("ROCKET_MEM_TLS_KEY_PATH", &key);
        },
        &["RESP", "RESP+TLS"],
    );
    let plain = found.remove("RESP").unwrap();
    let tls = found.remove("RESP+TLS").unwrap();
    (guard, plain, tls)
}

/// Like `spawn_rocket_mem`, but bootstraps one ACL user before starting. ACL bootstrap is
/// file-only (rocket-mem.toml's own comment: no flat `ROCKET_MEM_*` env var can express the
/// `[[acl.users]]` array of tables), so this writes a temporary `rocket-mem.toml` and spawns
/// with that directory as the child's cwd (rocket-mem auto-picks up `./rocket-mem.toml`).
/// Deliberately leaks the temp directory (`TempDir::into_path`) rather than threading its
/// lifetime through the return value: the file is read once at startup, before the banner
/// line this function waits for is ever printed, so nothing needs it to survive any longer —
/// same tradeoff every short-lived test process makes for its own `/tmp` scratch space.
#[allow(dead_code)]
pub fn spawn_rocket_mem_with_acl(
    aof_path: &std::path::Path,
    username: &str,
    password: &str,
) -> (RocketMemGuard, String) {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    std::fs::write(
        dir.join("rocket-mem.toml"),
        format!(
            "[[acl.users]]\nusername = \"{username}\"\npassword = \"{password}\"\nenabled = true\nrules = [\"allcommands\", \"allkeys\"]\n"
        ),
    )
    .expect("write rocket-mem.toml");

    let aof_path = aof_path.to_path_buf();
    let (guard, mut found) = spawn_and_parse(
        move |cmd| {
            cmd.current_dir(&dir)
                .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_AOF_PATH", &aof_path);
        },
        &["RESP"],
    );
    (guard, found.remove("RESP").unwrap())
}

/// Like `spawn_rocket_mem_with_acl`, but also binds a TLS RESP listener (see
/// `spawn_rocket_mem_with_tls`) — mirrors a real deployment that has both ACLs and TLS enabled
/// together (e.g. `rocket-mem.toml`'s own example: `[[acl.users]]` plus `tls_resp_addr`).
/// Returns the TLS `"host:port"` string only; the combined scenario this exists for always
/// connects over TLS.
#[allow(dead_code)]
pub fn spawn_rocket_mem_with_acl_and_tls(
    aof_path: &std::path::Path,
    username: &str,
    password: &str,
) -> (RocketMemGuard, String) {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    std::fs::write(
        dir.join("rocket-mem.toml"),
        format!(
            "[[acl.users]]\nusername = \"{username}\"\npassword = \"{password}\"\nenabled = true\nrules = [\"allcommands\", \"allkeys\"]\n"
        ),
    )
    .expect("write rocket-mem.toml");

    let aof_path = aof_path.to_path_buf();
    let cert = tls_fixture_cert_path();
    let key = fixture("test-key.pem");
    let (guard, mut found) = spawn_and_parse(
        move |cmd| {
            cmd.current_dir(&dir)
                .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_AOF_PATH", &aof_path)
                .env("ROCKET_MEM_TLS_RESP_ADDR", "127.0.0.1:0")
                .env("ROCKET_MEM_TLS_CERT_PATH", &cert)
                .env("ROCKET_MEM_TLS_KEY_PATH", &key);
        },
        &["RESP", "RESP+TLS"],
    );
    (guard, found.remove("RESP+TLS").unwrap())
}

/// Wires up an `RocketMemMcpServer` and an `rmcp` client on opposite ends of an in-process
/// duplex pipe, completes the MCP handshake on both sides, and returns the connected client.
/// Shared by every test file that needs to drive tools through a real MCP client rather than
/// calling Rust functions directly. Each `tests/*.rs` file compiles this `support` module into
/// its own separate test binary, and not every one of those binaries calls this particular
/// helper (e.g. `tests/pool.rs` only needs `spawn_rocket_mem`) — hence `allow(dead_code)` rather
/// than a per-binary warning.
#[allow(dead_code)]
pub async fn connect_client_and_server(pool: Pool) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    ().serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake")
}
