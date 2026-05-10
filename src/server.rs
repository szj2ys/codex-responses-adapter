//! Server bootstrap and lifecycle management.

use crate::config::AdapterConfig;
use crate::handler::ServerConfig;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

/// Run the server with the given configuration.
pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = TcpListener::bind(&addr).await?;
    
    info!("Server listening on http://{}", addr);
    
    // Server loop would go here - simplified for now
    // Full implementation will integrate with handler module
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_parsing() {
        let addr: SocketAddr = "127.0.0.1:6789".parse().unwrap();
        assert_eq!(addr.port(), 6789);
    }
}
