//! The guest server: plain HTTP/1.1 on the LAN, and the same over iroh for the page on the web, where each
//! bidirectional stream after the first carries one exchange (see the module docs of [`super`]).
//!
//! Every path starts with a link's token; anything else is a 404, so the server answers nobody who was not
//! given a link.
//!
//! - `GET /<token>/`                 the page, with `flipflop_web.js` and `flipflop_web_bg.wasm` next to it
//! - `GET /<token>/ws`               WebSocket: the document's sync messages (LAN)
//! - `GET /<token>/dl/<file id>`     download an offer (a folder as a zip of what it holds, streamed)
//! - `GET|HEAD /<token>/play/<file id>[/<track>]`  a song of the music tab (or an album's song), ranges
//!   included, for the page's player
//! - `PUT /<token>/up?name=<name>`   upload a file (only while the link allows it)

use std::{
    convert::Infallible,
    io::{self, Write as _},
    net::IpAddr,
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt, stream::BoxStream};
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming, SizeHint},
    header::{self, HeaderValue},
};
use hyper_util::rt::TokioIo;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpListener,
    sync::mpsc,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, handshake::derive_accept_key, protocol::Role},
};

use super::{GuestSession, MAX_FRAME, Origin, Out, TAG_CONTROL};
use crate::core::{
    Core,
    music::serve_music,
    sharing::{self, open_source},
};

const PAGE: &str = include_str!("page.html");
const WASM_JS: &str = include_str!("assets/flipflop_web.js");
const WASM: &[u8] = include_bytes!("assets/flipflop_web_bg.wasm");
const CHUNK: usize = 64 * 1024;

/// Accept connections until the task is dropped; dropping it also drops every connection.
pub(super) async fn serve(listener: TcpListener, core: Core) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, addr) = match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::debug!("guest server: accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                // A browser on this very machine reaches us through the LAN address too.
                let ip = match addr.ip() {
                    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
                    ip => ip,
                };
                connections.spawn(serve_io(stream, core.clone(), Origin::Lan(ip)));
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// Serve one connection of the page on the web until it closes. Its first stream is the sync stream
/// (length-prefixed messages, the first one `{"t":"join","token":…}`); the others are HTTP exchanges.
pub(in crate::core) async fn serve_tunnel(core: Core, conn: Connection) {
    let origin = Origin::Iroh(conn.remote_id());
    let Ok((send, recv)) = conn.accept_bi().await else {
        return;
    };
    let mut sync = tokio::spawn(tunnel_sync(core.clone(), send, recv, origin.clone()));
    let mut streams = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = conn.accept_bi() => {
                let Ok((send, recv)) = accepted else { break };
                streams.spawn(serve_io(tokio::io::join(recv, send), core.clone(), origin.clone()));
            }
            _ = &mut sync => break,
            Some(_) = streams.join_next(), if !streams.is_empty() => {}
        }
    }
    sync.abort();
    conn.close(0u32.into(), b"bye");
}

async fn read_frame(recv: &mut RecvStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.ok()?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return None;
    }
    let mut frame = vec![0u8; len];
    recv.read_exact(&mut frame).await.ok()?;
    Some(frame)
}

async fn write_frame(send: &mut SendStream, frame: &[u8]) -> bool {
    send.write_all(&(frame.len() as u32).to_be_bytes())
        .await
        .is_ok()
        && send.write_all(frame).await.is_ok()
}

async fn tunnel_sync(core: Core, mut send: SendStream, mut recv: RecvStream, origin: Origin) {
    // Whoever does not say which link they hold within a few seconds is not a guest.
    let join = tokio::time::timeout(std::time::Duration::from_secs(10), read_frame(&mut recv))
        .await
        .ok()
        .flatten();
    let token = join
        .filter(|f| f.first() == Some(&TAG_CONTROL))
        .and_then(|f| serde_json::from_slice::<serde_json::Value>(&f[1..]).ok())
        .and_then(|v| v["token"].as_str().map(str::to_owned));
    let Some((gid, _)) = token.and_then(|t| core.guest_group(&t)) else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let Some(session) = core.guest_attach(&gid, &origin, tx) else {
        return;
    };
    let (in_tx, mut incoming) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Some(frame) = read_frame(&mut recv).await {
            if in_tx.send(frame).is_err() {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            frame = incoming.recv() => match frame {
                Some(frame) => core.guest_frame(&session, &frame),
                None => break,
            },
            out = rx.recv() => match out {
                Some(Out::Frame(frame)) => {
                    if !write_frame(&mut send, &frame).await {
                        break;
                    }
                }
                Some(Out::Close) | None => break,
            },
        }
    }
    reader.abort();
    let _ = send.finish();
}

