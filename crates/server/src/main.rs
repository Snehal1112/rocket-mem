use std::io::IsTerminal;
use std::sync::Arc;

/// Wraps `text` in an ANSI SGR code, or returns it unchanged when `color` is
/// false (no tty, or `NO_COLOR` set) -- see the startup-banner code below.
fn paint(code: &str, text: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// Column width the label ("storage", "cluster", ...) is padded to before the value starts, so
/// every top-level banner line's value lines up in the same column and sub-items (cluster nodes,
/// listeners) can indent to that same column underneath.
const BANNER_LABEL_WIDTH: usize = 10;

/// A banner line's length once `paint`'s ANSI SGR codes are stripped back out -- the box border
/// has to size itself and pad each line to the *visible* width, not the byte length of a string
/// that may have escape codes spliced into the middle of it (e.g. `cluster_summary`, which
/// colors just the node id).
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

/// A padded, dimmed banner label (`"storage   "`, `"cluster   "`, ...), padded to
/// `BANNER_LABEL_WIDTH` before coloring so the ANSI codes don't throw off later width padding.
fn banner_label(text: &str, color: bool) -> String {
    paint("2", &format!("{text:<BANNER_LABEL_WIDTH$}"), color)
}

/// Prints `title` and `body` lines inside a box border sized to the widest line, so the startup
/// banner reads as one deliberate block instead of a loose stack of println!s.
fn print_banner(title: &str, body: &[String], color: bool) {
    let inner_width = body
        .iter()
        .map(|l| visible_width(l))
        .chain(std::iter::once(visible_width(title)))
        .max()
        .unwrap_or(0);

    let border = |left: &str, right: &str| {
        paint(
            "36",
            &format!("{left}{}{right}", "─".repeat(inner_width + 2)),
            color,
        )
    };
    let row = |content: &str, pad_to: usize| {
        let pad = " ".repeat(pad_to.saturating_sub(visible_width(content)));
        format!(
            "{} {content}{pad} {}",
            paint("36", "│", color),
            paint("36", "│", color)
        )
    };

    println!("\n{}", border("┌", "┐"));
    println!("{}", row(title, inner_width));
    println!("{}", border("├", "┤"));
    for line in body {
        println!("{}", row(line, inner_width));
    }
    println!("{}", border("└", "┘"));
    println!();
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = rocket_mem::config::load().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config error: {e}"),
        )
    })?;

    // Two separate tty checks, not one shared `color`: the tracing subscriber writes to
    // stderr while the startup banner below writes to stdout, and the two file descriptors
    // can have different tty-ness under asymmetric redirection (e.g. `rocket-mem 2>app.log`
    // with stdout still a terminal) -- sharing one check would leak ANSI codes into
    // whichever stream the wrong check was based on.
    let log_color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    // Hoisted out of the `EnvFilter::new(...)` call so the config summary below can log the
    // directive that is actually in force. `config.log_level` is only the fallback -- `RUST_LOG`
    // wins (see `resolve_log_filter_directive`) -- so logging the config field would have the
    // summary claim `info` while the process emits `debug` lines.
    let log_filter_directive = rocket_mem::config::resolve_log_filter_directive(&config.log_level);
    let filter = tracing_subscriber::EnvFilter::new(&log_filter_directive);
    tracing_subscriber::fmt()
        .with_ansi(log_color)
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "rocket-mem starting");

    // The machine-readable counterpart to the boxed startup banner printed further down this
    // function, not a replacement for it -- see the verbose logging spec's "Decision: the
    // startup banner stays separate".
    //
    // This lands at `info`, so it reaches every operator's log file and every log aggregator.
    // `Config` transitively holds credential material -- `acl.users` carries plaintext
    // passwords and rule tokens, `replicaof_auth_password` is the leader's plaintext password,
    // and the TLS cert/key/CA paths name private key material on disk -- so every field below is
    // enumerated by hand and each is either a bind address, a non-credential path, a level
    // string, or a plain boolean derived from a secret-bearing field's *presence*.
    //
    // The *credential* half of that rule is now structural rather than a call-site convention:
    // `acl::AclUser`, `config::AclUserConfig` and `config::Config` all have hand-written
    // redacting `Debug` impls, so a `{:?}` of the whole `Config` anywhere in the crate can no
    // longer print a plaintext password or a raw rule token. `Config`'s impl is destructuring and
    // therefore exhaustive at compile time -- a new field cannot slip past it unconsidered.
    // What those impls still render, deliberately, is the residue: the ACL usernames,
    // `replicaof_auth_username`, and the TLS cert/key/CA paths. So enumerating fields by hand
    // remains the rule here even though a `?config` would no longer leak a secret. Deliberately
    // absent: `tls_cert_path`, `tls_key_path`, `tls_ca_path` (summarised only as the two
    // `tls_*_enabled` booleans) and every `acl.users` field (summarised only as
    // `acl_enabled`/`acl_user_count`).
    // `startup_logging.rs`'s `secret_bearing_config_is_never_rendered_into_the_summary` is the
    // regression guard for all of that -- it starts the binary with a real-shaped ACL user, a
    // real `replicaof_auth_password` and TLS material, and fails if any of it reaches stderr.
    //
    // `tls_enabled` means "this process is serving TLS listeners", derived from the addresses
    // rather than from cert/key presence: cert and key set with no `tls_*_addr` binds no TLS
    // listener at all, and `validate_tls` already rejects the reverse, so the addresses are the
    // honest signal. TLS *replication* is a separate switch (`tls_ca_path`) that turns on no
    // listener, so it gets its own field instead of being folded into this one.
    tracing::info!(
        addr = %config.addr,
        rmp_addr = %config.rmp_addr,
        metrics_addr = %config.metrics_addr,
        aof_path = %config.aof_path,
        snapshot_path = %config.snapshot_path,
        log_filter = %log_filter_directive,
        log_value_max_bytes = config.log_value_max_bytes,
        slowlog_threshold_micros = config.slowlog_threshold_micros,
        cluster_mode = config.cluster_config.is_some(),
        acl_enabled = !config.acl.users.is_empty(),
        acl_user_count = config.acl.users.len(),
        tls_enabled = config.tls_resp_addr.is_some() || config.tls_rmp_addr.is_some(),
        tls_replication_enabled = config.tls_ca_path.is_some(),
        "resolved config summary"
    );

    let metrics_handle = rocket_mem::metrics::recorder_handle();

    let aof_path = std::path::PathBuf::from(&config.aof_path);
    let aof_path = aof_path.as_path();
    let snapshot_path = std::path::PathBuf::from(&config.snapshot_path);
    let snapshot_path = snapshot_path.as_path();

    // Microseconds, not milliseconds: 10ms is already a very long time for an in-memory store,
    // so the useful tuning range is below it. 0 disables the slow log.
    let slowlog_threshold = std::time::Duration::from_micros(config.slowlog_threshold_micros);

    // Cluster mode is opt-in and all-or-nothing: the topology file names every node's slot
    // range, and cluster_node_id says which line is this process. Both must be set
    // together -- one without the other is an operator mistake that would otherwise start a
    // node in standalone mode while its neighbours redirect keys to it.
    let mut listeners: Vec<(&str, String)> = Vec::new();
    let mut cluster_summary = paint("2", "standalone (no cluster_config set)", color);
    let mut cluster_topology_lines: Vec<String> = Vec::new();

    // Before the cluster block, because `spawn_peer_prober` builds a `tokio::time::interval`,
    // which panics outright on a zero period. A bad timer must fail startup with a readable
    // error, not abort the process from inside a spawned task.
    rocket_mem::config::validate_cluster_health(&config)?;

    let cluster = match (&config.cluster_config, &config.cluster_node_id) {
        (Some(path), Some(node_id)) => {
            let cluster_config =
                rocket_mem::cluster::ClusterConfig::load(std::path::Path::new(path), node_id)?;
            cluster_summary = format!(
                "node '{}' owns slots {}-{} ({} node{} total)",
                paint("1", &cluster_config.myself().id, color),
                cluster_config.myself().first_slot,
                cluster_config.myself().last_slot,
                cluster_config.nodes().len(),
                if cluster_config.nodes().len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
            // The one-line summary above only ever names this node; the full topology (every
            // other node's id/addr/slot range) previously wasn't shown anywhere at startup --
            // an operator had to already know cluster.conf's contents or query a running node
            // with `CLUSTER NODES` to find out where the other shards are.
            cluster_topology_lines = cluster_config
                .topology_summary()
                .lines()
                .map(str::to_string)
                .collect();
            Some(Arc::new(cluster_config))
        }
        (Some(_), None) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cluster_config is set but cluster_node_id is not",
            ))
        }
        (None, Some(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cluster_node_id is set but cluster_config is not",
            ))
        }
        (None, None) => None,
    };

    let acl_users: Vec<rocket_mem::acl::AclUser> = config
        .acl
        .users
        .iter()
        .map(rocket_mem::acl::from_bootstrap_config)
        .collect::<Result<_, _>>()
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("acl bootstrap: {e}"),
            )
        })?;

    // A config with two `[[acl.users]]` blocks sharing the same username currently would start
    // cleanly with only the later one live, silently discarding the first -- a fail-quiet
    // security bug if the discarded entry was the intended, more-restrictive one. Fail loudly
    // instead of letting `Vec` -> `HashMap` insertion order decide which definition wins.
    {
        let mut seen = std::collections::HashSet::new();
        for user in &acl_users {
            if !seen.insert(user.username.as_str()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("acl bootstrap: duplicate username '{}'", user.username),
                ));
            }
        }
    }

    // The generation the manifest currently names -- 0, and so the bare configured paths, for
    // every deployment that has never run `BGREWRITEAOF`. Read once and used for both halves of
    // startup: `recover` resolves it internally, and the writer must be opened at the very same
    // generation, or every write this process makes would land in a file the manifest does not
    // name and be lost at the next restart.
    let acl_summary = if acl_users.is_empty() {
        paint(
            "33",
            "no users configured -- auth disabled, every client is trusted",
            color,
        )
    } else {
        format!(
            "{} user{} configured, auth required",
            acl_users.len(),
            if acl_users.len() == 1 { "" } else { "s" }
        )
    };

    let generation = rocket_mem::aof::read_generation(snapshot_path)?;
    let engine = Arc::new(rocket_mem::aof::recover(aof_path, snapshot_path)?);
    let storage_summary = format!(
        "recovered {} + {} (generation {generation})",
        snapshot_path.display(),
        aof_path.display()
    );

    // `open_at_generation`, never `open`: it opens generation `generation`'s file while keeping
    // `base_path()` at the bare configured `aof_path`, so the next `BGREWRITEAOF` rotates onto
    // `<aof_path>.<generation + 1>` rather than double-suffixing the resolved name.
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open_at_generation(
            aof_path,
            generation,
            rocket_mem::aof::FsyncPolicy::EverySecond,
        )
        .expect("failed to open AOF file"),
    );

    // `with_aof` hands the apply loop the same `AofWriter` `serve()` gets, so a replicated
    // multi-key write and a concurrent `SAVE` on this node serialize on one lock — see
    // `ReplicationHandle::aof`.
    let mut handle = rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        snapshot_path.to_path_buf(),
    )
    .with_aof(Arc::clone(&aof))
    // Not `config.addr`: that is the *plaintext* RESP listen address unconditionally, so a TLS
    // deployment used to advertise a port a TLS peer must not dial. `announce_addr` falls back to
    // `config.addr` when `replica_announce_addr` is unset, so this is byte-for-byte the old
    // behaviour for every deployment that does not set the new field. See
    // docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md.
    .with_own_addr(rocket_mem::config::announce_addr(&config))
    .with_slowlog_threshold(slowlog_threshold)
    .with_log_value_max_bytes(config.log_value_max_bytes)
    .with_acl_bootstrap(acl_users)
    // `min_replicas_to_write == 0` (the default) keeps fencing off, matching every deployment
    // before this feature existed. See design contract §2.5 and the NOREPLICAS gate in
    // dispatcher.rs's dispatch_and_log_inner.
    .with_min_replicas(
        config.min_replicas_to_write,
        std::time::Duration::from_secs(config.min_replicas_max_lag_secs),
    );
    if let Some(cluster) = cluster {
        // Cluster mode only: the prober needs peers, and a standalone node has none. It is purely
        // observational -- it makes `CLUSTER NODES`/`SHARDS`/`INFO` tell the truth about which
        // peers are answering, and changes nothing about routing, promotion, or the topology
        // file. See docs/superpowers/plans/2026-09-09-failover-safety-primitives/.
        let peer_health = rocket_mem::cluster_health::spawn_peer_prober(
            &cluster,
            std::time::Duration::from_secs(config.cluster_probe_interval_secs),
            std::time::Duration::from_secs(config.cluster_node_timeout_secs),
        );
        handle = handle.with_cluster(cluster).with_peer_health(peer_health);
    }
    if let Some(ca_path) = &config.tls_ca_path {
        let client_config = rocket_mem::tls::load_client_config(std::path::Path::new(ca_path))
            .expect("failed to load replication TLS CA certificate");
        handle = handle.with_replication_tls_client_config(client_config);
    }
    let replication = Arc::new(handle);

    // Must run before the auto-connect block below fires off a connection attempt: an invalid
    // `replicaof_auth_username`/`replicaof_auth_password` pairing should fail startup outright,
    // not spawn a doomed connection first and reject the config afterwards. `validate_tls` is
    // hoisted here too (rather than left by the TLS listeners below) for the same reason: it is a
    // pure function of `&Config` with no dependency on anything constructed later, so a broken TLS
    // config must abort startup before the auto-connect block below can load a leader's snapshot
    // into this node's engine and append to its own AOF.
    rocket_mem::config::validate_replicaof(&config)?;
    rocket_mem::config::validate_tls(&config)?;
    // Same reasoning as the two above, and the same placement: a pure function of `&Config` whose
    // failure must abort before anything binds. An unparseable announced address would otherwise
    // survive to the leader's `INFO REPLICATION`, which renders it as `ip=?,port=0` -- an operator
    // sees a broken-looking replica and has nothing to grep for.
    rocket_mem::config::validate_replica_announce_addr(&config)?;
    // Same placement, same reasoning: a nonzero `min_replicas_to_write` paired with a zero lag
    // window refuses every write forever, so it must abort startup rather than come up looking
    // healthy and reject the first client write.
    rocket_mem::config::validate_min_replicas(&config)?;

    // The misconfiguration the announce-address spec exists to make visible: a follower serving
    // TLS still tells its leader to find it at `addr`, the plaintext RESP listen address, because
    // `announce_addr` is deliberately dumb rather than guessing at `tls_resp_addr`. One line, at
    // startup, above every bind -- never per-command. See
    // docs/superpowers/specs/2026-09-10-replica-announce-addr-spec.md's "Warn when the announced
    // address contradicts the transport".
    //
    // `config.addr` is rendered unescaped, matching the `addr = %config.addr` field in the
    // resolved-config summary above. It is an operator-supplied local config value, not the
    // network-supplied `PSYNC` bulk that `connection.rs` routes through `logging::escape_ident` --
    // no remote party can put bytes here.
    if rocket_mem::config::should_warn_plaintext_announce(&config) {
        tracing::warn!(
            announced = %config.addr,
            "replica_announce_addr is unset while a TLS listener is configured -- this node \
             advertises its plaintext address to its leader"
        );
    }

    // A configured `replicaof` auto-connects on every startup, closing the "restarted follower
    // silently comes back as standalone" footgun documented in
    // docs/superpowers/specs/2026-08-30-sprint-5-spec.md. Fire-and-forget: this spawns its own
    // task and never awaits the connection, so placement relative to the listeners below has no
    // functional effect -- and a leader that isn't up yet falls into the same 1-second-backoff
    // reconnect loop a later mid-stream disconnect would use, not a startup failure.
    replication.start_replicating_from_config(&config);

    let metrics_listener = tokio::net::TcpListener::bind(&config.metrics_addr).await?;
    // Each listener site below resolves its address string once and both logs and pushes it,
    // rather than calling `local_addr()` twice. The `protocol` label is the very same `&str`
    // the banner's `listeners` block uses, so the log and the banner can never disagree about
    // a listener's name. Both are rendered with `%` so the field lands unquoted and greppable,
    // matching the rest of this series' fields -- and, since the field-consistency sweep, the
    // `protocol` field on `connection.rs`'s and `rmp_connection.rs`'s connection spans too, which
    // used to render `protocol="resp"`/`protocol="rmp"` against these unquoted uppercase ones.
    // `metrics` stays lowercase: it names this HTTP endpoint, not one of the two wire protocols
    // the project spells RESP and RMP everywhere else, and only this one site ever emits it.
    let metrics_addr_str = format!("http://{}/metrics", metrics_listener.local_addr()?);
    tracing::info!(protocol = %"metrics", addr = %metrics_addr_str, "listener bound");
    listeners.push(("metrics", metrics_addr_str));
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        metrics_listener,
        metrics_handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));

    let rmp_listener = tokio::net::TcpListener::bind(&config.rmp_addr).await?;
    let rmp_addr_str = rmp_listener.local_addr()?.to_string();
    tracing::info!(protocol = %"RMP", addr = %rmp_addr_str, "listener bound");
    listeners.push(("RMP", rmp_addr_str));
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    if let (Some(tls_addr), Some(cert), Some(key)) = (
        &config.tls_resp_addr,
        &config.tls_cert_path,
        &config.tls_key_path,
    ) {
        let tls_config = rocket_mem::tls::load_server_config(
            std::path::Path::new(cert),
            std::path::Path::new(key),
        )?;
        let tls_listener = tokio::net::TcpListener::bind(tls_addr).await?;
        let tls_addr_str = tls_listener.local_addr()?.to_string();
        tracing::info!(protocol = %"RESP+TLS", addr = %tls_addr_str, "listener bound");
        listeners.push(("RESP+TLS", tls_addr_str));
        tokio::spawn(rocket_mem::serve_tls(
            tls_listener,
            tls_config,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));
    }

    if let (Some(tls_rmp_addr), Some(cert), Some(key)) = (
        &config.tls_rmp_addr,
        &config.tls_cert_path,
        &config.tls_key_path,
    ) {
        let tls_config = rocket_mem::tls::load_server_config(
            std::path::Path::new(cert),
            std::path::Path::new(key),
        )?;
        let tls_rmp_listener = tokio::net::TcpListener::bind(tls_rmp_addr).await?;
        let tls_rmp_addr_str = tls_rmp_listener.local_addr()?.to_string();
        tracing::info!(protocol = %"RMP+TLS", addr = %tls_rmp_addr_str, "listener bound");
        listeners.push(("RMP+TLS", tls_rmp_addr_str));
        tokio::spawn(rocket_mem::rmp_connection::serve_tls(
            tls_rmp_listener,
            tls_config,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));
    }

    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    let resp_addr_str = listener.local_addr()?.to_string();
    tracing::info!(protocol = %"RESP", addr = %resp_addr_str, "listener bound");
    listeners.push(("RESP", resp_addr_str));

    let title = paint(
        "1;36",
        &format!("rocket-mem v{}", env!("CARGO_PKG_VERSION")),
        color,
    );
    let mut body = Vec::new();
    body.push(format!(
        "{}{storage_summary}",
        banner_label("storage", color)
    ));
    body.push(format!("{}{acl_summary}", banner_label("acl", color)));
    body.push(format!(
        "{}{cluster_summary}",
        banner_label("cluster", color)
    ));
    for line in &cluster_topology_lines {
        body.push(format!("{:BANNER_LABEL_WIDTH$}{line}", ""));
    }
    // A live count, not a hardcoded message: this only reports INBOUND replicas (nodes
    // currently PSYNC'd to this one). This node's own OUTBOUND role (whether it is itself
    // replicating from a configured `replicaof` target) is reported separately, right below --
    // see docs/superpowers/specs/2026-09-09-replicaof-config-file-spec.md. A replica CAN already
    // be registered here, though -- a TLS RESP listener (spawned above, before this point)
    // starts accepting connections immediately, so a fast-connecting replica's PSYNC can land
    // before this banner prints, even though the plaintext RESP listener (served only after the
    // banner, at the bottom of this function) cannot.
    let replica_addrs = replication.registry.addrs();
    if replica_addrs.is_empty() {
        body.push(format!(
            "{}none connected yet -- REPLICAOF is a live command; INFO REPLICATION shows current state",
            banner_label("replicas", color)
        ));
    } else {
        body.push(format!(
            "{}{} connected",
            banner_label("replicas", color),
            replica_addrs.len()
        ));
        for (i, addr) in replica_addrs.iter().enumerate() {
            let shown = addr.as_deref().unwrap_or("?");
            body.push(format!("{:BANNER_LABEL_WIDTH$}slave{i} {shown}", ""));
        }
    }
    if let Some(target) = &config.replicaof {
        let auth_note = if config.replicaof_auth_username.is_some() {
            "auth configured"
        } else {
            "no auth"
        };
        body.push(format!(
            "{}replicating from {target} ({auth_note})",
            banner_label("replicaof", color)
        ));
    }
    body.push(paint("2", "listeners", color));
    let listener_label_width = listeners.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, addr) in &listeners {
        body.push(format!(
            "{:BANNER_LABEL_WIDTH$}{}  {addr}",
            "",
            paint("1", &format!("{label:<listener_label_width$}"), color)
        ));
    }
    print_banner(&title, &body, color);

    // No `shutdown (info)` event: the spec's Startup catalogue row names one, but there is
    // nothing to log it from -- `rocket_mem::serve` is an unconditional `loop` (connection.rs)
    // with no break, and this binary installs no `tokio::signal` handler anywhere in the
    // crate, so a SIGTERM/SIGKILL ends the process before any Rust code, including a line
    // here, would run. Logging a shutdown event requires adding real signal handling first,
    // which is a graceful-shutdown feature in its own right, not a logging change --
    // deferred, and marked as such in the spec's event catalogue.
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
