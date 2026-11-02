import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { tarExecutable } from "./lib/tar.mjs";

test("Windows archive operations use System32 tar regardless of PATH", () => {
  assert.equal(tarExecutable({ platform: "win32", systemRoot: "C:\\Windows" }), "C:\\Windows\\System32\\tar.exe");
  assert.equal(tarExecutable({ platform: "win32", systemRoot: "D:\\Windows" }), "D:\\Windows\\System32\\tar.exe");
  assert.throws(() => tarExecutable({ platform: "win32", systemRoot: "Windows" }), /must be absolute/u);
});

test("POSIX archive operations retain the system tar", () => {
  assert.equal(tarExecutable({ platform: "darwin" }), "tar");
  assert.equal(tarExecutable({ platform: "linux" }), "tar");
});

test("tar CLI propagates failures to the caller", () => {
  const helper = path.resolve(import.meta.dirname, "lib/tar.mjs");
  assert.throws(() => execFileSync(process.execPath, [helper, "--remnifold-invalid-option"], { stdio: "pipe" }), error => error.status !== 0);
});
