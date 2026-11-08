package io.github.fes.festerm;

import android.graphics.ImageFormat;
import android.graphics.Rect;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.media.MediaMuxer;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.util.Range;

import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ReadOnlyBufferException;
import java.util.Arrays;
import java.util.concurrent.TimeUnit;

/** One silent H.264 MP4 in caller-owned private staging. */
public final class RecordingEncoder implements AutoCloseable {
    private static final String MIME = "video/avc";
    private static final int FPS = 30;
    private static final long FRAME_TIMEOUT = TimeUnit.SECONDS.toNanos(30);
    private static final long FINISH_TIMEOUT = TimeUnit.SECONDS.toNanos(120);

    private final int width, height;
    private final byte[] rgba;
    private final MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
    private FileOutputStream file;
    private MediaCodec codec;
    private MediaMuxer muxer;
    private boolean codecStarted, muxerStarted, closed, finished, failed;
    private int track = -1;
    private long nextFrame;

    public RecordingEncoder(String path, int width, int height) throws IOException {
        if (width < 16 || height < 16 || width > 3840 || height > 2160
                || width % 2 != 0 || height % 2 != 0) {
            throw new IOException("Video dimensions must be even and between 16x16 and 3840x2160");
        }
        this.width = width;
        this.height = height;
        this.rgba = new byte[width * height * 4];
        try {
            FileDescriptor descriptor = Os.open(path,
                OsConstants.O_WRONLY | OsConstants.O_CREAT | OsConstants.O_EXCL | OsConstants.O_NOFOLLOW,
                OsConstants.S_IRUSR | OsConstants.S_IWUSR);
            file = new FileOutputStream(descriptor);
            muxer = new MediaMuxer(file.getFD(), MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4);
            codec = createCodec(width, height);
            codecStarted = true;
        } catch (ErrnoException error) {
            IOException failure = new IOException("Could not create the private MP4 file", error);
            closeAfterFailure(failure);
            throw failure;
        } catch (IOException | IllegalArgumentException | IllegalStateException | SecurityException error) {
            closeAfterFailure(error);
            throw error;
        }
    }

    private static MediaCodec createCodec(int width, int height) throws IOException {
        IOException failure = new IOException(
            "No system H.264 encoder supports the requested frame size and flexible YUV");
        for (MediaCodecInfo candidate : new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos()) {
            if (!candidate.isEncoder()) continue;
            boolean supportsMime = false;
            for (String type : candidate.getSupportedTypes()) {
                if (MIME.equalsIgnoreCase(type)) supportsMime = true;
            }
            if (!supportsMime) continue;

            MediaCodecInfo.CodecCapabilities capabilities;
            try {
                capabilities = candidate.getCapabilitiesForType(MIME);
            } catch (IllegalArgumentException error) {
                failure.addSuppressed(error);
                continue;
            }
            boolean flexible = false;
            for (int color : capabilities.colorFormats) {
                if (color == MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible) flexible = true;
            }
            MediaCodecInfo.VideoCapabilities video = capabilities.getVideoCapabilities();
            if (!flexible || video == null || !video.areSizeAndRateSupported(width, height, FPS)) continue;

            long desiredBitrate = Math.max(2_000_000L, Math.min(24_000_000L, (long) width * height * 4));
            Range<Integer> bitrates = video.getBitrateRange();
            int bitrate = (int) Math.max(bitrates.getLower(), Math.min(bitrates.getUpper(), desiredBitrate));
            MediaFormat format = MediaFormat.createVideoFormat(MIME, width, height);
            format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
            format.setInteger(MediaFormat.KEY_BIT_RATE, bitrate);
            format.setInteger(MediaFormat.KEY_FRAME_RATE, FPS);
            format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
            MediaCodec encoder = null;
            try {
                encoder = MediaCodec.createByCodecName(candidate.getName());
                encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                encoder.start();
                return encoder;
            } catch (IOException | IllegalArgumentException | IllegalStateException error) {
                failure.addSuppressed(error);
                if (encoder != null) {
                    try {
                        encoder.release();
                    } catch (IllegalStateException releaseError) {
                        failure.addSuppressed(releaseError);
                        throw failure;
                    }
                }
            }
        }
        throw failure;
    }

