-- Re-resolve existing references after adding percent-decoding.
UPDATE documents SET content_hash = '';
