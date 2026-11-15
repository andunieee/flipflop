package dev.flipflop.app;

import android.media.MediaDataSource;

import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.HashMap;
import java.util.Map;

/**
 * A song on Rust's HTTP server (127.0.0.1), read as if it were a file. Given a URL, Android's player
 * streams it, and a stream it cannot measure (an Ogg file's length is at its end) it cannot seek in,
 * which listening along needs. Given this, it asks for the size and reads where it wants.
 *
 * But the player reads a source like this one on its own thread and waits for it, and while it waits
 * every call to it waits too, the main thread's included: so the song is fetched ahead, all of it, by
 * a thread of its own, and reads are answered from memory. What the player wants before it arrives
 * (an Ogg file's end, first thing) is fetched for it then. A few blocks per request, not one request
 * to the end: a song from another device comes over the one connection to it, and a long stream
 * nobody reads holds up the rest.
 */
class HttpSource extends MediaDataSource {
    private static final int BLOCK = 64 * 1024;
    /** Blocks asked for at a time. */
    private static final int SPAN = 8;
    /** Fetched ahead whole up to this size (a long recording only as the player reads it). */
    private static final long WHOLE = 64L * 1024 * 1024;
    /** A stream from another device can stall (its connection finding another way): give up on it
     * this soon... */
    private static final int TIMEOUT_MS = 8000;
    /** ...and start another from where it stopped, this many times in a row. */
    private static final int TRIES = 6;

    private final String url;
    private volatile long size = -1;
    private volatile boolean closed = false;

    /** Blocks by index, as they arrive; guarded by itself. */
    private final Map<Long, byte[]> blocks = new HashMap<>();

    HttpSource(String url) {
        this.url = url;
        Thread ahead = new Thread(this::fetchAhead, "song ahead");
        ahead.setDaemon(true);
        ahead.start();
    }

    @Override
    public synchronized long getSize() throws IOException {
        if (size < 0) {
            HttpURLConnection head = (HttpURLConnection) new URL(url).openConnection();
            head.setConnectTimeout(TIMEOUT_MS);
            head.setReadTimeout(TIMEOUT_MS);
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
    public int readAt(long position, byte[] buffer, int offset, int length) throws IOException {
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

    @Override
    public void close() {
        closed = true;
    }

    private byte[] kept(long index) {
        synchronized (blocks) {
            return blocks.get(index);
        }
    }

    /** Block `index`, from memory, or fetched now with the few after it. */
    private byte[] block(long index) throws IOException {
        byte[] kept = kept(index);
        if (kept != null) return kept;
        fetch(index);
        kept = kept(index);
        if (kept == null) throw new IOException("the song could not be read");
        return kept;
    }

    /** The whole song, in order, unless it is long; what is there already is not asked again. */
    private void fetchAhead() {
        try {
            long total = getSize();
            if (total > WHOLE) return;
            long count = (total + BLOCK - 1) / BLOCK;
            for (long index = 0; index < count && !closed; index++) {
                if (kept(index) == null) fetch(index);
            }
        } catch (IOException e) {
            // The player's own reads try again, and say so if they cannot.
        }
    }

    /** Fetch block `index` and up to `SPAN - 1` after it that are not there yet, trying again a few
     * times from where a stalled stream stopped. */
    private void fetch(long index) throws IOException {
        long last = Math.min((size + BLOCK - 1) / BLOCK, index + SPAN) - 1;
        while (last > index && kept(last) != null) last--;
        long start = index * BLOCK;
        long end = Math.min(size, (last + 1) * BLOCK);
        byte[] span = new byte[(int) (end - start)];
        int got = 0;
        int failed = 0;
        while (got < span.length && !closed) {
            HttpURLConnection c = (HttpURLConnection) new URL(url).openConnection();
            try {
                c.setConnectTimeout(TIMEOUT_MS);
                c.setReadTimeout(TIMEOUT_MS);
                c.setRequestProperty("Range", "bytes=" + (start + got) + "-" + (end - 1));
                int code = c.getResponseCode();
                if (code != HttpURLConnection.HTTP_PARTIAL) {
                    throw new IOException("the song could not be read from " + (start + got) + " (" + code + ")");
                }
                try (InputStream in = c.getInputStream()) {
                    while (got < span.length) {
                        int n = in.read(span, got, span.length - got);
                        if (n < 0) throw new IOException("the song ended early");
                        got += n;
                        failed = 0;
                    }
                }
            } catch (IOException e) {
                if (++failed >= TRIES) throw e;
            } finally {
                c.disconnect();
            }
        }
        if (got < span.length) throw new IOException("the song was let go");
        synchronized (blocks) {
            for (long i = index; i <= last; i++) {
                int from = (int) ((i - index) * BLOCK);
                int to = (int) Math.min(span.length, from + BLOCK);
                byte[] block = new byte[to - from];
                System.arraycopy(span, from, block, 0, block.length);
                blocks.put(i, block);
            }
        }
    }
}