    /** Reads the borrowed direct buffer synchronously and never retains it. */
    public void append(ByteBuffer frame) throws IOException {
        requireActive();
        if (frame == null || frame.remaining() != rgba.length) {
            throw new IOException("The video frame has the wrong dimensions");
        }
        try {
            frame.get(rgba);
            for (int i = 3; i < rgba.length; i += 4) {
                if ((rgba[i] & 0xff) != 255) throw new IOException("Video frames must be opaque RGBA");
            }
            long deadline = System.nanoTime() + FRAME_TIMEOUT;
            int index = acquireInput(deadline);
            ByteBuffer input = codec.getInputBuffer(index);
            if (input == null) throw new IOException("Android did not provide an encoder input buffer");
            int inputSize = input.remaining();
            Image image = codec.getInputImage(index);
            if (image == null || image.getFormat() != ImageFormat.YUV_420_888) {
                throw new IOException("The Android encoder cannot expose flexible YUV frames");
            }
            fillImage(image);
            codec.queueInputBuffer(index, 0, inputSize, frameTimeUs(nextFrame), 0);
            nextFrame = Math.addExact(nextFrame, 1);
            drain(false, deadline);
        } catch (IOException error) {
            failed = true;
            throw error;
        } catch (IllegalArgumentException | IllegalStateException | ArithmeticException
                 | IndexOutOfBoundsException | ReadOnlyBufferException error) {
            failed = true;
            throw new IOException("Android H.264 encoding failed", error);
        }
    }

    public void finish() throws IOException {
        requireActive();
        if (nextFrame == 0) throw new IOException("The video has no frames");
        try {
            long deadline = System.nanoTime() + FINISH_TIMEOUT;
            int index = acquireInput(deadline);
            codec.queueInputBuffer(index, 0, 0, frameTimeUs(nextFrame),
                MediaCodec.BUFFER_FLAG_END_OF_STREAM);
            if (!drain(true, deadline) || !muxerStarted) {
                throw new IOException("Android did not finalize the video stream");
            }
            muxer.stop();
            muxerStarted = false;
            file.getFD().sync();
            close();
            finished = true;
        } catch (IOException error) {
            failed = true;
            throw error;
        } catch (IllegalArgumentException | IllegalStateException | ArithmeticException error) {
            failed = true;
            throw new IOException("Android MP4 finalization failed", error);
        }
    }

    private void requireActive() throws IOException {
        if (closed || finished || failed) throw new IOException("The video encoder is no longer active");
    }

    private int acquireInput(long deadline) throws IOException {
        while (System.nanoTime() < deadline) {
            drain(false, deadline);
            int index = codec.dequeueInputBuffer(10_000);
            if (index >= 0) return index;
            if (index != MediaCodec.INFO_TRY_AGAIN_LATER) {
                throw new IOException("Android returned an invalid encoder input status");
            }
        }
        throw new IOException("The Android encoder did not accept a video frame in time");
    }

