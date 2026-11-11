package io.github.fes.festerm.tests;

import android.app.*;
import android.content.*;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.media.MediaMetadataRetriever;
import android.net.Uri;
import android.os.*;
import android.provider.DocumentsContract;
import android.system.Os;
import android.system.OsConstants;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.inputmethod.*;
import io.github.fes.festerm.FestermActivity;
import io.github.fes.festerm.RecordingEncoder;
import java.io.*;
import java.lang.reflect.*;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.atomic.AtomicReference;

/** Runs in a separate test APK; no test entry points ship in the application. */
public final class PlatformSmoke extends Instrumentation {
    private boolean recordingValidation;

    @Override public void onCreate(Bundle args) {
        super.onCreate(args);
        recordingValidation = "true".equals(args.getString("recording-validation"));
        start();
    }

    private static void require(boolean condition, String label) {
        if (!condition) throw new AssertionError(label);
    }

    private void onMain(Runnable action) {
        AtomicReference<Throwable> failure = new AtomicReference<>();
        runOnMainSync(() -> {
            try { action.run(); } catch (Throwable error) { failure.set(error); }
        });
        if (failure.get() != null) throw new AssertionError(failure.get());
    }

    private static void requireColor(Bitmap frame, int x, int y, int red, int green, int blue, String label) {
        int color = frame.getPixel(x, y);
        require(Math.abs(Color.red(color) - red) < 70
            && Math.abs(Color.green(color) - green) < 70
            && Math.abs(Color.blue(color) - blue) < 70, label);
    }

    private static List<Long> videoTimes(File file) throws IOException {
        MediaExtractor extractor = new MediaExtractor();
        try {
            extractor.setDataSource(file.getPath());
            require(extractor.getTrackCount() == 1, "one synchronized video track");
            require("video/avc".equals(extractor.getTrackFormat(0).getString(MediaFormat.KEY_MIME)),
                "synchronized H.264 track");
            extractor.selectTrack(0);
            List<Long> times = new ArrayList<>();
            do {
                long time = extractor.getSampleTime();
                if (time < 0) break;
                times.add(time);
            } while (extractor.advance());
            return times;
        } finally {
            extractor.release();
        }
    }

    private static List<Long> videoTimes(Context context, Uri uri) throws IOException {
        MediaExtractor extractor = new MediaExtractor();
        try {
            extractor.setDataSource(context, uri, null);
            require(extractor.getTrackCount() == 1, "one provider MP4 track");
            require("video/avc".equals(extractor.getTrackFormat(0).getString(MediaFormat.KEY_MIME)),
                "provider MP4 is H.264");
            extractor.selectTrack(0);
            List<Long> times = new ArrayList<>();
            do {
                long time = extractor.getSampleTime();
                if (time < 0) break;
                times.add(time);
            } while (extractor.advance());
            return times;
        } finally {
            extractor.release();
        }
    }

    private static boolean tap(AccessibilityNodeInfo node, String label) {
        if (node == null) return false;
        CharSequence text = node.getText(), description = node.getContentDescription();
        if ((text != null && label.equalsIgnoreCase(text.toString()))
            || (description != null && label.equalsIgnoreCase(description.toString()))) {
            AccessibilityNodeInfo candidate = node;
            while (candidate != null) {
                if (candidate.isEnabled() && candidate.isClickable()
                    && candidate.performAction(AccessibilityNodeInfo.ACTION_CLICK)) return true;
                candidate = candidate.getParent();
            }
        }
        for (int index = 0; index < node.getChildCount(); index++) {
            if (tap(node.getChild(index), label)) return true;
        }
        return false;
    }

