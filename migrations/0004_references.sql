-- Existing pages need a rescan to populate link references.
UPDATE documents SET content_hash = '';
