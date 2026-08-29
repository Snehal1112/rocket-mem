use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:6379").await?;
    println!("Listening on {}", listener.local_addr()?);
    let engine = Arc::new(engine::Engine::new());
    rocket_mem::serve(listener, engine).await;
    Ok(())
}
