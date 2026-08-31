use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let metrics_handle = rocket_mem::metrics::recorder_handle();

    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path =
        std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());
    let aof_path = std::path::Path::new(&aof_path);
    let snapshot_path =
        std::env::var("ROCKET_MEM_SNAPSHOT_PATH").unwrap_or_else(|_| "./dump.snapshot".to_string());
    let snapshot_path = std::path::Path::new(&snapshot_path);

    // Microseconds, not milliseconds: 10ms is already a very long time for an in-memory store,
    // so the useful tuning range is below it. 0 disables the slow log.
    let slowlog_threshold = std::env::var("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(std::time::Duration::from_micros)
        .unwrap_or_else(|| std::time::Duration::from_millis(10));

    // Cluster mode is opt-in and all-or-nothing: the topology file names every node's slot
    // range, and ROCKET_MEM_CLUSTER_NODE_ID says which line is this process. Both must be set
    // together -- one without the other is an operator mistake that would otherwise start a
    // node in standalone mode while its neighbours redirect keys to it.
    let cluster = match (
        std::env::var("ROCKET_MEM_CLUSTER_CONFIG"),
        std::env::var("ROCKET_MEM_CLUSTER_NODE_ID"),
    ) {
        (Ok(path), Ok(node_id)) => {
            let config =
                rocket_mem::cluster::ClusterConfig::load(std::path::Path::new(&path), &node_id)?;
            println!(
                "Cluster mode enabled: node '{}' at {} owns slots {}-{} of {} nodes",
                config.myself().id,
                config.myself().addr,
                config.myself().first_slot,
                config.myself().last_slot,
                config.nodes().len()
            );
            Some(Arc::new(config))
        }
        (Ok(_), Err(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ROCKET_MEM_CLUSTER_CONFIG is set but ROCKET_MEM_CLUSTER_NODE_ID is not",
            ))
        }
        (Err(_), Ok(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ROCKET_MEM_CLUSTER_NODE_ID is set but ROCKET_MEM_CLUSTER_CONFIG is not",
            ))
        }
        (Err(_), Err(_)) => None,
    };

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
    .with_slowlog_threshold(slowlog_threshold);
    if let Some(cluster) = cluster {
        handle = handle.with_cluster(cluster);
    }
    let replication = Arc::new(handle);

    let metrics_addr =
        std::env::var("ROCKET_MEM_METRICS_ADDR").unwrap_or_else(|_| "127.0.0.1:9121".to_string());
    let metrics_listener = tokio::net::TcpListener::bind(&metrics_addr).await?;
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

    let rmp_addr =
        std::env::var("ROCKET_MEM_RMP_ADDR").unwrap_or_else(|_| "127.0.0.1:6380".to_string());
    let rmp_listener = tokio::net::TcpListener::bind(&rmp_addr).await?;
    println!("RMP listening on {}", rmp_listener.local_addr()?);
    tokio::spawn(rocket_mem::rmp_connection::serve(
        rmp_listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
    ));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
