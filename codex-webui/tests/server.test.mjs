import assert from "node:assert/strict";
import test from "node:test";
import { parseArgs } from "../server.mjs";

test("parses pnpm script argument separator before server flags", () => {
  const options = parseArgs(["--dev", "--", "--port", "4322", "--shell"]);
  assert.equal(options.dev, true);
  assert.equal(options.port, 4322);
  assert.equal(options.shell, true);
  assert.deepEqual(options.codexArgs, []);
});

test("uses separator before non-server flags as codex args", () => {
  const options = parseArgs(["--dev", "--", "--model", "gpt-test"]);
  assert.deepEqual(options.codexArgs, ["--model", "gpt-test"]);
});
