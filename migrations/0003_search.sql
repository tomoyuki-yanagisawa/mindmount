CREATE VIRTUAL TABLE document_search USING fts5(title, body, tokenize = 'trigram');

CREATE TRIGGER document_search_delete AFTER DELETE ON documents BEGIN
    DELETE FROM document_search WHERE rowid = old.id;
END;

CREATE TRIGGER document_search_update_id AFTER UPDATE OF id ON documents
WHEN old.id != new.id BEGIN
    DELETE FROM document_search WHERE rowid = old.id;
END;

-- Existing indexes need a rescan to populate searchable content.
UPDATE documents SET content_hash = '';
