package com.flipflop.app;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkRequest;
import android.util.Log;

/**
 * Tells native code when the device's networks change.
 *
 * iroh watches routes itself on desktop, but Android denies native code the
 * netlink socket that needs, so iroh never learns that Wi-Fi or mobile data
 * came back (or went away) and keeps using stale sockets and relay state.
 * ConnectivityManager sees every change; each one is forwarded through the
 * static {@code onNetworkChanged} callback, which native code registers (the
 * class is loaded from the embedded dex, see src/android.rs).
 */
public final class NetworkWatch {
    private static final String TAG = "flipflop-net";

    static native void onNetworkChanged();

    private static ConnectivityManager.NetworkCallback callback;

    private NetworkWatch() {}

    /** Starts watching (once; later calls are no-ops). Any thread. */
    public static synchronized void start(Context context) {
        if (callback != null) {
            return;
        }
        ConnectivityManager connectivity =
                (ConnectivityManager)
                        context.getApplicationContext()
                                .getSystemService(Context.CONNECTIVITY_SERVICE);
        if (connectivity == null) {
            Log.w(TAG, "no ConnectivityManager; network changes go unnoticed");
            return;
        }
        callback =
                new ConnectivityManager.NetworkCallback() {
                    @Override
                    public void onAvailable(Network network) {
                        onNetworkChanged();
                    }

                    @Override
                    public void onLost(Network network) {
                        onNetworkChanged();
                    }

                    @Override
                    public void onLinkPropertiesChanged(Network network, LinkProperties link) {
                        // A new address on the same network (DHCP renew,
                        // IPv6 privacy rotation) strands sockets too.
                        onNetworkChanged();
                    }
                };
        // Every network, not just the default one: a phone on Wi-Fi without
        // internet still reaches LAN peers over it. Callbacks come in bursts
        // (and once per network on registering); native code coalesces them.
        try {
            connectivity.registerNetworkCallback(new NetworkRequest.Builder().build(), callback);
        } catch (RuntimeException e) {
            Log.w(TAG, "cannot watch the network", e);
            callback = null;
        }
    }
}
