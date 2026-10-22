use nats_hub::MetricsCollector;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

/// Spawn a minimal HTTP/1.0 server that responds to `GET /metrics` with the
/// Prometheus text exposition format. Uses only `std::net` — no framework.
/// Runs on a dedicated OS thread; clones the `Arc<MetricsCollector>` so it
/// reads live counters.
pub(super) fn spawn_metrics_server(addr: String, metrics: Arc<MetricsCollector>) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[hub-server] metrics: failed to bind {addr}: {e}");
                return;
            }
        };
        eprintln!("[hub-server] metrics endpoint listening on http://{addr}/metrics");
        // Accept loop — each request is handled inline (metrics scrapes are
        // rare and cheap; no need for a pool).
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let metrics = metrics.clone();
            // Handle each connection in its own thread to avoid blocking the
            // accept loop on a slow client.
            let _ = std::thread::Builder::new()
                .name("metrics-http".into())
                .spawn(move || handle_metrics_conn(&mut stream, &metrics));
        }
    });
}

fn handle_metrics_conn(stream: &mut std::net::TcpStream, metrics: &MetricsCollector) {
    let mut buf = [0u8; 1024];
    // Read just enough to see the request line.
    let _ = stream.read(&mut buf);
    let request = String::from_utf8_lossy(&buf);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let (status, body) = if path == "/metrics" {
        ("200 OK", metrics.render_prometheus())
    } else {
        ("404 Not Found", String::new())
    };

    let response = format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
