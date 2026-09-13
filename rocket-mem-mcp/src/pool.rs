use redis::aio::ConnectionManager;
use redis::{Client, RedisResult};

/// A cheap-to-clone handle to a rocket-mem connection. `ConnectionManager` already multiplexes
/// concurrent requests over one underlying connection and reconnects automatically, so no
/// separate pooling crate (bb8/deadpool) is needed — every tool call just clones this and
/// issues its command.
#[derive(Clone)]
pub struct Pool {
    manager: ConnectionManager,
}

impl Pool {
    pub async fn connect(target_addr: &str) -> RedisResult<Self> {
        let client = Client::open(format!("redis://{target_addr}"))?;
        let manager = client.get_connection_manager().await?;
        Ok(Self { manager })
    }

    pub fn connection(&self) -> ConnectionManager {
        self.manager.clone()
    }
}
