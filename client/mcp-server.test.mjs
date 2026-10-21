import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { callTool } from "./mcp-server.mjs";
import { getReadContext } from "./memory-client.mjs";

async function listen(t, handler) {
  const server = createServer(handler);
  await new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  t.after(() => new Promise((resolveClose) => server.close(resolveClose)));
  return `http://127.0.0.1:${server.address().port}`;
}

function exchange(message) {
  return new Promise((resolveExchange, reject) => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("./mcp-server.mjs", import.meta.url))], { stdio: ["pipe", "pipe", "pipe"] });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let stderr = "";
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`MCP response timed out: ${stderr}`));
    }, 5_000);
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    lines.once("line", (line) => {
      clearTimeout(timeout);
      resolveExchange(JSON.parse(line));
    });
    child.stdin.end(`${JSON.stringify(message)}\n`);
  });
}

test("MCP初期化は実装済みprotocolとplugin manifestのversionを返す", async () => {
  const manifest = JSON.parse(readFileSync(new URL("../.codex-plugin/plugin.json", import.meta.url), "utf8"));
  const response = await exchange({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2099-01-01" } });
  assert.equal(response.result.protocolVersion, "2025-06-18");
  assert.equal(response.result.serverInfo.version, manifest.version);
  assert.equal(response.result.serverInfo.name, "remnifold");
});

test("tools/listは作業場所が必要なツールだけにsourceFolderを公開する", async () => {
  const response = await exchange({ jsonrpc: "2.0", id: 1, method: "tools/list" });
  const schemas = new Map(response.result.tools.map((tool) => [tool.name, tool.inputSchema.properties]));
  for (const name of ["add_memory", "consolidate_memory", "whoAmI"]) {
    assert.ok(schemas.get(name).sourceFolder, `${name} must expose sourceFolder`);
  }
  for (const name of ["search_memory", "listMemories", "listDocuments", "listTopics", "getDocument", "listSpaces"]) {
    assert.equal(schemas.get(name).sourceFolder, undefined, `${name} must not expose sourceFolder`);
  }
});

test("consolidate_memoryは明示projectまたは作業場所を使って即時清書する", async (t) => {
  const calls = [];
  const request = async (path, options) => {
    calls.push([path, options]);
    return { status: "consolidated", consolidationId: "consolidation-1", memoryId: "summary-1", sourceCount: 21 };
  };
  const explicit = await callTool("consolidate_memory", { projectId: "repo-explicit" }, { request });
  assert.deepEqual(calls[0], ["/v4/consolidate", {
    method: "POST",
    body: { projectId: "repo-explicit", force: true },
    timeoutMs: 120_000,
  }]);
  assert.equal(explicit.structuredContent.projectId, "repo-explicit");

  const sourceFolder = mkdtempSync(join(tmpdir(), "memory-consolidate-source-"));
  t.after(() => rmSync(sourceFolder, { recursive: true, force: true }));
  const derived = await callTool("consolidate_memory", { sourceFolder }, { request });
  assert.match(calls[1][1].body.projectId, /^repo_memory_consolidate_source_.*__[a-f0-9]{16}$/);
  assert.equal(derived.structuredContent.sourceCount, 21);

  await assert.rejects(callTool("consolidate_memory", {}, { resolveContext: async () => null, request }), /projectId.*workspace/);
  await assert.rejects(callTool("consolidate_memory", { projectId: " " }, { request }), /projectId/);
});

test("作業場所を解決できない手動保存も共通containerへ保存し出典を捏造しない", async () => {
  let requestBody;
  const result = await callTool("add_memory", { content: "保存内容" }, {
    resolveContext: async () => { throw new Error("roots unavailable"); },
    request: async (path, options) => {
      assert.equal(path, "/v3/documents");
      requestBody = options.body;
      return { id: "saved-id", status: "created" };
    },
  });
  assert.equal(requestBody.containerTag, "memories");
  assert.equal(requestBody.metadata.sm_scope, "shared");
  assert.equal(requestBody.metadata.project, undefined);
  assert.equal(requestBody.metadata.sm_project_id, undefined);
  assert.equal(result.structuredContent.containerTag, "memories");
});

test("sourceFolderから保存先を導出して手動保存の出典へ記録する", async (t) => {
  const sourceFolder = mkdtempSync(join(tmpdir(), "memory-mcp-source-"));
  t.after(() => rmSync(sourceFolder, { recursive: true, force: true }));
  let requestBody;
  const result = await callTool("add_memory", { content: "Chrome拡張機能の実装", sourceFolder }, {
    request: async (path, options) => {
      assert.equal(path, "/v3/documents");
      requestBody = options.body;
      return { id: "saved-id", status: "created" };
    },
  });
  assert.equal(requestBody.containerTag, "memories");
  assert.match(requestBody.metadata.sm_project_id, /^repo_memory_mcp_source_.*__[a-f0-9]{16}$/);
  assert.match(requestBody.metadata.project, /^memory_mcp_source_/);
  assert.equal(result.structuredContent.containerTag, "memories");
});

test("forgetの既定対象は共通containerだけで、旧project空間へ拡張しない", async () => {
  const calls = [];
  const request = async (path, options) => {
    calls.push([path, options]);
    return { message: "forgotten" };
  };
  await callTool("add_memory", { content: "削除対象", action: "forget" }, { context: getReadContext(process.cwd()), request });
  await callTool("add_memory", { content: "限定削除", action: "forget", containerTag: "legacy-project" }, { request });
  assert.deepEqual(calls.map(([, options]) => options.body.containerTag), ["memories", "legacy-project"]);
  assert.ok(calls.every(([path, options]) => path === "/v4/memories" && options.method === "DELETE"));
});

test("forgetはgetDocumentで得たIDをcontainerと共に送る", async () => {
  let requestBody;
  const result = await callTool("add_memory", {
    action: "forget",
    documentId: "codex-turn-v2:0123456789abcdef",
    containerTag: "legacy-project",
  }, { request: async (path, options) => {
    assert.equal(path, "/v4/memories");
    assert.equal(options.method, "DELETE");
    requestBody = options.body;
    return { id: "codex-turn-v2:0123456789abcdef", message: "Memory forgotten" };
  } });
  assert.deepEqual(requestBody, {
    containerTag: "legacy-project",
    documentId: "codex-turn-v2:0123456789abcdef",
  });
  assert.equal(result.structuredContent.id, "codex-turn-v2:0123456789abcdef");
});

test("listTopicsは全空間のtopic件数と未分類件数をページ付きで返す", async () => {
  const result = await callTool("listTopics", { page: 2, limit: 40 }, { request: async (path) => {
    assert.equal(path, "/v4/topics?page=2&limit=40");
    return { topics: [{ topic: "Cloudflare D1", documentCount: 7 }], unclassifiedCount: 3,
      pagination: { currentPage: 2, limit: 40, totalItems: 41, totalPages: 2 } };
  } });
  assert.deepEqual(result.structuredContent, {
    topics: [{ topic: "Cloudflare D1", documentCount: 7 }],
    unclassifiedCount: 3,
    pagination: { currentPage: 2, limit: 40, totalItems: 41, totalPages: 2 },
  });
  await callTool("listTopics", {}, { request: async (path) => {
    assert.equal(path, "/v4/topics");
    return { topics: [], unclassifiedCount: 0 };
  } });
});

test("whoAmIは共通保存先とworkspace provenanceを区別する", async () => {
  const workspace = getReadContext(process.cwd());
  const result = await callTool("whoAmI", {}, { context: workspace, request: async (path) => {
    if (path === "/v3/session") return { user: { id: "user", name: "User" }, role: "owner", accessType: "api-key" };
    if (path === "/v3/container-tags") return { spaces: [{ containerTag: "memories", memoryCount: 1 }] };
    assert.fail(`unexpected request: ${path}`);
  } });
  assert.equal(result.structuredContent.activeSpace, "memories");
  assert.equal(result.structuredContent.defaultStorageContainer, "memories");
  assert.equal(result.structuredContent.workspaceProvenance, workspace.containerTag);
});

test("全保存先の話題検索は作業場所を解決せず実行できる", async () => {
  let contextResolutions = 0;
  const result = await callTool("search_memory", { query: "Chrome拡張機能" }, {
    resolveContext: async () => { contextResolutions += 1; throw new Error("must not resolve"); },
    settings: { recallMode: "direct", maxMemories: 5, minimumSimilarity: 0.7 },
    request: async (path, { body }) => {
      assert.equal(path, "/v4/search");
      assert.equal(body.allContainers, true);
      return { searchScope: "all-containers", searchedContainers: [], spaceDiscoveryComplete: true, results: [] };
    },
  });
  assert.equal(contextResolutions, 0);
  assert.equal(result.structuredContent.searchScope, "all-containers");
  assert.deepEqual(result.structuredContent.results, []);
});

test("roots対応クライアントでは単一のworkspace rootを遅延取得する", async (t) => {
  const sourceFolder = mkdtempSync(join(tmpdir(), "memory-mcp-root-"));
  const codexHome = mkdtempSync(join(tmpdir(), "memory-mcp-home-"));
  t.after(() => {
    rmSync(sourceFolder, { recursive: true, force: true });
    rmSync(codexHome, { recursive: true, force: true });
  });
  const environment = {
    ...process.env,
    CODEX_HOME: codexHome,
    REMNIFOLD_API_URL: "",
    REMNIFOLD_API_KEY: "",
    KAGAYOI_MEMORY_API_URL: "",
    KAGAYOI_MEMORY_API_KEY: "",
  };
  await new Promise((resolveTest, reject) => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("./mcp-server.mjs", import.meta.url))], { env: environment, stdio: ["pipe", "pipe", "pipe"] });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let step = 0;
    let stderr = "";
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`MCP roots exchange timed out: ${stderr}`));
    }, 5_000);
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    lines.on("line", (line) => {
      try {
        const message = JSON.parse(line);
        if (step === 0) {
          assert.equal(message.id, 1);
          child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "add_memory", arguments: { content: "保存" } } })}\n`);
        } else if (step === 1) {
          assert.equal(message.method, "roots/list");
          child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { roots: [{ uri: pathToFileURL(sourceFolder).href }] } })}\n`);
        } else {
          assert.equal(message.id, 2);
          assert.equal(message.result.isError, true);
          assert.match(message.result.content[0].text, /configuration requires baseUrl and apiKey/);
          clearTimeout(timeout);
          child.stdin.end();
          resolveTest();
        }
        step += 1;
      } catch (error) {
        clearTimeout(timeout);
        child.kill();
        reject(error);
      }
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: { roots: { listChanged: true } } } })}\n`);
  });
});

test("並行呼び出しはroots取得を共有し、取得中の更新後は全件が新しいrootを使う", async (t) => {
  const oldRoot = mkdtempSync(join(tmpdir(), "memory-mcp-old-root-"));
  const newRoot = mkdtempSync(join(tmpdir(), "memory-mcp-new-root-"));
  const codexHome = mkdtempSync(join(tmpdir(), "memory-mcp-concurrent-home-"));
  t.after(() => {
    rmSync(oldRoot, { recursive: true, force: true });
    rmSync(newRoot, { recursive: true, force: true });
    rmSync(codexHome, { recursive: true, force: true });
  });
  const documents = [];
  const baseUrl = await listen(t, (request, response) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      documents.push(JSON.parse(body));
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ id: `saved-${documents.length}`, status: "created" }));
    });
  });
  const expectedTag = getReadContext(newRoot).containerTag;
  const environment = {
    ...process.env,
    CODEX_HOME: codexHome,
    REMNIFOLD_API_URL: baseUrl,
    REMNIFOLD_API_KEY: "concurrent-test-key",
  };

  await new Promise((resolveTest, reject) => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("./mcp-server.mjs", import.meta.url))], { env: environment, stdio: ["pipe", "pipe", "pipe"] });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    const rootRequests = [];
    const toolResponses = [];
    let stderr = "";
    let finished = false;
    const finishError = (error) => {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      child.kill();
      reject(error);
    };
    const timeout = setTimeout(() => finishError(new Error(`Concurrent MCP roots exchange timed out: ${stderr}`)), 8_000);
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", finishError);
    lines.on("line", (line) => {
      try {
        const message = JSON.parse(line);
        if (message.id === 1) {
          for (const id of [2, 3]) {
            child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method: "tools/call", params: { name: "add_memory", arguments: { content: `保存${id}` } } })}\n`);
          }
          return;
        }
        if (message.method === "roots/list") {
          rootRequests.push(message.id);
          if (rootRequests.length === 1) {
            setTimeout(() => {
              try {
                assert.equal(rootRequests.length, 1, "parallel calls must share the pending roots request");
                child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/roots/list_changed" })}\n`);
                child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 4, method: "tools/call", params: { name: "add_memory", arguments: { content: "保存4" } } })}\n`);
              } catch (error) { finishError(error); }
            }, 50);
          } else if (rootRequests.length === 2) {
            child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: rootRequests[0], result: { roots: [{ uri: pathToFileURL(oldRoot).href }] } })}\n`);
            setTimeout(() => child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: rootRequests[1], result: { roots: [{ uri: pathToFileURL(newRoot).href }] } })}\n`), 25);
          } else {
            assert.fail("roots/list must run once per generation");
          }
          return;
        }
        if ([2, 3, 4].includes(message.id)) {
          toolResponses.push(message);
          assert.equal(message.result.isError, undefined, message.result.content?.[0]?.text);
          if (toolResponses.length === 3) {
            assert.equal(rootRequests.length, 2);
            assert.equal(documents.length, 3);
            assert.ok(documents.every((document) => document.containerTag === "memories"));
            assert.ok(documents.every((document) => document.metadata.sm_project_id === expectedTag));
            finished = true;
            clearTimeout(timeout);
            child.stdin.end();
            child.once("close", resolveTest);
          }
        }
      } catch (error) { finishError(error); }
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: { roots: { listChanged: true } } } })}\n`);
  });
});
