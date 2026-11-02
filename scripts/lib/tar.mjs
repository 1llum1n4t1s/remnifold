import { execFileSync } from "node:child_process";
import path from "node:path";
import { pathToFileURL } from "node:url";

// Git BashのGNU tarはD:\\...をremote archiveとして解釈するため、Windows標準版へ固定する。
export function tarExecutable({ platform = process.platform, systemRoot = process.env.SystemRoot ?? process.env.WINDIR } = {}) {
  if (platform !== "win32") return "tar";
  const root = systemRoot ?? "C:\\Windows";
  if (!path.win32.isAbsolute(root)) throw new Error("Windows SystemRoot must be absolute");
  return path.win32.join(root, "System32", "tar.exe");
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    execFileSync(tarExecutable(), process.argv.slice(2), { stdio: "inherit", windowsHide: true });
  } catch (error) {
    process.exitCode = Number.isInteger(error.status) ? error.status : 1;
    if (!Number.isInteger(error.status)) process.stderr.write(`${error.message}\n`);
  }
}
