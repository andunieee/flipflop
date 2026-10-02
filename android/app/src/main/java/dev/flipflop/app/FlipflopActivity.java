package dev.flipflop.app;

import android.Manifest;
import android.app.NativeActivity;
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
import android.os.ParcelUuid;
import android.util.Log;

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
 * Hosts the Rust (Slint) app and provides the Android-only pieces it cannot do from native code:
 * the multicast lock for mDNS, runtime permissions, the QR scanner and Bluetooth LE.
 *
 * BLE protocol (shared with src/ble/linux.rs): every device runs a GATT server with one service
 * (SERVICE) and one readable characteristic (PAYLOAD) holding {"n": name, "t": ticket}, advertises
 * the service UUID, and scans for it. Each peer is read once (re-reading would open and close a GATT
 * connection to a device that iroh-ble-transport may be using, which tears its link down) and
 * reported to Rust; peers that stop advertising are reported lost.
 */
public class FlipflopActivity extends NativeActivity {
    private static final String TAG = "flipflop";
    static final UUID SERVICE = UUID.fromString("f11bf10b-0000-4f1f-9b0a-8e2d6e7a0001");
    static final UUID PAYLOAD = UUID.fromString("f11bf10b-0000-4f1f-9b0a-8e2d6e7a0002");
    static final long RETRY_MS = 5_000;
    static final long LOST_MS = 45_000;
    static final int REQ_BLE = 1;
    static final int REQ_PICK = 2;

    static {
        System.loadLibrary("flipflop");
    }

    // implemented in src/platform.rs
    private static native void nativeQrScanned(String text);
    private static native void nativeFilePicked(String kind, String path);
    private static native void nativeBlePeer(String address, String json);
    private static native void nativeBleLost(String address);
    private static native void nativeBleStatus(String text);

    private final Handler main = new Handler(Looper.getMainLooper());
    private WifiManager.MulticastLock multicastLock;

    private String pickKind = "file";

    private volatile byte[] payload = new byte[0];
    private boolean bleWanted = false;
    private boolean bleRunning = false;

    private BluetoothAdapter adapter;
    private BluetoothGattServer gattServer;
    private BluetoothLeAdvertiser advertiser;
    private BluetoothLeScanner scanner;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // mDNS (LAN discovery) needs multicast packets, which Android filters unless a lock is held.
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            multicastLock = wifi.createMulticastLock("flipflop-mdns");
            multicastLock.setReferenceCounted(false);
            multicastLock.acquire();
        }
        super.onCreate(savedInstanceState);
        // The app follows the system's light/dark choice; keep the system bar icons readable on it.
        boolean night = (getResources().getConfiguration().uiMode
                & android.content.res.Configuration.UI_MODE_NIGHT_MASK) == android.content.res.Configuration.UI_MODE_NIGHT_YES;
        if (!night && Build.VERSION.SDK_INT >= 23) {
            int flags = android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            if (Build.VERSION.SDK_INT >= 26) flags |= android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
            getWindow().getDecorView().setSystemUiVisibility(flags);
        }
    }

    @Override
    protected void onDestroy() {
        stopBle();
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
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

    /** Copy what the user picked into app storage (the offerer serves it from there) and tell Rust. */
    private void stagePicked(Intent data, String kind) {
        List<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        new Thread(() -> {
            for (Uri uri : uris) {
                try {
                    String name = displayName(uri);
                    File dir = new File(getFilesDir(), "staged/" + System.nanoTime());
                    if (!dir.mkdirs()) continue;
                    File out = new File(dir, name);
                    try (InputStream in = getContentResolver().openInputStream(uri);
                         OutputStream os = new FileOutputStream(out)) {
                        copy(in, os);
                    }
                    nativeFilePicked(kind, out.getAbsolutePath());
                } catch (Exception e) {
                    Log.w(TAG, "could not read " + uri, e);
                }
            }
        }, "flipflop-stage").start();
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

    /**
     * Called from Rust when a download finished in app storage: move it to the public
     * Download/flipflop folder. Returns the file's new location, or "" if it stays where it is.
     */
    public String saveToDownloads(String path) {
        File src = new File(path);
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                String mime = MimeTypeMap.getSingleton()
                        .getMimeTypeFromExtension(MimeTypeMap.getFileExtensionFromUrl(Uri.encode(src.getName())));
                ContentValues values = new ContentValues();
                values.put(MediaStore.Downloads.DISPLAY_NAME, src.getName());
                values.put(MediaStore.Downloads.MIME_TYPE, mime == null ? "application/octet-stream" : mime);
                values.put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/flipflop");
                ContentResolver cr = getContentResolver();
                Uri uri = cr.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
                if (uri == null) return "";
                try (InputStream in = new FileInputStream(src); OutputStream out = cr.openOutputStream(uri)) {
                    copy(in, out);
                }
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
            if (!missing.isEmpty()) requestPermissions(missing.toArray(new String[0]), REQ_BLE);
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
            nativeBleStatus("Bluetooth: waiting for permission…");
            requestPermissions(missing.toArray(new String[0]), REQ_BLE);
            return;
        }
        startBle();
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != REQ_BLE) return;
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
        // The user may have just switched Bluetooth on.
        if (bleWanted && !bleRunning) startBleIfPermitted();
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
