package dev.flipflop.app;

import android.media.MediaDataSource;

import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * A song on Rust's HTTP server (127.0.0.1), read as if it were a file. Given a URL, Android's player
 * streams it, and a stream it cannot measure (an Ogg file's length is at its end) it cannot seek in,
 * which listening along needs. Given this, it asks for the size and reads where it wants.
 *
 * Read in blocks, the last few kept (players read their headers over and over), from one stream
 * that goes on to the end: a song from another device comes over the network, and each new range
 * starts a new transfer, so only a jump opens another. Called on the player's own thread.
 */
class HttpSource extends MediaDataSource {
    private static final int BLOCK = 64 * 1024;
    private static final int KEPT = 32;

    private final String url;
    private long size = -1;

    /** Blocks by index, least recently used first. */
    private final LinkedHashMap<Long, byte[]> blocks = new LinkedHashMap<Long, byte[]>(KEPT, 0.75f, true) {
        @Override
        protected boolean removeEldestEntry(Map.Entry<Long, byte[]> eldest) {
            return size() > KEPT;
        }
    };

    private HttpURLConnection connection;
    private InputStream stream;
    /** Where `stream` is in the song. */
    private long streamAt = -1;

    HttpSource(String url) {
        this.url = url;
    }

    @Override
    public synchronized long getSize() throws IOException {
        if (size < 0) {
            HttpURLConnection head = (HttpURLConnection) new URL(url).openConnection();
            try {
                head.setRequestMethod("HEAD");
                if (head.getResponseCode() != HttpURLConnection.HTTP_OK) {
                    throw new IOException("the song is not there (" + head.getResponseCode() + ")");
                }
                size = head.getContentLengthLong();
            } finally {
                head.disconnect();
            }
        }
        return size;
    }

    @Override
    public synchronized int readAt(long position, byte[] buffer, int offset, int length) throws IOException {
        long total = getSize();
        if (position >= total) return -1;
        int done = 0;
        while (done < length && position + done < total) {
            long at = position + done;
            byte[] block = block(at / BLOCK);
            int from = (int) (at % BLOCK);
            if (from >= block.length) break;
            int n = Math.min(length - done, block.length - from);
            System.arraycopy(block, from, buffer, offset + done, n);
            done += n;
        }
        return done == 0 ? -1 : done;
    }

    private byte[] block(long index) throws IOException {
        byte[] kept = blocks.get(index);
        if (kept != null) return kept;
        long start = index * BLOCK;
        int want = (int) Math.min(BLOCK, size - start);
        if (stream == null || streamAt != start) open(start);
        byte[] block = new byte[want];
        int got = 0;
        while (got < want) {
            int n = stream.read(block, got, want - got);
            if (n < 0) {
                close();
                throw new IOException("the song ended early");
            }
            got += n;
        }
        streamAt = start + want;
        blocks.put(index, block);
        return block;
    }

    /** A stream from `start` to the end of the song. */
    private void open(long start) throws IOException {
        close();
        HttpURLConnection c = (HttpURLConnection) new URL(url).openConnection();
        c.setRequestProperty("Range", "bytes=" + start + "-");
        int code = c.getResponseCode();
        if (code != HttpURLConnection.HTTP_PARTIAL) {
            c.disconnect();
            throw new IOException("the song could not be read from " + start + " (" + code + ")");
        }
        connection = c;
        stream = c.getInputStream();
        streamAt = start;
    }

    @Override
    public synchronized void close() {
        if (stream != null) {
            try {
                stream.close();
            } catch (IOException ignored) {
            }
        }
        if (connection != null) connection.disconnect();
        stream = null;
        connection = null;
        streamAt = -1;
    }
}
