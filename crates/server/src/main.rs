use std::io::IsTerminal;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = rocket_mem::config::load().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config error: {e}"),
        )
    })?;

    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    let filter = tracing_subscriber::EnvFilter::new(
        rocket_mem::config::resolve_log_filter_directive(&config.log_level),
    );
    tracing_subscriber::fmt()
        .with_ansi(color)
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "rocket-mem starting");

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
    let cluster = match (&config.cluster_config, &config.cluster_node_id) {
        (Some(path), Some(node_id)) => {
            let cluster_config =
                rocket_mem::cluster::ClusterConfig::load(std::path::Path::new(path), node_id)?;
            println!(
                "Cluster mode enabled: node '{}' at {} owns slots {}-{} of {} nodes",
                cluster_config.myself().id,
                cluster_config.myself().addr,
                cluster_config.myself().first_slot,
                cluster_config.myself().last_slot,
                cluster_config.nodes().len()
            );
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
    let generation = rocket_mem::aof::read_generation(snapshot_path)?;
    let engine = Arc::new(rocket_mem::aof::recover(aof_path, snapshot_path)?);
    println!(
        "Recovered state from {} and {} (generation {generation})",
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
    .with_slowlog_threshold(slowlog_threshold)
    .with_acl_bootstrap(acl_users);
    if let Some(cluster) = cluster {
        handle = handle.with_cluster(cluster);
    }
    if let Some(ca_path) = &config.tls_ca_path {
        let client_config = rocket_mem::tls::load_client_config(std::path::Path::new(ca_path))
            .expect("failed to load replication TLS CA certificate");
        handle = handle.with_replication_tls_client_config(client_config);
    }
    let replication = Arc::new(handle);

    let metrics_listener = tokio::net::TcpListener::bind(&config.metrics_addr).await?;
    println!(
        "Metrics on http://{}/metrics",
        metrics_listener.local_addr()?
    );
    tokio::spawn(rocket_mem::metrics::serve_metrics(
        metrics_listener,
        metrics_handle,
        Arc::clone(&engine),
        Arc::clone(&replication),
    ));

    let rmp_listener = tokio::net::TcpListener::bind(&config.rmp_addr).await?;
    println!("RMP listening on {}", rmp_listener.local_addr()?);
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    rocket_mem::config::validate_tls(&config)?;

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
        println!("TLS listening on {}", tls_listener.local_addr()?);
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
        println!("RMP TLS listening on {}", tls_rmp_listener.local_addr()?);
        tokio::spawn(rocket_mem::rmp_connection::serve_tls(
            tls_rmp_listener,
            tls_config,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
        ));
    }

    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
