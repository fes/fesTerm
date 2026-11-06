package io.github.fes.festerm.tests;

import android.app.*;
import android.content.*;
import android.os.*;
import android.view.*;
import android.view.inputmethod.*;
import io.github.fes.festerm.FestermActivity;
import java.io.*;
import java.lang.reflect.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.atomic.AtomicReference;

/** Runs in a separate test APK; no test entry points ship in the application. */
public final class PlatformSmoke extends Instrumentation {
    @Override public void onCreate(Bundle args) { super.onCreate(args); start(); }

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

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            FestermActivity activity = (FestermActivity) startActivitySync(
                new Intent(getTargetContext(), FestermActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            waitForIdleSync();
            Thread.sleep(2000);
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
            result.putString("stream", "PASS: Keystore encryption/tamper/delete, clipboard, privacy, document URI, JNI IME, TLS classes\n");
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("stream", "FAIL: " + error.getClass().getName() + ": " + error.getMessage() + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }
}
