use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::var("ROCKET_MEM_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string());
    let aof_path =
        std::env::var("ROCKET_MEM_AOF_PATH").unwrap_or_else(|_| "./appendonly.aof".to_string());

    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            std::path::Path::new(&aof_path),
            rocket_mem::aof::FsyncPolicy::EverySecond,
        )
        .expect("failed to open AOF file"),
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Listening on {}", listener.local_addr()?);
    rocket_mem::serve(listener, engine, aof).await;
    Ok(())
}