async fn ws_session<S>(
    core: Core,
    session: GuestSession,
    mut ws: WebSocketStream<S>,
    mut rx: mpsc::UnboundedReceiver<Out>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Binary(bytes))) => core.guest_frame(&session, &bytes),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Some(Out::Frame(frame)) => {
                    if ws.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close) | None => {
                    let _ = ws.close(None).await;
                    break;
                }
            },
        }
    }
}

async fn serve_io<T>(io: T, core: Core, origin: Origin)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |req| {
        let (core, origin) = (core.clone(), origin.clone());
        async move { Ok::<_, Infallible>(route(&core, &origin, req).await) }
    });
    // The web page finishes its side of a stream once the request is sent, which hyper would otherwise
    // take for the connection closing.
    let _ = hyper::server::conn::http1::Builder::new()
        .half_close(true)
        .serve_connection(TokioIo::new(io), service)
        .with_upgrades()
        .await;
}

// --------------------------------------------------------------- bodies

/// A whole body, or one streamed from a producer task, or from a stream.
enum Body {
    Full(Option<Bytes>),
    Stream {
        rx: mpsc::Receiver<io::Result<Bytes>>,
        /// The body's length, when it is known before streaming it.
        len: Option<u64>,
    },
    Bytes {
        stream: BoxStream<'static, io::Result<Bytes>>,
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
            Body::Bytes { stream, .. } => stream
                .poll_next_unpin(cx)
                .map(|chunk| chunk.map(|r| r.map(Frame::data))),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Body::Full(data) => SizeHint::with_exact(data.as_ref().map_or(0, |d| d.len() as u64)),
            Body::Stream { len: Some(len), .. } | Body::Bytes { len, .. } => {
                SizeHint::with_exact(*len)
            }
            // Streamed as it is made (a folder's zip): chunked.
            Body::Stream { len: None, .. } => SizeHint::default(),
        }
    }
}

type Reply = Response<Body>;

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

fn error(status: StatusCode, msg: &str) -> Reply {
    reply(status, "text/plain; charset=utf-8", msg.to_string())
}

fn not_found() -> Reply {
    error(StatusCode::NOT_FOUND, "Not found")
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
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        super::encode(name)
    )
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(req: &Request<Incoming>, key: &str) -> Option<String> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .map(decode)
}

// -------------------------------------------------------------- routing

async fn route(core: &Core, origin: &Origin, req: Request<Incoming>) -> Reply {
    let path = req.uri().path().to_string();
    let mut parts = path.trim_start_matches('/').split('/');
    let token = parts.next().unwrap_or_default();
    let Some((gid, _)) = core.guest_group(token) else {
        return not_found();
    };
    let rest: Vec<&str> = parts.collect();
    match (req.method().clone(), rest.as_slice()) {
        (Method::GET, [] | [""]) => reply(StatusCode::OK, "text/html; charset=utf-8", PAGE),
        (Method::GET, ["flipflop_web.js"]) => {
            reply(StatusCode::OK, "text/javascript; charset=utf-8", WASM_JS)
        }
        (Method::GET, ["flipflop_web_bg.wasm"]) => reply(StatusCode::OK, "application/wasm", WASM),
        (Method::GET, ["ws"]) => upgrade(core, &gid, origin, req),
        (Method::GET, ["dl", fid]) => download(core, &gid, fid, origin).await,
        (method @ (Method::GET | Method::HEAD), ["play", fid, track @ ..]) if track.len() <= 1 => {
            let Ok(track) = track.first().map_or(Ok(0), |t| t.parse()) else {
                return not_found();
            };
            let range = req
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok());
            play(core, &gid, fid, track, method == Method::HEAD, range).await
        }
        (Method::PUT, ["up"]) => upload(core, &gid, origin, req).await,
        _ => not_found(),
    }
}

