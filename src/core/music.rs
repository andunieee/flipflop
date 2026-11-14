//! The music tab: audio files offered to be played, not downloaded. An offer is shaped like a file
//! offer, of kind [`MUSIC_KIND`], but sits in the group document's own `music` map (see
//! [`crate::groupdoc`]), so peers without the music tab never see it, let alone fetch it. A folder
//! is offered whole as an album ([`ALBUM_KIND`]): it travels as a folder does (its listing, then its
//! files' bytes), and the offer lists its songs with where each one starts in that stream, so a song
//! of it is a range of the album like any other.
//!
//! Players get a song from [`Core::music_source`]: the address of a small HTTP server on 127.0.0.1
//! that answers each request, ranges included (players seek with them), from the file itself when
//! the song is ours, or else with a download from the offerer starting at the requested byte (see
//! [`Core::fetch`]'s wire format). Nothing is kept: every seek is a new request. The guest server
//! answers the same way for browsers ([`serve_music`]).
//!
//! - `GET|HEAD /<token>/<group>/<file>/<track>/<name>`: a song (track 0) or an album's song

use std::{convert::Infallible, io, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use async_compression::tokio::bufread::ZstdDecoder;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream, StreamExt};
use http_body_util::StreamBody;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{self, HeaderName, HeaderValue},
};
use hyper_util::rt::TokioIo;
use iroh::EndpointId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, BufReader};

pub(super) use crate::groupdoc::MUSIC_KIND;
use crate::groupdoc::{ALBUM_KIND, FileOffer, Track, is_music};

use super::{
    Core,
    sharing::{DownloadRequest, open_source, walk_folder},
};

/// What can be offered to the music tab: audio that desktop players, Android's and browsers' play.
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wav"];
const CHUNK: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Music {
    /// `http://127.0.0.1:<port>/<token>`, once the server runs.
    server: tokio::sync::OnceCell<String>,
}

