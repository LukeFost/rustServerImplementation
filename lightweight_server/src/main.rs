use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;
use std::io;

use bytes::Bytes;
use futures_util::StreamExt;
use http::StatusCode;
use http::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE, ACCEPT_RANGES};
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::service::service_fn;
use hyper::server::conn::http1;
use hyper::{Request, Response};
use hyper::body::{Frame, Incoming};
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
use tokio::io::{AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;
use sysinfo::{NetworkExt, System, SystemExt, CpuExt};

/// Parse the Range header value and return start and end byte positions
fn parse_range_header(range_header: &str, file_size: u64) -> Option<(u64, u64)> {
    let range_str = range_header.strip_prefix("bytes=")?;
    let mut parts = range_str.split('-');
    let start = parts.next()?.parse::<u64>().ok()?;
    let end = parts.next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(file_size - 1);
    
    Some((start, end.min(file_size - 1)))
}

/// A helper function to produce an error response with a consistent `io::Error` type.
fn error_response(status: StatusCode, message: String) -> Response<BoxBody<Bytes, io::Error>> {
    Response::builder()
        .status(status)
        .body(
            Full::new(Bytes::from(message))
                .map_err(|_never: Infallible| io::Error::new(io::ErrorKind::Other, "never"))
                .boxed()
        )
        .unwrap()
}

/// Serve a static file with proper range request handling
async fn serve_file(path: &str, mime: &str, range_header: Option<&str>) -> io::Result<Response<BoxBody<Bytes, io::Error>>> {
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Error reading file `{}`: {:?}", path, e);
            return Ok(error_response(StatusCode::NOT_FOUND, "Not Found".to_string()));
        }
    };

    let metadata = file.metadata().await?;
    let file_size = metadata.len();

    // Build response based on whether there's a range request
    match range_header {
        Some(range) => {
            if let Some((start, end)) = parse_range_header(range, file_size) {
                // Seek to the requested position
                file.seek(SeekFrom::Start(start)).await?;

                let content_length = end - start + 1;
                let content_range = format!("bytes {}-{}/{}", start, end, file_size);

                // Create a limited reader stream that only reads the requested range
                let file_stream = ReaderStream::new(file)
                    .take(content_length.try_into().unwrap())
                    .map(|res| res.map(Frame::data));

                let body = StreamBody::new(file_stream);
                let boxed_body = BodyExt::boxed(body);

                Ok(Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(CONTENT_TYPE, mime)
                    .header(CONTENT_LENGTH, content_length.to_string())
                    .header(CONTENT_RANGE, content_range)
                    .header(ACCEPT_RANGES, "bytes")
                    .body(boxed_body)
                    .unwrap())
            } else {
                Ok(error_response(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    format!("Invalid range request. File size: {}", file_size)
                ))
            }
        }
        None => {
            // Serve the entire file
            let file_stream = ReaderStream::new(file)
                .map(|res| res.map(Frame::data));

            let body = StreamBody::new(file_stream);
            let boxed_body = BodyExt::boxed(body);

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, mime)
                .header(CONTENT_LENGTH, file_size.to_string())
                .header(ACCEPT_RANGES, "bytes")
                .body(boxed_body)
                .unwrap())
        }
    }
}

async fn handle_request(req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, io::Error>>, Infallible> {
    println!("Received {} request for {}", req.method(), req.uri());
    
    let path = req.uri().path();
    let range_header = req.headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok());

    let response = match path {
        "/" => {
            serve_file("static/index.html", "text/html", range_header).await
                .unwrap_or_else(|_| error_response(StatusCode::NOT_FOUND, "Not Found".to_string()))
        }
        "/video" => {
            serve_file("static/video.mp4", "video/mp4", range_header).await
                .unwrap_or_else(|_| error_response(StatusCode::NOT_FOUND, "Not Found".to_string()))
        }
        _ => {
            error_response(StatusCode::NOT_FOUND, "Not Found".to_string())
        }
    };

    Ok(response)
}

async fn monitor_resources() {
    let mut system = System::new_all();
    system.refresh_all();
    
    loop {
        sleep(Duration::from_secs(5)).await;
        
        system.refresh_cpu();
        system.refresh_networks_list();
        
        let cpu_usage: f32 = system.cpus().iter().map(|cpu| cpu.cpu_usage()).sum();
        let cpu_count = system.cpus().len();
        let avg_cpu_usage = cpu_usage / cpu_count as f32;
        
        let networks = system.networks();
        let mut total_in = 0u64;
        let mut total_out = 0u64;
        
        for (_, network) in networks {
            total_in += network.received();
            total_out += network.transmitted();
        }
        
        println!(
            "[METRICS] CPU Usage: {:.2}% | Network: In: {} bytes / Out: {} bytes (total)",
            avg_cpu_usage, total_in, total_out
        );
    }
}

async fn handle_connection(stream: tokio::net::TcpStream) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let builder = http1::Builder::new();
    
    match timeout(
        Duration::from_secs(30), // Increased timeout for video streaming
        builder.serve_connection(io, service_fn(handle_request))
    ).await {
        Ok(result) => {
            if let Err(e) = result {
                eprintln!("Error serving connection: {:?}", e);
            }
        }
        Err(_) => {
            println!("Connection timed out");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pretty_env_logger::init();
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Starting server on http://{}", addr);

    tokio::spawn(monitor_resources());

    let listener = TcpListener::bind(addr).await?;
    
    loop {
        let (stream, addr) = listener.accept().await?;
        println!("Accepted connection from: {}", addr);

        tokio::spawn(async move {
            handle_connection(stream).await;
        });
    }
}