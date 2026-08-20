# Knowledge Base

The Echo Agent knowledge base system provides long-term document retrieval capabilities for conversations through vector indexing and semantic search. Once documents are uploaded, the system automatically extracts, chunks, embeds, and indexes content for on-demand querying by the Agent.

Documents are chunked at 1200 characters with a 120-character overlap between adjacent chunks (`knowledge.chunkSize` and `knowledge.chunkOverlap`). Both are measured in characters, not tokens.

## Architecture Overview

The knowledge base system consists of the following components:

| Component | Responsibility |
|-----------|---------------|
| document tool | Document upload and management (risk_level: read_only) |
| knowledge tool | Vector retrieval queries (risk_level: read_only) |
| Document extractors | Convert various file formats to plain text |
| Chunking engine | Split long text into semantic segments |
| Embedding model | Convert text segments into vector representations |
| FAISS index | Vector similarity search engine |

```
Upload document → Extract text → Chunk → Embed → FAISS index → Query retrieval
```

## Supported File Formats

| Format | Extension | Extractor | Notes |
|--------|-----------|-----------|-------|
| PDF | `.pdf` | PDF extractor | Supports text-based PDFs; scanned documents require OCR preprocessing |
| Word | `.docx` | DOCX extractor | Only `.docx` supported, not legacy `.doc` |
| Excel | `.xlsx` | XLSX extractor | Extracts by sheet, tables converted to text rows |
| PowerPoint | `.pptx` | PPTX extractor | Extracts slide text content and speaker notes |

!!! warning "Format limitations"
    - Legacy Office formats (`.doc`, `.xls`, `.ppt`) are not supported
    - Scanned PDFs must be processed with OCR before uploading
    - Encrypted or password-protected files cannot be extracted
    - Maximum file size depends on deployment configuration

## Document Upload Workflow

Use the `document` tool to upload files. The system automatically executes the full processing pipeline:

### 1. Upload

Submit files to the knowledge base using the document tool:

```
document.upload(file="product-manual.pdf", collection="default")
```

### 2. Text Extraction

The system selects the appropriate extractor based on file extension and converts file content to plain text.

### 3. Chunking

Long text is split into fixed-size segments with overlapping regions between adjacent chunks to maintain contextual coherence.

Default parameters:

| Option | Default | Unit |
|--------|---------|------|
| `knowledge.chunkSize` | 1200 | characters |
| `knowledge.chunkOverlap` | 120 | characters |

### 4. Embedding

Each text segment is converted into a high-dimensional vector representation through the embedding model, capturing semantic information.

Embedding configuration lives under the `memory` section and is shared with the memory system:

| Option | Default | Description |
|--------|---------|-------------|
| `memory.embeddingBackend` | `auto` | `auto` probes the provider at startup and silently falls back to the local model on failure; `local` uses the local model directly; `provider` forces the provider and errors out if the probe fails |
| `memory.embeddingModel` | empty | Provider-side embedding model; left empty, the provider decides |
| `memory.localEmbeddingModel` | `BAAI/bge-small-zh-v1.5` | Local fastembed fallback model; an empty string disables the fallback |

### 5. Vector indexing

Chunk vectors are indexed with FAISS for semantic similarity retrieval.

## Vector index

Retrieval rests on two files: a JSON index holding chunk text and metadata (including a file manifest, which is what makes deletions and renames detectable), and an `.npz` sidecar next to it holding the vectors, physically isolated from the memory system's vector table.

Implementation characteristics:

- **Exact search**: `IndexFlatIP` compares against every vector rather than approximating; vectors are L2-normalised, so the inner product is the cosine similarity
- **Persistence**: the sidecar is written alongside the index and loaded on restart
- **Change detection**: the sidecar records a content hash per chunk, so a vector is recognised as stale even when the chunk id is unchanged and only its text moved
- **Degradation**: without `faiss` or `numpy` installed, vector search returns no results and retrieval falls back to the keyword path rather than failing

!!! note "The index is derived data"
    Both the JSON index and the sidecar can be regenerated from the source documents. A corrupted index should be rebuilt, not repaired.

## Querying the Knowledge Base

Use the `knowledge` tool for semantic retrieval:

```
knowledge.query(query="How to configure database connection", top_k=5)
```

Parameter reference:

| Parameter | Description | Default |
|-----------|-------------|---------|
| `query` | Natural language query text | Required |
| `top_k` | Number of most relevant segments to return | 5 |
| `collection` | Target knowledge base collection | `"default"` |
| `threshold` | Similarity threshold; results below this are filtered | 0.7 |

Results include matched text segments, source filenames, and similarity scores.

## Document Management

Manage uploaded documents through the `document` tool:

```
# List uploaded documents
document.list(collection="default")

# Delete a specific document (also removes corresponding index entries)
document.delete(doc_id="xxx")

# Upload and replace an existing document
document.upload(file="product-manual-v2.pdf", collection="default", replace=true)
```

## Configuration

Knowledge base configuration resides in the system config:

```yaml
knowledge:
  enabled: true
  docs_dir: data/knowledge
  index_path: data/knowledge_index.json
  chunk_size: 1200
  chunk_overlap: 120
  max_results: 5
  auto_index: true
  allowed_extensions:
    - .md
    - .txt
    - .pdf
```

Embedding and reranking models are configured in the `memory` section (`embedding_backend`, `rerank_enabled` and related fields), not under `knowledge`. The index is a local JSON file at `index_path`, so there is no FAISS index path to set.

## Dashboard Knowledge Page

Select **Knowledge** from the Dashboard left navigation to:

- View all uploaded documents and their status (processing, indexed, failed)
- Manually upload new documents
- Delete or replace existing documents
- View chunk counts and indexing status per document
- Test queries and preview retrieval results

<!-- Screenshot placeholder: Knowledge page overview showing document list and upload area -->

## Usage Examples

### Uploading Product Documents

```
# Upload a PDF product manual
document.upload(file="echo-agent-manual.pdf", collection="product")

# Upload a Word FAQ document
document.upload(file="faq.docx", collection="faq")
```

### Querying the Knowledge Base

```
# Query product features
knowledge.query(query="What channels does Echo Agent support", collection="product", top_k=3)

# Query FAQ
knowledge.query(query="How to reset password", collection="faq")
```

### Batch Upload

```
# Organize multiple documents by collection
document.upload(file="api-reference.pdf", collection="technical")
document.upload(file="deployment-guide.docx", collection="technical")
document.upload(file="release-notes.xlsx", collection="changelog")
```

## Best Practices

### Document Preparation

- **Clear structure**: Use headings, paragraphs, and lists to improve chunking quality
- **Avoid image-only content**: Ensure key information exists as text
- **Appropriate granularity**: Avoid overly large files; split by topic when possible
- **Naming conventions**: Use meaningful filenames for easy identification in document lists

### Collection Management

- Organize collections by domain (e.g., `product`, `technical`, `faq`)
- Regularly clean up outdated documents to maintain index quality
- Use replacement rather than addition when updating versions to avoid duplicate content

### Query Optimization

- Use natural language to describe questions; avoid overly short keywords
- Set `top_k` appropriately; too high introduces noise
- Use the `collection` parameter to narrow search scope
- Set an appropriate `threshold` to filter low-relevance results