    private Uri recordingTree(FestermActivity activity) throws Exception {
        String target = "primary:Documents/fes-recording-00dd";
        for (android.content.UriPermission permission : activity.getContentResolver().getPersistedUriPermissions()) {
            if (permission.isReadPermission() && permission.isWritePermission()
                && DocumentsContract.isTreeUri(permission.getUri())
                && target.equals(DocumentsContract.getTreeDocumentId(permission.getUri()))) return permission.getUri();
        }
        Field request = FestermActivity.class.getDeclaredField("PICK_RECORDING_FOLDER");
        request.setAccessible(true);
        Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)
            .putExtra(DocumentsContract.EXTRA_INITIAL_URI,
                DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", target));
        int code = request.getInt(null);
        onMain(() -> activity.startActivityForResult(pick, code));
        UiAutomation automation = getUiAutomation();
        long deadline = SystemClock.uptimeMillis() + 30000;
        while (SystemClock.uptimeMillis() < deadline) {
            for (android.content.UriPermission permission : activity.getContentResolver().getPersistedUriPermissions()) {
                if (permission.isReadPermission() && permission.isWritePermission()
                    && DocumentsContract.isTreeUri(permission.getUri())
                    && target.equals(DocumentsContract.getTreeDocumentId(permission.getUri()))) return permission.getUri();
            }
            AccessibilityNodeInfo root = automation.getRootInActiveWindow();
            if (!tap(root, "Use this folder")) tap(root, "Allow");
            Thread.sleep(300);
        }
        throw new AssertionError("recording folder picker did not grant the test directory");
    }

