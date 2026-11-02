import { execFileSync } from "node:child_process";
import path from "node:path";
import { buildNativePackage, REPOSITORY_ROOT } from "./package-plugin.mjs";
import { NATIVE_TARGETS, nativeTarget } from "./native-distribution.mjs";

const args = process.argv.slice(2);
const options = {};
for (let i = 0; i < args.length; i++) {
  if (!["--target", "--binary", "--output", "--source-commit"].includes(args[i]) || !args[i + 1]) throw new Error("Expected --target, --binary, --output or --source-commit");
  options[args[i].slice(2)] = args[++i];
}
const target = options.target ?? Object.keys(NATIVE_TARGETS).find(key => NATIVE_TARGETS[key].platform === process.platform && NATIVE_TARGETS[key].arch === process.arch);
nativeTarget(target);
if (!options.binary) execFileSync("cargo", ["build", "--locked", "--release", "--manifest-path", path.join(REPOSITORY_ROOT, "native/Cargo.toml"), "--target", target], { stdio: "inherit" });
const result = await buildNativePackage({ target, binary: options.binary, output: options.output, sourceCommit: options["source-commit"] });
console.log(JSON.stringify(result));
