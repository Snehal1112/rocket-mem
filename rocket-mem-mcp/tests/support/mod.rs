use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn rocket_mem_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ROCKET_MEM_BIN") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/rocket-mem")
}

/// Spawns the real compiled `rocket-mem` binary bound to an OS-assigned port, reading its
/// startup banner on stdout to discover which port it actually got. Returns the child (so the
/// caller can kill it) and the bound `"host:port"` string. Mirrors
/// `crates/server/tests/kill_and_recover.rs`'s `spawn_server`.
pub fn spawn_rocket_mem(aof_path: &std::path::Path) -> (Child, String) {
    let bin = rocket_mem_bin();
    let mut child = Command::new(&bin)
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", aof_path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "failed to spawn {bin:?}: {e} — build it first with `cargo build -p rocket-mem` \
                 from the repo root, or set ROCKET_MEM_BIN"
            )
        });

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    let mut addr = None;
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().trim_matches(|c| c == '│' || c == ' ');
                let mut parts = trimmed.split_whitespace();
                if parts.next() == Some("RESP") {
                    if let Some(addr_str) = parts.next() {
                        addr = Some(addr_str.to_string());
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let addr = addr.expect("rocket-mem never printed its listening address on stdout");
    (child, addr)
}
