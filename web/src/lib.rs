//! The guest page's engine. [`Doc`] is the group's Automerge document, the very `GroupDoc` the app uses, so
//! the page and the app always agree on its layout and its sync protocol. With the `tunnel` feature,
//! [`Tunnel`] is iroh in the browser: it dials the app through its home relay (browsers have no UDP) and
//! carries the document's sync messages on the connection's first stream, and the page's downloads and
//! uploads as HTTP/1.1 exchanges on the following ones, `Connection: close`.

use wasm_bindgen::prelude::*;

#[allow(dead_code)]
#[path = "../../src/groupdoc.rs"]
mod groupdoc;

use groupdoc::GroupDoc;

/// The host's side of the document's sync state.
const HOST: &str = "host";
/// First byte of a message: [`TAG_CONTROL`] + JSON, or [`TAG_SYNC`] + an Automerge sync message.
const TAG_SYNC: u8 = 1;

#[cfg(feature = "tunnel")]
#[wasm_bindgen(start)]
fn start() {
    tracing_wasm::set_as_global_default_with_config(tracing_wasm::WASMLayerConfigBuilder::new().set_max_level(tracing::Level::INFO).build());
}

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// The group's document, as this guest sees it.
#[wasm_bindgen]
pub struct Doc {
    doc: GroupDoc,
}

#[wasm_bindgen]
impl Doc {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Doc {
        Doc { doc: GroupDoc::new() }
    }

    /// A message from the app (the part after its tag byte). Whether the document changed.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<bool, JsError> {
        self.doc.receive_sync(HOST, bytes).map_err(js_err)
    }

    /// The next message for the app, ready to send (tag byte included), if there is something to say.
    pub fn message(&mut self) -> Option<Vec<u8>> {
        let message = self.doc.sync_message(HOST)?;
        let mut frame = vec![TAG_SYNC];
        frame.extend_from_slice(&message);
        Some(frame)
    }

    /// The connection was lost: what we knew of the app's copy no longer holds.
    pub fn reset(&mut self) {
        self.doc.reset_sync(HOST);
    }

    /// Everything the page shows, as JSON.
    pub fn snapshot(&self) -> String {
        let members: Vec<_> = self.doc.members().into_iter().map(|(id, name)| serde_json::json!({ "id": id, "name": name })).collect();
        let files: Vec<_> = self
            .doc
            .files()
            .into_iter()
            .map(|f| {
                serde_json::json!({
                    "id": f.id, "name": f.name, "size": f.size, "from": f.from, "from_name": f.from_name, "ts": f.ts,
                    "stopped": f.stopped, "downloaded": f.downloaded.keys().collect::<Vec<_>>(),
                })
            })
            .collect();
        let texts: Vec<_> = self
            .doc
            .texts()
            .into_iter()
            .map(|t| serde_json::json!({ "id": t.id, "from": t.from, "name": t.name, "text": t.text, "ts": t.ts }))
            .collect();
        serde_json::json!({ "members": members, "files": files, "texts": texts, "scratch": self.doc.scratch() }).to_string()
    }

    pub fn add_text(&mut self, me: &str, name: &str, text: &str) {
        self.doc.add_text(me, name, text);
    }

    pub fn delete_text(&mut self, id: &str) {
        self.doc.delete_text(id);
    }

    /// Make the scratchpad read `text` (only the span that differs is edited). Whether anything changed.
    pub fn set_scratch(&mut self, text: &str) -> bool {
        self.doc.set_scratch(text)
    }
}

impl Default for Doc {
    fn default() -> Self {
        Doc::new()
    }
}

/// Where a cursor at char offset `pos` of `old` ends up in `new`.
#[wasm_bindgen]
pub fn map_cursor(old: &str, new: &str, pos: usize) -> usize {
    groupdoc::map_cursor(old, new, pos)
}

#[cfg(feature = "tunnel")]
pub use tunnel::*;

#[cfg(feature = "tunnel")]
mod tunnel {
    use std::str::FromStr;
    use std::sync::Arc;

