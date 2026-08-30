use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path =
        std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());
    let aof_path = std::path::Path::new(&aof_path);
    let snapshot_path =
        std::env::var("ROCKET_MEM_SNAPSHOT_PATH").unwrap_or_else(|_| "./dump.snapshot".to_string());
    let snapshot_path = std::path::Path::new(&snapshot_path);

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
    let replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            snapshot_path.to_path_buf(),
        )
        .with_aof(Arc::clone(&aof)),
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof, replication).await;
    Ok(())
}
