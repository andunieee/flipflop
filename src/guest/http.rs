//! The browser link's HTTP server: plain HTTP/1.1 on the local network,
//! and the same over iroh for the page on the web (`web/`), which reaches us
//! through the relay: there each bidirectional stream carries one exchange.
//!
//! Every path starts with a link's random token; anything else is a 404, so
//! the server answers nobody who wasn't given a link. A link only answers
//! over the transport it was made for (LAN or web), and the upload routes
//! only while it allows uploads (403 otherwise).
//!
//! - `GET  /<token>/`                   the page
//! - `GET  /<token>/api/state?v=<n>`    what's offered, long-polled until it changes from `n`
//! - `GET  /<token>/dl/<id>[/<file>]`   download an offer (one file, or a zip)
//! - `POST /<token>/api/text`           text typed into the page
//! - `POST /<token>/api/batch`          announce files about to be uploaded
//! - `PUT  /<token>/api/upload/<batch>/<file>`  one file's bytes
//! - `POST /<token>/api/cancel/<batch>` the browser gave up on a batch

use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Frame, Incoming, SizeHint};
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use iroh::endpoint::Connection;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use std::sync::Mutex;

use super::Hub;

/// How long a state request waits for something to change.
const LONG_POLL: Duration = Duration::from_secs(25);
/// Biggest JSON or text body accepted.
const MAX_SMALL_BODY: usize = 1024 * 1024;

const PAGE: &str = include_str!("page.html");

/// Accept connections until the task is dropped; dropping it also drops
/// every connection.
pub(super) async fn serve(listener: TcpListener, hub: Arc<Mutex<Hub>>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::debug!("browser link: accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                connections.spawn(serve_io(stream, hub.clone(), false));
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// Serve the web page's connection until it closes or the task is dropped.
pub(super) async fn serve_tunnel(conn: Connection, hub: Arc<Mutex<Hub>>) {
    let mut streams = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = conn.accept_bi() => {
                let Ok((send, recv)) = accepted else { break };
                streams.spawn(serve_io(tokio::io::join(recv, send), hub.clone(), true));
            }
            Some(_) = streams.join_next(), if !streams.is_empty() => {}
        }
    }
}

/// `web`: the connection came through the web page's tunnel.
async fn serve_io<T>(io: T, hub: Arc<Mutex<Hub>>, web: bool)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |req| {
        let hub = hub.clone();
        async move { Ok::<_, Infallible>(route(&hub, web, req).await) }
    });
    // The web page finishes its side of a stream once the request is sent,
    // which hyper would otherwise take for the connection closing.
    let _ = hyper::server::conn::http1::Builder::new()
        .half_close(true)
        .serve_connection(TokioIo::new(io), service)
        .await;
}

// --------------------------------------------------------------- bodies

/// A whole body, or one streamed from a producer task.
pub(super) enum Body {
    Full(Option<Bytes>),
    Stream {
        rx: mpsc::Receiver<io::Result<Bytes>>,
        len: u64,
    },
}

impl hyper::body::Body for Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            Body::Full(data) => Poll::Ready(data.take().map(|d| Ok(Frame::data(d)))),
            Body::Stream { rx, .. } => rx
                .poll_recv(cx)
                .map(|chunk| chunk.map(|r| r.map(Frame::data))),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Body::Full(data) => {
                SizeHint::with_exact(data.as_ref().map_or(0, |d| d.len() as u64))
            }
            Body::Stream { len, .. } => SizeHint::with_exact(*len),
        }
    }
}

pub(super) type Reply = Response<Body>;

