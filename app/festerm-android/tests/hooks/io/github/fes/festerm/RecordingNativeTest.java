package io.github.fes.festerm;

/** Present only in explicitly requested native-validation APKs. */
public final class RecordingNativeTest {
    private RecordingNativeTest() {}
    public static native int encode(String directory);
}
