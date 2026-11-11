package dev.flipflop.app;

import android.Manifest;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattServer;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Environment;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.ParcelUuid;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;
import android.util.Log;

import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;

import com.google.zxing.integration.android.IntentIntegrator;
import com.google.zxing.integration.android.IntentResult;

import org.json.JSONArray;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

/**
 * The app's activity. The UI (Freya) and the node are native code (libflipflop_app, started by NativeActivity in
 * {@code android_main}); this provides the Android-only pieces they cannot do from there: the multicast lock for
 * mDNS, runtime permissions, the window insets, pickers and shares, the QR scanner and Bluetooth LE.
 *
 * BLE protocol (shared with src/ble/linux.rs): every device runs a GATT server with one service
 * (SERVICE) and one readable characteristic (PAYLOAD) holding {"n": name, "t": ticket}, advertises
 * the service UUID, and scans for it. Each peer is read once (re-reading would open and close a GATT
 * connection to a device that iroh-ble-transport may be using, which tears its link down) and
 * reported to Rust; peers that stop advertising are reported lost.
 */
public class FlipflopActivity extends android.app.NativeActivity {
    private static final String TAG = "flipflop";
    static final UUID SERVICE = UUID.fromString("f11bf10b-0000-4f1f-9b0a-8e2d6e7a0001");
    static final UUID PAYLOAD = UUID.fromString("f11bf10b-0000-4f1f-9b0a-8e2d6e7a0002");
    static final long RETRY_MS = 5_000;
    static final long LOST_MS = 45_000;
    static final int REQ_BLE = 1;
    static final int REQ_PICK = 2;
    static final int REQ_NOTIFY = 3;

    static {
        // NativeActivity opens the same library (android.app.lib_name) for android_main; loading it here
        // first lets the JVM find the native methods below.
        System.loadLibrary("flipflop_app");
    }

    // implemented in src/platform.rs
    /** Hands the JVM and this activity to the Rust side; must come before anything else. */
    private static native void nativeInit(android.app.Activity activity);
    private static native void nativeQrScanned(String text);
    /**
     * `path`: a file path, or a content:// URI kept in place (then `name` is its display name).
     * `group`: the group it goes to, "" for the open one.
     */
    private static native void nativeFilePicked(String kind, String path, String name, String group);
    /** Another app shared `text` and/or `files` files; the UI asks where they go, then calls {@link #deliverShared}. */
    private static native void nativeShared(String text, int files);
    /** Room taken by the system bars and the keyboard, in logical pixels. */
    private static native void nativeInsets(float left, float top, float right, float bottom);
    private static native void nativeBlePeer(String address, String json);
    private static native void nativeBleLost(String address);
    private static native void nativeBleStatus(String text);

    private final Handler main = new Handler(Looper.getMainLooper());
    private WifiManager.MulticastLock multicastLock;

    private String pickKind = "file";
    /** The files of the last share, until the user picks where they go. */
    private List<Uri> shared = new ArrayList<>();

    // The node outlives an activity while files are moving, and only tells the first one these.
    private static volatile byte[] payload = new byte[0];
    private static boolean bleWanted = false;
    private static boolean notifyAsked = false;
    private static boolean bleAsked = false;
    private boolean visible = false;
    private boolean bleRunning = false;

