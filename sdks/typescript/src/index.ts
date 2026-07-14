/**
 * TypeScript SDK for the memoricai /v1 HTTP API. Zero dependencies (global fetch).
 *
 * ```ts
 * import { MemoricaiClient } from "memoricai";
 *
 * const client = new MemoricaiClient("http://localhost:7373", "mc_...");
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
  raw?: string;
}

export interface BatchIngestRequest {
  documents: AddDocumentRequest[];
  containerTag?: string;
  entityContext?: string;
  metadata?: Record<string, unknown>;
}

export interface BatchIngestResponse {
  results: Array<{ id?: string; status: string; error?: string }>;
  success: number;
  failed: number;
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

export interface DocumentListRequest {
  containerTags?: string[];
  page?: number;
  limit?: number;
  status?: string;
  sort?: string;
  order?: "asc" | "desc";
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

export interface ContextRequest {
  q: string;
  containerTag?: string;
  mode?: "auto" | "lookup" | "aggregation";
  budgetTokens?: number;
  maxSources?: number;
  threshold?: number;
  rewriteQuery?: boolean;
  filters?: unknown;
  includeDigest?: boolean;
}

export interface ContextEvidence {
  rank: number;
  sourceId: string;
  documentId: string;
  sessionId?: string;
  date?: string;
  score: number;
  included: boolean;
  availableChars: number;
  includedChars: number;
  truncated: boolean;
  omissionReason?: "sourceLimit" | "budget" | "noContent";
  content?: string;
}

export interface ContextOmission {
  rank: number;
  sourceId: string;
  documentId: string;
  reason: "sourceLimit" | "budget" | "noContent";
}

export interface ContextDiagnostics {
  mode: "lookup" | "aggregation";
  aggregationQuery: boolean;
  budgetTokens: number;
  budgetChars: number;
  usedChars: number;
  estimatedTokens: number;
  digestChars: number;
  evidenceChars: number;
  sourcesConsidered: number;
  sourcesSelected: number;
  sourcesIncluded: number;
  sourcesOmitted: number;
  truncatedSources: number;
  digestTruncated: boolean;
  hardTruncated: false;
  omissions: ContextOmission[];
}

export interface ContextResponse {
  context: string;
  digest?: string;
  evidence: ContextEvidence[];
  diagnostics: ContextDiagnostics;
  timing: number;
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

export interface ProfileRequest {
  containerTag: string;
  q?: string;
  threshold?: number;
  filters?: unknown;
  include?: string[];
  buckets?: string[];
}

export interface MemoryInput {
  content: string;
  isStatic?: boolean;
  metadata?: Record<string, unknown>;
}

export interface CreatedMemory {
  id: string;
  memory: string;
  isStatic: boolean;
  createdAt: string;
}

export interface CreateMemoriesResponse {
  documentId: string | null;
  memories: CreatedMemory[];
}

export interface ForgetCandidate {
  id: string;
  memory: string;
  similarity: number;
}

export interface ForgetMatchingResponse {
  dryRun: boolean;
  count: number;
  forgetBatchId?: string;
  summary: string;
  candidates?: ForgetCandidate[];
  forgotten?: ForgetCandidate[];
}

export interface MemoricaiMemory {
  id: string;
  memory: string;
  version: number;
  isLatest: boolean;
  [key: string]: unknown;
}

export interface Pagination {
  currentPage: number;
  limit: number;
  totalItems: number;
  totalPages: number;
}

export interface Project {
  id: string;
  name: string;
  containerTag: string;
  emoji?: string;
  createdAt: string;
  updatedAt: string;
  isExperimental: boolean;
  documentCount?: number;
}

export interface OrganizationSettings {
  shouldLlmFilter: boolean;
  filterPrompt?: string;
  categories?: string[];
  includeItems?: string[];
  excludeItems?: string[];
  chunkSize: number;
}

export interface UpdateSettingsRequest {
  shouldLlmFilter?: boolean;
  filterPrompt?: string;
  categories?: string[];
  includeItems?: string[];
  excludeItems?: string[];
  chunkSize?: number;
}

export interface SessionResponse {
  user: { id: string; email: string; name?: string };
  org: { id: string; name: string; metadata?: unknown };
}

export interface CreateScopedKeyRequest {
  containerTag: string;
  name?: string;
  expiresInDays?: number;
  rateLimitMax?: number;
  rateLimitTimeWindow?: number;
}

export interface CreateScopedKeyResponse {
  key: string;
  id: string;
  name: string;
  containerTag: string;
  expiresAt?: string;
  allowedEndpoints: string[];
}

export interface ProfileBucket {
  key: string;
  description: string;
}

export interface InferredMemory {
  id: string;
  memory: string;
  parentCount: number;
  createdAt: string;
  updatedAt: string;
  metadata: Record<string, unknown>;
}

export interface AnalyticsQuery {
  period?: "1h" | "24h" | "7d" | "30d" | "90d" | "all";
  page?: number;
  limit?: number;
}

export interface Connection {
  id: string;
  provider: string;
  userId?: string;
  email?: string;
  documentLimit: number;
  containerTags: string[];
  expiresAt?: string;
  metadata: Record<string, unknown>;
  lastSyncedAt?: string;
  createdAt: string;
}

export interface CreateConnectionRequest {
  redirectUrl?: string;
  containerTags?: string[];
  documentLimit?: number;
  metadata?: Record<string, unknown>;
}

export interface CreateConnectionResponse {
  id: string;
  authLink: string | null;
  expiresIn: string | null;
  redirectsTo: string | null;
}

export interface SyncRun {
  id: string;
  connectionId: string;
  status: string;
  triggerType: string;
  errorKind?: string;
  startedAt: string;
  completedAt?: string;
  itemsProcessed: number;
  itemsFailed: number;
  error?: string;
}

export interface RegisterOAuthClientRequest {
  redirect_uris: string[];
  client_name?: string;
  grant_types?: Array<"authorization_code" | "refresh_token">;
  token_endpoint_auth_method?: "none" | "client_secret_post";
}

export interface RegisterOAuthClientResponse {
  client_id: string;
  client_secret?: string;
  redirect_uris: string[];
  grant_types: string[];
  token_endpoint_auth_method: string;
}

export interface OAuthTokenRequest {
  grant_type: "authorization_code" | "refresh_token";
  client_id: string;
  client_secret?: string;
  code?: string;
  redirect_uri?: string;
  code_verifier?: string;
  refresh_token?: string;
}

export interface OAuthTokenResponse {
  access_token: string;
  token_type: string;
  expires_in: number;
  refresh_token?: string;
  scope?: string;
}

type QueryValue = string | number | boolean | undefined;

export interface RawRequestOptions {
  query?: Record<string, QueryValue>;
  headers?: HeadersInit;
  body?: BodyInit;
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

  private url(path: string, query?: Record<string, QueryValue>): string {
    const url = new URL(this.baseUrl + path);
    for (const [key, value] of Object.entries(query ?? {})) {
      if (value !== undefined) url.searchParams.set(key, String(value));
    }
    return url.toString();
  }

  /**
   * Low-level transport for router streaming and forward-compatible access to
   * newly introduced engine endpoints. Prefer the typed methods below.
   */
  async requestRaw(
    method: string,
    path: string,
    options: RawRequestOptions = {},
  ): Promise<Response> {
    for (let attempt = 0; ; attempt++) {
      const headers = new Headers(options.headers);
      if (!headers.has("Authorization")) {
        headers.set("Authorization", `Bearer ${this.apiKey}`);
      }
      const resp = await fetch(this.url(path, options.query), {
        method,
        headers,
        body: options.body,
        signal: AbortSignal.timeout(this.timeoutMs),
      });
      if (resp.ok) return resp;
      const status = resp.status;
      const text = await resp.text();
      if ((status === 429 || status >= 500) && attempt < this.maxRetries) {
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

  /** Send JSON and decode a JSON response. */
  async request<T>(
    method: string,
    path: string,
    body?: unknown,
    options: Omit<RawRequestOptions, "body"> = {},
  ): Promise<T> {
    const headers = new Headers(options.headers);
    if (body !== undefined) headers.set("Content-Type", "application/json");
    const resp = await this.requestRaw(method, path, {
      ...options,
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    const text = await resp.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  async health(): Promise<{ service: string; status: string; version: string }> {
    return this.request("GET", "/health");
  }

  async openapi(): Promise<Record<string, unknown>> {
    return this.request("GET", "/v1/openapi");
  }

  async oauthMetadata(): Promise<Record<string, unknown>> {
    return this.request("GET", "/.well-known/oauth-authorization-server");
  }

  async registerOAuthClient(
    req: RegisterOAuthClientRequest,
  ): Promise<RegisterOAuthClientResponse> {
    return this.request("POST", "/api/auth/oauth2/register", req);
  }

  async exchangeOAuthToken(req: OAuthTokenRequest): Promise<OAuthTokenResponse> {
    const form = new URLSearchParams();
    for (const [key, value] of Object.entries(req)) {
      if (value !== undefined) form.set(key, value);
    }
    const response = await this.requestRaw("POST", "/api/auth/oauth2/token", {
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: form,
    });
    return response.json() as Promise<OAuthTokenResponse>;
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

  async addDocuments(req: BatchIngestRequest): Promise<BatchIngestResponse> {
    return this.request("POST", "/v1/documents/batch", req);
  }

  async uploadFile(
    content: Blob | ArrayBuffer | Uint8Array,
    filename: string,
    options: {
      containerTag?: string;
      containerTags?: string[];
      metadata?: Record<string, unknown>;
      contentType?: string;
    } = {},
  ): Promise<IngestResponse> {
    const form = new FormData();
    if (options.containerTag !== undefined) form.append("containerTag", options.containerTag);
    for (const tag of options.containerTags ?? []) form.append("containerTags", tag);
    if (options.metadata !== undefined) {
      form.append("metadata", JSON.stringify(options.metadata));
    }
    const blob = content instanceof Blob
      ? content
      : new Blob(
          [content instanceof Uint8Array ? content.slice().buffer as ArrayBuffer : content],
          { type: options.contentType ?? "application/octet-stream" },
        );
    form.append("file", blob, filename);
    const response = await this.requestRaw("POST", "/v1/documents/file", { body: form });
    return response.json() as Promise<IngestResponse>;
  }

  async getDocument(id: string): Promise<MemoricaiDocument> {
    return this.request("GET", `/v1/documents/${encodeURIComponent(id)}`);
  }

  async deleteDocument(id: string): Promise<unknown> {
    return this.request("DELETE", `/v1/documents/${encodeURIComponent(id)}`);
  }

  async patchDocument(
    id: string,
    req: { content?: string; metadata?: Record<string, unknown> },
  ): Promise<MemoricaiDocument> {
    return this.request("PATCH", `/v1/documents/${encodeURIComponent(id)}`, req);
  }

  async listDocuments(req: DocumentListRequest = {}): Promise<DocumentListResponse> {
    return this.request("POST", "/v1/documents/list", req);
  }

  async listDocumentsWithMemories(
    req: Pick<DocumentListRequest, "containerTags" | "page" | "limit"> = {},
  ): Promise<{ documents: Array<MemoricaiDocument & { memoryEntries: MemoricaiMemory[] }>; pagination: Pagination }> {
    return this.request("POST", "/v1/documents/documents", req);
  }

  async listProcessingDocuments(): Promise<{ documents: MemoricaiDocument[] }> {
    return this.request("GET", "/v1/documents/processing");
  }

  async bulkDeleteDocuments(req: {
    ids?: string[];
    containerTags?: string[];
  }): Promise<{ success: boolean; deletedCount: number }> {
    return this.request("DELETE", "/v1/documents/bulk", req);
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

  /** POST /v1/context — bounded, source-aware context ready for an LLM prompt. */
  async buildContext(req: ContextRequest): Promise<ContextResponse> {
    return this.request("POST", "/v1/context", req);
  }

  /** POST /v1/profile — static/dynamic/bucketed user profile. */
  async profile(req: ProfileRequest): Promise<ProfileResponse> {
    return this.request("POST", "/v1/profile", req);
  }

  // ---------------- memories ----------------

  /** POST /v1/memories — create memories directly (no extraction). */
  async createMemories(
    containerTag: string,
    memories: MemoryInput[],
  ): Promise<CreateMemoriesResponse> {
    return this.request("POST", "/v1/memories", { containerTag, memories });
  }

  /** PATCH /v1/memories — versioned update (appends a new version). */
  async patchMemory(req: {
    id?: string;
    content?: string;
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

  /**
   * POST /v1/memories/forget-matching — semantic bulk forget.
   * `dryRun` defaults to **true** client-side (the server default is false); pass
   * `dryRun: false` explicitly to actually delete.
   */
  async forgetMatching(req: {
    containerTag: string;
    query: string;
    threshold?: number;
    maxForget?: number;
    dryRun?: boolean;
    reason?: string;
  }): Promise<ForgetMatchingResponse> {
    return this.request("POST", "/v1/memories/forget-matching", { dryRun: true, ...req });
  }

  // ---------------- projects / tags ----------------

  async listProjects(): Promise<{ projects: Project[] }> {
    return this.request("GET", "/v1/projects");
  }

  async listContainerTags(): Promise<{ projects: Project[] }> {
    return this.listProjects();
  }

  async createProject(req: { name: string; emoji?: string }): Promise<Project> {
    return this.request("POST", "/v1/projects", req);
  }

  async deleteProject(
    id: string,
    req: { action: "move" | "delete"; targetProjectId?: string } = { action: "delete" },
  ): Promise<Record<string, unknown>> {
    return this.request("DELETE", `/v1/projects/${encodeURIComponent(id)}`, req);
  }

  async updateContainerTag(
    tag: string,
    req: { name?: string; entityContext?: string },
  ): Promise<Project> {
    return this.request("PATCH", `/v1/container-tags/${encodeURIComponent(tag)}`, req);
  }

  async deleteContainerTag(tag: string): Promise<Record<string, unknown>> {
    return this.request("DELETE", `/v1/container-tags/${encodeURIComponent(tag)}`);
  }

  // ---------------- settings / auth ----------------

  async getSettings(): Promise<OrganizationSettings> {
    return this.request("GET", "/v1/settings");
  }

  async updateSettings(req: UpdateSettingsRequest): Promise<OrganizationSettings> {
    return this.request("PATCH", "/v1/settings", req);
  }

  async resetSettings(confirmation = "RESET"): Promise<Record<string, unknown>> {
    return this.request("POST", "/v1/settings/reset", { confirmation });
  }

  async session(): Promise<SessionResponse> {
    return this.request("GET", "/v1/session");
  }

  async createScopedKey(req: CreateScopedKeyRequest): Promise<CreateScopedKeyResponse> {
    return this.request("POST", "/v1/auth/scoped-key", req);
  }

  async revokeScopedKey(id: string): Promise<{ success: boolean }> {
    return this.request("DELETE", `/v1/auth/scoped-key/${encodeURIComponent(id)}`);
  }

  // ---------------- profile buckets / inferred memories ----------------

  async listProfileBuckets(containerTag?: string): Promise<{ buckets: ProfileBucket[] }> {
    return this.request("POST", "/v1/profile/buckets", { containerTag });
  }

  async createProfileBucket(req: {
    containerTag?: string;
    key: string;
    description: string;
  }): Promise<ProfileBucket> {
    return this.request("POST", "/v1/buckets", req);
  }

  async listInferredMemories(tag: string): Promise<{ memories: InferredMemory[]; total: number }> {
    return this.request("GET", `/v1/container-tags/${encodeURIComponent(tag)}/inferred`);
  }

  async reviewInferredMemory(
    tag: string,
    memoryId: string,
    action: "approve" | "decline" | "undo",
  ): Promise<Record<string, unknown>> {
    return this.request(
      "POST",
      `/v1/container-tags/${encodeURIComponent(tag)}/inferred/${encodeURIComponent(memoryId)}/review`,
      { action },
    );
  }

  // ---------------- analytics ----------------

  private analytics<T>(resource: string, query: AnalyticsQuery = {}): Promise<T> {
    return this.request("GET", `/v1/analytics/${resource}`, undefined, {
      query: { period: query.period, page: query.page, limit: query.limit },
    });
  }

  analyticsUsage(query: AnalyticsQuery = {}): Promise<Record<string, unknown>> {
    return this.analytics("usage", query);
  }

  analyticsErrors(query: AnalyticsQuery = {}): Promise<Record<string, unknown>> {
    return this.analytics("errors", query);
  }

  analyticsLogs(query: AnalyticsQuery = {}): Promise<Record<string, unknown>> {
    return this.analytics("logs", query);
  }

  analyticsMemory(): Promise<{ totalMemories: number }> {
    return this.analytics("memory");
  }

  analyticsChat(): Promise<{ tokensSaved: number; costSavedUsd: number }> {
    return this.analytics("chat");
  }

  // ---------------- connections ----------------

  async listConnections(req?: {
    containerTags?: string[];
    provider?: string;
  }): Promise<Connection[]> {
    return req === undefined
      ? this.request("GET", "/v1/connections")
      : this.request("POST", "/v1/connections/list", req);
  }

  async createConnection(
    provider: string,
    req: CreateConnectionRequest = {},
  ): Promise<CreateConnectionResponse> {
    return this.request("POST", `/v1/connections/${encodeURIComponent(provider)}`, req);
  }

  async getConnection(id: string): Promise<Connection> {
    return this.request("GET", `/v1/connections/${encodeURIComponent(id)}`);
  }

  async deleteConnection(
    idOrProvider: string,
    deleteDocuments = true,
  ): Promise<Record<string, unknown>> {
    return this.request(
      "DELETE",
      `/v1/connections/${encodeURIComponent(idOrProvider)}`,
      undefined,
      { query: { deleteDocuments } },
    );
  }

  async importConnection(idOrProvider: string): Promise<Record<string, unknown>> {
    return this.request(
      "POST",
      `/v1/connections/${encodeURIComponent(idOrProvider)}/import`,
      {},
    );
  }

  async connectionSyncRuns(id: string): Promise<SyncRun[]> {
    return this.request("GET", `/v1/connections/${encodeURIComponent(id)}/sync-runs`);
  }

  async connectionResources(
    id: string,
    query: { page?: number; perPage?: number } = {},
  ): Promise<unknown> {
    return this.request(
      "GET",
      `/v1/connections/${encodeURIComponent(id)}/resources`,
      undefined,
      { query },
    );
  }

  async configureConnection(id: string, configuration: unknown): Promise<unknown> {
    return this.request(
      "POST",
      `/v1/connections/${encodeURIComponent(id)}/configure`,
      configuration,
    );
  }

  // ---------------- memory router / MCP OAuth helpers ----------------

  /** Returns the raw response so callers retain access to streamed SSE bodies. */
  routerRequest(
    upstreamUrl: string,
    body: unknown,
    upstreamApiKey: string,
    containerTag?: string,
  ): Promise<Response> {
    const headers = new Headers({
      Authorization: `Bearer ${upstreamApiKey}`,
      "Content-Type": "application/json",
      "x-memoricai-api-key": this.apiKey,
    });
    if (containerTag !== undefined) headers.set("x-mc-project", containerTag);
    const target = encodeURI(upstreamUrl.replace(/%/g, "%25"))
      .replace(/\?/g, "%3F")
      .replace(/#/g, "%23");
    return this.requestRaw("POST", `/v1/router/${target}`, {
      headers,
      body: JSON.stringify(body),
    });
  }

  mcpSessionWithKey(): Promise<Record<string, unknown>> {
    return this.request("GET", "/v1/mcp/session-with-key");
  }

  connectMcpScope(body: unknown): Promise<Record<string, unknown>> {
    return this.request("POST", "/v1/mcp/connect-scope", body);
  }

  provision(orgName: string, email: string): Promise<Record<string, unknown>> {
    return this.request("POST", "/v1/admin/provision", { orgName, email });
  }
}

export default MemoricaiClient;
