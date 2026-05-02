import { WTerm } from "@wterm/dom";
import { GhosttyCore } from "@wterm/ghostty";
import "./styles.css";

const INPUT_FRAME = 0x00;
const RESIZE_FRAME = 0x01;

function requireElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing ${id} element`);
  }
  return element;
}

const terminalElement = requireElement("terminal");
const statusElement = requireElement("status");
const encoder = new TextEncoder();
let socket: WebSocket | null = null;
const pendingFrames: Uint8Array[] = [];

function showStatus(message: string): void {
  statusElement.textContent = message;
  statusElement.classList.remove("hidden");
}

function clearStatus(): void {
  statusElement.textContent = "";
  statusElement.classList.add("hidden");
}

function encodeInput(data: string): Uint8Array {
  const encoded = encoder.encode(data);
  const frame = new Uint8Array(1 + encoded.length);
  frame[0] = INPUT_FRAME;
  frame.set(encoded, 1);
  return frame;
}

function encodeResize(cols: number, rows: number): Uint8Array {
  const frame = new Uint8Array(5);
  const view = new DataView(frame.buffer);
  view.setUint8(0, RESIZE_FRAME);
  view.setUint16(1, cols, false);
  view.setUint16(3, rows, false);
  return frame;
}

function sendFrame(frame: Uint8Array): void {
  const payload = frame.buffer.slice(
    frame.byteOffset,
    frame.byteOffset + frame.byteLength,
  ) as ArrayBuffer;
  if (socket?.readyState === WebSocket.OPEN) {
    socket.send(payload);
    return;
  }
  pendingFrames.push(new Uint8Array(payload));
}

function flushPendingFrames(): void {
  while (socket?.readyState === WebSocket.OPEN && pendingFrames.length > 0) {
    const frame = pendingFrames.shift();
    if (frame) {
      const payload = frame.buffer.slice(
        frame.byteOffset,
        frame.byteOffset + frame.byteLength,
      ) as ArrayBuffer;
      socket.send(payload);
    }
  }
}

function websocketUrl(): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/api/pty`;
}

async function main(): Promise<void> {
  const core = await GhosttyCore.load({ scrollbackLimit: 10000 });
  const term = new WTerm(terminalElement, {
    core,
    autoResize: true,
    cursorBlink: false,
    onData(data) {
      sendFrame(encodeInput(data));
    },
    onResize(cols, rows) {
      sendFrame(encodeResize(cols, rows));
    },
    onTitle(title) {
      document.title = title
        ? `${title} - Codex Web Terminal`
        : "Codex Web Terminal";
    },
  });

  socket = new WebSocket(websocketUrl());
  socket.binaryType = "arraybuffer";

  socket.addEventListener("open", () => {
    clearStatus();
    flushPendingFrames();
    term.focus();
  });

  socket.addEventListener("message", (event) => {
    if (event.data instanceof ArrayBuffer) {
      term.write(new Uint8Array(event.data));
      return;
    }

    if (event.data instanceof Blob) {
      void event.data.arrayBuffer().then((buffer) => {
        term.write(new Uint8Array(buffer));
      });
      return;
    }

    term.write(String(event.data));
  });

  socket.addEventListener("close", () => {
    showStatus("Terminal session disconnected.");
  });

  socket.addEventListener("error", () => {
    showStatus("Terminal connection failed.");
  });

  await term.init();
  term.focus();
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.message : String(error);
  showStatus(`Failed to start terminal: ${message}`);
});