    private BluetoothAdapter adapter;
    private BluetoothGattServer gattServer;
    private BluetoothLeAdvertiser advertiser;
    private BluetoothLeScanner scanner;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        nativeInit(this);
        // mDNS (LAN discovery) needs multicast packets, which Android filters unless a lock is held.
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            multicastLock = wifi.createMulticastLock("flipflop-mdns");
            multicastLock.setReferenceCounted(false);
            multicastLock.acquire();
        }
        super.onCreate(savedInstanceState);
        edgeToEdge();
        // Reopened from Recents, Android hands us the share we already took care of.
        if (savedInstanceState == null && (getIntent().getFlags() & Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) == 0) {
            takeShare(getIntent());
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        takeShare(intent);
    }

    // ---- window ---------------------------------------------------------------------------

    /**
     * NativeActivity gives the UI the whole window, under the system bars: their size (and the keyboard's) goes
     * to Rust, which keeps the content clear of them. The surface needs focus for key events to arrive.
     */
    @SuppressWarnings("deprecation")
    private void edgeToEdge() {
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        if (Build.VERSION.SDK_INT < 35) {
            getWindow().setStatusBarColor(android.graphics.Color.TRANSPARENT);
            getWindow().setNavigationBarColor(android.graphics.Color.TRANSPARENT);
        }
        if (Build.VERSION.SDK_INT >= 29) {
            getWindow().setStatusBarContrastEnforced(false);
            getWindow().setNavigationBarContrastEnforced(false);
        }
        View decor = getWindow().getDecorView();
        ViewCompat.setOnApplyWindowInsetsListener(decor, (v, insets) -> {
            float density = getResources().getDisplayMetrics().density;
            androidx.core.graphics.Insets bars = insets.getInsets(
                    WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout());
            int ime = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom;
            nativeInsets(bars.left / density, bars.top / density, bars.right / density,
                    Math.max(bars.bottom, ime) / density);
            return insets;
        });
        decor.post(() -> {
            View surface = findSurface(decor);
            if (surface != null) {
                surface.setFocusable(true);
                surface.setFocusableInTouchMode(true);
                surface.requestFocus();
            }
        });
    }

    /** Called from Rust: dark status and navigation bar icons on a light UI, light ones on a dark UI. */
    public void setLightBars(boolean light) {
        runOnUiThread(() -> {
            androidx.core.view.WindowInsetsControllerCompat bars =
                    WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
            bars.setAppearanceLightStatusBars(light);
            bars.setAppearanceLightNavigationBars(light);
        });
    }

    private static View findSurface(View view) {
        if (view instanceof SurfaceView) return view;
        if (view instanceof ViewGroup) {
            ViewGroup group = (ViewGroup) view;
            for (int i = 0; i < group.getChildCount(); i++) {
                View found = findSurface(group.getChildAt(i));
                if (found != null) return found;
            }
        }
        return null;
    }

    // ---- shares from other apps -----------------------------------------------------------

    /** Text or files shared from another app: the UI asks where they go. */
    private void takeShare(Intent intent) {
        String action = intent.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) return;
        CharSequence extra = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        String text = extra == null ? "" : extra.toString().trim();
        List<Uri> uris = streams(intent);
        if (text.isEmpty() && uris.isEmpty()) return;
        shared = uris;
        nativeShared(text, uris.size());
    }

    @SuppressWarnings("deprecation")
    private static List<Uri> streams(Intent intent) {
        List<Uri> uris = new ArrayList<>();
        if (Intent.ACTION_SEND_MULTIPLE.equals(intent.getAction())) {
            ArrayList<Uri> many = Build.VERSION.SDK_INT >= 33
                    ? intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri.class)
                    : intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (many != null) for (Uri uri : many) if (uri != null) uris.add(uri);
        } else {
            Uri one = Build.VERSION.SDK_INT >= 33
                    ? intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class)
                    : intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (one != null) uris.add(one);
        }
        return uris;
    }

    /** Called from Rust: the user picked where the last share's files go. */
    public void deliverShared(String group) {
        runOnUiThread(() -> {
            List<Uri> uris = shared;
            shared = new ArrayList<>();
            if (!uris.isEmpty()) stageShared(uris, group);
        });
    }

    /**
     * The UI's event loop cannot be started again in this process (winit allows one per process), and NativeActivity
     * waits here for it to end, which it never does: the app would hang. So the activity's end is the process's (the
     * manifest handles every configuration change, so that is only when the user or the system closes it). The next
     * launch starts afresh.
     */
    @Override
    protected void onDestroy() {
        stopBle();
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        android.os.Process.killProcess(android.os.Process.myPid());
        super.onDestroy();
    }

    // ---- QR scanning ----------------------------------------------------------------------

    /** Called from Rust. */
    public void startQrScan() {
        runOnUiThread(() -> new IntentIntegrator(this)
                .setDesiredBarcodeFormats(IntentIntegrator.QR_CODE)
                .setPrompt("Scan a flipflop QR code")
                .setBeepEnabled(false)
                .setOrientationLocked(false)
                .initiateScan());
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode == REQ_PICK) {
            if (resultCode == RESULT_OK && data != null) stagePicked(data, pickKind);
            return;
        }
        IntentResult result = IntentIntegrator.parseActivityResult(requestCode, resultCode, data);
        if (result != null) {
            if (result.getContents() != null) nativeQrScanned(result.getContents());
            return;
        }
        super.onActivityResult(requestCode, resultCode, data);
    }

    // ---- file pickers, clipboard, links, apps, downloads ----------------------------------

    /** Called from Rust: "file" picks anything, "media" photos and videos. */
    public void pickFiles(String kind) {
        runOnUiThread(() -> {
            pickKind = kind;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            intent.setType("*/*");
            if ("media".equals(kind)) {
                intent.putExtra(Intent.EXTRA_MIME_TYPES, new String[]{"image/*", "video/*"});
            }
            startActivityForResult(intent, REQ_PICK);
        });
    }

    /**
     * What the user picked. A plain local file is shared where it is: we keep the permission to read
     * its URI, and Rust opens it when needed ({@link #openContentFd}). Anything else (a cloud
     * provider, a stream, no persistable permission) is copied into app storage, and the offerer
     * serves the copy.
     */
    private void stagePicked(Intent data, String kind) {
        List<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        stage(uris, kind, "");
    }

    /** Files another app shared with us, offered to `group`; photos and videos count as media. */
    private void stageShared(List<Uri> uris, String group) {
        stage(uris, null, group);
    }

    /** `kind` null: from each file's type. */
    private void stage(List<Uri> uris, String kind, String group) {
        new Thread(() -> {
            for (Uri uri : uris) {
                try {
                    String k = kind != null ? kind : kindOf(uri);
                    String name = displayName(uri);
                    if (keepInPlace(uri)) {
                        nativeFilePicked(k, uri.toString(), name, group);
                        continue;
                    }
                    File dir = new File(getFilesDir(), "staged/" + System.nanoTime());
                    if (!dir.mkdirs()) continue;
                    File out = new File(dir, name);
                    try (InputStream in = getContentResolver().openInputStream(uri);
                         OutputStream os = new FileOutputStream(out)) {
                        copy(in, os);
                    }
                    nativeFilePicked(k, out.getAbsolutePath(), "", group);
                } catch (Exception e) {
                    Log.w(TAG, "could not read " + uri, e);
                }
            }
        }, "flipflop-stage").start();
    }

    private String kindOf(Uri uri) {
        String mime = getContentResolver().getType(uri);
        if (mime == null) return "file";
        if (mime.startsWith("image/") || mime.startsWith("video/")) return "media";
        if (mime.equals("application/vnd.android.package-archive")) return "apk";
        return "file";
    }

    /** Is this a regular, seekable file we may keep reading later? Takes the permission if so. */
    private boolean keepInPlace(Uri uri) {
        try (ParcelFileDescriptor pfd = getContentResolver().openFileDescriptor(uri, "r")) {
            if (pfd == null) return false;
            StructStat st = Os.fstat(pfd.getFileDescriptor());
            if (!OsConstants.S_ISREG(st.st_mode)) return false;
            Os.lseek(pfd.getFileDescriptor(), 0, OsConstants.SEEK_SET);
            getContentResolver().takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
            return true;
        } catch (Exception e) {
            return false; // no persistable permission, not seekable, ...: copy instead
        }
    }

    /** Called from Rust: a new descriptor (the caller owns it) for a picked file, or -1. */
    public int openContentFd(String uri) {
        try (ParcelFileDescriptor pfd = getContentResolver().openFileDescriptor(Uri.parse(uri), "r")) {
            return pfd == null ? -1 : pfd.detachFd();
        } catch (Exception e) {
            Log.w(TAG, "could not open " + uri, e);
            return -1;
        }
    }

    /** Called from Rust: we no longer share this file. */
    public void releaseContentUri(String uri) {
        try {
            getContentResolver().releasePersistableUriPermission(Uri.parse(uri), Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } catch (Exception ignored) {
        }
    }

    private String displayName(Uri uri) {
        String name = null;
        try (Cursor c = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst()) name = c.getString(0);
        } catch (Exception ignored) {
        }
        if (name == null || name.isEmpty()) name = uri.getLastPathSegment();
        if (name == null || name.isEmpty()) name = "file";
        return name.replace('/', '_');
    }

    private static void copy(InputStream in, OutputStream out) throws java.io.IOException {
        byte[] buf = new byte[64 * 1024];
        int n;
        while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
    }

    /** Called from Rust: JSON array of [apk path, label, detail] for every launchable app. */
    public String listApps() {
        PackageManager pm = getPackageManager();
        List<String[]> apps = new ArrayList<>();
        for (ApplicationInfo info : pm.getInstalledApplications(0)) {
            if (pm.getLaunchIntentForPackage(info.packageName) == null) continue;
            String version = "";
            try {
                PackageInfo pi = pm.getPackageInfo(info.packageName, 0);
                if (pi.versionName != null) version = " · " + pi.versionName;
            } catch (Exception ignored) {
            }
            boolean split = info.splitSourceDirs != null && info.splitSourceDirs.length > 0;
            apps.add(new String[]{info.sourceDir, String.valueOf(pm.getApplicationLabel(info)),
                    info.packageName + version + (split ? " · split APK, may not install alone" : "")});
        }
        apps.sort((a, b) -> a[1].compareToIgnoreCase(b[1]));
        JSONArray out = new JSONArray();
        for (String[] a : apps) {
            JSONArray row = new JSONArray();
            for (String f : a) row.put(f);
            out.put(row);
        }
        return out.toString();
    }

    /** Called from Rust. */
    public void copyText(String text) {
        runOnUiThread(() -> {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm != null) cm.setPrimaryClip(ClipData.newPlainText("flipflop", text));
        });
    }

    /** Called from Rust. */
    public String pasteText() {
        ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null || !cm.hasPrimaryClip() || cm.getPrimaryClip().getItemCount() == 0) return "";
        CharSequence text = cm.getPrimaryClip().getItemAt(0).coerceToText(this);
        return text == null ? "" : text.toString();
    }

    /** Called from Rust. */
    public void openUrl(String url) {
        runOnUiThread(() -> {
            try {
                startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
            } catch (Exception e) {
                Log.w(TAG, "no app for " + url, e);
            }
        });
    }

    /** Called from Rust: open a file saved in the public Downloads folder with whatever app handles it. */
    public void openFile(String path) {
        runOnUiThread(() -> {
            try {
                Uri uri = null;
                try (Cursor c = getContentResolver().query(MediaStore.Files.getContentUri("external"),
                        new String[]{MediaStore.Files.FileColumns._ID}, MediaStore.Files.FileColumns.DATA + "=?",
                        new String[]{path}, null)) {
                    if (c != null && c.moveToFirst()) {
                        uri = Uri.withAppendedPath(MediaStore.Files.getContentUri("external"), String.valueOf(c.getLong(0)));
                    }
                }
                if (uri == null) {
                    Log.w(TAG, "no content URI for " + path);
                    return;
                }
                String mime = MimeTypeMap.getSingleton()
                        .getMimeTypeFromExtension(MimeTypeMap.getFileExtensionFromUrl(Uri.encode(new File(path).getName())));
                Intent view = new Intent(Intent.ACTION_VIEW);
                view.setDataAndType(uri, mime == null ? "*/*" : mime);
                view.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                startActivity(Intent.createChooser(view, null));
            } catch (Exception e) {
                Log.w(TAG, "could not open " + path, e);
            }
        });
    }

    /**
     * Called from Rust when a download finished in app storage: move it to the public
     * Download/flipflop folder. Returns the file's or folder's new location, or "" if it stays where it is.
     */
    public String saveToDownloads(String path) {
        File src = new File(path);
        if (src.isDirectory()) return saveFolderToDownloads(src);
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                ContentResolver cr = getContentResolver();
                Uri uri = insertDownload(src, Environment.DIRECTORY_DOWNLOADS + "/flipflop");
                String name = src.getName();
                try (Cursor c = cr.query(uri, new String[]{MediaStore.Downloads.DISPLAY_NAME}, null, null, null)) {
                    if (c != null && c.moveToFirst()) name = c.getString(0);
                }
                src.delete();
                return Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
                        .getAbsolutePath() + "/flipflop/" + name;
            }
            File dir = new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "flipflop");
            if (!dir.isDirectory() && !dir.mkdirs()) return "";
            File out = new File(dir, src.getName());
            try (InputStream in = new FileInputStream(src); OutputStream os = new FileOutputStream(out)) {
                copy(in, os);
            }
            src.delete();
            return out.getAbsolutePath();
        } catch (Exception e) {
            Log.w(TAG, "could not save " + path + " to Downloads", e);
            return "";
        }
    }

    /** A new entry in the public Downloads under relativePath, with the bytes of src. */
    private Uri insertDownload(File src, String relativePath) throws java.io.IOException {
        String mime = MimeTypeMap.getSingleton()
                .getMimeTypeFromExtension(MimeTypeMap.getFileExtensionFromUrl(Uri.encode(src.getName())));
        ContentValues values = new ContentValues();
        values.put(MediaStore.Downloads.DISPLAY_NAME, src.getName());
        values.put(MediaStore.Downloads.MIME_TYPE, mime == null ? "application/octet-stream" : mime);
        values.put(MediaStore.Downloads.RELATIVE_PATH, relativePath);
        ContentResolver cr = getContentResolver();
        Uri uri = cr.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
        if (uri == null) throw new java.io.IOException("no entry for " + src.getName());
        try (InputStream in = new FileInputStream(src); OutputStream out = cr.openOutputStream(uri)) {
            copy(in, out);
        } catch (Exception e) {
            cr.delete(uri, null, null);
            throw e;
        }
        return uri;
    }

    /** All or nothing: if anything fails, what was copied goes away and the folder stays where it is. */
    private String saveFolderToDownloads(File src) {
        File base = new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "flipflop");
        File dest = new File(base, src.getName());
        for (int n = 2; folderTaken(dest); n++) dest = new File(base, src.getName() + " (" + n + ")");
        List<Uri> inserted = new ArrayList<>();
        try {
            copyFolder(src, dest, Environment.DIRECTORY_DOWNLOADS + "/flipflop/" + dest.getName(), inserted);
        } catch (Exception e) {
            Log.w(TAG, "could not save " + src + " to Downloads", e);
            for (Uri uri : inserted) getContentResolver().delete(uri, null, null);
            deleteTree(dest);
            return "";
        }
        deleteTree(src);
        return dest.getAbsolutePath();
    }

    /** On Android 10 the file system does not show us the public folders; MediaStore shows what we saved there. */
    private boolean folderTaken(File folder) {
        if (Build.VERSION.SDK_INT != 29) return folder.exists();
        String name = folder.getName().replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_");
        try (Cursor c = getContentResolver().query(MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                new String[]{MediaStore.Downloads._ID}, MediaStore.Downloads.RELATIVE_PATH + " LIKE ? ESCAPE '\\'",
                new String[]{Environment.DIRECTORY_DOWNLOADS + "/flipflop/" + name + "/%"}, null)) {
            return c != null && c.moveToFirst();
        }
    }

    /**
     * Android 10 only lets us into the public folders through MediaStore (where empty folders cannot
     * exist). Elsewhere, plain files: MediaStore would rename some (".env" to "_.env"), and then the
     * folder could not be rehosted, its hash having changed.
     */
    private void copyFolder(File from, File to, String relativePath, List<Uri> inserted) throws java.io.IOException {
        boolean mediaStore = Build.VERSION.SDK_INT == 29;
        File[] children = from.listFiles();
        if (children == null) throw new java.io.IOException("cannot read " + from);
        if (!mediaStore && !to.isDirectory() && !to.mkdirs()) throw new java.io.IOException("cannot create " + to);
        for (File child : children) {
            if (child.isDirectory()) {
                copyFolder(child, new File(to, child.getName()), relativePath + "/" + child.getName(), inserted);
            } else if (mediaStore) {
                inserted.add(insertDownload(child, relativePath));
            } else {
                try (InputStream in = new FileInputStream(child);
                     OutputStream out = new FileOutputStream(new File(to, child.getName()))) {
                    copy(in, out);
                }
            }
        }
    }

    private static void deleteTree(File file) {
        File[] children = file.listFiles();
        if (children != null) for (File child : children) deleteTree(child);
        file.delete();
    }

    // ---- transfers ------------------------------------------------------------------------

    /** Called from Rust (under its locks, so it must not wait): what is moving; an empty title for nothing. */
    public void transfersChanged(String title, String text, int percent) {
        main.post(() -> {
            TransferService.update(this, title, text, percent);
            askToNotify();
        });
    }

    /** Without this permission the notification does not show on Android 13+; the service still runs. */
    private void askToNotify() {
        if (Build.VERSION.SDK_INT < 33 || notifyAsked || !visible || !TransferService.wanted()) return;
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return;
        notifyAsked = true;
        requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, REQ_NOTIFY);
    }

    // ---- BLE: lifecycle & permissions -----------------------------------------------------

    /** Called from Rust once the app is up. */
    public void bleStart() {
        runOnUiThread(() -> {
            bleWanted = true;
            startBleIfPermitted();
        });
    }

    /** Called from Rust whenever our name/ticket changes. */
    public void bleSetPayload(String json) {
        payload = json.getBytes(StandardCharsets.UTF_8);
    }

    private String[] neededPermissions() {
        if (Build.VERSION.SDK_INT >= 31) {
            return new String[]{Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_ADVERTISE,
                    Manifest.permission.BLUETOOTH_CONNECT};
        }
        return new String[]{Manifest.permission.ACCESS_FINE_LOCATION};
    }

    /** Called from Rust (any thread). */
    public boolean blePermissionsGranted() {
        for (String p : neededPermissions()) {
            if (checkSelfPermission(p) != PackageManager.PERMISSION_GRANTED) return false;
        }
        return true;
    }

    /** Called from Rust. */
    public void requestBlePermissions() {
        runOnUiThread(() -> {
            List<String> missing = new ArrayList<>();
            for (String p : neededPermissions()) {
                if (checkSelfPermission(p) != PackageManager.PERMISSION_GRANTED) missing.add(p);
            }
            if (missing.isEmpty() || bleAsked) return;
            bleAsked = true;
            requestPermissions(missing.toArray(new String[0]), REQ_BLE);
        });
    }

    private void startBleIfPermitted() {
        if (!bleWanted || bleRunning) return;
        if (!getPackageManager().hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)) {
            nativeBleStatus("Bluetooth: this device has no BLE");
            return;
        }
        List<String> missing = new ArrayList<>();
        for (String p : neededPermissions()) {
            if (checkSelfPermission(p) != PackageManager.PERMISSION_GRANTED) missing.add(p);
        }
        if (!missing.isEmpty()) {
            // Once per launch: this runs on every resume, and that includes the one after a denial.
            if (bleAsked) return;
            bleAsked = true;
            nativeBleStatus("Bluetooth: waiting for permission…");
            requestPermissions(missing.toArray(new String[0]), REQ_BLE);
            return;
        }
        startBle();
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_NOTIFY) {
            // Empty: another dialog was up, so the user never saw it.
            if (grantResults.length == 0) notifyAsked = false;
            return;
        }
        if (requestCode != REQ_BLE) return;
        // Empty: another dialog was up, and Android answered from inside requestPermissions, so
        // asking again here would never end. The resume after that dialog asks instead.
        if (grantResults.length == 0) {
            bleAsked = false;
            return;
        }
        for (int r : grantResults) {
            if (r != PackageManager.PERMISSION_GRANTED) {
                nativeBleStatus("Bluetooth: permission denied");
                return;
            }
        }
        startBleIfPermitted();
    }

    @Override
    protected void onResume() {
        super.onResume();
        visible = true;
        // The user may have just switched Bluetooth on.
        if (bleWanted && !bleRunning) startBleIfPermitted();
        // Start now, while Android allows it: a transfer that begins later, out of sight, must find
        // the service already running.
        TransferService.visible(this);
        askToNotify();
    }

    @Override
    protected void onPause() {
        visible = false;
        super.onPause();
    }

    private void startBle() {
        BluetoothManager manager = (BluetoothManager) getSystemService(Context.BLUETOOTH_SERVICE);
        adapter = manager == null ? null : manager.getAdapter();
        if (adapter == null || !adapter.isEnabled()) {
            nativeBleStatus("Bluetooth: turned off (enable it, then return to the app)");
            return;
        }
        bleRunning = true;
        startGattServer(manager);
        startAdvertising();
        startScanning();
        main.postDelayed(expiry, 5_000);
    }

    private void stopBle() {
        bleRunning = false;
        main.removeCallbacks(expiry);
        try {
            if (scanner != null) scanner.stopScan(scanCallback);
            if (advertiser != null) advertiser.stopAdvertising(advertiseCallback);
            if (gattServer != null) gattServer.close();
        } catch (RuntimeException e) {
            Log.w(TAG, "stopBle", e);
        }
        scanner = null;
        advertiser = null;
        gattServer = null;
    }

    // ---- BLE: peripheral (serve our payload) ----------------------------------------------

    private void startGattServer(BluetoothManager manager) {
        gattServer = manager.openGattServer(this, new BluetoothGattServerCallback() {
            @Override
            public void onCharacteristicReadRequest(BluetoothDevice device, int requestId, int offset,
                                                    BluetoothGattCharacteristic characteristic) {
                byte[] data = payload;
                if (offset > data.length) {
                    gattServer.sendResponse(device, requestId, android.bluetooth.BluetoothGatt.GATT_INVALID_OFFSET, offset, null);
                    return;
                }
                gattServer.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset,
                        Arrays.copyOfRange(data, offset, data.length));
            }
        });
        if (gattServer == null) return;
        BluetoothGattService service = new BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY);
        service.addCharacteristic(new BluetoothGattCharacteristic(PAYLOAD,
                BluetoothGattCharacteristic.PROPERTY_READ, BluetoothGattCharacteristic.PERMISSION_READ));
        gattServer.addService(service);
    }

    private final AdvertiseCallback advertiseCallback = new AdvertiseCallback() {
        @Override
        public void onStartSuccess(AdvertiseSettings settingsInEffect) {
            nativeBleStatus("Bluetooth: advertising and scanning");
        }

        @Override
        public void onStartFailure(int errorCode) {
            nativeBleStatus("Bluetooth: advertising failed (" + errorCode + "), scanning only");
        }
    };

    private void startAdvertising() {
        advertiser = adapter.getBluetoothLeAdvertiser();
        if (advertiser == null) {
            nativeBleStatus("Bluetooth: this device cannot advertise, scanning only");
            return;
        }
        advertiser.startAdvertising(
                new AdvertiseSettings.Builder()
                        .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
                        .setConnectable(true)
                        .setTimeout(0)
                        .build(),
                new AdvertiseData.Builder()
                        .setIncludeDeviceName(false)
                        .addServiceUuid(new ParcelUuid(SERVICE))
                        .build(),
                advertiseCallback);
    }

    // ---- BLE: central (find peers and read their payload) ---------------------------------

    private final Map<String, Long> lastSeen = new HashMap<>();      // address -> last advert
    private final Map<String, Long> lastRead = new HashMap<>();      // address -> time of the successful read
    private final Map<String, Long> retryAt = new HashMap<>();       // address -> earliest time to retry a failed read
    private final Map<String, Boolean> reported = new HashMap<>();   // addresses reported to Rust
    private final ArrayDeque<BluetoothDevice> readQueue = new ArrayDeque<>();
    private boolean reading = false;

    private final ScanCallback scanCallback = new ScanCallback() {
        @Override
        public void onScanResult(int callbackType, ScanResult result) {
            BluetoothDevice device = result.getDevice();
            String addr = device.getAddress();
            long now = System.currentTimeMillis();
            lastSeen.put(addr, now);
            Long retry = retryAt.get(addr);
            if (!lastRead.containsKey(addr) && (retry == null || now >= retry) && !readQueue.contains(device)) {
                readQueue.add(device);
                pumpReads();
            }
        }

        @Override
        public void onScanFailed(int errorCode) {
            nativeBleStatus("Bluetooth: scan failed (" + errorCode + ")");
        }
    };

    private void startScanning() {
        scanner = adapter.getBluetoothLeScanner();
        if (scanner == null) return;
        scanner.startScan(
                List.of(new ScanFilter.Builder().setServiceUuid(new ParcelUuid(SERVICE)).build()),
                new ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build(),
                scanCallback);
    }

    /** Tell Rust about peers that stopped advertising. */
    private final Runnable expiry = new Runnable() {
        @Override
        public void run() {
            long now = System.currentTimeMillis();
            for (String addr : new ArrayList<>(lastSeen.keySet())) {
                if (now - lastSeen.get(addr) > LOST_MS) {
                    lastSeen.remove(addr);
                    lastRead.remove(addr);
                    retryAt.remove(addr);
                    if (reported.remove(addr) != null) nativeBleLost(addr);
                }
            }
            if (bleRunning) main.postDelayed(this, 5_000);
        }
    };

    /** GATT connections are expensive and flaky when parallel: read one device at a time. */
    private void pumpReads() {
        if (reading || readQueue.isEmpty()) return;
        reading = true;
        BluetoothDevice device = readQueue.poll();
        final String addr = device.getAddress();
        final byte[][] result = {null};
        final Runnable finish = new Runnable() {
            boolean done = false;
            @Override
            public void run() {
                if (done) return;
                done = true;
                reading = false;
                if (result[0] != null) {
                    lastRead.put(addr, System.currentTimeMillis());
                    reported.put(addr, true);
                    nativeBlePeer(addr, new String(result[0], StandardCharsets.UTF_8));
                } else {
                    // retry soon, but don't hammer
                    retryAt.put(addr, System.currentTimeMillis() + RETRY_MS);
                }
                pumpReads();
            }
        };
        BluetoothGattCallback callback = new BluetoothGattCallback() {
            @Override
            public void onConnectionStateChange(BluetoothGatt gatt, int status, int newState) {
                if (newState == BluetoothProfile.STATE_CONNECTED && status == BluetoothGatt.GATT_SUCCESS) {
                    gatt.discoverServices();
                } else {
                    gatt.close();
                    main.post(finish);
                }
            }

            @Override
            public void onServicesDiscovered(BluetoothGatt gatt, int status) {
                BluetoothGattService s = gatt.getService(SERVICE);
                BluetoothGattCharacteristic c = s == null ? null : s.getCharacteristic(PAYLOAD);
                if (c == null || !gatt.readCharacteristic(c)) gatt.disconnect();
            }

            @Override
            @SuppressWarnings("deprecation")
            public void onCharacteristicRead(BluetoothGatt gatt, BluetoothGattCharacteristic c, int status) {
                if (status == BluetoothGatt.GATT_SUCCESS) result[0] = c.getValue();
                gatt.disconnect();
            }

            @Override
            public void onCharacteristicRead(BluetoothGatt gatt, BluetoothGattCharacteristic c, byte[] value, int status) {
                if (status == BluetoothGatt.GATT_SUCCESS) result[0] = value;
                gatt.disconnect();
            }
        };
        try {
            device.connectGatt(this, false, callback, BluetoothDevice.TRANSPORT_LE);
        } catch (RuntimeException e) {
            Log.w(TAG, "connectGatt", e);
            main.post(finish);
            return;
        }
        main.postDelayed(finish, 15_000); // give up on a stuck connection
    }
}
