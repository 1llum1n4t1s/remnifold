#!/usr/bin/env node
import { readFileSync, realpathSync, statSync } from "node:fs";
import { isAbsolute } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { api, getReadContext, searchIndex, listIndex, listTopics, readDocument, formatSearchResult, discoverSearchContainers } from "./memory-client.mjs";
import { DEFAULT_MEMORY_CONTAINER } from "./Import-RemnifoldHistory.mjs";
import { buildMemoryIndex } from "./memory-index.mjs";

const SERVER_VERSION = JSON.parse(readFileSync(new URL("../.codex-plugin/plugin.json", import.meta.url), "utf8")).version;
const PROTOCOL_VERSION = "2025-06-18";
const RUNTIME_PLUGIN_ROOT = realpathSync(fileURLToPath(new URL("..", import.meta.url)));
const pathKey = (value) => process.platform === "win32" ? value.toLowerCase() : value;

const stringSchema = { type: "string", minLength: 1 };
const containerProperty = { type: "string", minLength: 1, maxLength: 160, description: "Optional memory space tag" };
const topicProperty = { type: "string", minLength: 1, maxLength: 80, description: "Optional exact topic label; use __unclassified__ for documents without a topic" };
const sourceFolderProperty = {
  type: "string",
  minLength: 1,
  maxLength: 4096,
  description: "Absolute current workspace folder. Use when the MCP client does not provide exactly one workspace root.",
};
const tools = [
  {
    name: "search_memory",
    description: "Search concise memory indexes by topic across every nonempty Remnifold space, independent of the current work folder. Returns document IDs, source spaces and dates; use getDocument for details. An explicit containerTag restricts the search to that space.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", minLength: 1, maxLength: 1000 },
        includeProfile: { type: "boolean", default: false, description: "Compatibility option. true adds the searched-space index, never extracted factual profiles." },
        containerTag: containerProperty,
      },
      required: ["query"],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "add_memory",
    description: "Save or forget a memory in the user's Cloudflare D1 database. Saves use the shared memories container by default; sourceFolder optionally records workspace provenance. Forget accepts a documentId from search/list/getDocument or exact stored content, and is always restricted to one explicit or default container.",
    inputSchema: {
      type: "object",
      properties: {
        content: { type: "string", minLength: 1, maxLength: 200000, description: "Content to save, or exact stored content to forget when documentId is unavailable" },
        documentId: { type: "string", minLength: 1, maxLength: 64, description: "Document ID returned by search_memory, listDocuments, or getDocument; valid only for forget" },
        action: { type: "string", enum: ["save", "forget"], default: "save" },
        containerTag: containerProperty,
        sourceFolder: sourceFolderProperty,
      },
      anyOf: [
        { required: ["content"] },
        { properties: { action: { const: "forget" } }, required: ["action", "documentId"] },
      ],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
  },
  {
    name: "consolidate_memory",
    description: "Create or refresh the consolidated checkpoint for one project immediately. Originals are retained; sourceFolder identifies the current project when projectId is omitted.",
    inputSchema: {
      type: "object",
      properties: {
        projectId: { type: "string", minLength: 1, maxLength: 160, description: "Project provenance ID. Omit to derive it from sourceFolder or the MCP workspace root." },
        sourceFolder: sourceFolderProperty,
      },
      additionalProperties: false,
    },
    annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "listMemories",
    description: "List recent memory indexes across all physical spaces; use getDocument for full details. An exact topic filters by content label, and an explicit containerTag restricts the physical space.",
    inputSchema: {
      type: "object",
      properties: { page: { type: "integer", minimum: 1 }, limit: { type: "integer", minimum: 1, maximum: 50 }, containerTag: containerProperty, topic: topicProperty },
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "listDocuments",
    description: "List stored document indexes with topic and provenance across all physical spaces; use getDocument for full details. An exact topic filters by content label, and an explicit containerTag restricts the physical space.",
    inputSchema: {
      type: "object",
      properties: { page: { type: "integer", minimum: 1 }, limit: { type: "integer", minimum: 1, maximum: 50 }, containerTag: containerProperty, topic: topicProperty },
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "getDocument",
    description: "Read one memory document from Cloudflare D1.",
    inputSchema: { type: "object", properties: { documentId: stringSchema }, required: ["documentId"], additionalProperties: false },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "listTopics",
    description: "List content topics across all legacy and shared memory spaces, including the count of unclassified documents.",
    inputSchema: {
      type: "object",
      properties: { page: { type: "integer", minimum: 1 }, limit: { type: "integer", minimum: 1, maximum: 100 } },
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "listSpaces",
    description: "List physical memory spaces and legacy project-folder provenance tags for compatibility. Use listTopics for ordinary browsing.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
  {
    name: "whoAmI",
    description: "Show the active self-hosted Remnifold context. sourceFolder can identify the current workspace when client roots are unavailable.",
    inputSchema: { type: "object", properties: { sourceFolder: sourceFolderProperty }, additionalProperties: false },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  },
];

function textResult(text, structuredContent) {
  return { content: [{ type: "text", text }], structuredContent };
}

export function contextFromSourceFolder(sourceFolder, workspaceSource = "sourceFolder") {
  if (typeof sourceFolder !== "string" || !sourceFolder.trim() || sourceFolder.length > 4_096 || !isAbsolute(sourceFolder)) {
    throw new Error("sourceFolder must be an absolute existing directory path.");
  }
  let folder;
  try {
    folder = realpathSync(sourceFolder);
    if (!statSync(folder).isDirectory()) throw new Error("not a directory");
  } catch {
    throw new Error("sourceFolder must be an absolute existing directory path.");
  }
  return { ...getReadContext(folder), workspaceSource };
}

export async function callTool(name, args, { request = api, context, settings, resolveContext } = {}) {
  if (!args || typeof args !== "object" || Array.isArray(args)) throw new Error("Tool arguments must be an object.");
  if (args.containerTag !== undefined && (typeof args.containerTag !== "string" || !args.containerTag.trim() || args.containerTag.length > 160)) {
    throw new Error("containerTag must be a nonempty string of at most 160 characters.");
  }
  const explicitTag = typeof args.containerTag === "string" ? args.containerTag.trim() : undefined;
  const explicitContext = args.sourceFolder === undefined ? context : contextFromSourceFolder(args.sourceFolder);
  let resolvedContext;
  async function workspaceContext() {
    if (resolvedContext !== undefined) return resolvedContext;
    resolvedContext = explicitContext || (typeof resolveContext === "function" ? await resolveContext() : null);
    return resolvedContext;
  }

  if (name === "search_memory") {
    if (typeof args.query !== "string" || !args.query.trim() || args.query.length > 1_000) throw new Error("query must be a nonempty string of at most 1000 characters.");
    // 通常検索は全保存先を横断するため、作業場所を解決できなくても安全に実行できる。
    const result = await searchIndex({ query: args.query, containerTag: explicitTag, request, settings });
    const structured = { ...result, ...(args.includeProfile === true ? { profile: { type: "space-index", containers: result.searchedContainers } } : {}) };
    return textResult(formatSearchResult(result), structured);
  }

  if (name === "add_memory") {
    if (args.action !== undefined && args.action !== "save" && args.action !== "forget") throw new Error("action must be save or forget.");
    const action = args.action || "save";
    const hasContent = typeof args.content === "string" && args.content.trim().length > 0 && args.content.length <= 200_000;
    const hasDocumentId = typeof args.documentId === "string" && args.documentId.trim().length > 0 && args.documentId.length <= 64;
    if (action === "save" && !hasContent) throw new Error("content must be a nonempty string of at most 200000 characters.");
    if (action === "save" && args.documentId !== undefined) throw new Error("documentId is valid only when action is forget.");
    if (action === "forget" && !hasContent && !hasDocumentId) throw new Error("forget requires content or documentId.");
    if (args.content !== undefined && !hasContent) throw new Error("content must be a nonempty string of at most 200000 characters.");
    if (args.documentId !== undefined && !hasDocumentId) throw new Error("documentId must be a nonempty string of at most 64 characters.");
    const containerTag = explicitTag || DEFAULT_MEMORY_CONTAINER;
    if (action === "forget") {
      const result = await request("/v4/memories", { method: "DELETE", body: {
        containerTag,
        ...(hasDocumentId ? { documentId: args.documentId.trim() } : {}),
        ...(hasContent ? { content: args.content } : {}),
      } });
      return textResult(result.message, { action: "forget", success: true, containerTag, message: result.message, id: result.id });
    }

    let workspace = explicitContext;
    if (!workspace && !explicitTag) {
      try { workspace = await workspaceContext(); }
      catch { workspace = null; }
    }
    const result = await request("/v3/documents", {
      method: "POST",
      body: { containerTag, content: args.content, metadata: { sm_source: "codex-mcp", sm_scope: workspace ? "project" : "shared",
        ...(workspace ? { project: workspace.projectName, sm_project_id: workspace.containerTag } : {}),
        memoryIndex: buildMemoryIndex({ request: args.content, sourceKind: "explicit-memory" }) } },
    });
    const message = `Memory saved in Cloudflare D1 (ID: ${result.id})`;
    return textResult(message, { action: "save", success: true, containerTag, message, id: result.id, status: result.status });
  }

  if (name === "consolidate_memory") {
    if (args.projectId !== undefined && (typeof args.projectId !== "string" || !args.projectId.trim() || args.projectId.length > 160)) {
      throw new Error("projectId must be a nonempty string of at most 160 characters.");
    }
    const workspace = args.projectId === undefined ? await workspaceContext() : null;
    const projectId = typeof args.projectId === "string" ? args.projectId.trim() : workspace?.containerTag;
    if (!projectId) throw new Error("projectId or exactly one workspace root/sourceFolder is required.");
    const result = await request("/v4/consolidate", {
      method: "POST",
      body: { projectId, force: true },
      timeoutMs: 120_000,
    });
    const messages = {
      consolidated: `Memory consolidation completed for ${projectId}.`,
      no_unconsolidated_memories: `No unconsolidated memories were found for ${projectId}.`,
      not_due: `Memory consolidation is not due for ${projectId}.`,
      busy: `Memory consolidation is already running for ${projectId}.`,
    };
    const message = result.message || messages[result.status] || `Memory consolidation request finished for ${projectId}.`;
    return textResult(message, { ...result, projectId });
  }

  if (name === "listMemories" || name === "listDocuments") {
    if (args.page !== undefined && (!Number.isInteger(args.page) || args.page < 1)) throw new Error("page must be a positive integer.");
    if (args.limit !== undefined && (!Number.isInteger(args.limit) || args.limit < 1 || args.limit > 50)) throw new Error("limit must be an integer from 1 through 50.");
    if (args.topic !== undefined && (typeof args.topic !== "string" || !args.topic.trim() || args.topic.length > 80)) {
      throw new Error("topic must be a nonempty string of at most 80 characters.");
    }
    const result = await listIndex({ containerTag: explicitTag, topic: args.topic?.trim(), page: args.page ?? 1, limit: args.limit ?? 10, request });
    const items = result.documents;
    return textResult(JSON.stringify(items), {
      [name === "listMemories" ? "memoryEntries" : "documents"]: items,
      pagination: result.pagination,
      containerTag: result.containerTag,
      topic: result.topic,
      listScope: result.listScope,
    });
  }

  if (name === "getDocument") {
    if (typeof args.documentId !== "string" || !args.documentId.trim()) throw new Error("documentId must be a nonempty string.");
    const result = await readDocument(args.documentId.trim(), request);
    return textResult(result.text, { document: result.document });
  }

  if (name === "listSpaces") {
    const result = await request("/v3/container-tags");
    return textResult(JSON.stringify(result.spaces, null, 2), result);
  }

  if (name === "listTopics") {
    if (args.page !== undefined && (!Number.isInteger(args.page) || args.page < 1)) throw new Error("page must be a positive integer.");
    if (args.limit !== undefined && (!Number.isInteger(args.limit) || args.limit < 1 || args.limit > 100)) throw new Error("limit must be an integer from 1 through 100.");
    const result = await listTopics(request, {
      ...(args.page === undefined ? {} : { page: args.page }),
      ...(args.limit === undefined ? {} : { limit: args.limit }),
    });
    return textResult(JSON.stringify(result, null, 2), result);
  }

  if (name === "whoAmI") {
    const workspace = await workspaceContext();
    const [session, discovery] = await Promise.all([request("/v3/session"), discoverSearchContainers(request)]);
    const structured = {
      userId: session.user.id,
      name: session.user.name,
      role: session.role,
      accessType: session.accessType,
      activeSpace: DEFAULT_MEMORY_CONTAINER,
      defaultStorageContainer: DEFAULT_MEMORY_CONTAINER,
      workspaceProvenance: workspace?.containerTag || null,
      workspaceSource: workspace?.workspaceSource || (explicitContext ? "provided-context" : "unavailable"),
      searchScope: "all-discovered-containers",
      readSpaces: discovery.tags,
      spaceDiscoveryComplete: discovery.complete,
      recallFormat: "document-index",
      storage: "cloudflare-d1",
    };
    return textResult(JSON.stringify(structured), structured);
  }

  throw new Error("Unknown tool");
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

let clientSupportsRoots = false;
let cachedRootContext = null;
let cachedRootGeneration = -1;
let rootGeneration = 0;
let rootResolution = null;
let nextServerRequestId = 1;
const pendingClientRequests = new Map();

function requestClient(method, params, timeoutMs = 1_000) {
  const id = `remnifold-${nextServerRequestId++}`;
  return new Promise((resolveRequest, reject) => {
    const timeout = setTimeout(() => {
      pendingClientRequests.delete(id);
      reject(new Error(`MCP client did not answer ${method}.`));
    }, timeoutMs);
    pendingClientRequests.set(id, {
      resolve(value) { clearTimeout(timeout); resolveRequest(value); },
      reject(error) { clearTimeout(timeout); reject(error); },
    });
    send({ jsonrpc: "2.0", id, method, params });
  });
}

async function resolveClientContext() {
  if (!clientSupportsRoots) return null;
  if (cachedRootGeneration === rootGeneration) return cachedRootContext;
  if (rootResolution?.generation === rootGeneration) return rootResolution.promise;

  const generation = rootGeneration;
  const promise = (async () => {
    let context = null;
    try {
      const response = await requestClient("roots/list", {});
      if (Array.isArray(response?.roots)) {
        const folders = [...new Set(response.roots.flatMap((root) => {
          if (!root || typeof root.uri !== "string") return [];
          try {
            const url = new URL(root.uri);
            if (url.protocol !== "file:") return [];
            return [realpathSync(fileURLToPath(url))];
          } catch { return []; }
        }))];
        if (folders.length === 1 && pathKey(folders[0]) !== pathKey(RUNTIME_PLUGIN_ROOT)) {
          context = contextFromSourceFolder(folders[0], "mcp-roots");
        }
      }
    } catch {
      // 利用できないrootsは明示sourceFolderへフォールバックする。
    }

    // 取得中にrootsが更新された場合、古い応答を待っていた呼び出しも新しい世代へ合流する。
    if (generation !== rootGeneration) return resolveClientContext();
    cachedRootContext = context;
    cachedRootGeneration = generation;
    return context;
  })();
  rootResolution = { generation, promise };
  try {
    return await promise;
  } finally {
    if (rootResolution?.promise === promise) rootResolution = null;
  }
}

async function handle(message) {
  if (!message || typeof message !== "object") return;
  if (message.method === undefined && message.id !== undefined) {
    const pending = pendingClientRequests.get(message.id);
    if (!pending) return;
    pendingClientRequests.delete(message.id);
    if (message.error) pending.reject(new Error(message.error.message || "MCP client request failed."));
    else pending.resolve(message.result);
    return;
  }
  if (message.id === undefined) {
    if (message.method === "notifications/roots/list_changed") {
      rootGeneration += 1;
      cachedRootContext = null;
      cachedRootGeneration = -1;
    }
    return;
  }

  try {
    let result;
    if (message.method === "initialize") {
      clientSupportsRoots = message.params?.capabilities?.roots !== undefined;
      rootGeneration += 1;
      cachedRootContext = null;
      cachedRootGeneration = -1;
      rootResolution = null;
      result = {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: "remnifold", version: SERVER_VERSION },
      };
    } else if (message.method === "ping") {
      result = {};
    } else if (message.method === "tools/list") {
      result = { tools };
    } else if (message.method === "tools/call") {
      result = await callTool(message.params?.name, message.params?.arguments ?? {}, { resolveContext: resolveClientContext });
    } else if (message.method === "resources/list" || message.method === "prompts/list") {
      result = { [message.method.startsWith("resources") ? "resources" : "prompts"]: [] };
    } else {
      send({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not found" } });
      return;
    }
    send({ jsonrpc: "2.0", id: message.id, result });
  } catch (error) {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        content: [{ type: "text", text: error instanceof Error ? error.message : "Remnifold request failed" }],
        isError: true,
      },
    });
  }
}

export function startMcpServer() {
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  input.on("line", (line) => {
    if (!line.trim() || line.length > 1024 * 1024) return;
    try {
      const message = JSON.parse(line);
      void handle(message);
    } catch {
      send({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "Parse error" } });
    }
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  startMcpServer();
}
