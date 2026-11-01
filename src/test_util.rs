//! Shared test scaffolding.

use std::net::SocketAddr;

/// Serves `app` on a free local port for the rest of the test.
pub async fn serve(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}