    private void providerRecording(FestermActivity activity) throws Exception {
        Uri tree = recordingTree(activity);
        ContentResolver resolver = activity.getContentResolver();
        Uri root = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree));
        String base = "recording-native-" + UUID.randomUUID().toString();
        Uri existing = DocumentsContract.createDocument(resolver, root,
            DocumentsContract.Document.MIME_TYPE_DIR, base);
        require(existing != null, "create preexisting export fixture");
        Uri keep = DocumentsContract.createDocument(resolver, existing, "text/plain", "keep.txt");
        require(keep != null, "create preexisting export marker");
        try (OutputStream output = resolver.openOutputStream(keep, "w")) {
            require(output != null, "open preexisting export marker");
            output.write("preserve".getBytes(StandardCharsets.UTF_8));
        }
        File parent = new File(activity.recordingDirectory());
        File local = new File(parent, "provider-test-" + UUID.randomUUID());
        require(local.mkdir(), "create provider recording test directory");
        Uri published = null;
        Uri appearance = null;
        List<Uri> documents = new ArrayList<>();
        List<Uri> videos = new ArrayList<>();
        try {
            Class<?> hooks = Class.forName("io.github.fes.festerm.RecordingNativeTest",
                true, activity.getClassLoader());
            String result = (String) hooks.getMethod("publish", String.class, String.class, String.class)
                .invoke(null, local.getPath(), tree.toString(), base);
            require(result != null, "Rust-to-Android provider recording publication");
            published = Uri.parse(result);
            require(!DocumentsContract.getDocumentId(existing).equals(DocumentsContract.getDocumentId(published)),
                "provider publication did not reuse an existing directory");
            Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(
                tree, DocumentsContract.getDocumentId(published));
            try (android.database.Cursor cursor = resolver.query(children,
                    new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                        DocumentsContract.Document.COLUMN_MIME_TYPE}, null, null, null)) {
                require(cursor != null, "list published provider files");
                while (cursor.moveToNext()) {
                    Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0));
                    documents.add(document);
                    String name = cursor.getString(1), mime = cursor.getString(2);
                    if ("recording-appearance.json".equals(name)) {
                        require(appearance == null && "application/json".equals(mime),
                            "one correctly typed appearance document");
                        appearance = document;
                    } else {
                        require(name.startsWith("session-") && name.endsWith(".mp4")
                            && "video/mp4".equals(mime), "unexpected provider document: " + name);
                        videos.add(document);
                    }
                }
            }
            require(videos.size() == 2, "two provider MP4s");
            require(appearance != null, "provider appearance document");
            try (InputStream input = resolver.openInputStream(appearance)) {
                require(input != null, "open provider appearance document");
                ByteArrayOutputStream contents = new ByteArrayOutputStream();
                byte[] buffer = new byte[1024];
                int count;
                while ((count = input.read(buffer)) != -1) contents.write(buffer, 0, count);
                org.json.JSONObject metadata = new org.json.JSONObject(
                    new String(contents.toByteArray(), StandardCharsets.UTF_8));
                require("jet_brains_mono".equals(metadata.getString("font"))
                    && metadata.getInt("width") == 256 && metadata.getInt("height") == 128
                    && !metadata.getBoolean("recovered_prefix"), "provider appearance identity");
            }
            List<Long> first = videoTimes(activity, videos.get(0));
            require(first.size() >= 4 && first.equals(videoTimes(activity, videos.get(1))),
                "provider MP4s retain the synchronized frame schedule");
            try (InputStream input = resolver.openInputStream(keep)) {
                require(input != null, "open existing provider marker");
                ByteArrayOutputStream contents = new ByteArrayOutputStream();
                byte[] buffer = new byte[64];
                int count;
                while ((count = input.read(buffer)) != -1) contents.write(buffer, 0, count);
                require(Arrays.equals(contents.toByteArray(), "preserve".getBytes(StandardCharsets.UTF_8)),
                    "existing provider export remains untouched");
            }
        } finally {
            for (Uri document : documents) require(DocumentsContract.deleteDocument(resolver, document),
                "remove provider test document");
            if (published != null) require(DocumentsContract.deleteDocument(resolver, published),
                "remove provider test export");
            require(DocumentsContract.deleteDocument(resolver, keep), "remove preexisting marker");
            require(DocumentsContract.deleteDocument(resolver, existing), "remove preexisting fixture");
            File staging = new File(local, "staging");
            File[] files = staging.listFiles();
            if (files != null) for (File file : files) {
                require(file.getName().startsWith("stream-") && file.delete(), "remove staged test MP4");
            }
            if (staging.exists()) require(staging.delete(), "remove provider staging");
            File sources = new File(local, "sources");
            File[] remaining = sources.listFiles();
            if (remaining != null) for (File file : remaining) {
                require(file.getName().startsWith("recording-") && file.delete(), "remove retained test source");
            }
            if (sources.exists()) require(sources.delete(), "remove provider source directory");
            File[] journals = local.listFiles();
            if (journals != null) for (File journal : journals) {
                if (journal.getName().startsWith("provider-") && journal.getName().endsWith(".json"))
                    require(journal.delete(), "remove provider test journal");
            }
            require(local.delete(), "remove provider test directory");
        }
    }

    private static void nativeRecording(FestermActivity activity, File directory) throws Exception {
        Class<?> hooks = Class.forName("io.github.fes.festerm.RecordingNativeTest",
            true, activity.getClassLoader());
        int status = (Integer) hooks.getMethod("encode", String.class).invoke(null, directory.getPath());
        File staging = new File(directory, "staging");
        File sources = new File(directory, "sources");
        File first = new File(staging, "stream-01.mp4"), second = new File(staging, "stream-02.mp4");
        try {
            require(status == 0, "Rust-to-Java synchronized recording export");
            List<Long> firstTimes = videoTimes(first), secondTimes = videoTimes(second);
            require(firstTimes.size() >= 4 && firstTimes.equals(secondTimes), "shared frame schedule");
            MediaMetadataRetriever red = new MediaMetadataRetriever(), green = new MediaMetadataRetriever();
            try {
                red.setDataSource(first.getPath());
                green.setDataSource(second.getPath());
                Bitmap redFrame = red.getFrameAtTime(100_000, MediaMetadataRetriever.OPTION_CLOSEST);
                Bitmap greenFrame = green.getFrameAtTime(100_000, MediaMetadataRetriever.OPTION_CLOSEST);
                require(redFrame != null && greenFrame != null, "decoded synchronized frames");
                int redPixels = 0, greenPixels = 0;
                for (int y = 0; y < redFrame.getHeight(); y++) for (int x = 0; x < redFrame.getWidth(); x++) {
                    int r = redFrame.getPixel(x, y), g = greenFrame.getPixel(x, y);
                    if (Color.red(r) > 90 && Color.red(r) > Color.green(r) * 3 / 2) redPixels++;
                    if (Color.green(g) > 90 && Color.green(g) > Color.red(g) * 3 / 2) greenPixels++;
                }
                require(redPixels > 10 && greenPixels > 10, "separate rendered terminal streams");
                redFrame.recycle();
                greenFrame.recycle();
            } finally {
                red.release();
                green.release();
            }
            File[] files = sources.listFiles();
            require(files != null && files.length == 1 && files[0].getName().endsWith(".fstrec"),
                "source retained after staging");
        } finally {
            if (first.exists()) require(first.delete(), "remove first Rust recording video");
            if (second.exists()) require(second.delete(), "remove second Rust recording video");
            if (staging.exists()) require(staging.delete(), "remove Rust recording staging");
            File[] files = sources.listFiles();
            if (files != null) for (File file : files) {
                require(file.getName().startsWith("recording-") && file.delete(), "remove Rust recording source");
            }
            if (sources.exists()) require(sources.delete(), "remove Rust recording source directory");
        }
    }

    private static void recordingEncoder(FestermActivity activity, boolean nativeValidation) throws Exception {
        File parent = new File(activity.recordingDirectory());
        require(parent.getCanonicalPath().startsWith(activity.getNoBackupFilesDir().getCanonicalPath() + "/"),
            "recording storage excludes Auto Backup");
        require((Os.stat(parent.getPath()).st_mode & 0777) == 0700, "private recording directory");
        File directory = new File(parent, "native-test-" + UUID.randomUUID());
        require(directory.mkdir(), "create recording test directory");
        File video = new File(directory, "video.mp4");
        File padded = new File(directory, "padded.mp4");
        File link = new File(directory, "link.mp4");
        try {
            try (RecordingEncoder encoder = activity.newRecordingEncoder(video.getPath(), 64, 64)) {
                ByteBuffer frame = ByteBuffer.allocateDirect(64 * 64 * 4);
                for (int index = 0; index < 12; index++) {
                    frame.clear();
                    for (int y = 0; y < 64; y++) for (int x = 0; x < 64; x++) {
                        boolean first = index < 6;
                        int red = first ? (x < 32 && y < 32 ? 255 : 0) : (x >= 32 ? 255 : 0);
                        int green = first ? (x >= 32 && y < 32 ? 255 : 0) : (y < 32 ? 255 : 0);
                        int blue = first ? (y >= 32 ? 255 : 0) : (x < 32 && y >= 32 ? 255 : 0);
                        frame.put((byte) red).put((byte) green).put((byte) blue).put((byte) 255);
                    }
                    frame.flip();
                    encoder.append(frame);
                }
                encoder.finish();
            }
            require((Os.stat(video.getPath()).st_mode & 0777) == 0600, "private video file");
            try (RecordingEncoder ignored = activity.newRecordingEncoder(video.getPath(), 64, 64)) {
                throw new AssertionError("existing video overwritten");
            } catch (IOException expected) {
                require(video.isFile(), "existing video preserved");
            }
            Os.symlink(video.getPath(), link.getPath());
            try (RecordingEncoder ignored = activity.newRecordingEncoder(link.getPath(), 64, 64)) {
                throw new AssertionError("symlink video accepted");
            } catch (IOException expected) {
                require(video.isFile(), "symlink target preserved");
            }

            MediaExtractor extractor = new MediaExtractor();
            try {
                extractor.setDataSource(video.getPath());
                require(extractor.getTrackCount() == 1, "one silent video track");
                MediaFormat format = extractor.getTrackFormat(0);
                require("video/avc".equals(format.getString(MediaFormat.KEY_MIME)), "H.264 track");
                require(format.getInteger(MediaFormat.KEY_WIDTH) == 64
                    && format.getInteger(MediaFormat.KEY_HEIGHT) == 64, "video dimensions");
                extractor.selectTrack(0);
                int count = 0;
                long previous = -1;
                do {
                    long time = extractor.getSampleTime();
                    if (time < 0) break;
                    require(time > previous, "monotonic frame times");
                    require(Math.abs(time - count * 1_000_000L / 30) <= 1, "30 fps timestamps");
                    previous = time;
                    count++;
                } while (extractor.advance());
                require(count == 12, "all frames encoded");
            } finally {
                extractor.release();
            }

            MediaMetadataRetriever retriever = new MediaMetadataRetriever();
            try {
                retriever.setDataSource(video.getPath());
                Bitmap first = retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST);
                Bitmap later = retriever.getFrameAtTime(8 * 1_000_000L / 30,
                    MediaMetadataRetriever.OPTION_CLOSEST);
                require(first != null && later != null, "decoded video frames");
                requireColor(first, 16, 16, 255, 0, 0, "top-left red and orientation");
                requireColor(first, 48, 16, 0, 255, 0, "top-right green");
                requireColor(first, 16, 48, 0, 0, 255, "bottom-left blue");
                requireColor(later, 48, 16, 255, 255, 0, "later frame yellow");
                requireColor(later, 16, 48, 0, 0, 255, "later frame blue");
                first.recycle();
                later.recycle();
            } finally {
                retriever.release();
            }

            try (RecordingEncoder encoder = activity.newRecordingEncoder(padded.getPath(), 66, 66)) {
                ByteBuffer frame = ByteBuffer.allocateDirect(66 * 66 * 4);
                for (int index = 0; index < 3; index++) {
                    frame.clear();
                    for (int pixel = 0; pixel < 66 * 66; pixel++) {
                        frame.put((byte) 0).put((byte) 255).put((byte) 0).put((byte) 255);
                    }
                    frame.flip();
                    encoder.append(frame);
                }
                encoder.finish();
            }
            MediaMetadataRetriever paddedRetriever = new MediaMetadataRetriever();
            try {
                paddedRetriever.setDataSource(padded.getPath());
                Bitmap frame = paddedRetriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST);
                require(frame != null && frame.getWidth() == 66 && frame.getHeight() == 66,
                    "unaligned frame dimensions");
                requireColor(frame, 60, 60, 0, 255, 0, "unaligned frame color and stride");
                frame.recycle();
            } finally {
                paddedRetriever.release();
            }
            if (nativeValidation) nativeRecording(activity, directory);
        } finally {
            if (link.exists()) require(link.delete(), "remove recording test link");
            if (video.exists()) require(video.delete(), "remove recording test video");
            if (padded.exists()) require(padded.delete(), "remove padded recording test video");
            require(directory.delete(), "remove recording test directory");
        }
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            FestermActivity activity = (FestermActivity) startActivitySync(
                new Intent(getTargetContext(), FestermActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            waitForIdleSync();
            Thread.sleep(2000);
            recordingEncoder(activity, recordingValidation);
            if (recordingValidation) providerRecording(activity);
            String reference = UUID.randomUUID().toString();
            byte[] fixture = "native-store-fixture".getBytes(StandardCharsets.UTF_8);
            require(activity.secretWrite(reference, fixture.clone(), false), "secret insert");
            require(Arrays.equals(activity.secretRead(reference), fixture), "secret round trip");
            require(!activity.secretWrite(reference, fixture.clone(), false), "insert must not replace");
            require(activity.secretWrite(reference, new byte[]{1, 2, 3}, true), "secret update");
            require(Arrays.equals(activity.secretRead(reference), new byte[]{1, 2, 3}), "updated value");
            File envelope = new File(activity.getNoBackupFilesDir(), "secrets/" + reference);
            require(envelope.isFile(), "ciphertext private storage");
            byte[] encrypted;
            try (FileInputStream source = new FileInputStream(envelope)) {
                encrypted = new byte[(int) envelope.length()];
                require(source.read(encrypted) == encrypted.length, "ciphertext read");
            }
            require(!Arrays.equals(encrypted, new byte[]{1, 2, 3}), "encrypted at rest");
            encrypted[encrypted.length - 1] ^= 1;
            try (FileOutputStream output = new FileOutputStream(envelope)) { output.write(encrypted); }
            boolean rejected = false;
            try { activity.secretRead(reference); } catch (java.security.GeneralSecurityException expected) { rejected = true; }
            require(rejected, "tampering rejected");
            require(activity.secretDelete(reference), "secret delete");
            require(activity.secretRead(reference) == null, "deleted reference missing");
            require(!activity.secretDelete(reference), "idempotent deletion");

            try (ParcelFileDescriptor root = ParcelFileDescriptor.adoptFd(activity.privateDirectoryDescriptor())) {
                android.system.StructStat pinned = Os.fstat(root.getFileDescriptor());
                android.system.StructStat expected = Os.stat(new File(activity.getFilesDir(), "festerm").getPath());
                require(pinned.st_uid == android.os.Process.myUid() && (pinned.st_mode & 0777) == 0700,
                    "native application root owner");
                require(pinned.st_dev == expected.st_dev && pinned.st_ino == expected.st_ino,
                    "native root keeps existing receipt location");
            }
            for (String suffix : new String[]{".bak", ".new", ".reserved"}) {
                String interruptedReference = UUID.randomUUID().toString();
                File interrupted = new File(activity.getNoBackupFilesDir(),
                    "secrets/" + interruptedReference + suffix);
                byte[] retained = new byte[]{8, 3, 5};
                java.nio.file.Files.createFile(interrupted.toPath(),
                    java.nio.file.attribute.PosixFilePermissions.asFileAttribute(
                        java.nio.file.attribute.PosixFilePermissions.fromString("rw-------")));
                try (FileOutputStream output = new FileOutputStream(interrupted)) {
                    output.write(retained);
                    output.getFD().sync();
                }
                android.system.StructStat owned = Os.lstat(interrupted.getPath());
                require(owned.st_uid == android.os.Process.myUid() && (owned.st_mode & 0777) == 0600,
                    "valid private interrupted fixture");
                boolean createRefused;
                try {
                    createRefused = !activity.secretWrite(interruptedReference, fixture.clone(), false);
                } catch (IOException expected) {
                    createRefused = true;
                }
                require(createRefused, "native create refuses interrupted material");
                boolean readRefused = false;
                try { activity.secretRead(interruptedReference); } catch (IOException expected) { readRefused = true; }
                require(readRefused, "interrupted native secret is not absence");
                require(Arrays.equals(java.nio.file.Files.readAllBytes(interrupted.toPath()), retained),
                    "interrupted material preserved");
                require(!new File(activity.getNoBackupFilesDir(), "secrets/" + interruptedReference).exists(),
                    "no base created from interrupted state");
                require(interrupted.delete(), "remove only owned smoke fixture");
            }

            activity.copyText("native clipboard fixture");
            require("native clipboard fixture".equals(activity.clipboardText()), "clipboard round trip");
            activity.copyText("");
            onMain(() -> {
                activity.privacy(true, true, true);
                require((activity.getWindow().getAttributes().flags & WindowManager.LayoutParams.FLAG_SECURE) != 0,
                    "secure snapshot flag");
                activity.privacy(false, false, false);
            });

            Field picking = FestermActivity.class.getDeclaredField("picking");
            Field pickRequest = FestermActivity.class.getDeclaredField("PICK_DOCUMENT");
            Method pickResult = FestermActivity.class.getDeclaredMethod(
                "onActivityResult", int.class, int.class, Intent.class);
            picking.setAccessible(true);
            pickRequest.setAccessible(true);
            pickResult.setAccessible(true);
            int request = pickRequest.getInt(null);
            onMain(() -> {
                try {
                    picking.setBoolean(activity, true);
                    Intent ungranted = new Intent().setData(android.net.Uri.fromFile(
                        new File(activity.getFilesDir(), "private.txt")));
                    pickResult.invoke(activity, request, Activity.RESULT_OK, ungranted);
                    require(!picking.getBoolean(activity), "file URI refused before scheduling import");
                } catch (ReflectiveOperationException error) {
                    throw new AssertionError(error);
                }
            });

            // The actual JNI input connection must accept Unicode and composition.
            Field inputField = FestermActivity.class.getDeclaredField("input");
            inputField.setAccessible(true);
            View input = (View) inputField.get(activity);
            onMain(() -> {
                activity.fieldDocument(1234, "a😀b", 2, 2, false, false);
                activity.keyboardMode(3);
                InputConnection connection = input.onCreateInputConnection(new EditorInfo());
                require(connection != null, "native input connection");
                require("a😀".contentEquals(connection.getTextBeforeCursor(20, 0)), "code point selection");
                require(connection.commitText("é", 1), "Unicode commit");
                activity.inputConsumed(0);
                activity.fieldDocument(1234, "stale", 5, 5, false, false);
                require("a😀é".contentEquals(connection.getTextBeforeCursor(20, 0)), "Unicode retained");
                require(connection.setComposingText("文", 1), "IME preedit");
                require(connection.commitText("文字", 1), "IME commit");
                require("a😀é文字".contentEquals(connection.getTextBeforeCursor(20, 0)), "composition replaces preedit");
                require(connection.deleteSurroundingTextInCodePoints(1, 0), "code point deletion");
                require("a😀é文".contentEquals(connection.getTextBeforeCursor(20, 0)), "deletion retained");
                require(connection.setSelection(0, 3), "Unicode range selection");
                require(connection.commitText("", 1), "selected text deletion");
                require("é文b".contentEquals(connection.getTextAfterCursor(20, 0)), "selected text removed");
                activity.fieldDocument(1235, "••", 2, 2, true, false);
                InputConnection secure = input.onCreateInputConnection(new EditorInfo());
                require(!connection.commitText("stale", 1), "old field input refused");
                CharSequence oldContext = connection.getTextBeforeCursor(20, 0);
                require(oldContext == null || oldContext.length() == 0, "old connection cannot read another field");
                require(secure.commitText("😀", 1), "secure Unicode input");
                require("•••".contentEquals(secure.getTextBeforeCursor(20, 0)), "secure context is masked");
                // The app-owned keyboard keeps the text-aware client for the same field.
                activity.keyboardMode(4);
                require(!secure.commitText("late", 1), "keyboard change retires the old connection");
                InputConnection custom = input.onCreateInputConnection(new EditorInfo());
                require(custom.commitText("😀", 1), "field-custom input reaches the field");
                activity.keyboardMode(0);
                require(!secure.commitText("late", 1), "hidden input refused");
                require(!custom.commitText("late", 1), "hidden field-custom input refused");
            });
            waitForIdleSync();
            Class<?> verifier = Class.forName("org.rustls.platformverifier.CertificateVerifier");
            require(verifier != null, "native TLS verifier packaged");
            Class.forName("kotlin.enums.EnumEntriesKt");
            result.putString("stream", "PASS: H.264 recording"
                + (recordingValidation ? ", Rust JNI export and Files-provider publication" : "")
                + ", Keystore encryption/tamper/delete, clipboard, privacy, document URI, JNI IME, TLS classes\n");
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("stream", "FAIL: " + error.getClass().getName() + ": " + error.getMessage() + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }
}
