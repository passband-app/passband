// Static server for the spike page on its own origin (default 127.0.0.1:5185),
// plus an optional "elsewhere" server that records the headers it was sent,
// standing in for a host a sender chose.
import http from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "web");
const TYPES = { ".html": "text/html", ".js": "text/javascript" };
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
  "base64",
);

export function serveStatic(port = 5185) {
  const server = http.createServer(async (req, res) => {
    const path = normalize(new URL(req.url, "http://x").pathname).replace(/^\/+/, "") || "index.html";
    try {
      const body = await readFile(join(ROOT, path));
      res.writeHead(200, { "Content-Type": TYPES[extname(path)] || "application/octet-stream", "Cache-Control": "no-store" });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  return new Promise((r, j) => server.once("error", j).listen(port, "127.0.0.1", () => r(server)));
}

/** A host that serves a PNG to anyone and remembers every request's headers. */
export function serveElsewhere(port = 5186) {
  const seen = [];
  const server = http.createServer((req, res) => {
    seen.push({ url: req.url, headers: req.headers });
    res.writeHead(200, { "Content-Type": "image/png", "Access-Control-Allow-Origin": "*" });
    res.end(PNG);
  });
  return new Promise((r, j) => server.once("error", j).listen(port, "127.0.0.1", () => r({ server, seen })));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await serveStatic(Number(process.env.PORT) || 5185);
  console.log(`spike page on http://127.0.0.1:${Number(process.env.PORT) || 5185}/`);
}
