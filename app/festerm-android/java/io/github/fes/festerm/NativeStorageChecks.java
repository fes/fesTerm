package io.github.fes.festerm;

import java.io.IOException;

/** Shared metadata predicates; only the Activity can turn OS handles into capabilities. */
final class NativeStorageChecks {
    static void privateEntry(int process, int application, int owner, int mode,
            long links, boolean correctType, boolean directory) throws IOException {
        if (application != process || owner != process || !correctType
                || (mode & 0777) != (directory ? 0700 : 0600)
                || (directory ? links == 0 : links != 1)) {
            throw new IOException("Native private storage ownership unavailable");
        }
    }

    static void nativeChild(int process, int owner, int group, int mode, long links,
            boolean directory, long device, long inode, long expectedDevice, long expectedInode)
            throws IOException {
        if (!directory || links == 0 || owner != process || group != process
                || (mode & 0002) != 0 || device != expectedDevice || inode != expectedInode) {
            throw new IOException("Native storage scope unavailable");
        }
    }
}
