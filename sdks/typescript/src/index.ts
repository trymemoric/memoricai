/**
 * TypeScript SDK for the memoricai /v1 HTTP API. Zero dependencies (global fetch).
 *
 * ```ts
 * import { MemoricaiClient } from "memoricai";
 *
 * const client = new MemoricaiClient("http://localhost:6767", "mc_...");
 * const doc = await client.addText("My name is Ada.", "mc_project_default");
 * await client.waitForDocument(doc.id);
 * const res = await client.searchMemories({
 *   q: "what is my name",
 *   containerTag: "mc_project_default",
 *   digest: true,
 * });
 * console.log(res.digest);
 * ```
 */

export class MemoricaiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(`api error ${status}: ${message}`);
    this.name = "MemoricaiError";
  }
}

export interface AddDocumentRequest {
  content: string;
  containerTag?: string;
  containerTags?: string[];
  customId?: string;
  metadata?: Record<string, unknown>;
  entityContext?: string;
  contentType?: string;
  title?: string;
}

export interface IngestResponse {
  id: string;
  status: string;
}

export interface MemoricaiDocument {
  id: string;
  status: string;
  content?: string;
  title?: string;
  type?: string;
  metadata?: Record<string, unknown>;
  containerTags?: string[];
  createdAt?: string;
  updatedAt?: string;
  [key: string]: unknown;
}

export interface DocumentListResponse {
  memories: MemoricaiDocument[];
  pagination: {
    currentPage: number;
    limit: number;
    totalItems: number;
    totalPages: number;
  };
}

export interface DocumentSearchRequest {
  q: string;
  containerTags?: string[];
  limit?: number;
  chunkThreshold?: number;
  documentThreshold?: number;
  docId?: string;
  includeFullDocs?: boolean;
  includeSummary?: boolean;
  rerank?: boolean;
  rewriteQuery?: boolean;
  filters?: unknown;
}

export interface ChunkHit {
  content: string;
  score: number;
  isRelevant: boolean;
}

export interface DocumentSearchResult {
  documentId: string;
  title?: string;
  type: string;
  score: number;
  chunks: ChunkHit[];
  metadata: Record<string, unknown>;
  content?: string;
  summary?: string;
  createdAt: string;
  updatedAt: string;
}

export interface DocumentSearchResponse {
  results: DocumentSearchResult[];
  timing: number;
  total: number;
}

export interface MemorySearchRequest {
  q: string;
  containerTag?: string;
  searchMode?: "memories" | "hybrid" | "documents";
  limit?: number;
  threshold?: number;
  rerank?: boolean;
  rewriteQuery?: boolean;
  filters?: unknown;
  include?: {
    documents?: boolean;
    relatedMemories?: boolean;
    forgottenMemories?: boolean;
  };
  /** Compose a compact, date-stamped context digest alongside the results. */
  digest?: boolean;
}

export interface MemorySearchResult {
  id: string;
  memory?: string;
  chunk?: string;
  similarity: number;
  metadata: Record<string, unknown>;
  updatedAt: string;
  version: number;
  rootMemoryId?: string;
  [key: string]: unknown;
}

export interface MemorySearchResponse {
  results: MemorySearchResult[];
  timing: number;
  total: number;
  digest?: string;
}

export interface Profile {
  static?: string[];
  dynamic?: string[];
  buckets?: Record<string, string[]>;
}

export interface ProfileResponse {
  profile: Profile;
  searchResults?: MemorySearchResponse;
}

export interface MemoryInput {
  content: string;
  isStatic?: boolean;
  metadata?: Record<string, unknown>;
}

export interface MemoricaiMemory {
  id: string;
  memory: string;
  version: number;
  isLatest: boolean;
  [key: string]: unknown;
}

export interface ClientOptions {
  /** Request timeout in milliseconds (default 120000). */
  timeoutMs?: number;
  /** Max retries for 429/5xx responses (default 4). */
  maxRetries?: number;
}

export class MemoricaiClient {
  private readonly baseUrl: string;
  private readonly apiKey: string;
  private readonly timeoutMs: number;
  private readonly maxRetries: number;

