ALTER TABLE mounts ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1;
ALTER TABLE documents ADD COLUMN missing_at INTEGER;

CREATE TABLE document_metadata (
    document_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    PRIMARY KEY (document_id, key),
    FOREIGN KEY (document_id) REFERENCES documents (id)
        ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE markdown_links (
    id INTEGER PRIMARY KEY NOT NULL,
    source_document_id INTEGER NOT NULL,
    target_document_id INTEGER,
    target_uri TEXT,
    target_anchor TEXT,
    raw_reference TEXT NOT NULL,
    source_heading TEXT,
    source_line INTEGER,
    resolution_status TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    FOREIGN KEY (source_document_id) REFERENCES documents (id)
        ON DELETE CASCADE ON UPDATE CASCADE,
    FOREIGN KEY (target_document_id) REFERENCES documents (id)
        ON DELETE SET NULL ON UPDATE CASCADE
);

CREATE INDEX markdown_links_source_document_id_index
    ON markdown_links (source_document_id);
CREATE INDEX markdown_links_target_document_id_index
    ON markdown_links (target_document_id);

INSERT INTO markdown_links
    (id, source_document_id, target_document_id, target_uri, target_anchor,
     raw_reference, source_heading, source_line, resolution_status, source_kind)
SELECT id, source_document_id, target_document_id, target_uri, target_anchor,
       raw_reference, NULL, NULL,
       CASE WHEN target_document_id IS NULL THEN 'unresolved' ELSE 'resolved' END,
       source_kind
FROM "references";

DROP TABLE "references";

CREATE TABLE knowledge_links (
    id INTEGER PRIMARY KEY NOT NULL,
    source_document_id INTEGER NOT NULL,
    target_document_id INTEGER,
    target_uri TEXT,
    relation_type TEXT NOT NULL,
    description TEXT,
    provenance TEXT,
    created_by TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'proposed',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (source_document_id) REFERENCES documents (id)
        ON DELETE CASCADE ON UPDATE CASCADE,
    FOREIGN KEY (target_document_id) REFERENCES documents (id)
        ON DELETE SET NULL ON UPDATE CASCADE
);

CREATE INDEX knowledge_links_source_document_id_index
    ON knowledge_links (source_document_id);
CREATE INDEX knowledge_links_target_document_id_index
    ON knowledge_links (target_document_id);
