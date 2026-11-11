package io.github.fes.festerm.tests;

import android.app.Instrumentation;
import android.content.Intent;
import android.os.Bundle;
import io.github.fes.festerm.FestermActivity;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;

/** Focused opt-in invocation; no authority or native handle crosses this hook. */
public final class RootFactorySmoke extends Instrumentation {
    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            FestermActivity activity = (FestermActivity) startActivitySync(
                new Intent(getTargetContext(), FestermActivity.class)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            waitForIdleSync();
            Thread.sleep(2000);
            int uid = android.os.Process.myUid();
            String domain = new String(Files.readAllBytes(Paths.get("/proc/self/attr/current")),
                StandardCharsets.US_ASCII).trim();
            if (uid < 10000 || uid != activity.getApplicationInfo().uid
                || !(domain.startsWith("u:r:untrusted_app:")
                    || domain.startsWith("u:r:untrusted_app_"))) {
                throw new AssertionError();
            }
            Class<?> hook = Class.forName("io.github.fes.festerm.RecordingNativeTest");
            Object status = hook.getMethod("applicationPrivateRoot").invoke(null);
            if (!Integer.valueOf(0).equals(status)) throw new AssertionError();
            result.putString("stream", "PASS: actual Rust current-app root factory\n");
            finish(-1, result);
        } catch (Throwable failure) {
            result.putString("stream", "FAIL: actual Rust current-app root factory\n");
            finish(0, result);
        }
    }
}
