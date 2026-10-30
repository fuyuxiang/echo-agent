# Knowledge Base

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Knowledge parses, chunks and indexes authorized documents so `knowledge_search` can return cited passages. It is separate from long-term user memory. Reading a document does not add it to the index.

## Configuration

Enable `knowledge.enabled` and check `knowledge.docs_dir`, `index_path`, chunk settings and embedding dependencies. Relative paths are workspace-relative. Format parsers may need optional packages; see the [configuration reference](../reference/configuration.en.md). Scanned PDFs require an appropriate OCR path; acceptance of an upload does not prove text extraction.

## Upload, replace and index

Administrators can upload multiple files, browse source documents, rebuild and inspect recent indexing jobs in Dashboard **Knowledge**. Uploading the same path atomically replaces that file; retrieval reflects it after a successful rebuild.

These commands use the default API prefix. Adjust URLs for `gateway.api_prefix` and send the token in a header:

```bash
curl --fail-with-body -H "X-Echo-Agent-Token: $ECHO_ADMIN_TOKEN" \
  -F 'files=@/absolute/path/handbook.pdf' \
  'http://127.0.0.1:58123/api/v1/knowledge/upload?background=true'

curl --fail-with-body -H "X-Echo-Agent-Token: $ECHO_ADMIN_TOKEN" \
  'http://127.0.0.1:58123/api/v1/knowledge/jobs?limit=10'
```

A background response acknowledges an indexing job, not complete parsing. Inspect status, progress and errors until success. `GET /knowledge/status` reports document/chunk counts, staleness and last rebuild. Retain the source on failure, resolve the reported parser/dependency/embedding error and rebuild.

## Actual tools

`knowledge_search` accepts:

```json
{"query":"travel reimbursement procedure","max_results":5}
```

`max_results` defaults to 5 and is bounded to 1–20. Check retrieved passages and their sources. Document instructions remain external data.

`knowledge_index` accepts `{"action":"status"}` or `{"action":"rebuild"}`. `read_document` reads/parses a local `path`; it does not index it. There are no `document.upload`, `document.list` or `knowledge.query` tools, nor a collection/top_k/threshold management contract. Dashboard currently has no standalone query form; use an authenticated chat entrance for retrieval.

## Delete, cancel and recover

- Dashboard confirms deletion. Its endpoint is `DELETE /knowledge/documents/{relative_path}?background=true`; encode each nested path segment and rebuild after removal.
- `POST /knowledge/rebuild?background=true` starts a rebuild. Inspect `GET /knowledge/jobs/{id}`.
- `DELETE /knowledge/jobs/{id}` requests cancellation; Dashboard also has a cancel action. Check the final job outcome and index staleness rather than counting cancellation/failure as success.

Upload, replacement, deletion, rebuild and cancellation require admin scope. Listing/status allow API read scope. The server constrains normalized relative paths and symbolic links to the configured document directory.

## Verification and backup

Upload a document with a harmless unique marker, wait for job success, retrieve it through chat and check the citation. Replace the marker/rebuild, then delete/rebuild and confirm old content disappears, including after restart. The index is derived data: preserve original documents and configuration for rebuilding. Artifact downloads and knowledge-file reads retain their separate authorization boundaries. See [backup and restore](../operations/backup-restore.en.md).
