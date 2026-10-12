package com.flipflop.app;

import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import java.io.File;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;

/**
 * Installed apps whose APK can be sent: launchable, user-installed (or
 * updated system) apps other than this one. Visibility of other packages on
 * Android 11+ comes from the manifest's launcher-intent query. Compiled into
 * the embedded dex like the other helpers (see src/android.rs).
 */
public class AppList {
    /** Fields per app in {@link #list}'s result. */
    public static final int STRIDE = 5;

    /**
     * Flattened rows of {@link #STRIDE} strings: label, package, base APK
     * path, version name, and "1" when the app also has split APKs.
     */
    public static String[] list(Context context) {
        PackageManager pm = context.getPackageManager();
        Intent launcher = new Intent(Intent.ACTION_MAIN);
        launcher.addCategory(Intent.CATEGORY_LAUNCHER);
        List<ResolveInfo> found = pm.queryIntentActivities(launcher, 0);
        HashSet<String> seen = new HashSet<>();
        ArrayList<String> rows = new ArrayList<>();
        for (ResolveInfo info : found) {
            ApplicationInfo app = info.activityInfo.applicationInfo;
            if (app.packageName.equals(context.getPackageName()) || !seen.add(app.packageName)) {
                continue;
            }
            boolean system = (app.flags & ApplicationInfo.FLAG_SYSTEM) != 0;
            boolean updated = (app.flags & ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0;
            if (system && !updated) {
                continue;
            }
            if (app.sourceDir == null || !new File(app.sourceDir).canRead()) {
                continue;
            }
            String version = "";
            try {
                PackageInfo pkg = pm.getPackageInfo(app.packageName, 0);
                if (pkg.versionName != null) {
                    version = pkg.versionName;
                }
            } catch (PackageManager.NameNotFoundException e) {
                continue;
            }
            boolean split = app.splitSourceDirs != null && app.splitSourceDirs.length > 0;
            rows.add(String.valueOf(app.loadLabel(pm)));
            rows.add(app.packageName);
            rows.add(app.sourceDir);
            rows.add(version);
            rows.add(split ? "1" : "0");
        }
        return rows.toArray(new String[0]);
    }

    /** The app's icon as `size`×`size` ARGB pixels, or null. */
    public static int[] icon(Context context, String packageName, int size) {
        try {
            Drawable drawable = context.getPackageManager().getApplicationIcon(packageName);
            Bitmap bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
            Canvas canvas = new Canvas(bitmap);
            drawable.setBounds(0, 0, size, size);
            drawable.draw(canvas);
            int[] pixels = new int[size * size];
            bitmap.getPixels(pixels, 0, size, 0, 0, size, size);
            bitmap.recycle();
            return pixels;
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            return null;
        }
    }
}
