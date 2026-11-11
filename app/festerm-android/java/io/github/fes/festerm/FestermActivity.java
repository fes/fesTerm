package io.github.fes.festerm;

import android.Manifest;
import android.app.KeyguardManager;
import android.app.NativeActivity;
import android.app.AlertDialog;
import android.content.*;
import android.content.ClipboardManager;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.Rect;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.net.Uri;
import android.os.*;
import android.provider.OpenableColumns;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.speech.*;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;
import android.text.*;
import android.util.AtomicFile;
import android.view.*;
import android.view.inputmethod.*;
import android.widget.FrameLayout;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.security.*;
import java.util.*;
import java.util.concurrent.*;
import javax.crypto.*;
import javax.crypto.spec.GCMParameterSpec;

/** Native services only. Rust owns the entire application UI and session policy. */
public final class FestermActivity extends NativeActivity {
    static { System.loadLibrary("festerm_android"); }

    private static final int BACKGROUND = Color.rgb(15, 20, 28);
    private static final int PICK_DOCUMENT = 40, MICROPHONE = 41, PICK_RECORDING_FOLDER = 42;
    private static final int AUTH_OK = 0, AUTH_CANCELLED = 1, AUTH_LOCKED_OUT = 2,
        AUTH_NO_CREDENTIAL = 3, AUTH_UNAVAILABLE = 4;
    private static final int MAX_DOCUMENT = 1024 * 1024, MAX_SECRET = 64 * 1024;
    private static final String KEY_ALIAS = "io.github.fes.festerm.secrets.v1";
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService files = Executors.newSingleThreadExecutor();
    private InputView input;
    private View shield, accessibilityHost;
    private boolean ready, destroyed, resumed;
    private Object windowLayout;
    private java.util.function.Consumer<Object> hingeListener;
    private Rect hinge;
    private volatile boolean protect, cover = true, hidePreview = true;
    private boolean picking, authPending;
    private long authToken;
    private CancellationSignal authentication;
    private SpeechRecognizer recognizer;
    private long speechGeneration;
    private boolean speechPermission, finishing;
    private Runnable speechDeadline, finishDeadline;
    private final BroadcastReceiver screenOff = new BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) {
            if (Intent.ACTION_SCREEN_OFF.equals(intent.getAction()) && ready) {
                cancelSpeech();
                nativeLifecycle(3);
                if (protect) shield(true);
            }
        }
    };

    private native void nativeInput(long target, long sequence, int kind, String text, int start, int end);
    private native void nativeInsets(float fraction, float top, float left, float bottom, float right);
    private native void nativeHinge(float left, float right);
    private native void nativeLifecycle(int event);
    private native void nativeAuthenticated(long token, int result);
    private native void nativePicked(String path);
    private native void nativeRecordingFolderPicked(String uri);
    private native void nativeSpeech(long generation, int status, String text);
    private native void nativeSpeechLevel(long generation, float rmsDb);

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setStatusBarColor(BACKGROUND);
        getWindow().setNavigationBarColor(BACKGROUND);
        if (Build.VERSION.SDK_INT >= 30) getWindow().setDecorFitsSystemWindows(true);
        input = new InputView();
        addContentView(input, new FrameLayout.LayoutParams(1, 1));
        // Transparent, input-free host for AccessKit's virtual node tree (ADR 0075).
        // NativeActivity delivers touches through its input queue, not views.
        // egui lays out in window coordinates, so the host must sit at the
        // window origin rather than inside the inset content frame.
        accessibilityHost = new View(this);
        accessibilityHost.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_YES);
        ((ViewGroup) getWindow().getDecorView())
                .addView(accessibilityHost, new FrameLayout.LayoutParams(-1, -1));
        shield = new View(this);
        shield.setBackgroundColor(BACKGROUND);
        shield.setClickable(true);
        addContentView(shield, new FrameLayout.LayoutParams(-1, -1));
        shield(true);
        getWindow().getDecorView().getViewTreeObserver().addOnGlobalLayoutListener(this::reportInsets);
        watchHinge();
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(screenOff, new IntentFilter(Intent.ACTION_SCREEN_OFF), Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(screenOff, new IntentFilter(Intent.ACTION_SCREEN_OFF));
        }
    }

    public String privateDirectory() throws IOException {
        File directory = new File(getFilesDir(), "festerm");
        if (!directory.isDirectory() && !directory.mkdirs()) throw new IOException("Storage unavailable");
        return directory.getCanonicalPath();
    }

    public int privateDirectoryDescriptor() throws IOException {
        try (ParcelFileDescriptor directory = privateChild(getFilesDir(), "festerm")) {
            return directory.detachFd();
        }
    }

    private void checkPrivateDescriptor(FileDescriptor descriptor, boolean directory)
            throws ErrnoException, IOException {
        StructStat metadata = Os.fstat(descriptor);
        boolean type = directory ? OsConstants.S_ISDIR(metadata.st_mode) : OsConstants.S_ISREG(metadata.st_mode);
        NativeStorageChecks.privateEntry(android.os.Process.myUid(), getApplicationInfo().uid,
            metadata.st_uid, metadata.st_mode, metadata.st_nlink, type, directory);
    }

    private ParcelFileDescriptor pinPrivateDirectory(String path) throws IOException {
        try {
            FileDescriptor descriptor = Os.open(path,
                OsConstants.O_RDONLY | OsConstants.O_DIRECTORY | OsConstants.O_NOFOLLOW
                    | OsConstants.O_CLOEXEC, 0);
            try {
                checkPrivateDescriptor(descriptor, true);
                return duplicatePrivateDescriptor(descriptor);
            } finally {
                Os.close(descriptor);
            }
        } catch (ErrnoException error) {
            throw new IOException("Native private storage unavailable", error);
        }
    }

    private ParcelFileDescriptor duplicatePrivateDescriptor(FileDescriptor descriptor)
            throws ErrnoException, IOException {
        ParcelFileDescriptor owned = ParcelFileDescriptor.dup(descriptor);
        try {
            Os.fcntlInt(owned.getFileDescriptor(), OsConstants.F_SETFD, OsConstants.FD_CLOEXEC);
            return owned;
        } catch (ErrnoException error) {
            owned.close();
            throw error;
        }
    }

    private ParcelFileDescriptor privateChild(File nativeParent, String child) throws IOException {
        if (nativeParent == null) throw new IOException("Native private storage unavailable");
        // Android may make files/no_backup 0771. They are not portable private
        // roots: pin the current Context's private data directory first, then
        // validate that exact native child beneath it, never a caller path.
        String nativeName = nativeParent.equals(getFilesDir()) ? "files"
            : nativeParent.equals(getNoBackupFilesDir()) ? "no_backup" : null;
        if (nativeName == null) throw new IOException("Native storage scope unavailable");
        try (ParcelFileDescriptor application = pinPrivateDirectory(getDataDir().getPath());
                ParcelFileDescriptor parent = pinNativeStorageDirectory(application, nativeParent, nativeName)) {
            String path = "/proc/self/fd/" + parent.getFd() + "/" + child;
            try {
                Os.mkdir(path, 0700);
                Os.fsync(parent.getFileDescriptor());
            } catch (ErrnoException error) {
                if (error.errno != OsConstants.EEXIST) {
                    throw new IOException("Native private storage unavailable", error);
                }
            }
            return pinPrivateDirectory(path);
        }
    }

    private ParcelFileDescriptor pinNativeStorageDirectory(ParcelFileDescriptor application,
            File nativeParent, String name) throws IOException {
        try {
            FileDescriptor descriptor = Os.open("/proc/self/fd/" + application.getFd() + "/" + name,
                OsConstants.O_RDONLY | OsConstants.O_DIRECTORY | OsConstants.O_NOFOLLOW
                    | OsConstants.O_CLOEXEC, 0);
            try {
                StructStat metadata = Os.fstat(descriptor);
                StructStat expected = Os.stat(nativeParent.getPath());
                NativeStorageChecks.nativeChild(android.os.Process.myUid(), metadata.st_uid,
                    metadata.st_gid, metadata.st_mode, metadata.st_nlink,
                    OsConstants.S_ISDIR(metadata.st_mode), metadata.st_dev, metadata.st_ino,
                    expected.st_dev, expected.st_ino);
                return duplicatePrivateDescriptor(descriptor);
            } finally {
                Os.close(descriptor);
            }
        } catch (ErrnoException error) {
            throw new IOException("Native storage scope unavailable", error);
        }
    }

    private SecretNamespace secretNamespace(ParcelFileDescriptor directory) throws IOException {
        Path root = Paths.get("/proc/self/fd/" + directory.getFd());
        return SecretNamespace.acquire(root, path -> {
            try {
                FileDescriptor file = Os.open(path.toString(),
                    OsConstants.O_RDONLY | OsConstants.O_NOFOLLOW | OsConstants.O_CLOEXEC
                        | OsConstants.O_NONBLOCK, 0);
                try {
                    checkPrivateDescriptor(file, false);
                } finally {
                    Os.close(file);
                }
            } catch (ErrnoException error) {
                throw new IOException("Native secret ownership unavailable", error);
            }
        }, () -> {
            try {
                checkPrivateDescriptor(directory.getFileDescriptor(), true);
                Os.fsync(directory.getFileDescriptor());
            } catch (ErrnoException error) {
                throw new IOException("Native secret publication unavailable", error);
            }
        });
    }

    public RecordingEncoder newRecordingEncoder(String path, int width, int height) throws IOException {
        return new RecordingEncoder(path, width, height);
    }

    public RecordingDestination newRecordingDestination(String treeUri) throws IOException {
        return new RecordingDestination(this, treeUri);
    }

    public String recordingDirectory() throws IOException {
        File root = getNoBackupFilesDir();
        if (root == null || !root.isDirectory()) throw new IOException("Recording storage unavailable");
        File directory = new File(root, "terminal-recordings");
        if (!directory.isDirectory() && !directory.mkdir() && !directory.isDirectory()) {
            throw new IOException("Recording storage unavailable");
        }
        try {
            StructStat metadata = Os.lstat(directory.getPath());
            if (!OsConstants.S_ISDIR(metadata.st_mode) || metadata.st_uid != android.os.Process.myUid()) {
                throw new IOException("Recording storage is not a private directory");
            }
            Os.chmod(directory.getPath(), OsConstants.S_IRWXU);
            metadata = Os.lstat(directory.getPath());
            if (!OsConstants.S_ISDIR(metadata.st_mode) || metadata.st_uid != android.os.Process.myUid()
                || (metadata.st_mode & 0777) != 0700) {
                throw new IOException("Recording storage is not a private directory");
            }
        } catch (ErrnoException error) {
            throw new IOException("Could not protect recording storage", error);
        }
        return directory.getCanonicalPath();
    }

    private void reportInsets() {
        if (!ready || input == null) return;
        View root = getWindow().getDecorView();
        WindowInsets insets = root.getRootWindowInsets();
        float height = root.getHeight();
        float width = root.getWidth();
        int keyboard = 0, top = 0, left = 0, bottom = 0, right = 0;
        if (height > 0 && insets != null) {
            if (Build.VERSION.SDK_INT >= 30) {
                Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                keyboard = insets.isVisible(WindowInsets.Type.ime()) ? insets.getInsets(WindowInsets.Type.ime()).bottom : 0;
                top = bars.top; left = bars.left; bottom = bars.bottom; right = bars.right;
            } else {
                Rect visible = new Rect();
                root.getWindowVisibleDisplayFrame(visible);
                keyboard = Math.max(0, root.getHeight() - visible.bottom);
                top = insets.getStableInsetTop(); left = insets.getStableInsetLeft();
                bottom = insets.getStableInsetBottom(); right = insets.getStableInsetRight();
            }
        }
        nativeInsets(height > 0 ? keyboard / height : 0,
            height > 0 ? top / height : 0, width > 0 ? left / width : 0,
            height > 0 ? bottom / height : 0, width > 0 ? right / width : 0);
        sendHinge();
    }

    /**
     * Folds and hinges come from the device's optional androidx.window.extensions
     * library, read by reflection so the APK carries no AndroidX dependency.
     * Without it, or on any failure, the app sees no hinge.
     */
    private void watchHinge() {
        try {
            Object extensions = Class.forName("androidx.window.extensions.WindowExtensionsProvider")
                .getMethod("getWindowExtensions").invoke(null);
            Object component = extensions.getClass().getMethod("getWindowLayoutComponent").invoke(extensions);
            if (component == null) return;
            java.util.function.Consumer<Object> listener = info -> runOnUiThread(() -> readHinge(info));
            component.getClass()
                .getMethod("addWindowLayoutInfoListener", android.app.Activity.class, java.util.function.Consumer.class)
                .invoke(component, this, listener);
            windowLayout = component;
            hingeListener = listener;
        } catch (Throwable ignored) {
            // No window extensions on this device.
        }
    }

    private void readHinge(Object info) {
        Rect found = null;
        try {
            Class<?> folding = Class.forName("androidx.window.extensions.layout.FoldingFeature");
            for (Object feature : (List<?>) info.getClass().getMethod("getDisplayFeatures").invoke(info)) {
                if (!folding.isInstance(feature)) continue;
                Rect bounds = (Rect) folding.getMethod("getBounds").invoke(feature);
                // Only a vertical fold splits a keyboard docked at the bottom.
                if (bounds != null && bounds.height() > bounds.width()) found = new Rect(bounds);
            }
        } catch (Throwable ignored) {
            found = null;
        }
        hinge = found;
        sendHinge();
    }

    private void sendHinge() {
        if (destroyed || !ready || input == null) return;
        float width = getWindow().getDecorView().getWidth();
        if (hinge == null || width <= 0) nativeHinge(-1, -1);
        else nativeHinge(hinge.left / width, hinge.right / width);
    }

    public void keyboardMode(int mode) {
        runOnUiThread(() -> {
            if (destroyed || (!resumed && mode != 0) || input.keyboardMode == mode) return;
            ready = true;
            input.mode(mode);
            reportInsets();
        });
    }

    public void fieldDocument(long id, String text, int start, int end, boolean secure, boolean multiline) {
        runOnUiThread(() -> {
            if (!destroyed) input.document(id, text, start, end, secure, multiline);
        });
    }

    public void inputConsumed(long sequence) {
        runOnUiThread(() -> {
            if (!destroyed && input.sequence == sequence) input.pending = false;
        });
    }

    public void inputClick() {
        runOnUiThread(() -> { if (!destroyed) input.playSoundEffect(SoundEffectConstants.CLICK); });
    }

    public String clipboardText() {
        ClipboardManager manager = getSystemService(ClipboardManager.class);
        ClipData clip = manager.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) return null;
        CharSequence text = clip.getItemAt(0).getText();
        if (text == null || text.length() > MAX_SECRET) return null;
        return text.toString();
    }

    public void copyText(String text) {
        if (text.length() > MAX_DOCUMENT) throw new IllegalArgumentException("Clipboard limit exceeded");
        getSystemService(ClipboardManager.class).setPrimaryClip(ClipData.newPlainText("fesTerm", text));
    }

    public void openBrowser(String url) {
        Uri uri = Uri.parse(url);
        if (!"https".equals(uri.getScheme()) || uri.getHost() == null) {
            throw new IllegalArgumentException("HTTPS link required");
        }
        // startActivity is safe from the native loop thread; failures return through JNI.
        startActivity(new Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE));
    }

    public boolean canAuthenticate() {
        return getSystemService(KeyguardManager.class).isDeviceSecure();
    }

    public void authenticateOwner(long token, boolean passcodeOnly, String reason) {
        runOnUiThread(() -> {
            if (destroyed) {
                nativeAuthenticated(token, AUTH_CANCELLED);
                return;
            }
            if (!canAuthenticate()) {
                nativeAuthenticated(token, AUTH_NO_CREDENTIAL);
                return;
            }
            // The newest request wins; the older prompt's reply is ignored by token.
            retireAuthentication();
            authPending = true;
            authToken = token;
            CancellationSignal signal = new CancellationSignal();
            authentication = signal;
            try {
                BiometricPrompt.Builder prompt = new BiometricPrompt.Builder(this)
                    .setTitle(reason == null || reason.isEmpty() ? "Unlock fesTerm" : reason)
                    .setSubtitle("Use your device security to continue");
                if (passcodeOnly && Build.VERSION.SDK_INT >= 30) {
                    prompt.setAllowedAuthenticators(BiometricManager.Authenticators.DEVICE_CREDENTIAL);
                } else {
                    prompt.setDeviceCredentialAllowed(true);
                }
                prompt.build().authenticate(signal, getMainExecutor(), new BiometricPrompt.AuthenticationCallback() {
                    private void finish(int result) {
                        if (!authPending || authToken != token) return;
                        authPending = false;
                        authentication = null;
                        nativeAuthenticated(token, result);
                    }
                    @Override public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                        finish(AUTH_OK);
                    }
                    @Override public void onAuthenticationError(int error, CharSequence message) {
                        finish(authResult(error));
                    }
                });
            } catch (RuntimeException error) {
                if (authPending && authToken == token) {
                    retireAuthentication();
                    nativeAuthenticated(token, AUTH_UNAVAILABLE);
                }
            }
        });
    }

    /** Retires exactly one request; a token that already completed or was replaced is ignored. */
    public void cancelAuthentication(long token) {
        runOnUiThread(() -> {
            if (authPending && authToken == token) retireAuthentication();
        });
    }

    private void retireAuthentication() {
        authPending = false;
        if (authentication != null) {
            authentication.cancel();
            authentication = null;
        }
    }

    private static int authResult(int error) {
        switch (error) {
            case BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED:
            case BiometricPrompt.BIOMETRIC_ERROR_CANCELED:
            case BiometricPrompt.BIOMETRIC_ERROR_TIMEOUT:
                return AUTH_CANCELLED;
            case BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT:
            case BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT_PERMANENT:
                return AUTH_LOCKED_OUT;
            case BiometricPrompt.BIOMETRIC_ERROR_NO_DEVICE_CREDENTIAL:
                return AUTH_NO_CREDENTIAL;
            default:
                return AUTH_UNAVAILABLE;
        }
    }

    /** AccessKit attaches here from the native loop thread. */
    public View accessibilityHost() {
        return accessibilityHost;
    }

    /** Accessibility services must not read the tree the privacy shield hides. */
    private void shield(boolean visible) {
        shield.setVisibility(visible ? View.VISIBLE : View.GONE);
        accessibilityHost.setVisibility(visible ? View.INVISIBLE : View.VISIBLE);
    }

    public void privacy(boolean enabled, boolean covered, boolean hide) {
        if (ready && protect == enabled && cover == covered && hidePreview == hide) return;
        protect = enabled;
        cover = covered;
        hidePreview = hide;
        runOnUiThread(() -> {
            if (destroyed) return;
            ready = true;
            if (enabled && (hide || covered)) {
                getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
            } else {
                getWindow().clearFlags(WindowManager.LayoutParams.FLAG_SECURE);
            }
            shield(covered || (!resumed && enabled && hide));
        });
    }

    @Override protected void onResume() {
        super.onResume();
        resumed = true;
        if (ready) {
            if (getSystemService(KeyguardManager.class).isDeviceLocked()) nativeLifecycle(3);
            nativeLifecycle(1);
        }
    }

    @Override protected void onPause() {
        resumed = false;
        if (protect && (hidePreview || cover)) shield(true);
        if (ready) {
            input.mode(0);
            if (!speechPermission) cancelSpeech();
            nativeLifecycle(0);
        }
        super.onPause();
    }

    @Override protected void onStop() {
        if (ready) {
            cancelSpeech();
            nativeLifecycle(2);
        }
        super.onStop();
    }

    @Override protected void onDestroy() {
        destroyed = true;
        cancelSpeech();
        if (authPending) {
            long token = authToken;
            retireAuthentication();
            nativeAuthenticated(token, AUTH_CANCELLED);
        }
        unregisterReceiver(screenOff);
        if (windowLayout != null) {
            try {
                windowLayout.getClass()
                    .getMethod("removeWindowLayoutInfoListener", java.util.function.Consumer.class)
                    .invoke(windowLayout, hingeListener);
            } catch (Throwable ignored) {
                // The listener dies with the activity either way.
            }
        }
        files.shutdown();
        main.removeCallbacksAndMessages(null);
        super.onDestroy();
    }

    @Override public void onBackPressed() {
        // Never silently close a terminal or discard a dirty editor.
        if (input != null && input.keyboardMode != 0) {
            input.emit(6, "", KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_ESCAPE);
        } else {
            moveTaskToBack(true);
        }
    }

    /** A picker grants only a read for an explicit import, never provider overwrite authority. */
    public void pickDocument() {
        runOnUiThread(() -> {
            if (destroyed || picking) { nativePicked(null); return; }
            picking = true;
            new AlertDialog.Builder(this).setTitle("Open a document")
                .setItems(new String[]{"Import from Files", "Open saved copy"}, (dialog, choice) -> {
                    if (choice == 0) importDocument(); else openSavedCopy();
                })
                .setOnCancelListener(dialog -> { picking = false; nativePicked(""); })
                .show();
        });
    }

    public void pickRecordingFolder() {
        runOnUiThread(() -> {
            if (destroyed || picking) { nativeRecordingFolderPicked(null); return; }
            picking = true;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
            try {
                startActivityForResult(intent, PICK_RECORDING_FOLDER);
            } catch (ActivityNotFoundException error) {
                picking = false;
                nativeRecordingFolderPicked(null);
            }
        });
    }

    private void openSavedCopy() {
        files.execute(() -> {
            ArrayList<File> copies = new ArrayList<>();
            try {
                File root = new File(privateDirectory(), "Documents/Imports");
                if (root.isDirectory()) {
                    try (DirectoryStream<Path> entries = Files.newDirectoryStream(root.toPath())) {
                        int scanned = 0;
                        for (Path path : entries) {
                            if (++scanned > 200) throw new IOException("Saved copy list exceeds its bound");
                            if (Files.isRegularFile(path, LinkOption.NOFOLLOW_LINKS)
                                    && !path.getFileName().toString().startsWith(".")) copies.add(path.toFile());
                        }
                    }
                }
                copies.sort(Comparator.comparing(File::getName));
            } catch (IOException | DirectoryIteratorException | SecurityException error) {
                main.post(() -> { picking = false; if (!destroyed) nativePicked(null); });
                return;
            }
            main.post(() -> {
                if (destroyed) return;
                String[] names = copies.stream().map(File::getName).toArray(String[]::new);
                AlertDialog.Builder dialog = new AlertDialog.Builder(this)
                    .setTitle("Saved local copies")
                    .setOnCancelListener(ignored -> { picking = false; nativePicked(""); });
                if (copies.isEmpty()) {
                    dialog.setMessage("No saved copies. Import a document from Files first.")
                        .setPositiveButton("OK", (ignored, button) -> { picking = false; nativePicked(""); });
                } else {
                    dialog.setItems(names, (ignored, index) -> {
                        picking = false;
                        nativePicked(copies.get(index).getAbsolutePath());
                    });
                }
                dialog.show();
            });
        });
    }

    private void importDocument() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT)
            .setType("text/*").addCategory(Intent.CATEGORY_OPENABLE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            startActivityForResult(intent, PICK_DOCUMENT);
        } catch (ActivityNotFoundException error) {
            picking = false;
            nativePicked(null);
        }
    }

    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == PICK_RECORDING_FOLDER) {
            recordingFolderResult(result, data);
            return;
        }
        if (request != PICK_DOCUMENT) return;
        if (result != RESULT_OK) {
            picking = false;
            nativePicked("");
            return;
        }
        Uri uri = data == null ? null : data.getData();
        if (uri == null || !"content".equals(uri.getScheme())) {
            picking = false;
            nativePicked(null);
            return;
        }
        files.execute(() -> {
            String imported = null;
            File destination = null;
            try {
                File directory = new File(privateDirectory(), "Documents/Imports");
                if (!directory.isDirectory() && !directory.mkdirs()) throw new IOException("Import unavailable");
                String extension = ".txt";
                String prefix = "Imported-";
                try (Cursor metadata = getContentResolver().query(uri,
                        new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
                    if (metadata != null && metadata.moveToFirst()) {
                        String name = metadata.getString(0);
                        if (name != null && name.length() <= 256) {
                            int dot = name.lastIndexOf('.');
                            if (dot > 0 && name.substring(dot).matches("\\.[a-zA-Z0-9]{1,12}")) {
                                extension = name.substring(dot).toLowerCase(Locale.ROOT);
                                name = name.substring(0, dot);
                            }
                            String safe = name.replaceAll("[^\\p{L}\\p{N}_ -]", "_").trim();
                            if (!safe.isEmpty()) {
                                int end = safe.offsetByCodePoints(0, Math.min(40, safe.codePointCount(0, safe.length())));
                                prefix = "Copy-" + safe.substring(0, end) + "-";
                            }
                        }
                    }
                }
                destination = File.createTempFile(prefix, extension, directory);
                try (InputStream source = getContentResolver().openInputStream(uri);
                     FileOutputStream output = new FileOutputStream(destination)) {
                    if (source == null) throw new IOException("Read unavailable");
                    byte[] buffer = new byte[8192];
                    int total = 0, count;
                    while ((count = source.read(buffer)) != -1) {
                        total += count;
                        if (total > MAX_DOCUMENT) throw new IOException("Import limit exceeded");
                        output.write(buffer, 0, count);
                    }
                    Arrays.fill(buffer, (byte) 0);
                    output.getFD().sync();
                    imported = destination.getCanonicalPath();
                }
            } catch (IOException | SecurityException error) {
                if (destination != null && !destination.delete()) {
                    android.util.Log.w("fesTerm", "Failed import cleanup");
                }
            }
            final String path = imported;
            main.post(() -> {
                picking = false;
                if (!destroyed) nativePicked(path);
            });
        });
    }

    private void recordingFolderResult(int result, Intent data) {
        if (result != RESULT_OK) {
            picking = false;
            nativeRecordingFolderPicked("");
            return;
        }
        Uri uri = data == null ? null : data.getData();
        int flags = data == null ? 0 : data.getFlags()
            & (Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        if (uri == null || flags != (Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION)) {
            picking = false;
            nativeRecordingFolderPicked(null);
            return;
        }
        try {
            files.execute(() -> {
                String chosen = null;
                try {
                    getContentResolver().takePersistableUriPermission(uri, flags);
                    new RecordingDestination(this, uri.toString());
                    chosen = uri.toString();
                } catch (IOException | SecurityException | IllegalArgumentException
                         | UnsupportedOperationException error) {
                    android.util.Log.w("fesTerm", "Recording folder access is unavailable");
                }
                final String destination = chosen;
                main.post(() -> {
                    picking = false;
                    if (!destroyed) nativeRecordingFolderPicked(destination);
                });
            });
        } catch (RejectedExecutionException error) {
            picking = false;
            nativeRecordingFolderPicked(null);
        }
    }

    private synchronized SecretKey secretKey(SecretNamespace namespace) throws GeneralSecurityException, IOException {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (store.containsAlias(KEY_ALIAS)) return (SecretKey) store.getKey(KEY_ALIAS, null);
        File[] entries = namespace.directory.toFile().listFiles();
        if (entries == null) throw new KeyStoreException("Secret key unavailable");
        for (File entry : entries) {
            if (!entry.getName().equals(SecretNamespace.LOCK)) throw new KeyStoreException("Secret key unavailable");
        }
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256).setRandomizedEncryptionRequired(true)
            .setUnlockedDeviceRequired(true).build());
        return generator.generateKey();
    }

    public synchronized byte[] secretRead(String reference) throws GeneralSecurityException, IOException {
        try (ParcelFileDescriptor directory = privateChild(getNoBackupFilesDir(), "secrets");
                SecretNamespace namespace = secretNamespace(directory)) {
            if (!namespace.existing(reference)) return null;
            byte[] envelope;
            try (InputStream source = Files.newInputStream(namespace.path(reference), LinkOption.NOFOLLOW_LINKS)) {
                ByteArrayOutputStream bounded = new ByteArrayOutputStream();
                byte[] buffer = new byte[4096];
                int count;
                while ((count = source.read(buffer, 0, Math.min(buffer.length, MAX_SECRET + 30 - bounded.size()))) > 0) {
                    bounded.write(buffer, 0, count);
                }
                envelope = bounded.toByteArray();
                if (source.read() != -1 || envelope.length < 29 || envelope[0] != 1) {
                    throw new IOException("Invalid secret envelope");
                }
            }
            try {
                Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
                cipher.init(Cipher.DECRYPT_MODE, secretKey(namespace), new GCMParameterSpec(128, envelope, 1, 12));
                cipher.updateAAD((KEY_ALIAS + "/" + reference).getBytes(StandardCharsets.US_ASCII));
                return cipher.doFinal(envelope, 13, envelope.length - 13);
            } finally {
                Arrays.fill(envelope, (byte) 0);
            }
        }
    }

    public synchronized boolean secretWrite(String reference, byte[] bytes, boolean replace)
            throws GeneralSecurityException, IOException {
        try (ParcelFileDescriptor directory = privateChild(getNoBackupFilesDir(), "secrets");
                SecretNamespace namespace = secretNamespace(directory)) {
            if (bytes.length > MAX_SECRET) throw new IllegalArgumentException("Secret limit exceeded");
            File file = namespace.path(reference).toFile();
            if (namespace.existing(reference) != replace) return false;
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, secretKey(namespace));
            cipher.updateAAD((KEY_ALIAS + "/" + reference).getBytes(StandardCharsets.US_ASCII));
            byte[] encrypted = cipher.doFinal(bytes);
            AtomicFile atomic = new AtomicFile(file);
            FileOutputStream output = null;
            try {
                if (!replace) {
                    byte[] envelope = new byte[13 + encrypted.length];
                    try {
                        envelope[0] = 1;
                        System.arraycopy(cipher.getIV(), 0, envelope, 1, 12);
                        System.arraycopy(encrypted, 0, envelope, 13, encrypted.length);
                        return namespace.create(reference, envelope);
                    } finally {
                        Arrays.fill(envelope, (byte) 0);
                    }
                }
                output = atomic.startWrite();
                output.write(1);
                output.write(cipher.getIV());
                output.write(encrypted);
                atomic.finishWrite(output);
                // Replacement remains under the same cross-instance lease;
                // acknowledge only after native private-file and directory checks.
                if (!namespace.existing(reference)) throw new IOException("Native secret replacement unavailable");
                try {
                    Os.fsync(directory.getFileDescriptor());
                } catch (ErrnoException error) {
                    throw new IOException("Native secret publication unavailable", error);
                }
                return true;
            } catch (IOException error) {
                if (output != null) atomic.failWrite(output);
                throw error;
            } finally {
                Arrays.fill(encrypted, (byte) 0);
            }
        } finally {
            Arrays.fill(bytes, (byte) 0);
        }
    }

    public synchronized boolean secretDelete(String reference) throws IOException {
        try (ParcelFileDescriptor directory = privateChild(getNoBackupFilesDir(), "secrets");
                SecretNamespace namespace = secretNamespace(directory)) {
            boolean present = namespace.existing(reference);
            if (present) {
                Files.delete(namespace.path(reference));
                try {
                    Os.fsync(directory.getFileDescriptor());
                } catch (ErrnoException error) {
                    throw new IOException("Native secret deletion unavailable", error);
                }
            }
            return present;
        }
    }

    public void speechCommand(long generation, int command) {
        runOnUiThread(() -> {
            if (destroyed) return;
            if (command == 0) {
                cancelSpeech();
                speechGeneration = generation;
                if (Build.VERSION.SDK_INT < 31 || !SpeechRecognizer.isOnDeviceRecognitionAvailable(this)) {
                    nativeSpeech(generation, -1, "");
                } else if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                    speechPermission = true;
                    requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, MICROPHONE);
                } else {
                    startSpeech(generation);
                }
            } else if (command == 2 && generation >= speechGeneration) {
                cancelSpeech();
            } else if (generation == speechGeneration) {
                if (command == 1) finishSpeech(); else cancelSpeech();
            }
        });
    }

    @Override public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request != MICROPHONE) return;
        boolean pending = speechPermission;
        speechPermission = false;
        if (!pending || destroyed) return;
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            startSpeech(speechGeneration);
        } else {
            nativeSpeech(speechGeneration, -1, "");
        }
    }

    private void startSpeech(long generation) {
        if (Build.VERSION.SDK_INT < 31) { nativeSpeech(generation, -1, ""); return; }
        recognizer = SpeechRecognizer.createOnDeviceSpeechRecognizer(this);
        recognizer.setRecognitionListener(new RecognitionListener() {
            private long lastLevel;
            private boolean current() { return !destroyed && generation == speechGeneration && recognizer != null; }
            private void result(Bundle bundle, boolean last) {
                if (!current()) return;
                ArrayList<String> texts = bundle.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION);
                String text = texts == null || texts.isEmpty() ? "" : texts.get(0);
                if (text.getBytes(StandardCharsets.UTF_8).length > 4096) {
                    nativeSpeech(generation, -1, "");
                    cancelSpeech();
                    return;
                }
                nativeSpeech(generation, last ? 3 : 2, text);
                if (last) cancelSpeech();
            }
            @Override public void onReadyForSpeech(Bundle parameters) { if (current()) nativeSpeech(generation, 1, ""); }
            @Override public void onBeginningOfSpeech() {}
            @Override public void onRmsChanged(float rms) {
                long now = SystemClock.uptimeMillis();
                if (!current() || now - lastLevel < 80) return;
                lastLevel = now;
                nativeSpeechLevel(generation, rms);
            }
            @Override public void onBufferReceived(byte[] buffer) {}
            @Override public void onEndOfSpeech() { if (current()) finishSpeech(); }
            @Override public void onError(int error) {
                if (!current()) return;
                nativeSpeech(generation, finishing ? 4 : -1, "");
                cancelSpeech();
            }
            @Override public void onResults(Bundle results) { result(results, true); }
            @Override public void onPartialResults(Bundle results) { result(results, false); }
            @Override public void onEvent(int event, Bundle parameters) {}
        });
        Intent intent = new Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
            .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            .putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            .putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            .putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1);
        try {
            recognizer.startListening(intent);
            speechDeadline = this::finishSpeech;
            main.postDelayed(speechDeadline, 45_000);
        } catch (SecurityException | IllegalStateException error) {
            nativeSpeech(generation, -1, "");
            cancelSpeech();
        }
    }

    private void finishSpeech() {
        if (recognizer == null || finishing) return;
        finishing = true;
        nativeSpeech(speechGeneration, 5, "");
        recognizer.stopListening();
        finishDeadline = () -> {
            nativeSpeech(speechGeneration, 4, "");
            cancelSpeech();
        };
        main.postDelayed(finishDeadline, 1500);
    }

    private void cancelSpeech() {
        speechPermission = false;
        finishing = false;
        if (speechDeadline != null) main.removeCallbacks(speechDeadline);
        if (finishDeadline != null) main.removeCallbacks(finishDeadline);
        if (recognizer != null) {
            recognizer.cancel();
            recognizer.destroy();
            recognizer = null;
        }
    }

    private final class InputView extends View {
        private final Editable text = new SpannableStringBuilder();
        private int keyboardMode;
        private long field;
        private long sequence;
        private boolean secure, multiline, pending;
        private InputConnection connection;

        /** Modes 3 and 4 edit the published form field; 4 has the app-owned keyboard on screen. */
        private boolean editsField(int mode) { return mode == 3 || mode == 4; }

        /** The app-owned keyboards draw their own keys: modes 2 (terminal) and 4 (field). */
        private boolean appOwnedKeyboard(int mode) { return mode == 2 || mode == 4; }

        InputView() {
            super(FestermActivity.this);
            setFocusable(true);
            setFocusableInTouchMode(true);
            setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
        }

        void emit(int kind, String value, int start, int end) {
            if (!ready || !resumed || keyboardMode == 0 || (protect && cover)) return;
            pending = true;
            nativeInput(editsField(keyboardMode) ? field : 0, ++sequence, kind, value, start, end);
        }

        void clear() {
            text.clear();
            Selection.setSelection(text, 0);
            pending = false;
            field = 0;
        }

        void mode(int mode) {
            if (keyboardMode == mode) return;
            keyboardMode = mode;
            InputMethodManager manager = getSystemService(InputMethodManager.class);
            if (mode == 0) {
                clear();
                secure = false;
                multiline = false;
                manager.hideSoftInputFromWindow(getWindowToken(), 0);
                clearFocus();
            } else {
                if (!editsField(mode)) { clear(); secure = false; multiline = false; }
                requestFocus();
                manager.restartInput(this);
                if (appOwnedKeyboard(mode)) {
                    manager.hideSoftInputFromWindow(getWindowToken(), 0);
                } else {
                    manager.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT);
                }
            }
        }

        void document(long id, String value, int start, int end, boolean secret, boolean multiple) {
            if (field == id && pending) return;
            boolean restart = field != id || secure != secret || multiline != multiple;
            field = id;
            secure = secret;
            multiline = multiple;
            if (!text.toString().equals(value)) text.replace(0, text.length(), value);
            int chars = Character.codePointCount(text, 0, text.length());
            Selection.setSelection(text, Character.offsetByCodePoints(text, 0, Math.min(start, chars)),
                Character.offsetByCodePoints(text, 0, Math.min(end, chars)));
            InputMethodManager manager = getSystemService(InputMethodManager.class);
            if (restart && editsField(keyboardMode)) manager.restartInput(this);
            manager.updateSelection(this, Selection.getSelectionStart(text), Selection.getSelectionEnd(text), -1, -1);
        }

        @Override public boolean onCheckIsTextEditor() { return true; }

        @Override public InputConnection onCreateInputConnection(EditorInfo info) {
            boolean terminal = !editsField(keyboardMode);
            int ownerMode = keyboardMode;
            long ownerField = field;
            info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            if (secure || terminal) info.inputType |= InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD;
            if (secure) info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD;
            if (multiline || terminal) info.inputType |= InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
                | (multiline || terminal ? EditorInfo.IME_ACTION_NONE : EditorInfo.IME_ACTION_NEXT);
            info.initialSelStart = Math.max(0, Selection.getSelectionStart(text));
            info.initialSelEnd = Math.max(0, Selection.getSelectionEnd(text));
            connection = new BaseInputConnection(this, true) {
                private boolean composing;
                private String composition = "";
                private boolean active() {
                    return connection == this && keyboardMode == ownerMode
                        && keyboardMode != 0 && (terminal || field == ownerField);
                }
                private boolean bounded(CharSequence value) {
                    if (value == null || value.length() > MAX_SECRET) {
                        emit(-1, "", 0, 0);
                        return false;
                    }
                    return true;
                }
                @Override public Editable getEditable() { return active() ? text : null; }
                @Override public void closeConnection() {
                    composing = false;
                    composition = "";
                    super.closeConnection();
                }
                private CharSequence display(CharSequence value) {
                    if (!secure) return value;
                    char[] masked = new char[Character.codePointCount(value, 0, value.length())];
                    Arrays.fill(masked, '\u2022');
                    return new String(masked);
                }
                @Override public boolean commitText(CharSequence value, int cursor) {
                    if (!active() || !bounded(value)) return false;
                    if (!composing && value.length() == 0
                            && Selection.getSelectionStart(text) != Selection.getSelectionEnd(text)) {
                        emit(2, "", 0, 0);
                    } else {
                        emit(composing ? 5 : 1, value.toString(), 0, 0);
                    }
                    composing = false;
                    composition = "";
                    boolean result = super.commitText(display(value), cursor);
                    if (terminal) clear();
                    return result;
                }
                @Override public boolean setComposingText(CharSequence value, int cursor) {
                    if (!active() || !bounded(value)) return false;
                    int count = Character.codePointCount(value, 0, value.length());
                    emit(4, value.toString(), count, count);
                    composing = true;
                    composition = value.toString();
                    return super.setComposingText(display(value), cursor);
                }
                @Override public boolean finishComposingText() {
                    if (!active()) return false;
                    if (composing) {
                        emit(5, composition, 0, 0);
                    }
                    composing = false;
                    composition = "";
                    return super.finishComposingText();
                }
                @Override public boolean deleteSurroundingText(int before, int after) {
                    if (!active() || before < 0 || before > 256 || after != 0) return false;
                    int cursor = Math.max(0, Selection.getSelectionStart(text));
                    int count = Character.codePointCount(text, Math.max(0, cursor - before), cursor);
                    boolean selected = Selection.getSelectionStart(text) != Selection.getSelectionEnd(text);
                    for (int i = 0; i < (terminal || selected ? Math.max(1, count) : count); i++) emit(2, "", 0, 0);
                    return super.deleteSurroundingText(before, after);
                }
                @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
                    if (!active() || before < 0 || before > 256 || after != 0) return false;
                    int cursor = Math.max(0, Selection.getSelectionStart(text));
                    int count = terminal ? before : Math.min(before, Character.codePointCount(text, 0, cursor));
                    for (int i = 0; i < count; i++) emit(2, "", 0, 0);
                    return super.deleteSurroundingTextInCodePoints(before, after);
                }
                @Override public boolean setSelection(int start, int end) {
                    if (!active() || start < 0 || end < start || end > text.length()) return false;
                    if (!terminal) emit(3, "", Character.codePointCount(text, 0, start), Character.codePointCount(text, 0, end));
                    return super.setSelection(start, end);
                }
                @Override public boolean sendKeyEvent(KeyEvent event) {
                    if (!active()) return false;
                    if (event.getAction() != KeyEvent.ACTION_DOWN) return true;
                    if (event.getKeyCode() == KeyEvent.KEYCODE_DEL) emit(2, "", 0, 0);
                    else if (event.getKeyCode() == KeyEvent.KEYCODE_ENTER) emit(1, "\n", 0, 0);
                    else {
                        int code = event.getUnicodeChar();
                        if (code != 0) emit(1, new String(Character.toChars(code)), 0, 0);
                        else emit(6, "", event.getKeyCode(), event.getKeyCode());
                    }
                    return true;
                }
                @Override public boolean performEditorAction(int action) {
                    if (!active()) return false;
                    emit(6, "", KeyEvent.KEYCODE_TAB, KeyEvent.KEYCODE_TAB);
                    return true;
                }
            };
            return connection;
        }
    }
}