fn extension(name: &str) -> Option<String> {
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

fn is_audio(name: &str) -> bool {
    extension(name).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// The songs of the folder at `root`, in path order, with where each starts in the folder's stream.
pub(super) fn album_tracks(root: &std::path::Path) -> Result<Vec<Track>> {
    let (listing, _) = walk_folder(root)?;
    let mut offset = listing.encode().len() as u64;
    let mut tracks = Vec::new();
    for (path, size) in listing.files {
        if is_audio(&path) {
            tracks.push(Track { path, size, offset });
        }
        offset += size;
    }
    if tracks.is_empty() {
        bail!(
            "there are no songs in {} (only {} files can be played)",
            root.display(),
            AUDIO_EXTENSIONS.join(", ")
        );
    }
    Ok(tracks)
}

/// The songs of an offer: an album's, or the one song it is.
fn tracks_of(offer: &FileOffer) -> Vec<Track> {
    if offer.kind == ALBUM_KIND {
        offer.tracks.clone()
    } else {
        vec![Track {
            path: offer.name.clone(),
            size: offer.size,
            offset: 0,
        }]
    }
}

/// The media type players expect for a song named `name`.
fn audio_type(name: &str) -> &'static str {
    match extension(name).as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

impl Core {
    /// Offer the audio file at `path` to the group's music tab, to be played by the others, or the
    /// folder at `path` as an album of the songs in it (see [`Core::offer_file`] for `name` and
    /// `staged`).
    pub async fn offer_music(
        &self,
        gid: &str,
        path: PathBuf,
        name: Option<String>,
        staged: bool,
    ) -> Result<String> {
        if path.is_dir() {
            return self.offer_file(gid, path, name, ALBUM_KIND, staged).await;
        }
        let shown = name.clone().unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if !is_audio(&shown) {
            bail!(
                "{shown} is not a song: only {} files can be played",
                AUDIO_EXTENSIONS.join(", ")
            );
        }
        self.offer_file(gid, path, name, MUSIC_KIND, staged).await
    }

    /// The address a player opens to play one of the group's songs (`track` 0), or an album's
    /// `track`: a server on this device that streams it from wherever it is.
    pub async fn music_source(&self, gid: &str, fid: &str, track: usize) -> Result<String> {
        self.require_group(gid)?;
        let offer = self
            .with_doc(gid, |d| d.file(fid))
            .filter(|f| is_music(&f.kind))
            .context("no such song")?;
        if offer.stopped {
            bail!("{} is no longer offered", offer.name);
        }
        let song = tracks_of(&offer)
            .into_iter()
            .nth(track)
            .context("no such song")?;
        let name = song.path.rsplit('/').next().unwrap_or_default();
        let base = self
            .0
            .music
            .server
            .get_or_try_init(|| self.start_music_server())
            .await?;
        Ok(format!(
            "{base}/{gid}/{fid}/{track}/{}",
            super::guest::encode(name)
        ))
    }

    async fn start_music_server(&self) -> Result<String> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        // Other users of this machine reach 127.0.0.1 too: only those given an address get an answer.
        let token = crate::groupdoc::random_hex(16);
        let (this, secret) = (self.clone(), token.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                };
                let (core, token) = (this.clone(), secret.clone());
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req| {
                        let (core, token) = (core.clone(), token.clone());
                        async move { Ok::<_, Infallible>(route(&core, &token, req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Ok(format!("http://127.0.0.1:{port}/{token}"))
    }

    /// Our own song's bytes from `from` on, read from where we share it from: the file, or the
    /// album's file at `track`.
    async fn open_own_song(
        &self,
        gid: &str,
        fid: &str,
        track: Option<&str>,
        from: u64,
    ) -> Result<Song> {
        let mut path = self
            .0
            .store
            .lock()
            .unwrap()
            .state
            .shares
            .iter()
            .find(|s| s.group == gid && s.file == fid)
            .map(|s| s.path.clone())
            .context("the song is not where it was")?;
        if let Some(track) = track {
            // Only what the folder holds: anyone in the group can write the track's path.
            let (root, track) = (path, track.to_owned());
            path = tokio::task::spawn_blocking(move || {
                let (listing, paths) = walk_folder(&root)?;
                listing
                    .files
                    .iter()
                    .position(|(p, _)| *p == track)
                    .map(|i| paths[i].clone())
                    .context("the song is not in the album anymore")
            })
            .await??;
        }
        let file = tokio::task::spawn_blocking(move || open_source(&path)).await??;
        let mut file = tokio::fs::File::from_std(file);
        file.seek(io::SeekFrom::Start(from)).await?;
        Ok(Box::new(file))
    }

    /// Ask someone who has the song (or album) for its bytes from `from` on. Returns where the stream starts
    /// (before `from` if the sender can only start earlier) and the stream itself.
    async fn open_remote_song(
        &self,
        gid: &str,
        offer: &crate::groupdoc::FileOffer,
        from: u64,
    ) -> Result<(u64, Song)> {
        let me = self.me();
        let sources: Vec<_> = std::iter::once(&offer.from)
            .chain(&offer.hosts)
            .filter(|id| **id != me)
            .filter_map(|id| id.parse::<EndpointId>().ok())
            .filter_map(|id| self.connection(id))
            .collect();
        let mut last = anyhow!("nobody who has it is online");
        for conn in sources {
            let attempt = async {
                let (mut tx, mut rx) = conn.open_bi().await?;
                let request = serde_json::to_vec(&DownloadRequest {
                    group: gid.to_owned(),
                    file: offer.id.clone(),
                    from,
                    resume: true,
                })?;
                tx.write_all(&request).await?;
                tx.finish()?;
                let mut header = [0u8; 9];
                rx.read_exact(&mut header)
                    .await
                    .map_err(|_| anyhow!("the sender refused or went away"))?;
                let start = match header[0] {
                    1 => bail!("the sender no longer shares it"),
                    2 => {
                        let mut more = [0u8; 16];
                        rx.read_exact(&mut more).await?;
                        u64::from_be_bytes(more[..8].try_into().unwrap())
                    }
                    // An older sender, which starts at the beginning.
                    _ => 0,
                };
                let song: Song = Box::new(RemoteSong {
                    body: ZstdDecoder::new(BufReader::with_capacity(CHUNK, rx)),
                    _tx: tx,
                });
                anyhow::Ok((start, song))
            };
            match attempt.await {
                Ok(opened) => return Ok(opened),
                Err(err) => last = err,
            }
        }
        Err(last)
    }
}

/// A song's bytes, from some offset on.
type Song = Box<dyn AsyncRead + Send + Unpin>;

/// The bytes of someone else's song, as they come from the sender.
struct RemoteSong {
    body: ZstdDecoder<BufReader<iroh::endpoint::RecvStream>>,
    /// Kept until the stream is dropped, which tells the sender to stop.
    _tx: iroh::endpoint::SendStream,
}

impl AsyncRead for RemoteSong {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.body).poll_read(cx, buf)
    }
}

/// An answer to a request for a song: what an HTTP server sends back.
pub(super) struct Served {
    pub status: StatusCode,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// The bytes and how many there are; none for `HEAD`, or for an error (see `error`).
    pub body: Option<(BoxStream<'static, io::Result<Bytes>>, u64)>,
    pub error: Option<String>,
}

impl Served {
    fn error(status: StatusCode, why: impl Into<String>) -> Served {
        Served {
            status,
            headers: vec![],
            body: None,
            error: Some(why.into()),
        }
    }
}

/// `bytes=<start>-[<end>]` (the only kind players send), as an inclusive range within `size`.
fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => size.checked_sub(1)?,
        end => end.parse::<u64>().ok()?.min(size.checked_sub(1)?),
    };
    (start <= end).then_some((start, end))
}

/// Answer a `GET` (or, `head`, a `HEAD`) of the group's song `fid` (`track` 0), or of an album's
/// `track`, for the `Range` asked.
pub(super) async fn serve_music(
    core: &Core,
    gid: &str,
    fid: &str,
    track: usize,
    head: bool,
    range: Option<&str>,
) -> Served {
    let Some((offer, song)) = core
        .has_group(gid)
        .then(|| core.with_doc(gid, |d| d.file(fid)))
        .flatten()
        .filter(|f| is_music(&f.kind) && !f.stopped)
        .and_then(|f| Some((tracks_of(&f).into_iter().nth(track)?, f)))
        .map(|(song, f)| (f, song))
    else {
        return Served::error(StatusCode::NOT_FOUND, "No such song");
    };
    let album = offer.kind == ALBUM_KIND;
    let size = song.size;
    let (start, end) = match range {
        Some(range) => match parse_range(range, size) {
            Some(range) => range,
            None => {
                let mut served = Served::error(StatusCode::RANGE_NOT_SATISFIABLE, "");
                if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                    served.headers.push((header::CONTENT_RANGE, v));
                }
                return served;
            }
        },
        None => (0, size.saturating_sub(1)),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };

    let body = if head || len == 0 {
        None
    } else if offer.from == core.me() {
        match core
            .open_own_song(gid, fid, album.then_some(song.path.as_str()), start)
            .await
        {
            Ok(song) => Some(song),
            Err(err) => return Served::error(StatusCode::NOT_FOUND, format!("{err:#}")),
        }
    } else {
        // Within an album's stream, the song starts at its offset.
        let from = song.offset + start;
        let (at, mut bytes) = match core.open_remote_song(gid, &offer, from).await {
            Ok(opened) => opened,
            Err(err) => return Served::error(StatusCode::BAD_GATEWAY, format!("{err:#}")),
        };
        // A sender that could not start where asked starts earlier: skip what was not asked for.
        if at > from {
            return Served::error(StatusCode::BAD_GATEWAY, "the sender started too late");
        }
        let skip = from - at;
        if skip > 0
            && tokio::io::copy(&mut (&mut bytes).take(skip), &mut tokio::io::sink())
                .await
                .ok()
                != Some(skip)
        {
            return Served::error(StatusCode::BAD_GATEWAY, "the sender went away");
        }
        Some(bytes)
    };
    let body = body.map(|song| {
        let bytes = stream::unfold((song, len), |(mut song, left)| async move {
            if left == 0 {
                return None;
            }
            let mut buf = vec![0u8; CHUNK.min(left as usize)];
            match song.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok(Bytes::from(buf)), (song, left - n as u64)))
                }
                Err(err) => Some((Err(err), (song, 0))),
            }
        });
        (bytes.boxed(), len)
    });

    let mut headers = vec![
        (header::ACCEPT_RANGES, HeaderValue::from_static("bytes")),
        (header::CONTENT_LENGTH, HeaderValue::from(len)),
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static(audio_type(&song.path)),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
    ];
    if range.is_some() {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")) {
            headers.push((header::CONTENT_RANGE, v));
        }
    }
    Served {
        status: if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        headers,
        body,
        error: None,
    }
}

