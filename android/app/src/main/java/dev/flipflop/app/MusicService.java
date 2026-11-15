package dev.flipflop.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.MediaPlayer;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.net.wifi.WifiManager;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.os.SystemClock;
import android.util.Log;

import androidx.core.app.ServiceCompat;
import androidx.core.content.ContextCompat;

/**
 * Plays the music tab's songs with Android's own player, as a foreground service, so the music goes on
 * with the screen off and the app out of sight. The notification, the lock screen and a headset's buttons
 * (through a media session) pause, skip and stop it. Rust keeps the queue: it says what to play (an address
 * on its own HTTP server, 127.0.0.1) and hears back what happened, with the ticket it gave. Everything here
 * is on the main thread.
 */
public class MusicService extends Service implements MediaPlayer.OnPreparedListener,
        MediaPlayer.OnCompletionListener, MediaPlayer.OnErrorListener, AudioManager.OnAudioFocusChangeListener {
    private static final String TAG = "flipflop";
    private static final String CHANNEL = "music";
    private static final int NOTIFICATION = 2;

    // What happened to a song; must match the MUSIC_* codes in src/platform.rs.
    static final int EVENT_LOADED = 0;
    static final int EVENT_PAUSED = 1;
    static final int EVENT_PLAYING = 2;
    static final int EVENT_ENDED = 3;
    static final int EVENT_FAILED = 4;

    private static final String ACTION_TOGGLE = "dev.flipflop.app.music.TOGGLE";
    private static final String ACTION_NEXT = "dev.flipflop.app.music.NEXT";
    private static final String ACTION_STOP = "dev.flipflop.app.music.STOP";

    // implemented in src/platform.rs
    private static native void nativeMusicEvent(long ticket, int what, String detail);
    /** "toggle" | "next" | "stop": Rust decides what that means for its queue. */
    private static native void nativeMusicControl(String action);

    // What to play, kept here for the service to pick up once it runs.
    private static boolean wanted = false;
    private static long ticket = 0;
    private static String url = null;
    private static String title = "";
    private static String artist = "";
    private static boolean paused = false;
    /** Where to start the song, in ms (listening along starts it midway). */
    private static long startAt = 0;
    /** How fast to play, the pitch kept (listening along catches up a little faster). */
    private static float speed = 1f;

    private static MusicService running;
    private static boolean starting = false;

    /** Where the song was (ms), when (on `SystemClock.elapsedRealtime`), and how fast it went on from
     * there: where it is now, for any thread, without asking the player, which can be busy a while. */
    private static final class Clock {
        final long position;
        final long at;
        final float rate;

        Clock(long position, long at, float rate) {
            this.position = position;
            this.at = at;
            this.rate = rate;
        }
    }
    private static volatile Clock clock = null;
    /** How often the clock is set again from the player while it plays. */
    private static final long CLOCK_EVERY_MS = 1000;
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            mark();
            if (player != null && prepared && !paused) main.postDelayed(this, CLOCK_EVERY_MS);
        }
    };

    private MediaPlayer player;
    /** The player has the song and can start. */
    private boolean prepared = false;
    /** Paused because another app took the sound for a moment: resume when it gives it back. */
    private boolean pausedForFocus = false;
    private MediaSession session;
    private AudioManager audio;
    private AudioFocusRequest focus;
    private WifiManager.WifiLock wifiLock;
    private boolean noisyRegistered = false;

    /** Headphones unplugged: pause, as every player does. */
    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            pauseByItself();
        }
    };

    /** Play `newUrl` in place of what plays, from `newStartAt` ms on. */
    static void play(Context context, long newTicket, String newUrl, String newTitle, String newArtist, long newStartAt) {
        wanted = true;
        startAt = newStartAt;
        ticket = newTicket;
        url = newUrl;
        title = newTitle;
        artist = newArtist;
        paused = false;
        if (running != null) {
            running.load();
            return;
        }
        if (starting) return;
        Context app = context.getApplicationContext();
        try {
            starting = true;
            ContextCompat.startForegroundService(app, new Intent(app, MusicService.class));
        } catch (IllegalStateException e) {
            starting = false;
            Log.w(TAG, "cannot play in the background", e);
            nativeMusicEvent(newTicket, EVENT_FAILED, "Android does not let the app start playing now");
        }
    }

    static void pause(boolean newPaused) {
        paused = newPaused;
        if (running != null) running.applyPause();
    }

    /** Where the song is, in ms; -1 before it can play. Called from any thread. */
    static long position() {
        Clock c = clock;
        if (c == null) return -1;
        return c.position + (long) ((SystemClock.elapsedRealtime() - c.at) * c.rate);
    }

    /** Set the clock from the player. */
    private void mark() {
        if (player == null || !prepared) {
            clock = null;
            return;
        }
        try {
            float rate = player.isPlaying() ? speed : 0f;
            clock = new Clock(player.getCurrentPosition(), SystemClock.elapsedRealtime(), rate);
        } catch (IllegalStateException e) {
            clock = null;
        }
    }

    /** Play faster or slower; 1 is as recorded. */
    static void speed(float newSpeed) {
        speed = newSpeed;
        if (running != null) running.applySpeed();
    }

    /** Go to `ms` in the song; before it can play, start there. */
    static void seek(long ms) {
        if (running == null || running.player == null || !running.prepared) {
            startAt = ms;
            return;
        }
        running.player.seekTo(ms, MediaPlayer.SEEK_CLOSEST);
        // Where it lands, until the player says.
        Clock c = clock;
        clock = new Clock(ms, SystemClock.elapsedRealtime(), c == null ? 0f : c.rate);
    }

    static void stop() {
        wanted = false;
        url = null;
        if (running != null) running.finish();
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager != null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, "Music", NotificationManager.IMPORTANCE_LOW);
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }
        audio = getSystemService(AudioManager.class);
        focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(attributes())
                .setOnAudioFocusChangeListener(this)
                .setWillPauseWhenDucked(false)
                .build();
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            // The song comes over the network: the Wi-Fi must not doze with the screen off.
            wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "flipflop:music");
            wifiLock.setReferenceCounted(false);
        }
        session = new MediaSession(this, "flipflop");
        session.setCallback(new MediaSession.Callback() {
            @Override
            public void onPlay() {
                if (paused) nativeMusicControl("toggle");
            }

            @Override
            public void onPause() {
                if (!paused) nativeMusicControl("toggle");
            }

            @Override
            public void onSkipToNext() {
                nativeMusicControl("next");
            }

            @Override
            public void onStop() {
                nativeMusicControl("stop");
            }
        });
    }

    private static AudioAttributes attributes() {
        return new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_TOGGLE.equals(action)) {
            nativeMusicControl("toggle");
        } else if (ACTION_NEXT.equals(action)) {
            nativeMusicControl("next");
        } else if (ACTION_STOP.equals(action)) {
            nativeMusicControl("stop");
        }
        if (running == this) return START_NOT_STICKY;
        starting = false;
        try {
            ServiceCompat.startForeground(this, NOTIFICATION, notification(),
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } catch (RuntimeException e) {
            Log.w(TAG, "cannot play in the background", e);
            if (wanted) nativeMusicEvent(ticket, EVENT_FAILED, "Android does not let the app play now");
            stopSelf();
            return START_NOT_STICKY;
        }
        running = this;
        if (wanted) load();
        else finish();
        // The queue lives in the process: if Android kills it, there is nothing to come back to.
        return START_NOT_STICKY;
    }

    private void load() {
        release();
        final long mine = ticket;
        player = new MediaPlayer();
        player.setAudioAttributes(attributes());
        player.setWakeMode(this, PowerManager.PARTIAL_WAKE_LOCK);
        player.setOnPreparedListener(this);
        player.setOnCompletionListener(this);
        player.setOnErrorListener(this);
        player.setOnSeekCompleteListener(mp -> {
            if (mp == player) mark();
        });
        try {
            // Not the URL: a stream it cannot measure, it cannot seek in.
            player.setDataSource(new HttpSource(url));
            player.prepareAsync();
        } catch (Exception e) {
            release();
            nativeMusicEvent(mine, EVENT_FAILED, "the player could not open it");
            return;
        }
        if (wifiLock != null) wifiLock.acquire();
        if (!noisyRegistered) {
            registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
            noisyRegistered = true;
        }
        session.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, artist)
                .build());
        session.setActive(true);
        show(PlaybackState.STATE_BUFFERING);
    }

    @Override
    public void onPrepared(MediaPlayer mp) {
        if (mp != player) return;
        prepared = true;
        if (startAt > 0) mp.seekTo(startAt, MediaPlayer.SEEK_CLOSEST);
        clock = new Clock(startAt, SystemClock.elapsedRealtime(), 0f);
        startAt = 0;
        nativeMusicEvent(ticket, EVENT_LOADED, "");
        applyPause();
    }

    @Override
    public void onCompletion(MediaPlayer mp) {
        if (mp != player) return;
        nativeMusicEvent(ticket, EVENT_ENDED, "");
    }

    @Override
    public boolean onError(MediaPlayer mp, int what, int extra) {
        if (mp != player) return true;
        long mine = ticket;
        release();
        nativeMusicEvent(mine, EVENT_FAILED, "Android's player cannot play it (" + what + ", " + extra + ")");
        return true;
    }

    /** Make the player match `paused`, as Rust asked. */
    private void applyPause() {
        if (player == null || !prepared) return;
        if (paused) {
            if (player.isPlaying()) player.pause();
        } else if (audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            pausedForFocus = false;
            player.setVolume(1f, 1f);
            player.start();
            applySpeed();
        } else {
            // Someone else holds the sound (a call): stay paused, and say so.
            paused = true;
            nativeMusicEvent(ticket, EVENT_PAUSED, "");
        }
        mark();
        main.removeCallbacks(tick);
        if (!paused) main.postDelayed(tick, CLOCK_EVERY_MS);
        show(paused ? PlaybackState.STATE_PAUSED : PlaybackState.STATE_PLAYING);
    }

    /** Only while it plays: a paused player given a speed starts. */
    private void applySpeed() {
        if (player == null || !prepared || !player.isPlaying()) return;
        try {
            if (player.getPlaybackParams().getSpeed() != speed) {
                player.setPlaybackParams(player.getPlaybackParams().setSpeed(speed));
                mark();
            }
        } catch (IllegalStateException | IllegalArgumentException e) {
            Log.w(TAG, "cannot play at " + speed, e);
        }
    }

    /** Paused by Android or the user's surroundings, not by Rust: tell it. */
    private void pauseByItself() {
        if (paused || player == null) return;
        paused = true;
        applyPause();
        nativeMusicEvent(ticket, EVENT_PAUSED, "");
    }

    @Override
    public void onAudioFocusChange(int change) {
        if (player == null) return;
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
                pauseByItself();
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                if (!paused) {
                    pauseByItself();
                    pausedForFocus = true;
                }
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK:
                if (prepared) player.setVolume(0.2f, 0.2f);
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                if (prepared) player.setVolume(1f, 1f);
                if (pausedForFocus && paused) {
                    pausedForFocus = false;
                    paused = false;
                    applyPause();
                    nativeMusicEvent(ticket, EVENT_PLAYING, "");
                }
                break;
            default:
                break;
        }
    }

    private void show(int state) {
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                        | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_STOP)
                .setState(state, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 1f)
                .build());
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager != null) manager.notify(NOTIFICATION, notification());
    }

    private PendingIntent action(String action) {
        Intent intent = new Intent(this, MusicService.class).setAction(action);
        return PendingIntent.getService(this, action.hashCode(), intent,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private Notification notification() {
        Intent open = new Intent(this, FlipflopActivity.class)
                .setAction(Intent.ACTION_MAIN)
                .addCategory(Intent.CATEGORY_LAUNCHER);
        Notification.Action toggle = new Notification.Action.Builder(
                Icon.createWithResource(this, paused ? android.R.drawable.ic_media_play : android.R.drawable.ic_media_pause),
                paused ? "Play" : "Pause", action(ACTION_TOGGLE)).build();
        Notification.Action next = new Notification.Action.Builder(
                Icon.createWithResource(this, android.R.drawable.ic_media_next), "Next", action(ACTION_NEXT)).build();
        Notification.Action stop = new Notification.Action.Builder(
                Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel), "Stop", action(ACTION_STOP)).build();
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(title.isEmpty() ? "FlipFlop" : title)
                .setContentText(artist)
                .setContentIntent(PendingIntent.getActivity(this, 0, open,
                        PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT))
                .setDeleteIntent(action(ACTION_STOP))
                .setCategory(Notification.CATEGORY_TRANSPORT)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOnlyAlertOnce(true)
                .addAction(toggle)
                .addAction(next)
                .addAction(stop)
                .setStyle(new Notification.MediaStyle()
                        .setMediaSession(session.getSessionToken())
                        .setShowActionsInCompactView(0, 1))
                .build();
    }

    /** Let the current song go (not the service). */
    private void release() {
        prepared = false;
        clock = null;
        main.removeCallbacks(tick);
        if (player != null) {
            player.release();
            player = null;
        }
    }

    private void finish() {
        running = null;
        release();
        audio.abandonAudioFocusRequest(focus);
        if (wifiLock != null && wifiLock.isHeld()) wifiLock.release();
        if (noisyRegistered) {
            unregisterReceiver(noisy);
            noisyRegistered = false;
        }
        session.setActive(false);
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        if (running == this) running = null;
        release();
        if (audio != null) audio.abandonAudioFocusRequest(focus);
        if (wifiLock != null && wifiLock.isHeld()) wifiLock.release();
        if (noisyRegistered) unregisterReceiver(noisy);
        session.release();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
