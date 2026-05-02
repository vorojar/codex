#!/usr/bin/env node
import { createServer } from "node:http";
import { createReadStream, existsSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import pty from "node-pty";
import { WebSocketServer } from "ws";
import { decodeClientFrame } from "./protocol.mjs";

const __filename = fileURLToPath(import.meta.url);
const packageRoot = dirname(__filename);
const repoRoot = resolve(packageRoot, "..");

const MIME_TYPES = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".wasm": "application/wasm",
};

const SERVER_FLAGS = new Set([
  "--dev",
  "--shell",
  "--host",
  "--port",
  "--cwd",
  "--codex-bin",
]);

export function parseArgs(argv) {
  const options = {
    dev: false,
    host: "127.0.0.1",
    port: 4321,
    cwd: repoRoot,
    codexBin: null,
    shell: false,
    codexArgs: [],
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--") {
      if (SERVER_FLAGS.has(argv[index + 1])) {
        continue;
      }
      options.codexArgs = argv.slice(index + 1);
      break;
    }
    if (arg === "--dev") {
      options.dev = true;
    } else if (arg === "--shell") {
      options.shell = true;
    } else if (arg === "--host") {
      options.host = requireValue(argv, (index += 1), arg);
    } else if (arg === "--port") {
      options.port = Number.parseInt(requireValue(argv, (index += 1), arg), 10);
      if (!Number.isFinite(options.port) || options.port <= 0) {
        throw new Error("--port must be a positive integer");
      }
    } else if (arg === "--cwd") {
      options.cwd = resolve(requireValue(argv, (index += 1), arg));
    } else if (arg === "--codex-bin") {
      options.codexBin = resolve(requireValue(argv, (index += 1), arg));
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return options;
}

function requireValue(argv, index, flag) {
  const value = argv[index];
  if (!value) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function commandFor(options) {
  if (options.shell) {
    return {
      command: process.env.SHELL || "/bin/sh",
      args: ["-l"],
    };
  }

  if (options.codexBin) {
    return {
      command: options.codexBin,
      args: options.codexArgs,
    };
  }

  const debugCodex = join(
    repoRoot,
    "codex-rs",
    "target",
    "debug",
    process.platform === "win32" ? "codex.exe" : "codex",
  );
  if (existsSync(debugCodex)) {
    return {
      command: debugCodex,
      args: options.codexArgs,
    };
  }

  return {
    command: "cargo",
    args: [
      "run",
      "--manifest-path",
      join(repoRoot, "codex-rs", "Cargo.toml"),
      "--bin",
      "codex",
      "--",
      ...options.codexArgs,
    ],
  };
}

function createPty(options) {
  const { command, args } = commandFor(options);
  return pty.spawn(command, args, {
    name: "xterm-256color",
    cols: 80,
    rows: 24,
    cwd: options.cwd,
    env: {
      ...process.env,
      TERM: "xterm-256color",
      COLORTERM: "truecolor",
      TERM_PROGRAM: "wterm",
      CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT: "1",
    },
  });
}

function isAllowedOrigin(request) {
  const origin = request.headers.origin;
  if (!origin) {
    return true;
  }

  try {
    return new URL(origin).host === request.headers.host;
  } catch {
    return false;
  }
}

function sendHttp(response, status, body, headers = {}) {
  response.writeHead(status, {
    "content-type": "text/plain; charset=utf-8",
    "cache-control": "no-store",
    ...headers,
  });
  response.end(body);
}

async function serveStatic(request, response) {
  const distRoot = join(packageRoot, "dist");
  const url = new URL(request.url ?? "/", "http://localhost");
  const pathname = decodeURIComponent(url.pathname);
  const relativePath = pathname === "/" ? "index.html" : pathname.slice(1);
  const resolvedPath = resolve(distRoot, relativePath);

  if (!resolvedPath.startsWith(`${distRoot}/`) && resolvedPath !== distRoot) {
    sendHttp(response, 403, "Forbidden");
    return;
  }

  const filePath = existsSync(resolvedPath)
    ? resolvedPath
    : join(distRoot, "index.html");
  const contentType =
    MIME_TYPES[extname(filePath)] || "application/octet-stream";
  response.writeHead(200, {
    "content-type": contentType,
    "cache-control": filePath.endsWith("index.html")
      ? "no-store"
      : "public, max-age=31536000, immutable",
  });
  createReadStream(filePath).pipe(response);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const server = createServer();
  const wss = new WebSocketServer({ noServer: true });

  if (options.dev) {
    const { createServer: createViteServer } = await import("vite");
    const vite = await createViteServer({
      root: packageRoot,
      server: { middlewareMode: true },
      appType: "spa",
    });
    server.on("request", (request, response) => {
      vite.middlewares(request, response, () => {
        sendHttp(response, 404, "Not found");
      });
    });
  } else {
    server.on("request", (request, response) => {
      if (request.url === "/healthz") {
        sendHttp(response, 200, "ok");
        return;
      }

      if (!existsSync(join(packageRoot, "dist", "index.html"))) {
        sendHttp(
          response,
          500,
          "Missing dist/. Run pnpm --filter @openai/codex-webui build first.",
        );
        return;
      }

      void serveStatic(request, response).catch((error) => {
        sendHttp(
          response,
          500,
          error instanceof Error ? error.message : String(error),
        );
      });
    });
  }

  server.on("upgrade", (request, socket, head) => {
    const url = new URL(request.url ?? "/", "http://localhost");
    if (url.pathname !== "/api/pty" || !isAllowedOrigin(request)) {
      socket.write("HTTP/1.1 403 Forbidden\r\n\r\n");
      socket.destroy();
      return;
    }

    wss.handleUpgrade(request, socket, head, (websocket) => {
      wss.emit("connection", websocket, request);
    });
  });

  wss.on("connection", (websocket) => {
    let child;
    try {
      child = createPty(options);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (websocket.readyState === websocket.OPEN) {
        websocket.send(Buffer.from(`Failed to start PTY: ${message}\r\n`));
        websocket.close();
      }
      return;
    }

    const output = child.onData((data) => {
      if (websocket.readyState === websocket.OPEN) {
        websocket.send(Buffer.from(data, "utf8"));
      }
    });

    child.onExit(({ exitCode, signal }) => {
      if (websocket.readyState === websocket.OPEN) {
        websocket.send(
          Buffer.from(
            `\r\n[process exited: ${signal ?? exitCode}]\r\n`,
            "utf8",
          ),
        );
        websocket.close();
      }
    });

    websocket.on("message", (data) => {
      const frame = decodeClientFrame(data);
      if (frame.type === "input") {
        child.write(frame.data);
      } else if (frame.type === "resize") {
        child.resize(frame.cols, frame.rows);
      }
    });

    websocket.on("close", () => {
      output.dispose();
      child.kill();
    });
  });

  await new Promise((resolveListen) => {
    server.listen(options.port, options.host, resolveListen);
  });

  const address = server.address();
  const port =
    typeof address === "object" && address ? address.port : options.port;
  console.log(`Codex web terminal listening on http://${options.host}:${port}`);
}

if (process.argv[1] === __filename) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}
