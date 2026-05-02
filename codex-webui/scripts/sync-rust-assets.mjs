#!/usr/bin/env node
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const packageRoot = resolve(dirname(__filename), "..");
const repoRoot = resolve(packageRoot, "..");
const distRoot = join(packageRoot, "dist");
const rustAssetsRoot = join(repoRoot, "codex-rs", "web-server", "assets");
const check = process.argv.includes("--check");

if (!existsSync(join(distRoot, "index.html"))) {
  throw new Error("Missing dist/index.html. Run the codex-webui build first.");
}

if (check) {
  const diff = diffTrees(distRoot, rustAssetsRoot);
  if (diff.length > 0) {
    throw new Error(
      `codex web assets are out of date:\n${diff.map((item) => `  ${item}`).join("\n")}`,
    );
  }
  console.log("codex web Rust assets are up to date.");
} else {
  rmSync(rustAssetsRoot, { force: true, recursive: true });
  mkdirSync(rustAssetsRoot, { recursive: true });
  cpSync(distRoot, rustAssetsRoot, { recursive: true });
  console.log(
    `Synced ${relative(repoRoot, distRoot)} to ${relative(repoRoot, rustAssetsRoot)}.`,
  );
}

function diffTrees(leftRoot, rightRoot) {
  const leftFiles = listFiles(leftRoot);
  const rightFiles = listFiles(rightRoot);
  const allFiles = new Set([...leftFiles.keys(), ...rightFiles.keys()]);
  const diff = [];

  for (const file of [...allFiles].sort()) {
    const left = leftFiles.get(file);
    const right = rightFiles.get(file);
    if (!left) {
      diff.push(`unexpected ${file}`);
    } else if (!right) {
      diff.push(`missing ${file}`);
    } else if (left !== right) {
      diff.push(`changed ${file}`);
    }
  }

  return diff;
}

function listFiles(root) {
  const files = new Map();
  if (!existsSync(root)) {
    return files;
  }
  walk(root, root, files);
  return files;
}

function walk(root, dir, files) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(root, path, files);
    } else if (entry.isFile()) {
      files.set(relative(root, path), sha256(path));
    }
  }
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}
