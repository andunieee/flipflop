//! Streaming zip writer for browser downloads of folders and multi-file
//! sends.
//!
//! Entries are stored (no compression: most of what people send is already
//! compressed) and written front to back with data descriptors, so nothing
//! has to be read twice and no output needs seeking. Header sizes depend only
//! on names and file sizes, so the archive's exact length is known before the
//! first byte, which lets the browser show real progress. Zip64 records are
//! used only where a size or offset needs them.

use std::io;
use std::path::PathBuf;

use bytes::Bytes;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

/// One archive member.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Path inside the archive, `/`-separated; directories end with `/`.
    pub name: String,
    /// The file to read; `None` for a directory.
    pub source: Option<PathBuf>,
    pub size: u64,
    /// MS-DOS (time, date) of the last modification.
    pub dos_time: (u16, u16),
}

const MAX32: u64 = 0xFFFF_FFFF;
const CHUNK: usize = 256 * 1024;

/// Flags: sizes and CRC follow the data (bit 3); names are UTF-8 (bit 11).
const FLAGS: u16 = 0x0008 | 0x0800;

fn needs_zip64(entry: &Entry, offset: u64) -> bool {
    entry.size >= MAX32 || offset >= MAX32
}

fn version_needed(zip64: bool, dir: bool) -> u16 {
    match (zip64, dir) {
        (true, _) => 45,
        (false, true) => 20,
        (false, false) => 10,
    }
}