fn reply(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Reply {
    let mut res = Response::new(Body::Full(Some(body.into())));
    *res.status_mut() = status;
    let headers = res.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

pub(super) fn json(value: &serde_json::Value) -> Reply {
    reply(StatusCode::OK, "application/json", value.to_string())
}

pub(super) fn error(status: StatusCode, msg: &str) -> Reply {
    reply(status, "text/plain; charset=utf-8", msg.to_string())
}

fn not_found() -> Reply {
    error(StatusCode::NOT_FOUND, "Not found")
}

/// A download of `len` bytes fed by the returned sender.
pub(super) fn download(name: &str, len: u64) -> (Reply, mpsc::Sender<io::Result<Bytes>>) {
    let (tx, rx) = mpsc::channel(8);
    let mut res = Response::new(Body::Stream { rx, len });
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&content_disposition(name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    (res, tx)
}

/// `attachment` with an ASCII fallback name and the real one in UTF-8.
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// The request body, if it is at most [`MAX_SMALL_BODY`] bytes.
async fn small_body(body: Incoming) -> Option<Bytes> {
    http_body_util::Limited::new(body, MAX_SMALL_BODY)
        .collect()
        .await
        .ok()
        .map(|collected| collected.to_bytes())
}

// -------------------------------------------------------------- routing

fn query_param<'a>(req: &'a Request<Incoming>, key: &str) -> Option<&'a str> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

async fn route(hub: &Mutex<Hub>, web: bool, req: Request<Incoming>) -> Reply {
    let path = req.uri().path().to_string();
    let mut parts = path.trim_start_matches('/').split('/');
    let token = parts.next().unwrap_or_default();
    let Some(shared) = hub.lock().unwrap().by_token(token, web) else {
        return not_found();
    };
    let shared = &shared;
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    shared.seen(&user_agent);
    let rest: Vec<&str> = parts.collect();
    let method = req.method().clone();
    let id = |i: usize| rest.get(i).and_then(|s| s.parse::<u64>().ok());
    let uploading = matches!(
        (&method, rest.first()),
        (&Method::POST | &Method::PUT, Some(&"api")) if rest.get(1) != Some(&"cancel")
    );
    if uploading && !shared.can_upload() {
        return error(StatusCode::FORBIDDEN, "Sending here is turned off");
    }

    match (&method, rest.as_slice()) {
        (&Method::GET, [] | [""]) => {
            let page = PAGE.replace("{{TOKEN}}", shared.token());
            reply(StatusCode::OK, "text/html; charset=utf-8", page)
        }
        (&Method::GET, ["api", "state"]) => {
            let since = query_param(&req, "v").and_then(|v| v.parse::<u64>().ok());
            json(&shared.state_after(since, LONG_POLL).await)
        }
        (&Method::GET, ["dl", _]) => match id(1) {
            Some(offer) => shared.download(offer, None).await,
            None => not_found(),
        },
        (&Method::GET, ["dl", _, _]) => match (id(1), id(2)) {
            (Some(offer), Some(file)) => shared.download(offer, Some(file as usize)).await,
            _ => not_found(),
        },
        (&Method::POST, ["api", "text"]) => match small_body(req.into_body()).await {
            Some(body) => match String::from_utf8(body.to_vec()) {
                Ok(text) if !text.trim().is_empty() => shared.receive_text(text).await,
                _ => error(StatusCode::BAD_REQUEST, "No text"),
            },
            None => error(StatusCode::PAYLOAD_TOO_LARGE, "Too much text"),
        },
        (&Method::POST, ["api", "batch"]) => {
            let Some(body) = small_body(req.into_body()).await else {
                return error(StatusCode::PAYLOAD_TOO_LARGE, "Too many files");
            };
            match serde_json::from_slice::<super::BatchRequest>(&body) {
                Ok(batch) if !batch.files.is_empty() => shared.open_batch(batch),
                _ => error(StatusCode::BAD_REQUEST, "No files"),
            }
        }
        (&Method::PUT, ["api", "upload", _, _]) => match (id(2), id(3)) {
            (Some(batch), Some(file)) => {
                shared
                    .upload(batch, file as usize, req.into_body())
                    .await
            }
            _ => not_found(),
        },
        (&Method::POST, ["api", "cancel", _]) => match id(2) {
            Some(batch) => {
                shared.browser_cancelled(batch);
                json(&serde_json::json!({}))
            }
            None => not_found(),
        },
        _ => not_found(),
    }
}

/// Next data chunk of an upload; `None` at the end.
pub(super) async fn next_chunk(body: &mut Incoming) -> Option<Result<Bytes, hyper::Error>> {
    loop {
        match body.frame().await? {
            Ok(frame) => {
                if let Ok(data) = frame.into_data() {
                    return Some(Ok(data));
                }
            }
            Err(e) => return Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_keeps_unicode_names() {
        assert_eq!(
            content_disposition("a b.txt"),
            "attachment; filename=\"a b.txt\"; filename*=UTF-8''a%20b.txt"
        );
        assert_eq!(
            content_disposition("été\".pdf"),
            "attachment; filename=\"_t__.pdf\"; filename*=UTF-8''%C3%A9t%C3%A9%22.pdf"
        );
    }
}
