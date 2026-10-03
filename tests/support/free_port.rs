use std::net::TcpListener;

/// Holds the port until a caller deliberately transfers it to a server.
pub fn free_port() -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", 0))
}
