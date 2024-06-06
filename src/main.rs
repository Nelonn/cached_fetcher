use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::net::SocketAddr;

use http_body_util::{Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener};
use std::str::FromStr;
use std::time::{Duration, Instant};
use lazy_static::lazy_static;
use reqwest::Client;
use reqwest::redirect::Policy;
use log::warn;
use tokio::sync::{Mutex, RwLock};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct CachedRequest {
    data: String,
    timestamp: Instant,
}

static DEBUG: bool = true;

lazy_static! {
    static ref SERVER: RwLock<String> = RwLock::new(String::new());
    static ref CACHE: Mutex<HashMap<String, CachedRequest>> = Mutex::new(HashMap::new());
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 3 {
        println!("Usage {} <listen_address> <remote_server>", args[0]);
        return Ok(());
    }

    let addr = SocketAddr::from_str(args[1].as_str()).expect("Invalid listen address");

    let server_input = args[2].as_str();
    let server = if server_input.ends_with('/') {
        warn!("Address must not end with '/'");
        &server_input[..server_input.len()-1]
    } else {
        server_input
    };

    {
        let mut global_server = SERVER.write().await;
        *global_server = server.to_string();
    }

    println!("Remote server set to {}", server);

    let listener = TcpListener::bind(addr).await?;

    println!("Listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(fetch_handler))
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}

async fn fetch_handler(req: Request<hyper::body::Incoming>) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path();

    let final_data = {
        let mut mutex_guard = CACHE.lock().await;

        let cached_data = {
            if let Some(entry) = mutex_guard.get(&path.to_string()) {
                if entry.timestamp.elapsed() < Duration::from_secs(60) {
                    Some(entry.data.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        let data = match cached_data {
            Some(cached_data) => {
                if DEBUG {
                    println!("{} - SEND CACHED", path)
                }
                cached_data
            },
            None => {
                let server = {
                    let read_guard = SERVER.read().await;
                    read_guard.clone()
                };
                let url = format!("{}{}", server, path);

                let fetched_data = fetch_json(url).await.expect("Unable to send request");

                mutex_guard.insert(
                    path.to_string(),
                    CachedRequest {
                        data: fetched_data.clone(),
                        timestamp: Instant::now(),
                    },
                );

                if DEBUG {
                    println!("{} - SEND FETCHED", path)
                }

                fetched_data
            }
        };

        data
    };

    Ok(Response::new(Full::new(Bytes::from(final_data))))
}

async fn fetch_json(url: String) -> Result<String> {
    let client = Client::builder()
        .redirect(Policy::limited(5))
        .build()?;

    let response = client.get(url).send().await?.text().await?;

    Ok(response)
}
