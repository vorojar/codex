import assert from "node:assert/strict";
import test from "node:test";
import { decodeClientFrame, INPUT_FRAME, RESIZE_FRAME } from "../protocol.mjs";

test("decodes input frames", () => {
  const frame = Buffer.concat([
    Buffer.from([INPUT_FRAME]),
    Buffer.from("hello"),
  ]);
  assert.deepEqual(decodeClientFrame(frame), { type: "input", data: "hello" });
});

test("decodes resize frames", () => {
  const frame = Buffer.alloc(5);
  frame.writeUInt8(RESIZE_FRAME, 0);
  frame.writeUInt16BE(120, 1);
  frame.writeUInt16BE(40, 3);

  assert.deepEqual(decodeClientFrame(frame), {
    type: "resize",
    cols: 120,
    rows: 40,
  });
});

test("rejects malformed resize frames", () => {
  assert.deepEqual(decodeClientFrame(Buffer.from([RESIZE_FRAME, 0, 80])), {
    type: "invalid",
    reason: "malformed resize frame",
  });
});

test("rejects zero resize dimensions", () => {
  const frame = Buffer.alloc(5);
  frame.writeUInt8(RESIZE_FRAME, 0);
  frame.writeUInt16BE(0, 1);
  frame.writeUInt16BE(24, 3);

  assert.deepEqual(decodeClientFrame(frame), {
    type: "invalid",
    reason: "resize dimensions must be positive",
  });
});
