export const INPUT_FRAME = 0x00;
export const RESIZE_FRAME = 0x01;

export function decodeClientFrame(frame) {
  const data = Buffer.isBuffer(frame) ? frame : Buffer.from(frame);
  if (data.length === 0) {
    return { type: "invalid", reason: "empty frame" };
  }

  const kind = data.readUInt8(0);
  if (kind === INPUT_FRAME) {
    return { type: "input", data: data.subarray(1).toString("utf8") };
  }

  if (kind === RESIZE_FRAME) {
    if (data.length !== 5) {
      return { type: "invalid", reason: "malformed resize frame" };
    }
    const cols = data.readUInt16BE(1);
    const rows = data.readUInt16BE(3);
    if (cols === 0 || rows === 0) {
      return { type: "invalid", reason: "resize dimensions must be positive" };
    }
    return { type: "resize", cols, rows };
  }

  return { type: "invalid", reason: `unknown frame type ${kind}` };
}
