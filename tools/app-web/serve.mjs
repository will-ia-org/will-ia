#!/usr/bin/env node
// Servidor de teste do app web do will-ia (docs/33): entrega dist/app-web e
// repassa /api (HTTP e WebSocket) ao servidor will-ia desta máquina. A página e
// a API ficam no mesmo endereço, como ficarão quando o próprio servidor
// entregar a página. Escuta só em loopback: para fora, use `tailscale serve`.
//
//   node tools/app-web/serve.mjs [--port 8891] [--api http://127.0.0.1:47300]
import http from "node:http";
import net from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const arg = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : def;
};
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../dist/app-web");
const port = Number(arg("--port", "8891"));
const api = new URL(arg("--api", "http://127.0.0.1:47300"));

const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".png": "image/png",
  ".webmanifest": "application/manifest+json",
};

// Token local do servidor (o mesmo que o app de desktop usa em "Esta máquina").
function localToken() {
  const dir =
    process.env.WILL_IA_DATA_DIR ||
    (process.platform === "darwin"
      ? path.join(os.homedir(), "Library/Application Support/dev.will.will-ia")
      : process.platform === "win32"
        ? path.join(process.env.APPDATA || "", "will", "will-ia", "data")
        : path.join(os.homedir(), ".local/share/will-ia"));
  try {
    return fs.readFileSync(path.join(dir, "ui-session.token"), "utf8").trim();
  } catch {
    return "";
  }
}

// A página foi pedida por `localhost` nesta máquina, sem passar por um proxy
// (tailscale serve e afins mandam outro Host e cabeçalhos de encaminhamento).
// Só nesse caso ela recebe o token local e entra direto em "Esta máquina".
function isLocalVisit(req) {
  const host = (req.headers.host || "").replace(/:\d+$/, "");
  const addr = req.socket.remoteAddress || "";
  const loopback = addr === "127.0.0.1" || addr === "::1" || addr === "::ffff:127.0.0.1";
  const forwarded = Object.keys(req.headers).some(
    (h) => h.startsWith("x-forwarded-") || h.startsWith("tailscale-") || h === "forwarded",
  );
  return loopback && !forwarded && (host === "localhost" || host === "127.0.0.1");
}

function serveIndex(req, res) {
  let html = fs.readFileSync(path.join(root, "index.html"), "utf8");
  const token = isLocalVisit(req) ? localToken() : "";
  if (token) {
    html = html.replace(
      "<script type=\"module\">",
      `<script>window.__WILL_IA_LOCAL_TOKEN__ = ${JSON.stringify(token)};</script>\n<script type="module">`,
    );
  }
  res.writeHead(200, { "content-type": types[".html"], "cache-control": "no-store" });
  res.end(html);
}

function serveStatic(req, res) {
  let rel = decodeURIComponent(new URL(req.url, "http://x").pathname);
  if (rel === "/" || rel === "/index.html") return serveIndex(req, res);
  const file = path.join(root, rel);
  if (!file.startsWith(root + path.sep) || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
    res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    res.end("não encontrado");
    return;
  }
  const headers = {
    "content-type": types[path.extname(file)] || "application/octet-stream",
    // Sem cache: cada compilação nova chega na hora (inclusive no iPad).
    "cache-control": "no-cache",
  };
  const gz = file + ".gz";
  if (/\bgzip\b/.test(req.headers["accept-encoding"] || "") && fs.existsSync(gz)) {
    headers["content-encoding"] = "gzip";
    res.writeHead(200, headers);
    fs.createReadStream(gz).pipe(res);
    return;
  }
  res.writeHead(200, headers);
  fs.createReadStream(file).pipe(res);
}

function upstreamHeaders(req) {
  const h = { ...req.headers, host: api.host };
  // Para o servidor, o pedido vem do próprio proxy (mesma origem).
  delete h.origin;
  delete h.referer;
  return h;
}

const server = http.createServer((req, res) => {
  if (!req.url.startsWith("/api/")) return serveStatic(req, res);
  const up = http.request(
    { host: api.hostname, port: api.port, method: req.method, path: req.url, headers: upstreamHeaders(req) },
    (r) => {
      res.writeHead(r.statusCode, r.headers);
      r.pipe(res);
    },
  );
  up.on("error", (e) => {
    res.writeHead(502, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: `servidor will-ia fora do ar: ${e.message}` }));
  });
  req.pipe(up);
});

// WebSocket (/api/events, /api/voice/audio): repassa o upgrade byte a byte.
server.on("upgrade", (req, socket, head) => {
  if (!req.url.startsWith("/api/")) return socket.destroy();
  const up = net.connect(Number(api.port), api.hostname, () => {
    const h = upstreamHeaders(req);
    let raw = `${req.method} ${req.url} HTTP/1.1\r\n`;
    for (const [k, v] of Object.entries(h)) raw += `${k}: ${v}\r\n`;
    up.write(raw + "\r\n");
    if (head?.length) up.write(head);
    socket.pipe(up).pipe(socket);
  });
  up.on("error", () => socket.destroy());
  socket.on("error", () => up.destroy());
});

server.listen(port, "127.0.0.1", () => {
  console.log(`will-ia web em http://localhost:${port}  →  API ${api.origin}`);
});