    use iroh::endpoint::{Connection, RecvStream, RelayMode, SendStream, presets};
    use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayUrl, SecretKey};
    use js_sys::{Function, Uint8Array};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    use super::js_err;

    /// Must match `GUEST_ALPN` in `src/core/guest.rs`.
    const ALPN: &[u8] = b"flipflop/guest/1";
    /// Upload chunk read from a `Blob` at a time.
    const CHUNK: f64 = 256.0 * 1024.0;
    /// Biggest response head accepted.
    const MAX_HEAD: usize = 64 * 1024;
    /// Biggest sync message accepted.
    const MAX_FRAME: usize = 32 * 1024 * 1024;

    /// A new identity for this browser: 32 random bytes, hex. The page keeps it, so the app names the same
    /// browser the same way every time.
    #[wasm_bindgen]
    pub fn new_secret() -> String {
        SecretKey::generate().to_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn secret_from_hex(hex: &str) -> Result<SecretKey, JsError> {
        let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)).collect::<Result<_, _>>().map_err(js_err)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| JsError::new("bad secret"))?;
        Ok(SecretKey::from_bytes(&bytes))
    }

    /// A connection to the app.
    #[wasm_bindgen]
    pub struct Tunnel {
        endpoint: Endpoint,
        conn: Connection,
    }

    #[wasm_bindgen]
    impl Tunnel {
        /// Dial the app `endpoint_id` through `relay_url`, its home relay, as the identity `secret` (hex).
        pub async fn connect(endpoint_id: String, relay_url: String, secret: String) -> Result<Tunnel, JsError> {
            let id = EndpointId::from_str(&endpoint_id).map_err(js_err)?;
            let relay = RelayUrl::from_str(&relay_url).map_err(js_err)?;
            // The app's relay is ours too: the only one that can reach it.
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(secret_from_hex(&secret)?)
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

        /// Open the stream the document syncs on. It must be the first thing done on a connection.
        pub async fn open_sync(&self, token: String) -> Result<Sync, JsError> {
            let (send, recv) = self.conn.open_bi().await.map_err(js_err)?;
            let sync = Sync {
                send: Arc::new(tokio::sync::Mutex::new(send)),
                recv: Arc::new(tokio::sync::Mutex::new(recv)),
            };
            let join = serde_json::json!({ "t": "join", "token": token }).to_string();
            let mut frame = vec![0u8];
            frame.extend_from_slice(join.as_bytes());
            sync.send(&frame).await?;
            Ok(sync)
        }

        /// One request. `headers` is `[name, value, name, value, …]`. A `body` is sent in chunks, reporting
        /// the bytes sent so far to `progress`; `signal` aborts it. Resolves once the response head arrived.
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

    /// The document's sync stream: whole messages, each prefixed with its length.
    #[wasm_bindgen]
    pub struct Sync {
        send: Arc<tokio::sync::Mutex<SendStream>>,
        recv: Arc<tokio::sync::Mutex<RecvStream>>,
    }

    impl Sync {
        async fn send(&self, frame: &[u8]) -> Result<(), JsError> {
            let mut send = self.send.lock().await;
            send.write_all(&(frame.len() as u32).to_be_bytes()).await.map_err(js_err)?;
            send.write_all(frame).await.map_err(js_err)
        }
    }

    #[wasm_bindgen]
    impl Sync {
        /// A message (tag byte included, as `Doc::message` makes them).
        #[wasm_bindgen(js_name = send)]
        pub async fn send_message(&self, frame: Vec<u8>) -> Result<(), JsError> {
            self.send(&frame).await
        }

        /// The next message from the app; `undefined` once the stream is over.
        pub async fn recv(&self) -> Result<Option<Uint8Array>, JsError> {
            let mut recv = self.recv.lock().await;
            let mut len = [0u8; 4];
            if recv.read_exact(&mut len).await.is_err() {
                return Ok(None);
            }
            let len = u32::from_be_bytes(len) as usize;
            if len > MAX_FRAME {
                return Err(JsError::new("message too long"));
            }
            let mut frame = vec![0u8; len];
            recv.read_exact(&mut frame).await.map_err(js_err)?;
            Ok(Some(Uint8Array::from(frame.as_slice())))
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
            let chunk = body.slice_with_f64_and_f64(sent, end).map_err(|_| JsError::new("could not read the file"))?;
            let buf = JsFuture::from(chunk.array_buffer()).await.map_err(|_| JsError::new("could not read the file"))?;
            let bytes = Uint8Array::new(&buf).to_vec();
            send.write_all(&bytes).await.map_err(js_err)?;
            sent += bytes.len() as f64;
            if let Some(progress) = progress {
                let _ = progress.call1(&JsValue::NULL, &JsValue::from_f64(sent));
            }
        }
        Ok(())
    }

    /// Bytes up to and including the blank line ending the response head (and whatever of the body came along).
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
            let headers = parsed.headers.iter().map(|h| (h.name.to_ascii_lowercase(), String::from_utf8_lossy(h.value).into_owned())).collect();
            let leftover = buf[head_len..].to_vec();
            Ok(Response { status, headers, leftover: (!leftover.is_empty()).then_some(leftover), recv: Arc::new(tokio::sync::Mutex::new(recv)) })
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
            self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone())
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
}