type Body = StreamBody<BoxStream<'static, io::Result<Frame<Bytes>>>>;

async fn route(core: &Core, token: &str, req: Request<Incoming>) -> Response<Body> {
    let path = req.uri().path().to_owned();
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let served = match parts.as_slice() {
        [t, gid, fid, track, ..]
            if *t == token && matches!(*req.method(), Method::GET | Method::HEAD) =>
        {
            let range = req
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok());
            match track.parse() {
                Ok(track) => {
                    serve_music(core, gid, fid, track, *req.method() == Method::HEAD, range).await
                }
                Err(_) => Served::error(StatusCode::NOT_FOUND, "Not found"),
            }
        }
        _ => Served::error(StatusCode::NOT_FOUND, "Not found"),
    };
    let body = match (served.body, served.error) {
        (Some((bytes, _)), _) => bytes.map(|b| b.map(Frame::data)).boxed(),
        (None, Some(why)) => stream::once(async move { Ok(Frame::data(Bytes::from(why))) }).boxed(),
        (None, None) => stream::empty().boxed(),
    };
    let mut res = Response::new(StreamBody::new(body));
    *res.status_mut() = served.status;
    res.headers_mut().extend(served.headers);
    res
}

#[cfg(test)]
mod tests {
    use super::{audio_type, parse_range};

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=4-6", 10), Some((4, 6)));
        assert_eq!(parse_range("bytes=4-60", 10), Some((4, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_range("items=0-", 10), None);
    }

    #[test]
    fn types() {
        assert_eq!(audio_type("Song.MP3"), "audio/mpeg");
        assert_eq!(audio_type("a.opus"), "audio/ogg");
        assert_eq!(audio_type("noext"), "application/octet-stream");
    }
}