fn upgrade(core: &Core, gid: &str, origin: &Origin, mut req: Request<Incoming>) -> Reply {
    let headers = req.headers();
    let wants = headers
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let Some(key) = headers
        .get("sec-websocket-key")
        .map(|k| derive_accept_key(k.as_bytes()))
        .filter(|_| wants)
    else {
        return error(StatusCode::BAD_REQUEST, "Expected a WebSocket");
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let Some(session) = core.guest_attach(gid, origin, tx) else {
        return not_found();
    };
    let core = core.clone();
    tokio::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(&mut req).await else {
            return;
        };
        let ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
        ws_session(core, session, ws, rx).await;
    });
    let mut res = Response::new(Body::Full(None));
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = res.headers_mut();
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&key) {
        h.insert("sec-websocket-accept", v);
    }
    res
}

async fn download(core: &Core, gid: &str, fid: &str, origin: &Origin) -> Reply {
    let (path, name, size, folder) = match core.guest_file(gid, fid) {
        Ok(found) => found,
        Err(err) => return error(StatusCode::NOT_FOUND, &err.to_string()),
    };
    if folder {
        return folder_zip(core, gid, fid, origin, path, name).await;
    }
    let file = match tokio::task::spawn_blocking(move || open_source(&path)).await {
        Ok(Ok(file)) => tokio::fs::File::from_std(file),
        _ => return error(StatusCode::NOT_FOUND, "The file is not readable here"),
    };
    let len = match file.metadata().await {
        Ok(m) if m.len() == size => size,
        _ => {
            return error(
                StatusCode::CONFLICT,
                "The file changed since it was offered",
            );
        }
    };
    let (tx, rx) = mpsc::channel(8);
    let (core, gid, fid, guest, guest_name) = (
        core.clone(),
        gid.to_owned(),
        fid.to_owned(),
        origin.id(),
        origin.name(),
    );
    let file_name = name.clone();
    tokio::spawn(async move {
        let sending = core.track_send(&gid, &fid, &file_name, &guest, &guest_name, len);
        let mut file = sending.counted(file);
        let mut buf = vec![0u8; CHUNK];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .send(Ok(Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return;
                }
            }
        }
        // Everything went into the connection; only now is it "downloaded".
        drop(tx);
        core.guest_downloaded(&gid, &fid, &guest);
    });
    let mut res = Response::new(Body::Stream { rx, len: Some(len) });
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

/// A folder offered to the group, as a zip the browser saves and opens itself. The listing is read before
/// answering (an unreadable folder is an error, not a download that stops partway); the zip is written on a
/// blocking thread, streamed as it is made.
async fn folder_zip(
    core: &Core,
    gid: &str,
    fid: &str,
    origin: &Origin,
    path: PathBuf,
    name: String,
) -> Reply {
    let read = path.clone();
    let (listing, paths) =
        match tokio::task::spawn_blocking(move || sharing::walk_folder(&read)).await {
            Ok(Ok(found)) => found,
            Ok(Err(err)) => {
                return error(
                    StatusCode::NOT_FOUND,
                    &format!("The folder is not readable here: {err}"),
                );
            }
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The folder could not be read",
                );
            }
        };
    let total = listing.files.iter().map(|(_, size)| *size).sum();
    let (tx, rx) = mpsc::channel(8);
    let (core, gid, fid, guest, guest_name) = (
        core.clone(),
        gid.to_owned(),
        fid.to_owned(),
        origin.id(),
        origin.name(),
    );
    let file_name = format!("{name}.zip");
    let shown = file_name.clone();
    tokio::task::spawn_blocking(move || {
        let sending = core.track_send(&gid, &fid, &shown, &guest, &guest_name, total);
        let writer = BodyWriter {
            tx,
            buf: Vec::with_capacity(BodyWriter::CHUNK),
        };
        match zip_folder(listing, &paths, writer, &sending) {
            Ok(()) => core.guest_downloaded(&gid, &fid, &guest),
            Err(err) => tracing::debug!("zip of {shown} for a guest stopped: {err:#}"),
        }
    });
    let mut res = Response::new(Body::Stream { rx, len: None });
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&file_name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

