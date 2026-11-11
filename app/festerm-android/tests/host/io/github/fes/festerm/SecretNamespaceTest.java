package io.github.fes.festerm;

import java.io.IOException;
import java.nio.channels.FileChannel;
import java.nio.file.*;
import java.nio.file.attribute.*;
import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/** Host filesystem/predicate tests, not Android Keystore/JNI/device evidence. */
public final class SecretNamespaceTest {
    private static final String REF = "263c626d-9921-49f5-8ac7-64b700492db2";
    private static final byte[] ENVELOPE = new byte[]{1, 5, 9, 7};
    private static final LinkOption NOFOLLOW = LinkOption.NOFOLLOW_LINKS;
    private static int cases;

    interface Checked {
        void run() throws Exception;
    }

    private static void require(boolean value) {
        if (!value) throw new AssertionError("Native namespace invariant");
    }

    private static void refused(Checked action) throws Exception {
        try {
            action.run();
        } catch (IOException expected) {
            return;
        }
        throw new AssertionError("Expected typed I/O refusal");
    }

    private static void run(String name, Checked action) throws Exception {
        action.run();
        cases++;
        System.out.println("PASS " + name);
    }

    static final class Fixture implements AutoCloseable {
        final Path directory;
        final int owner;
        Fixture(Path parent) throws IOException {
            directory = Files.createDirectory(parent.resolve(UUID.randomUUID().toString()),
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")));
            owner = ((Number) Files.getAttribute(directory, "unix:uid", NOFOLLOW)).intValue();
        }
        void guard(Path path) throws IOException {
            Map<String, Object> metadata = Files.readAttributes(path, "unix:uid,mode,nlink", NOFOLLOW);
            NativeStorageChecks.privateEntry(owner, owner, ((Number) metadata.get("uid")).intValue(),
                ((Number) metadata.get("mode")).intValue(), ((Number) metadata.get("nlink")).longValue(),
                Files.isRegularFile(path, NOFOLLOW), false);
        }
        void sync() throws IOException {
            try (FileChannel directoryChannel = FileChannel.open(directory, StandardOpenOption.READ)) {
                directoryChannel.force(true);
            }
        }
        SecretNamespace open() throws IOException {
            return SecretNamespace.acquire(directory, this::guard, this::sync);
        }
        Path put(String suffix, byte[] bytes) throws IOException {
            Path file = directory.resolve(REF + suffix);
            Files.createFile(file,
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")));
            Files.write(file, bytes);
            return file;
        }
        @Override public void close() throws IOException {
            try (java.util.stream.Stream<Path> paths = Files.walk(directory)) {
                for (Path path : (Iterable<Path>) paths.sorted(Comparator.reverseOrder())::iterator) {
                    Files.delete(path);
                }
            }
        }
    }

    private static Process contender(Path directory) throws IOException {
        return new ProcessBuilder(
            Paths.get(System.getProperty("java.home"), "bin", "java").toString(),
            "-Xmx64m", "-XX:-UsePerfData", "-Djava.io.tmpdir=" + System.getProperty("java.io.tmpdir"),
            "-cp", System.getProperty("java.class.path"), SecretNamespaceTest.class.getName(),
            "--contender", directory.toString()).inheritIO().start();
    }

    private static int finished(Process process) throws Exception {
        if (!process.waitFor(15, TimeUnit.SECONDS)) {
            process.destroyForcibly();
            throw new AssertionError("Bounded child did not finish");
        }
        return process.exitValue();
    }

    public static void main(String[] args) throws Exception {
        if (args[0].equals("--contender")) {
            Path directory = Paths.get(args[1]);
            int owner = ((Number) Files.getAttribute(directory, "unix:uid", NOFOLLOW)).intValue();
            try (SecretNamespace namespace = SecretNamespace.acquire(directory, path -> {
                    Map<String, Object> stat = Files.readAttributes(path, "unix:uid,mode,nlink", NOFOLLOW);
                    NativeStorageChecks.privateEntry(owner, owner, ((Number) stat.get("uid")).intValue(),
                        ((Number) stat.get("mode")).intValue(), ((Number) stat.get("nlink")).longValue(),
                        Files.isRegularFile(path, NOFOLLOW), false);
                }, () -> {
                    try (FileChannel parent = FileChannel.open(directory, StandardOpenOption.READ)) {
                        parent.force(true);
                    }
                })) {
                System.exit(namespace.create(REF, ENVELOPE) ? 0 : 3);
            } catch (IOException refused) {
                System.exit(2);
            }
            return;
        }
        Path parent = Paths.get(args[0]);
        run("native-current-owner", () ->
            NativeStorageChecks.privateEntry(12345, 12345, 12345, 0700, 2, true, true));
        run("native-foreign-owner", () -> refused(() ->
            NativeStorageChecks.privateEntry(12345, 12345, 12346, 0700, 2, true, true)));
        run("native-foreign-application", () -> refused(() ->
            NativeStorageChecks.privateEntry(12345, 12346, 12345, 0700, 2, true, true)));
        run("native-nonprivate-root", () -> refused(() ->
            NativeStorageChecks.privateEntry(12345, 12345, 12345, 0771, 2, true, true)));
        run("native-context-child", () ->
            NativeStorageChecks.nativeChild(12345, 12345, 12345, 0771, 2, true, 1, 2, 1, 2));
        run("native-substituted-child", () -> refused(() ->
            NativeStorageChecks.nativeChild(12345, 12345, 12345, 0771, 2, true, 1, 3, 1, 2)));
        run("native-foreign-group", () -> refused(() ->
            NativeStorageChecks.nativeChild(12345, 12345, 12346, 0771, 2, true, 1, 2, 1, 2)));
        run("native-world-writable-child", () -> refused(() ->
            NativeStorageChecks.nativeChild(12345, 12345, 12345, 0773, 2, true, 1, 2, 1, 2)));
        run("create-reopen-no-overwrite", () -> {
            try (Fixture fixture = new Fixture(parent)) {
                try (SecretNamespace namespace = fixture.open()) {
                    require(!namespace.existing(REF));
                    require(namespace.create(REF, ENVELOPE));
                    require(!Files.exists(fixture.directory.resolve(REF + ".reserved"), NOFOLLOW));
                    require(((Number) Files.getAttribute(namespace.path(REF), "unix:nlink")).longValue() == 1);
                    require(!namespace.create(REF, new byte[]{9}));
                }
                try (SecretNamespace reopened = fixture.open()) {
                    require(reopened.existing(REF));
                    require(!reopened.create(REF, new byte[]{9}));
                    require(Arrays.equals(Files.readAllBytes(reopened.path(REF)), ENVELOPE));
                }
            }
        });
        for (String suffix : new String[]{".bak", ".new", ".reserved"}) {
            run("preserve-collision-" + suffix, () -> {
                try (Fixture fixture = new Fixture(parent)) {
                    Path retained = fixture.put(suffix, ENVELOPE);
                    try (SecretNamespace namespace = fixture.open()) {
                        refused(() -> namespace.create(REF, new byte[]{9}));
                        refused(() -> namespace.existing(REF));
                    }
                    require(Arrays.equals(Files.readAllBytes(retained), ENVELOPE));
                    require(!Files.exists(fixture.directory.resolve(REF), NOFOLLOW));
                }
            });
        }
        for (String point : new String[]{"reservation-created", "reserved", "base-created",
                "writing", "before-base-force", "written", "published", "before-marker-delete",
                "marker-deleted", "committed"}) {
            run("retain-write-ambiguity-" + point, () -> {
                try (Fixture fixture = new Fixture(parent); SecretNamespace namespace = fixture.open()) {
                    byte[] envelope = new byte[8193];
                    Arrays.fill(envelope, (byte) 37);
                    refused(() -> namespace.create(REF, envelope, step -> {
                        if (step.equals(point)) throw new IOException("injected write ambiguity");
                    }));
                    if (point.equals("committed") || point.equals("marker-deleted")) {
                        require(namespace.existing(REF));
                        require(!namespace.create(REF, new byte[]{9}));
                        require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), envelope));
                        require(!Files.exists(fixture.directory.resolve(REF + ".reserved"), NOFOLLOW));
                    } else {
                        Path retained = fixture.directory.resolve(REF + ".reserved");
                        byte[] before = Files.readAllBytes(retained);
                        require(before.length == 0);
                        refused(() -> namespace.existing(REF));
                        refused(() -> namespace.create(REF, new byte[]{9}));
                        require(Arrays.equals(Files.readAllBytes(retained), before));
                        if (point.equals("reservation-created") || point.equals("reserved")) {
                            require(!Files.exists(namespace.path(REF), NOFOLLOW));
                        } else {
                            byte[] base = Files.readAllBytes(namespace.path(REF));
                            if (point.equals("base-created")) require(base.length == 0);
                            else if (point.equals("writing")) {
                                require(base.length == 4096);
                                require(Arrays.equals(base, Arrays.copyOf(envelope, 4096)));
                            } else require(Arrays.equals(base, envelope));
                        }
                    }
                }
            });
        }
        run("publication-race-never-replaces", () -> {
            try (Fixture fixture = new Fixture(parent); SecretNamespace namespace = fixture.open()) {
                refused(() -> namespace.create(REF, ENVELOPE, step -> {
                    if (step.equals("reserved")) fixture.put("", new byte[]{42});
                }));
                require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), new byte[]{42}));
                require(Files.size(fixture.directory.resolve(REF + ".reserved")) == 0);
                refused(() -> namespace.existing(REF));
            }
        });
        for (int failingSync : new int[]{2, 3, 4}) {
            run("directory-sync-failure-" + failingSync, () -> {
                try (Fixture fixture = new Fixture(parent)) {
                    AtomicInteger syncs = new AtomicInteger();
                    try (SecretNamespace namespace = SecretNamespace.acquire(fixture.directory, fixture::guard, () -> {
                            if (syncs.incrementAndGet() == failingSync) throw new IOException("injected sync failure");
                            fixture.sync();
                        })) {
                        refused(() -> namespace.create(REF, ENVELOPE));
                        if (failingSync == 4) {
                            require(namespace.existing(REF));
                            require(!namespace.create(REF, new byte[]{9}));
                            require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), ENVELOPE));
                        } else {
                            refused(() -> namespace.existing(REF));
                            refused(() -> namespace.create(REF, ENVELOPE));
                            require(Files.exists(fixture.directory.resolve(REF + ".reserved"), NOFOLLOW));
                        }
                        if (failingSync == 2) require(!Files.exists(namespace.path(REF), NOFOLLOW));
                        else require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), ENVELOPE));
                    }
                }
            });
        }
        for (String guarded : new String[]{".reserved", ""}) {
            run("publication-guard-failure-" + (guarded.isEmpty() ? "base" : "marker"), () -> {
                try (Fixture fixture = new Fixture(parent);
                        SecretNamespace namespace = SecretNamespace.acquire(fixture.directory, path -> {
                            fixture.guard(path);
                            if (path.getFileName().toString().equals(REF + guarded)) {
                                throw new IOException("injected owner check failure");
                            }
                        }, fixture::sync)) {
                    refused(() -> namespace.create(REF, ENVELOPE));
                    require(Files.exists(fixture.directory.resolve(REF + ".reserved"), NOFOLLOW));
                    refused(() -> namespace.existing(REF));
                    if (guarded.isEmpty()) require(Files.size(namespace.path(REF)) == 0);
                    else require(!Files.exists(namespace.path(REF), NOFOLLOW));
                }
            });
        }
        run("marker-delete-failure-keeps-base-uncertain", () -> {
            try (Fixture fixture = new Fixture(parent); SecretNamespace namespace = fixture.open()) {
                Path marker = fixture.directory.resolve(REF + ".reserved");
                Path retained = fixture.directory.resolve("owned-marker");
                refused(() -> namespace.create(REF, ENVELOPE, step -> {
                    if (step.equals("before-marker-delete")) {
                        Files.move(marker, retained);
                        Files.createDirectory(marker);
                        Files.createFile(marker.resolve("owned-blocker"));
                    }
                }));
                require(Files.size(retained) == 0);
                require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), ENVELOPE));
                refused(() -> namespace.existing(REF));
                refused(() -> namespace.create(REF, new byte[]{9}));
                require(Files.exists(marker.resolve("owned-blocker"), NOFOLLOW));
            }
        });
        run("marker-delete-missing-is-explicit-failure-not-ack", () -> {
            try (Fixture fixture = new Fixture(parent); SecretNamespace namespace = fixture.open()) {
                refused(() -> namespace.create(REF, ENVELOPE, step -> {
                    if (step.equals("before-marker-delete")) {
                        Files.delete(fixture.directory.resolve(REF + ".reserved"));
                    }
                }));
                require(namespace.existing(REF));
                require(!namespace.create(REF, new byte[]{9}));
                require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), ENVELOPE));
            }
        });
        run("foreign-scope-symlink", () -> {
            try (Fixture fixture = new Fixture(parent); Fixture foreign = new Fixture(parent)) {
                Path retained = foreign.put("", ENVELOPE);
                Files.createSymbolicLink(fixture.directory.resolve(REF), retained);
                try (SecretNamespace namespace = fixture.open()) {
                    refused(() -> namespace.create(REF, new byte[]{9}));
                    refused(() -> namespace.existing(REF));
                }
                require(Arrays.equals(Files.readAllBytes(retained), ENVELOPE));
            }
        });
        run("foreign-scope-hardlink", () -> {
            try (Fixture fixture = new Fixture(parent); Fixture foreign = new Fixture(parent)) {
                Path retained = foreign.put("", ENVELOPE);
                Files.createLink(fixture.directory.resolve(REF), retained);
                try (SecretNamespace namespace = fixture.open()) {
                    refused(() -> namespace.create(REF, new byte[]{9}));
                }
                require(Arrays.equals(Files.readAllBytes(retained), ENVELOPE));
            }
        });
        run("lock-file-substitution", () -> {
            try (Fixture fixture = new Fixture(parent); Fixture foreign = new Fixture(parent)) {
                Path retained = foreign.put("", ENVELOPE);
                Files.createSymbolicLink(fixture.directory.resolve(SecretNamespace.LOCK), retained);
                refused(() -> fixture.open().close());
                require(Arrays.equals(Files.readAllBytes(retained), ENVELOPE));
            }
        });
        run("same-process-lock-busy-before-opening", () -> {
            try (Fixture fixture = new Fixture(parent); SecretNamespace namespace = fixture.open()) {
                AtomicInteger guards = new AtomicInteger();
                refused(() -> SecretNamespace.acquire(fixture.directory, path -> guards.incrementAndGet(), () -> {}).close());
                require(guards.get() == 0);
                require(finished(contender(fixture.directory)) == 2);
                require(namespace.create(REF, ENVELOPE));
            }
        });
        run("independent-process-creators", () -> {
            try (Fixture fixture = new Fixture(parent)) {
                Process first = contender(fixture.directory);
                Process second = contender(fixture.directory);
                int left = finished(first);
                int right = finished(second);
                require((left == 0 && (right == 2 || right == 3))
                    || (right == 0 && (left == 2 || left == 3)));
                try (SecretNamespace namespace = fixture.open()) {
                    require(namespace.existing(REF));
                    require(Arrays.equals(Files.readAllBytes(namespace.path(REF)), ENVELOPE));
                }
            }
        });
        System.out.println("Host native namespace cases=" + cases + " failures=0");
    }
}
