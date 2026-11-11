package io.github.fes.festerm;

import java.io.Closeable;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.channels.FileLock;
import java.nio.channels.OverlappingFileLockException;
import java.nio.file.*;
import java.nio.file.attribute.PosixFilePermissions;

/**
 * Native-vault namespace only. The Activity supplies a pinned, validated root;
 * neither references nor ordinary configuration select this directory.
 */
final class SecretNamespace implements Closeable {
    static final String LOCK = ".namespace";
    private static final LinkOption NOFOLLOW = LinkOption.NOFOLLOW_LINKS;
    private static final java.util.concurrent.Semaphore LOCAL = new java.util.concurrent.Semaphore(1);

    interface Guard {
        void check(Path path) throws IOException;
    }

    interface Sync {
        void run() throws IOException;
    }

    interface Step {
        void reached(String step) throws IOException;
    }

    final Path directory;
    private final Guard guard;
    private final Sync sync;
    private final FileChannel channel;
    private final FileLock lock;

    static SecretNamespace acquire(Path directory, Guard guard, Sync sync) throws IOException {
        if (!LOCAL.tryAcquire()) throw new IOException("Secret namespace busy");
        try {
            return acquireLocal(directory, guard, sync);
        } catch (IOException | RuntimeException error) {
            LOCAL.release();
            throw error;
        }
    }

    private static SecretNamespace acquireLocal(Path directory, Guard guard, Sync sync) throws IOException {
        Path name = directory.resolve(LOCK);
        if (Files.exists(name, NOFOLLOW)) guard.check(name);
        FileChannel channel = FileChannel.open(name,
            new java.util.HashSet<>(java.util.Arrays.asList(
                StandardOpenOption.WRITE, StandardOpenOption.CREATE, NOFOLLOW)),
            PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")));
        FileLock lock = null;
        try {
            guard.check(name);
            try {
                lock = channel.tryLock();
            } catch (OverlappingFileLockException busy) {
                throw new IOException("Secret namespace busy", busy);
            }
            if (lock == null) throw new IOException("Secret namespace busy");
            sync.run();
            return new SecretNamespace(directory, guard, sync, channel, lock);
        } catch (IOException | RuntimeException error) {
            try {
                if (lock != null) lock.release();
            } finally {
                channel.close();
            }
            throw error;
        }
    }

    private SecretNamespace(Path directory, Guard guard, Sync sync,
            FileChannel channel, FileLock lock) {
        this.directory = directory;
        this.guard = guard;
        this.sync = sync;
        this.channel = channel;
        this.lock = lock;
    }

    Path path(String reference) {
        if (!reference.matches("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")) {
            throw new IllegalArgumentException("Invalid secret reference");
        }
        return directory.resolve(reference);
    }

    private boolean present(Path path) throws IOException {
        // exists() suppresses permission/I/O errors. Only a definite ENOENT is
        // absence; an unreadable or malformed entry never permits new issuance.
        try {
            Files.readAttributes(path, java.nio.file.attribute.BasicFileAttributes.class, NOFOLLOW);
        } catch (NoSuchFileException missing) {
            return false;
        }
        guard.check(path);
        return true;
    }

    boolean existing(String reference) throws IOException {
        Path base = path(reference);
        for (String suffix : new String[]{".bak", ".new", ".reserved"}) {
            if (present(directory.resolve(reference + suffix))) {
                throw new IOException("Uncertain native secret material");
            }
        }
        return present(base);
    }

    boolean create(String reference, byte[] envelope) throws IOException {
        return create(reference, envelope, step -> {});
    }

    boolean create(String reference, byte[] envelope, Step step) throws IOException {
        if (existing(reference)) return false;
        Path stage = directory.resolve(reference + ".reserved");
        // This reference-specific stage is itself the durable reservation.
        // On any ambiguity it is deliberately retained; no reset/retry cleanup.
        try (FileChannel output = FileChannel.open(stage,
                new java.util.HashSet<>(java.util.Arrays.asList(
                    StandardOpenOption.WRITE, StandardOpenOption.CREATE_NEW, NOFOLLOW)),
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))) {
            guard.check(stage);
            output.force(true);
            sync.run();
            step.reached("reserved");
            ByteBuffer remaining = ByteBuffer.wrap(envelope);
            while (remaining.hasRemaining()) output.write(remaining);
            output.force(true);
            step.reached("written");
        }
        // link is atomic create-new, unlike ATOMIC_MOVE which may replace an
        // existing target even without REPLACE_EXISTING on some providers.
        Files.createLink(path(reference), stage);
        sync.run();
        step.reached("published");
        Files.delete(stage);
        sync.run();
        step.reached("committed");
        return true;
    }

    @Override public void close() throws IOException {
        try {
            lock.release();
        } finally {
            try {
                channel.close();
            } finally {
                LOCAL.release();
            }
        }
    }
}