  constructor(baseUrl: string, apiKey: string, options: ClientOptions = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.apiKey = apiKey;
    this.timeoutMs = options.timeoutMs ?? 120_000;
    this.maxRetries = options.maxRetries ?? 4;
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
  ): Promise<T> {
    for (let attempt = 0; ; attempt++) {
      const resp = await fetch(this.baseUrl + path, {
        method,
        headers: {
          Authorization: `Bearer ${this.apiKey}`,
          ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
        },
        body: body !== undefined ? JSON.stringify(body) : undefined,
        signal: AbortSignal.timeout(this.timeoutMs),
      });
      if (resp.ok) {
        const text = await resp.text();
        return (text ? JSON.parse(text) : undefined) as T;
      }
      const status = resp.status;
      const text = await resp.text();
      if ([429, 500, 502, 503].includes(status) && attempt < this.maxRetries) {
        await new Promise((r) => setTimeout(r, 2 ** attempt * 1000));
        continue;
      }
      let message = text;
      try {
        const parsed = JSON.parse(text) as { message?: string; error?: string };
        message = parsed.message ?? parsed.error ?? text;
      } catch {
        /* raw body */
      }
      throw new MemoricaiError(status, message);
    }
  }

  async health(): Promise<{ status: string; version: string }> {
    return this.request("GET", "/health");
  }

  // ---------------- documents ----------------

  /** POST /v1/documents — returns instantly with status "queued". */
  async addDocument(req: AddDocumentRequest): Promise<IngestResponse> {
    return this.request("POST", "/v1/documents", req);
  }

  /** Convenience: ingest plain text into a container tag. */
  async addText(content: string, containerTag: string): Promise<IngestResponse> {
    return this.addDocument({ content, containerTag });
  }

  async getDocument(id: string): Promise<MemoricaiDocument> {
    return this.request("GET", `/v1/documents/${encodeURIComponent(id)}`);
  }

  async deleteDocument(id: string): Promise<unknown> {
    return this.request("DELETE", `/v1/documents/${encodeURIComponent(id)}`);
  }

  async listDocuments(req: {
    containerTags?: string[];
    page?: number;
    limit?: number;
    status?: string;
  }): Promise<DocumentListResponse> {
    return this.request("POST", "/v1/documents/list", req);
  }

  /** POST /v1/documents/search — chunk-level RAG over documents. */
  async searchDocuments(
    req: DocumentSearchRequest,
  ): Promise<DocumentSearchResponse> {
    return this.request("POST", "/v1/documents/search", req);
  }

  /** Poll until the document reaches "done" (throws on failed/timeout). */
  async waitForDocument(
    id: string,
    timeoutMs = 120_000,
  ): Promise<MemoricaiDocument> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const doc = await this.getDocument(id);
      if (doc.status === "done") return doc;
      if (doc.status === "failed") {
        throw new MemoricaiError(500, `document ${id} failed processing`);
      }
      if (Date.now() >= deadline) {
        throw new MemoricaiError(408, `timed out waiting for document ${id}`);
      }
      await new Promise((r) => setTimeout(r, 400));
    }
  }

  // ---------------- search / profile ----------------

  /** POST /v1/search — memory-graph search; digest: true adds ready-to-inject context. */
  async searchMemories(req: MemorySearchRequest): Promise<MemorySearchResponse> {
    return this.request("POST", "/v1/search", req);
  }

  /** POST /v1/profile — static/dynamic/bucketed user profile. */
  async profile(req: {
    containerTag: string;
    q?: string;
    threshold?: number;
    include?: string[];
    buckets?: string[];
  }): Promise<ProfileResponse> {
    return this.request("POST", "/v1/profile", req);
  }

  // ---------------- memories ----------------

  /** POST /v1/memories — create memories directly (no extraction). */
  async createMemories(
    containerTag: string,
    memories: MemoryInput[],
  ): Promise<{ memories: MemoricaiMemory[] }> {
    return this.request("POST", "/v1/memories", { containerTag, memories });
  }

  /** PATCH /v1/memories — versioned update (appends a new version). */
  async patchMemory(req: {
    id: string;
    newContent: string;
    metadata?: Record<string, unknown>;
  }): Promise<MemoricaiMemory> {
    return this.request("PATCH", "/v1/memories", req);
  }

  /** DELETE /v1/memories — forget one memory by id or exact content. */
  async forgetMemory(req: {
    containerTag: string;
    id?: string;
    content?: string;
    reason?: string;
  }): Promise<MemoricaiMemory> {
    return this.request("DELETE", "/v1/memories", req);
  }

  /** POST /v1/memories/forget-matching — semantic bulk forget (dryRun defaults true server-side only when set; pass explicitly). */
  async forgetMatching(req: {
    containerTag: string;
    query: string;
    threshold?: number;
    maxForget?: number;
    dryRun?: boolean;
    reason?: string;
  }): Promise<unknown> {
    return this.request("POST", "/v1/memories/forget-matching", req);
  }
}

export default MemoricaiClient;
