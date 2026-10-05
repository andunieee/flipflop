package dev.tunnelmanager.slint;

import android.app.Activity;
import android.app.Fragment;
import android.content.ClipData;
import android.content.Intent;

/**
 * Runs the system file picker for the native app.
 *
 * NativeActivity drops onActivityResult, so the picker is started from this
 * headless fragment instead, which gets the result and hands the picked
 * content URIs to native code ({@link #onPicked}, registered from Rust; an
 * empty array means cancelled). The class is compiled into a dex at build
 * time and loaded at runtime (see src/android.rs), so it is not part of the
 * APK's own code.
 */
public class FilePicker extends Fragment {
    private static final String TAG = "tunnelmanager-file-picker";
    private static final int REQUEST = 0x7a11;

    static native void onPicked(String[] uris);

    /** Must be called on the UI thread. */
    public static void pick(Activity activity) {
        FilePicker picker = new FilePicker();
        activity.getFragmentManager().beginTransaction().add(picker, TAG).commitNow();
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        try {
            picker.startActivityForResult(intent, REQUEST);
        } catch (RuntimeException e) {
            // No document picker on this device.
            picker.done(new String[0]);
        }
    }

    @Override
    public void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQUEST) {
            return;
        }
        String[] uris = new String[0];
        if (resultCode == Activity.RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                uris = new String[clip.getItemCount()];
                for (int i = 0; i < uris.length; i++) {
                    uris[i] = clip.getItemAt(i).getUri().toString();
                }
            } else if (data.getData() != null) {
                uris = new String[] {data.getData().toString()};
            }
        }
        done(uris);
    }

    private void done(String[] uris) {
        if (isAdded()) {
            getFragmentManager().beginTransaction().remove(this).commitAllowingStateLoss();
        }
        onPicked(uris);
    }
}
