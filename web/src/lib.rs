//! The browser link's page on the web reaches the app through iroh: it dials
//! the app's endpoint over its home relay (browsers have no UDP) and sends
//! the very requests the page makes on the LAN, one HTTP/1.1 exchange per
//! bidirectional stream, `Connection: close`. The app serves them with the
//! same routes (`src/guest/http.rs`), token check included.

use std::str::FromStr;
use std::sync::Arc;

use iroh::endpoint::{presets, Connection, RecvStream, RelayMode, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayUrl};
use js_sys::{Function, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// Must match `GUEST_ALPN` in `src/native/node.rs`.
const ALPN: &[u8] = b"flipflop/guest/0";
/// Upload chunk read from a `Blob` at a time.
const CHUNK: f64 = 256.0 * 1024.0;
/// Biggest response head accepted.
const MAX_HEAD: usize = 64 * 1024;

#[wasm_bindgen(start)]
fn start() {
    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );
}

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// A connection to the app.
#[wasm_bindgen]
pub struct Tunnel {
    endpoint: Endpoint,
    conn: Connection,
}

#[wasm_bindgen]
impl Tunnel {
    /// Dial the app `endpoint_id` through `relay_url`, its home relay.
    pub async fn connect(endpoint_id: String, relay_url: String) -> Result<Tunnel, JsError> {
        let id = EndpointId::from_str(&endpoint_id).map_err(js_err)?;
        let relay = RelayUrl::from_str(&relay_url).map_err(js_err)?;
        // The app's relay is ours too: the only one that can reach it.
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(RelayMap::from(relay.clone())))
            .bind()
            .await
            .map_err(js_err)?;
        let addr = EndpointAddr::new(id).with_relay_url(relay);
        let conn = endpoint.connect(addr, ALPN).await.map_err(js_err)?;
        Ok(Tunnel { endpoint, conn })
    }

    /// The connection is gone; make a new one.
    pub fn closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    pub async fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
        self.endpoint.close().await;
    }

    /// One request. `headers` is `[name, value, name, value, …]`. A `body`
    /// is sent in chunks, reporting the bytes sent so far to `progress`;
    /// `signal` aborts it. Resolves once the response head arrived.
    pub async fn request(
        &self,
        method: String,
        path: String,
        headers: Vec<String>,
        body: Option<web_sys::Blob>,
        progress: Option<Function>,
        signal: Option<web_sys::AbortSignal>,
    ) -> Result<Response, JsError> {
        let (mut send, mut recv) = self.conn.open_bi().await.map_err(js_err)?;
        let len = body.as_ref().map_or(0.0, |b| b.size());
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: flipflop\r\nConnection: close\r\n");
        for pair in headers.chunks_exact(2) {
            let (name, value) = (&pair[0], &pair[1]);
            if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                return Err(JsError::new("bad header"));
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {len}\r\n\r\n"));
        send.write_all(head.as_bytes()).await.map_err(js_err)?;
        if let Some(body) = body {
            send_body(&mut send, &body, progress.as_ref(), signal.as_ref()).await?;
        }
        send.finish().map_err(js_err)?;
        Response::read_head(recv_head(&mut recv).await?, recv)
    }
}

async fn send_body(
    send: &mut SendStream,
    body: &web_sys::Blob,
    progress: Option<&Function>,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<(), JsError> {
    let len = body.size();
    let mut sent = 0.0;
    while sent < len {
        if signal.is_some_and(|s| s.aborted()) {
            let _ = send.reset(0u32.into());
            return Err(JsError::new("Cancelled"));
        }
        let end = (sent + CHUNK).min(len);
        let chunk = body
            .slice_with_f64_and_f64(sent, end)
            .map_err(|_| JsError::new("could not read the file"))?;
        let buf = JsFuture::from(chunk.array_buffer())
            .await
            .map_err(|_| JsError::new("could not read the file"))?;
        let bytes = Uint8Array::new(&buf).to_vec();
        send.write_all(&bytes).await.map_err(js_err)?;
        sent += bytes.len() as f64;
        if let Some(progress) = progress {
            let _ = progress.call1(&JsValue::NULL, &JsValue::from_f64(sent));
        }
    }
    Ok(())
}

/// Bytes up to and including the blank line ending the response head (and
/// whatever of the body came along).
async fn recv_head(recv: &mut RecvStream) -> Result<Vec<u8>, JsError> {
    let mut buf = Vec::new();
    loop {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > MAX_HEAD {
            return Err(JsError::new("response head too long"));
        }
        match recv.read_chunk(MAX_HEAD).await.map_err(js_err)? {
            Some(chunk) => buf.extend_from_slice(&chunk),
            None => return Err(JsError::new("connection closed before a response")),
        }
    }
}

/// A response whose body is read chunk by chunk.
#[wasm_bindgen]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    /// Body bytes that came with the head.
    leftover: Option<Vec<u8>>,
    recv: Arc<tokio::sync::Mutex<RecvStream>>,
}

impl Response {
    fn read_head(buf: Vec<u8>, recv: RecvStream) -> Result<Response, JsError> {
        let mut parsed_headers = [httparse::EMPTY_HEADER; 32];
        let mut parsed = httparse::Response::new(&mut parsed_headers);
        let httparse::Status::Complete(head_len) = parsed.parse(&buf).map_err(js_err)? else {
            return Err(JsError::new("incomplete response head"));
        };
        let status = parsed.code.unwrap_or(500);
        let headers = parsed
            .headers
            .iter()
            .map(|h| {
                (
                    h.name.to_ascii_lowercase(),
                    String::from_utf8_lossy(h.value).into_owned(),
                )
            })
            .collect();
        let leftover = buf[head_len..].to_vec();
        Ok(Response {
            status,
            headers,
            leftover: (!leftover.is_empty()).then_some(leftover),
            recv: Arc::new(tokio::sync::Mutex::new(recv)),
        })
    }
}

#[wasm_bindgen]
impl Response {
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
    }

    /// The next piece of the body; `undefined` at its end.
    pub async fn chunk(&mut self) -> Result<Option<Uint8Array>, JsError> {
        if let Some(bytes) = self.leftover.take() {
            return Ok(Some(Uint8Array::from(bytes.as_slice())));
        }
        let mut recv = self.recv.lock().await;
        let chunk = recv.read_chunk(1024 * 1024).await.map_err(js_err)?;
        Ok(chunk.map(|c| Uint8Array::from(&c[..])))
    }

    /// The whole body as text.
    pub async fn text(&mut self) -> Result<String, JsError> {
        let mut body = self.leftover.take().unwrap_or_default();
        let mut recv = self.recv.lock().await;
        while let Some(chunk) = recv.read_chunk(1024 * 1024).await.map_err(js_err)? {
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Stop reading (a cancelled download).
    pub async fn cancel(&self) {
        let _ = self.recv.lock().await.stop(0u32.into());
    }
}
