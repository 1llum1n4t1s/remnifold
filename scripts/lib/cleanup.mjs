import * as cleanupFs from "node:fs";
import * as cleanupPath from "node:path";

// 許可rootの境界とリンクを検査してから標準APIへ渡す。
export function assertCleanupPath(target, allowedRoot) {
  if (typeof target !== 'string' || !target || typeof allowedRoot !== 'string' || !allowedRoot || !cleanupPath.isAbsolute(target) || !cleanupPath.isAbsolute(allowedRoot)) throw new Error('Cleanup paths must be absolute');
  const resolved = cleanupPath.resolve(target);
  const relative = cleanupPath.relative(cleanupPath.resolve(allowedRoot), resolved);
  if (!relative || relative === '..' || relative.startsWith('..' + cleanupPath.sep) || cleanupPath.isAbsolute(relative)) throw new Error('Cleanup target is outside the allowed root');
  for (let current = resolved; ; current = cleanupPath.dirname(current)) {
    try { if (cleanupFs.lstatSync(current).isSymbolicLink()) throw new Error('Cleanup path contains a link'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (cleanupPath.dirname(current) === current) break;
  }
  const pending = [resolved];
  while (pending.length) {
    const current = pending.pop();
    let stat;
    try { stat = cleanupFs.lstatSync(current); } catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (stat.isSymbolicLink()) throw new Error('Cleanup directory contains a link');
    if (stat.isDirectory()) for (const entry of cleanupFs.readdirSync(current)) pending.push(cleanupPath.join(current, entry));
  }
  return resolved;
}
