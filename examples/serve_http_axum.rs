//! A smart HTTP Git server on axum, with girt's `serve-async` feature.
//!
//! Serves every bare repository under a directory, for `git clone`, `git fetch` and `git push`:
//!
//! ```sh
//! cargo run --example serve_http_axum --features serve-async -- /srv/git 127.0.0.1:8080
//! git clone http://127.0.0.1:8080/project.git
//! ```
//!
//! Each request runs its pack work on Tokio's blocking pool and streams the response as it is
//! written. A client that reads slowly stalls its own pack generation, and one that disconnects
//! cancels it. There is no authentication: anyone who can reach the address can push.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::TryStreamExt;
use girt::config::ConfigInputs;
use girt::serve::http::{NO_CACHE_HEADERS, Service};
use girt::serve::{Protocol, SpawnBlocking};
use tokio_util::io::{ReaderStream, StreamReader};

/// How many response bytes wait between the pack writer and the client.
const RESPONSE_BUFFER: usize = 64 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: serve_http_axum <root> [address]")?,
    );
    let address = arguments
        .next()
        .map(|address| address.to_string_lossy().into_owned())
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("serving {} on http://{address}", root.display());
    axum::serve(listener, router(root)).await?;
    Ok(())
}

/// Routes `GET /<repo>/info/refs` and `POST /<repo>/<service>` to the repositories under `root`.
pub fn router(root: PathBuf) -> Router {
    Router::new()
        .route("/{repo}/info/refs", get(info_refs))
        .route("/{repo}/{service}", post(service_request))
        .with_state(Arc::new(root))
}

async fn info_refs(
    State(root): State<Arc<PathBuf>>,
    axum::extract::Path(repo): axum::extract::Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let service = query
        .as_deref()
        .unwrap_or_default()
        .split('&')
        .find_map(|parameter| parameter.strip_prefix("service="))
        .and_then(|name| Service::from_query_value(name.as_bytes()));
    let Some(service) = service else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Some(path) = repository(&root, &repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let protocol = protocol(&headers);
    let (writer, reader) = tokio::io::duplex(RESPONSE_BUFFER);
    tokio::spawn(async move {
        if let Err(error) = service
            .advertise_async(
                SpawnBlocking,
                path,
                ConfigInputs::default(),
                protocol,
                writer,
            )
            .await
        {
            eprintln!("advertising {repo}: {error}");
        }
    });
    streamed(service.advertisement_content_type(), reader)
}

async fn service_request(
    State(root): State<Arc<PathBuf>>,
    axum::extract::Path((repo, service)): axum::extract::Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(service) = Service::from_query_value(service.as_bytes()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = repository(&root, &repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let protocol = protocol(&headers);
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .map(|value| value.as_bytes().to_vec());
    let body = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
    let (writer, reader) = tokio::io::duplex(RESPONSE_BUFFER);
    tokio::spawn(async move {
        let answered = service
            .respond_async(
                SpawnBlocking,
                path,
                ConfigInputs::default(),
                protocol,
                encoding.as_deref(),
                body,
                writer,
            )
            .await;
        // The client has already seen any `ERR` packet; the status line is sent.
        if let Err(error) = answered {
            eprintln!("answering {repo}: {error}");
        }
    });
    streamed(service.result_content_type(), reader)
}

/// A response whose body streams from `reader` as the serving task writes it.
fn streamed(content_type: &'static str, reader: tokio::io::DuplexStream) -> Response {
    let mut response = Body::from_stream(ReaderStream::new(reader)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    for (name, value) in NO_CACHE_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

/// The repository a URL names, refusing names that could leave `root`.
fn repository(root: &Path, name: &str) -> Option<PathBuf> {
    let safe = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']);
    safe.then(|| root.join(name))
}

fn protocol(headers: &HeaderMap) -> Protocol {
    let value = headers
        .get("git-protocol")
        .map(HeaderValue::as_bytes)
        .unwrap_or_default();
    Protocol::from_parameters(value)
}