fn local_header(entry: &Entry, zip64: bool) -> Vec<u8> {
    let name = entry.name.as_bytes();
    let mut out = Vec::with_capacity(30 + name.len() + 20);
    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&version_needed(zip64, entry.source.is_none()).to_le_bytes());
    out.extend_from_slice(&FLAGS.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // stored
    out.extend_from_slice(&entry.dos_time.0.to_le_bytes());
    out.extend_from_slice(&entry.dos_time.1.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // crc: in the descriptor
    let unknown = if zip64 { MAX32 as u32 } else { 0 };
    out.extend_from_slice(&unknown.to_le_bytes()); // compressed size
    out.extend_from_slice(&unknown.to_le_bytes()); // size
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&(if zip64 { 20u16 } else { 0 }).to_le_bytes());
    out.extend_from_slice(name);
    if zip64 {
        out.extend_from_slice(&0x0001u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
    }
    out
}

fn descriptor(crc: u32, size: u64, zip64: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    if zip64 {
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    } else {
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
    }
    out
}

fn central_header(entry: &Entry, crc: u32, offset: u64, zip64: bool) -> Vec<u8> {
    let name = entry.name.as_bytes();
    let dir = entry.source.is_none();
    let mut out = Vec::with_capacity(46 + name.len() + 28);
    out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes()); // made by: MS-DOS, 4.5
    out.extend_from_slice(&version_needed(zip64, dir).to_le_bytes());
    out.extend_from_slice(&FLAGS.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&entry.dos_time.0.to_le_bytes());
    out.extend_from_slice(&entry.dos_time.1.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    let (size32, offset32) = if zip64 {
        (MAX32 as u32, MAX32 as u32)
    } else {
        (entry.size as u32, offset as u32)
    };
    out.extend_from_slice(&size32.to_le_bytes());
    out.extend_from_slice(&size32.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&(if zip64 { 28u16 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    out.extend_from_slice(&(if dir { 0x10u32 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&offset32.to_le_bytes());
    out.extend_from_slice(name);
    if zip64 {
        out.extend_from_slice(&0x0001u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out
}

fn end_records(count: u64, cd_offset: u64, cd_size: u64, any_zip64: bool) -> Vec<u8> {
    let zip64 = any_zip64 || count >= 0xFFFF || cd_offset >= MAX32 || cd_size >= MAX32;
    let mut out = Vec::with_capacity(98);
    if zip64 {
        let record_offset = cd_offset + cd_size;
        out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());

        out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&record_offset.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let (count16, size32, offset32) = if zip64 {
        (0xFFFF, MAX32 as u32, MAX32 as u32)
    } else {
        (count as u16, cd_size as u32, cd_offset as u32)
    };
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&size32.to_le_bytes());
    out.extend_from_slice(&offset32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Exact length of the archive [`write`] produces for `entries`.
pub fn archive_len(entries: &[Entry]) -> u64 {
    let mut offset = 0u64;
    let mut central = 0u64;
    let mut any_zip64 = false;
    for entry in entries {
        let zip64 = needs_zip64(entry, offset);
        any_zip64 |= zip64;
        central += central_header(entry, 0, offset, zip64).len() as u64;
        offset += local_header(entry, zip64).len() as u64
            + entry.size
            + descriptor(0, entry.size, zip64).len() as u64;
    }
    offset + central + end_records(entries.len() as u64, offset, central, any_zip64).len() as u64
}

/// Stream the archive of `entries` into `tx`; `sent` is told the running
/// byte count after each chunk. Fails if a file is not the size it had when
/// `entries` was made (the announced length would be wrong).
pub async fn write(
    entries: &[Entry],
    tx: &mpsc::Sender<io::Result<Bytes>>,
    mut sent: impl FnMut(u64),
) -> io::Result<()> {
    let closed = || io::Error::new(io::ErrorKind::BrokenPipe, "download closed");
    let mut offset = 0u64;
    let mut central = Vec::new();
    let mut any_zip64 = false;
    for entry in entries {
        let zip64 = needs_zip64(entry, offset);
        any_zip64 |= zip64;
        let start = offset;
        let header = local_header(entry, zip64);
        offset += header.len() as u64;
        tx.send(Ok(header.into())).await.map_err(|_| closed())?;

        let mut crc = crc32fast::Hasher::new();
        if let Some(path) = &entry.source {
            let mut file = tokio::fs::File::open(path).await?;
            let mut left = entry.size;
            while left > 0 {
                let mut buf = vec![0u8; CHUNK.min(left as usize)];
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("{} got shorter", path.display()),
                    ));
                }
                buf.truncate(n);
                crc.update(&buf);
                left -= n as u64;
                offset += n as u64;
                tx.send(Ok(buf.into())).await.map_err(|_| closed())?;
                sent(offset);
            }
        }
        let crc = crc.finalize();
        let tail = descriptor(crc, entry.size, zip64);
        offset += tail.len() as u64;
        tx.send(Ok(tail.into())).await.map_err(|_| closed())?;
        central.extend_from_slice(&central_header(entry, crc, start, zip64));
    }
    let central_len = central.len() as u64;
    let mut end = central;
    end.extend_from_slice(&end_records(
        entries.len() as u64,
        offset,
        central_len,
        any_zip64,
    ));
    offset += end.len() as u64;
    tx.send(Ok(end.into())).await.map_err(|_| closed())?;
    sent(offset);
    Ok(())
}

/// MS-DOS (time, date) of a modification time, in local time.
pub fn dos_time(modified: Option<std::time::SystemTime>) -> (u16, u16) {
    use chrono::{Datelike, Local, Timelike};
    let at: chrono::DateTime<Local> = modified
        .map(Into::into)
        .unwrap_or_else(Local::now);
    // MS-DOS dates start in 1980.
    if at.year() < 1980 {
        return (0, 0x21);
    }
    let time = ((at.hour() << 11) | (at.minute() << 5) | (at.second() / 2)) as u16;
    let date = ((((at.year() - 1980) as u32) << 9) | (at.month() << 5) | at.day()) as u16;
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn archive(entries: &[Entry]) -> Vec<u8> {
        let (tx, mut rx) = mpsc::channel(4);
        let entries = entries.to_vec();
        let writer = tokio::spawn(async move { write(&entries, &tx, |_| {}).await });
        let mut out = Vec::new();
        while let Some(chunk) = rx.recv().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        writer.await.unwrap().unwrap();
        out
    }

    #[tokio::test]
    async fn length_is_known_up_front() {
        let dir = std::env::temp_dir().join(format!("tm-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello zip").unwrap();
        std::fs::write(dir.join("b.bin"), vec![7u8; 300_000]).unwrap();
        let entries = vec![
            Entry {
                name: "photos/".into(),
                source: None,
                size: 0,
                dos_time: dos_time(None),
            },
            Entry {
                name: "photos/a.txt".into(),
                source: Some(dir.join("a.txt")),
                size: 9,
                dos_time: dos_time(None),
            },
            Entry {
                name: "photos/b.bin".into(),
                source: Some(dir.join("b.bin")),
                size: 300_000,
                dos_time: dos_time(None),
            },
        ];
        let bytes = archive(&entries).await;
        assert_eq!(bytes.len() as u64, archive_len(&entries));
        assert_eq!(&bytes[..4], &0x0403_4b50u32.to_le_bytes());
        // End of central directory: 3 entries, no zip64 needed.
        let end = &bytes[bytes.len() - 22..];
        assert_eq!(&end[..4], &0x0605_4b50u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([end[10], end[11]]), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn huge_entries_switch_to_zip64() {
        let big = Entry {
            name: "big.iso".into(),
            source: Some("/dev/null".into()),
            size: 5 << 30,
            dos_time: (0, 0x21),
        };
        assert!(needs_zip64(&big, 0));
        let small = Entry { size: 1, ..big.clone() };
        assert!(!needs_zip64(&small, 0));
        assert!(needs_zip64(&small, 5 << 30));
        // header + data + descriptor, central record, zip64 end + locator + end.
        let expected = (30 + 7 + 20) + (5 << 30) + 24 + (46 + 7 + 28) + 56 + 20 + 22;
        assert_eq!(archive_len(&[big]), expected);
    }
}