/// Writes the folder (`listing`, its files at `paths`) as a zip into `writer`, uncompressed (the files are
/// usually already compressed, and it keeps the zip going out as fast as the disk reads). Counts the files'
/// bytes, not the zip's own headers, as sent.
fn zip_folder(
    listing: sharing::Listing,
    paths: &[PathBuf],
    writer: BodyWriter,
    sending: &sharing::SendMeter,
) -> anyhow::Result<()> {
    let mut zip = zip::ZipWriter::new_stream(writer);
    let folder_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    for dir in &listing.dirs {
        if let Some(dir) = zip_path(dir) {
            zip.add_directory(format!("{dir}/"), folder_options)?;
        }
    }
    let mut buf = vec![0u8; CHUNK];
    for (path, (rel, size)) in paths.iter().zip(&listing.files) {
        let Some(rel) = zip_path(rel) else {
            continue;
        };
        let options = folder_options
            .unix_permissions(0o644)
            .large_file(*size >= u32::MAX as u64);
        zip.start_file(rel, options)?;
        let mut file = std::fs::File::open(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let mut written = 0u64;
        loop {
            let n = std::io::Read::read(&mut file, &mut buf)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
            written += n as u64;
            sending.add(n as u64);
        }
        if written != *size {
            anyhow::bail!("{} changed since it was offered", path.display());
        }
    }
    let mut out = zip.finish()?.into_inner();
    out.flush()?;
    Ok(())
}

/// A path from a folder's listing as a zip entry: `/`-separated, without `.` or `..`, and without what
/// Windows takes as a separator inside a name.
fn zip_path(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|p| p.replace(['\\', ':'], "_"))
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// A `std` writer that sends what it writes on the response's channel, a chunk at a time. Used by the zip
/// writer, which is blocking.
struct BodyWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl BodyWriter {
    const CHUNK: usize = 64 * 1024;

    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.buf.len() + bytes.len() > Self::CHUNK {
            self.flush_buf()?;
        }
        if bytes.len() >= Self::CHUNK {
            self.tx
                .blocking_send(Ok(Bytes::copy_from_slice(bytes)))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the download went away"))
        } else {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn flush_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let bytes = Bytes::from(std::mem::take(&mut self.buf));
        self.tx
            .blocking_send(Ok(bytes))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the download went away"))
    }
}

impl io::Write for BodyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.send(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buf()
    }
}

async fn play(
    core: &Core,
    gid: &str,
    fid: &str,
    track: usize,
    head: bool,
    range: Option<&str>,
) -> Reply {
    let served = serve_music(core, gid, fid, track, head, range).await;
    let mut res = match (served.body, served.error) {
        (Some((stream, len)), _) => Response::new(Body::Bytes { stream, len }),
        (None, Some(why)) => error(served.status, &why),
        (None, None) => Response::new(Body::Full(None)),
    };
    *res.status_mut() = served.status;
    res.headers_mut().extend(served.headers);
    res
}

async fn upload(core: &Core, gid: &str, origin: &Origin, req: Request<Incoming>) -> Reply {
    let Some(name) = query_param(&req, "name").filter(|n| !n.trim().is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "No file name");
    };
    let mut up = match core.begin_guest_upload(gid, &name).await {
        Ok(up) => up,
        Err(err) => return error(StatusCode::FORBIDDEN, &err.to_string()),
    };
    let mut body = req.into_body();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return error(StatusCode::BAD_REQUEST, "The upload was interrupted");
        };
        if let Ok(data) = frame.into_data() {
            if let Err(err) = up.write(&data).await {
                return error(StatusCode::INSUFFICIENT_STORAGE, &err.to_string());
            }
        }
    }
    match up.finish(core, gid, origin).await {
        Ok(fid) => reply(
            StatusCode::OK,
            "application/json",
            serde_json::json!({ "id": fid }).to_string(),
        ),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}
