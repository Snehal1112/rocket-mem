use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = rocket_mem::config::load().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config error: {e}"),
        )
    })?;

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

    let engine = Arc::new(rocket_mem::aof::recover(aof_path, snapshot_path)?);
    println!(
        "Recovered state from {} and {}",
        snapshot_path.display(),
        aof_path.display()
    );

    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(aof_path, rocket_mem::aof::FsyncPolicy::EverySecond)
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

    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
