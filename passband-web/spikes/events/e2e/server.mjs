// Tiny static server for the spike page: a different origin from the daemon.
import http from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
const root = new URL("..", import.meta.url).pathname;
const types = { ".html": "text/html", ".js": "text/javascript" };
export function serve(port = 5174) {
  const srv = http.createServer(async (req, res) => {
    const path = normalize(new URL(req.url, "http://x").pathname === "/" ? "/e2e/page.html" : new URL(req.url, "http://x").pathname);
    if (path.includes("..")) return res.writeHead(400).end();
    try {
      const body = await readFile(join(root, path));
      res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" }).end(body);
    } catch { res.writeHead(404).end(); }
  });
  return new Promise((r) => srv.listen(port, "localhost", () => r(srv)));
}
