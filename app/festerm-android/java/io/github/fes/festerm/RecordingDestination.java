package io.github.fes.festerm;

import android.content.ContentResolver;
import android.content.Context;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;

import java.io.IOException;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;

/** Storage Access Framework operations for one visible recording export. */
public final class RecordingDestination {
    private static final int MAX_CHILDREN = 10000;
    private final ContentResolver resolver;
    private final Uri tree, root;
    private final Map<String, Uri> documents = new HashMap<>();
    private final Set<String> openedForWrite = new HashSet<>();
    private Uri directory;

    public RecordingDestination(Context context, String authority) throws IOException {
        resolver = context.getContentResolver();
        tree = Uri.parse(authority);
        if (!"content".equals(tree.getScheme()) || tree.getAuthority() == null
            || !DocumentsContract.isTreeUri(tree)) {
            throw new IOException("Choose a Files folder for recording export");
        }
        requirePermission();
        root = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree));
        Metadata metadata = metadata(root);
        if (!DocumentsContract.Document.MIME_TYPE_DIR.equals(metadata.mime)
            || (metadata.flags & DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE) == 0) {
            throw new IOException("The selected Files folder cannot create documents");
        }
    }

    public synchronized String[] createDirectory(String name) throws IOException {
        if (directory != null) throw new IOException("An export folder was already created");
        requirePermission();
        Set<String> before = childIds(root);
        Uri created = DocumentsContract.createDocument(
            resolver, root, DocumentsContract.Document.MIME_TYPE_DIR, name);
        if (created == null) throw new IOException("The provider could not create an export folder");
        Metadata result = requireNewChild(root, created, before);
        if (!DocumentsContract.Document.MIME_TYPE_DIR.equals(result.mime)
            || (result.flags & DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE) == 0
            || !childIds(created).isEmpty()) {
            throw new IOException("The provider did not create an empty writable export folder");
        }
        directory = created;
        return new String[]{created.toString(), result.name};
    }

    public synchronized String[] createVideo(String folder, String name) throws IOException {
        return createFile(folder, name, "video/mp4");
    }

    public synchronized String[] createAppearance(String folder, String name) throws IOException {
        if (!"recording-appearance.json".equals(name)) {
            throw new IOException("The recording appearance name is invalid");
        }
        return createFile(folder, name, "application/json");
    }

    private String[] createFile(String folder, String name, String mime) throws IOException {
        if (directory == null || !directory.toString().equals(folder)) {
            throw new IOException("The recording export folder is unavailable");
        }
        requirePermission();
        Set<String> before = childIds(directory);
        Uri created = DocumentsContract.createDocument(resolver, directory, mime, name);
        if (created == null) throw new IOException("The provider could not create a recording document");
        Metadata result = requireNewChild(directory, created, before);
        if (!mime.equals(result.mime)) {
            throw new IOException("The provider created an unexpected recording document type");
        }
        String identity = created.toString();
        if (documents.putIfAbsent(identity, created) != null) {
            throw new IOException("The provider reused a recording document identity");
        }
        return new String[]{identity, result.name};
    }

    public synchronized int openForWrite(String identity) throws IOException {
        requirePermission();
        Uri document = documents.get(identity);
        if (document == null || !openedForWrite.add(identity)) {
            throw new IOException("The new recording document is unavailable for writing");
        }
        return open(document, "w");
    }

    public synchronized int openForRead(String identity) throws IOException {
        requirePermission();
        Uri document = documents.get(identity);
        if (document == null || !openedForWrite.contains(identity)) {
            throw new IOException("The new recording document is unavailable for verification");
        }
        return open(document, "r");
    }

    private int open(Uri document, String mode) throws IOException {
        ParcelFileDescriptor opened = resolver.openFileDescriptor(document, mode);
        if (opened == null) throw new IOException("The provider could not open the recording document");
        try (ParcelFileDescriptor descriptor = opened) {
            return descriptor.detachFd();
        }
    }

    private void requirePermission() throws IOException {
        for (UriPermission permission : resolver.getPersistedUriPermissions()) {
            if (tree.equals(permission.getUri()) && permission.isReadPermission()
                && permission.isWritePermission()) return;
        }
        throw new IOException("Recording folder access expired; choose the folder again");
    }

    private Metadata requireNewChild(Uri parent, Uri child, Set<String> before) throws IOException {
        if (!tree.getAuthority().equals(child.getAuthority())
            || !DocumentsContract.isChildDocument(resolver, parent, child)) {
            throw new IOException("The provider returned a document outside the chosen folder");
        }
        Metadata result = metadata(child);
        if (!result.id.equals(DocumentsContract.getDocumentId(child))
            || result.id.equals(DocumentsContract.getDocumentId(parent)) || before.contains(result.id)) {
            throw new IOException("The provider returned an existing document");
        }
        return result;
    }

    private Set<String> childIds(Uri parent) throws IOException {
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(
            tree, DocumentsContract.getDocumentId(parent));
        Set<String> ids = new HashSet<>();
        try (Cursor cursor = resolver.query(children,
                new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID}, null, null, null)) {
            if (cursor == null) throw new IOException("The provider cannot list the export folder");
            while (cursor.moveToNext()) {
                if (ids.size() >= MAX_CHILDREN) throw new IOException("The export folder is too large");
                String id = cursor.getString(0);
                if (id == null || !ids.add(id)) throw new IOException("The provider returned invalid children");
            }
        }
        return ids;
    }

    private Metadata metadata(Uri document) throws IOException {
        String[] columns = {
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_FLAGS,
        };
        try (Cursor cursor = resolver.query(document, columns, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IOException("The provider cannot inspect the export document");
            }
            String id = cursor.getString(0), name = cursor.getString(1), mime = cursor.getString(2);
            int flags = cursor.getInt(3);
            if (id == null || name == null || mime == null || cursor.moveToNext()) {
                throw new IOException("The provider returned invalid document metadata");
            }
            return new Metadata(id, name, mime, flags);
        }
    }

    private static final class Metadata {
        final String id, name, mime;
        final int flags;

        Metadata(String id, String name, String mime, int flags) {
            this.id = id;
            this.name = name;
            this.mime = mime;
            this.flags = flags;
        }
    }
}
