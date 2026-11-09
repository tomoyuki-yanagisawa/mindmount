CREATE TABLE mounts (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_path TEXT NOT NULL,
    directory_path TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX mounts_directory_path_unique ON mounts (directory_path);

CREATE TABLE documents (
    id INTEGER PRIMARY KEY NOT NULL,
    mount_id INTEGER NOT NULL,
    relative_path TEXT NOT NULL,
    title TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    modified_at INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL,
    UNIQUE (mount_id, relative_path),
    FOREIGN KEY (mount_id) REFERENCES mounts (id) ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE INDEX documents_mount_id_index ON documents (mount_id);

CREATE TABLE "references" (
    id INTEGER PRIMARY KEY NOT NULL,
    source_document_id INTEGER NOT NULL,
    target_document_id INTEGER,
    target_uri TEXT,
    target_anchor TEXT,
    raw_reference TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    FOREIGN KEY (source_document_id) REFERENCES documents (id) ON DELETE CASCADE ON UPDATE CASCADE,
    FOREIGN KEY (target_document_id) REFERENCES documents (id) ON DELETE SET NULL ON UPDATE CASCADE
);

CREATE INDEX references_source_document_id_index ON "references" (source_document_id);
CREATE INDEX references_target_document_id_index ON "references" (target_document_id);
