-- Re-extract titles once after switching Markdown parsers.
UPDATE documents SET content_hash = '';