    private boolean drain(boolean waitForEnd, long deadline) throws IOException {
        while (System.nanoTime() < deadline) {
            int index = codec.dequeueOutputBuffer(info, waitForEnd ? 10_000 : 0);
            if (index == MediaCodec.INFO_TRY_AGAIN_LATER) {
                if (!waitForEnd) return false;
                continue;
            }
            if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                if (muxerStarted) throw new IOException("Android changed the video format twice");
                track = muxer.addTrack(codec.getOutputFormat());
                if (track < 0) throw new IOException("Could not add the Android video track");
                muxer.start();
                muxerStarted = true;
                continue;
            }
            if (index == MediaCodec.INFO_OUTPUT_BUFFERS_CHANGED) continue;
            if (index < 0) throw new IOException("Android returned an invalid encoder output status");
            try {
                ByteBuffer output = codec.getOutputBuffer(index);
                if (output == null) throw new IOException("Android did not provide an encoded frame");
                if ((info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0 && info.size > 0) {
                    if (!muxerStarted) throw new IOException("Android produced a frame before its video format");
                    output.position(info.offset);
                    output.limit(info.offset + info.size);
                    muxer.writeSampleData(track, output, info);
                }
            } finally {
                codec.releaseOutputBuffer(index, false);
            }
            if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) return true;
        }
        throw new IOException("The Android encoder did not finish in time");
    }

    private void fillImage(Image image) throws IOException {
        Rect crop = image.getCropRect();
        if (crop.width() != width || crop.height() != height || crop.left < 0 || crop.top < 0
                || crop.right > image.getWidth() || crop.bottom > image.getHeight()
                || crop.left % 2 != 0 || crop.top % 2 != 0) {
            throw new IOException("Android returned an incompatible video frame crop");
        }
        Image.Plane[] planes = image.getPlanes();
        if (planes.length != 3) throw new IOException("Android returned invalid YUV planes");
        Plane yPlane = new Plane(planes[0]), uPlane = new Plane(planes[1]), vPlane = new Plane(planes[2]);
        for (int y = 0; y < height; y += 2) {
            for (int x = 0; x < width; x += 2) {
                int sumR = 0, sumG = 0, sumB = 0;
                for (int dy = 0; dy < 2; dy++) {
                    for (int dx = 0; dx < 2; dx++) {
                        int offset = ((y + dy) * width + x + dx) * 4;
                        int red = rgba[offset] & 0xff;
                        int green = rgba[offset + 1] & 0xff;
                        int blue = rgba[offset + 2] & 0xff;
                        yPlane.put(crop.left + x + dx, crop.top + y + dy,
                            ((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16);
                        sumR += red;
                        sumG += green;
                        sumB += blue;
                    }
                }
                int red = (sumR + 2) / 4, green = (sumG + 2) / 4, blue = (sumB + 2) / 4;
                int cx = (crop.left + x) / 2, cy = (crop.top + y) / 2;
                uPlane.put(cx, cy, ((-38 * red - 74 * green + 112 * blue + 128) >> 8) + 128);
                vPlane.put(cx, cy, ((112 * red - 94 * green - 18 * blue + 128) >> 8) + 128);
            }
        }
    }

    private static final class Plane {
        private final ByteBuffer buffer;
        private final int base, rowStride, pixelStride;

        Plane(Image.Plane plane) throws IOException {
            buffer = plane.getBuffer();
            base = buffer.position();
            rowStride = plane.getRowStride();
            pixelStride = plane.getPixelStride();
            if (rowStride <= 0 || pixelStride <= 0) throw new IOException("Invalid Android YUV stride");
        }

        void put(int column, int row, int value) throws IOException {
            long offset = (long) base + (long) row * rowStride + (long) column * pixelStride;
            if (column < 0 || row < 0 || (long) column * pixelStride >= rowStride
                    || offset < base || offset >= buffer.limit()) {
                throw new IOException("Android YUV plane is too small");
            }
            buffer.put((int) offset, (byte) Math.max(0, Math.min(255, value)));
        }
    }

    private static long frameTimeUs(long index) throws IOException {
        try {
            return Math.multiplyExact(index, 1_000_000L) / FPS;
        } catch (ArithmeticException error) {
            throw new IOException("The video duration overflows", error);
        }
    }

    private void closeAfterFailure(Exception failure) {
        try {
            close();
        } catch (IOException cleanup) {
            failure.addSuppressed(cleanup);
        }
    }

    @Override public void close() throws IOException {
        if (closed) return;
        closed = true;
        IOException failure = null;
        if (codec != null) {
            if (codecStarted) {
                try { codec.stop(); } catch (IllegalStateException error) {
                    failure = new IOException("Could not stop the Android encoder", error);
                }
                codecStarted = false;
            }
            try { codec.release(); } catch (IllegalStateException error) {
                failure = merge(failure, new IOException("Could not release the Android encoder", error));
            }
            codec = null;
        }
        if (muxer != null) {
            if (muxerStarted) {
                try { muxer.stop(); } catch (IllegalStateException error) {
                    failure = merge(failure, new IOException("Could not stop the Android MP4 writer", error));
                }
                muxerStarted = false;
            }
            try { muxer.release(); } catch (IllegalStateException error) {
                failure = merge(failure, new IOException("Could not release the Android MP4 writer", error));
            }
            muxer = null;
        }
        if (file != null) {
            try { file.close(); } catch (IOException error) { failure = merge(failure, error); }
            file = null;
        }
        Arrays.fill(rgba, (byte) 0);
        if (failure != null) throw failure;
    }

    private static IOException merge(IOException first, IOException next) {
        if (first == null) return next;
        first.addSuppressed(next);
        return first;
    }
}
