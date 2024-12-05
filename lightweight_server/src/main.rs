use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper::server::conn::http1;
use hyper::body::Incoming as IncomingBody;
use tokio::net::TcpListener;
use tokio::time::sleep;
use sysinfo::{NetworkExt, System, SystemExt, CpuExt};

static HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>My Rust Server</title>
</head>
<body>
    <h1>Hello, Rust and Tokio!</h1>
    <p>This is a simple webpage served by a Hyper + Tokio server.</p>
</body>
</html>
"#;

async fn handle_request(req: Request<IncomingBody>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path();
    match path {
        "/" => {
            Ok(Response::new(Full::new(Bytes::from(HTML))))
        },
        _ => {
            let not_found = Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap();
            Ok(not_found)
        }
    }
}

async fn monitor_resources() {
    let mut system = System::new_all();
    
    // Initial load to populate CPU & network
    system.refresh_all();
    
    loop {
        sleep(Duration::from_secs(5)).await;
        
        // Refresh only CPU and network data for efficiency
        system.refresh_cpu();
        system.refresh_networks_list();
        
        // CPU usage: average over all CPUs
        let cpu_usage: f32 = system.cpus().iter().map(|cpu| cpu.cpu_usage()).sum();
        let cpu_count = system.cpus().len();
        let avg_cpu_usage = cpu_usage / cpu_count as f32;
        
        // Compute network usage (in/out) since last measurement
        let networks = system.networks();
        let mut total_in = 0u64;
        let mut total_out = 0u64;
        
        for (interface_name, network) in networks {
            total_in += network.received();
            total_out += network.transmitted();
        }
        
        // Print the metrics
        println!(
            "[METRICS] CPU Usage: {:.2}% | Network: In: {} bytes / Out: {} bytes (total)",
            avg_cpu_usage, total_in, total_out
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pretty_env_logger::init();
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Starting server on http://{}", addr);

    // Spawn a background task for resource monitoring
    tokio::spawn(monitor_resources());

    let listener = TcpListener::bind(addr).await?;
    
    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        
        // Spawn a new task per connection
        tokio::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(handle_request))
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}