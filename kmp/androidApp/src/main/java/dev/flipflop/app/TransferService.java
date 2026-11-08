package dev.flipflop.app;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.wifi.WifiManager;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;

import androidx.core.app.NotificationChannelCompat;
import androidx.core.app.NotificationCompat;
import androidx.core.app.NotificationManagerCompat;
import androidx.core.app.ServiceCompat;
import androidx.core.content.ContextCompat;

/**
 * Runs while files are moving, so that Android leaves the process (where the node lives) alone when the
 * app is out of sight, and shows their progress. Holds the CPU and Wi-Fi awake too, or a transfer stalls
 * once the screen goes off. Rust says what is moving; everything here is on the main thread.
 */
public class TransferService extends Service {
    private static final String TAG = "flipflop";
    private static final String CHANNEL = "transfers";
    private static final int NOTIFICATION = 1;
    /** Renewed by every update, so a stalled transfer does not keep the phone awake for long. */
    private static final long AWAKE_MS = 10 * 60_000;

    private static TransferService running;
    private static boolean starting = false;
    private static String title = "";
    private static String text = "";
    private static int percent = -1;

    private PowerManager.WakeLock wakeLock;
    private WifiManager.WifiLock wifiLock;

    /** What is moving now; an empty title for nothing. */
    static void update(Context context, String newTitle, String newText, int newPercent) {
        title = newTitle;
        text = newText;
        percent = newPercent;
        sync(context);
    }

    static boolean wanted() {
        return !title.isEmpty();
    }

    /** Make the service match what is moving. Android 12+ refuses to start it from the background. */
    static void sync(Context context) {
        if (running != null) {
            if (wanted()) running.show();
            else running.finish();
            return;
        }
        // A service asked to start must reach startForeground, so it can't be called off before that:
        // `onStartCommand` looks again.
        if (!wanted() || starting) return;
        Context app = context.getApplicationContext();
        try {
            starting = true;
            ContextCompat.startForegroundService(app, new Intent(app, TransferService.class));
        } catch (IllegalStateException e) {
            starting = false;
            Log.w(TAG, "cannot keep running in the background", e);
        }
    }

    @Override
    @SuppressWarnings("deprecation") // the low latency mode that replaces it needs the screen on
    public void onCreate() {
        super.onCreate();
        NotificationManagerCompat.from(this).createNotificationChannel(
                new NotificationChannelCompat.Builder(CHANNEL, NotificationManagerCompat.IMPORTANCE_LOW)
                        .setName("File transfers")
                        .build());
        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "flipflop:transfers");
        wakeLock.setReferenceCounted(false);
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "flipflop:transfers");
            wifiLock.setReferenceCounted(false);
            wifiLock.acquire();
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        starting = false;
        try {
            ServiceCompat.startForeground(this, NOTIFICATION, notification(),
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } catch (RuntimeException e) {
            Log.w(TAG, "cannot keep running in the background", e);
            stopSelf();
            return START_NOT_STICKY;
        }
        running = this;
        sync(this);
        // If Android kills us anyway the node is gone with us, and there is nothing to come back for.
        return START_NOT_STICKY;
    }

    private void show() {
        wakeLock.acquire(AWAKE_MS);
        NotificationManager manager = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager != null) manager.notify(NOTIFICATION, notification());
    }

    private void finish() {
        running = null;
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    private Notification notification() {
        Intent open = new Intent(this, MainActivity.class)
                .setAction(Intent.ACTION_MAIN)
                .addCategory(Intent.CATEGORY_LAUNCHER);
        return new NotificationCompat.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(wanted() ? title : "flipflop")
                .setContentText(text)
                .setProgress(100, Math.max(percent, 0), percent < 0)
                .setContentIntent(PendingIntent.getActivity(this, 0, open,
                        PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT))
                .setCategory(NotificationCompat.CATEGORY_PROGRESS)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .build();
    }

    @Override
    public void onDestroy() {
        if (running == this) running = null;
        if (wakeLock != null && wakeLock.isHeld()) wakeLock.release();
        if (wifiLock != null && wifiLock.isHeld()) wifiLock.release();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
