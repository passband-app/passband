// Three origins for the pairing spike:
//   app      http://localhost:5176   the web client (the only origin the daemon lists)
//   control  http://127.0.0.1:5177   stands in for signup.passband.app's redirect
//   evil     http://127.0.0.1:5178   any other page on the web
import http from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const DAEMON = process.env.SPIKE_DAEMON || "http://127.0.0.1:8851";

const csp =
  "default-src 'none'; script-src 'self' 'unsafe-inline'; " +
  `connect-src ${DAEMON}; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`;

function app(req, res) {
  const path = new URL(req.url, "http://x").pathname;
  if (path === "/connect.js") {
    return readFile(`${here}app/connect.js`).then((b) => {
      res.writeHead(200, { "content-type": "text/javascript", "content-security-policy": csp });
      res.end(b);
    });
  }
  if (path === "/" || path === "/connect") {
    return readFile(`${here}app/index.html`, "utf8").then((html) => {
      const cfg = `<script>window.SPIKE_CONTROL_URL="http://127.0.0.1:5177/app/auth";` +
        `window.SPIKE_EXTRA_SERVERS=[/^http:\\/\\/127\\.0\\.0\\.1:8851$/];</script>`;
      res.writeHead(200, {
        "content-type": "text/html",
        "content-security-policy": csp,
        "referrer-policy": "no-referrer",
      });
      res.end(html.replace("<head>", `<head>${cfg}`));
    });
  }
  res.writeHead(404).end();
}

// Control stub: the real one does Google sign-in and asks the warden for a
// code. Here the test passes the code in, and the stub only does the redirect.
function control(req, res) {
  const u = new URL(req.url, "http://x");
  if (u.pathname !== "/app/auth") return res.writeHead(404).end();
  const frag = new URLSearchParams({
    url: DAEMON,
    code: u.searchParams.get("test_code") ?? "",
    state: u.searchParams.get("state") ?? "",
  });
  res.writeHead(302, {
    location: `http://localhost:5176/connect#${frag}`,
    "referrer-policy": "no-referrer",
  });
  res.end();
}

function evil(req, res) {
  res.writeHead(200, { "content-type": "text/html" });
  res.end("<!doctype html><title>evil</title><p>a page on another origin</p>");
}

http.createServer(app).listen(5176, "localhost");
http.createServer(control).listen(5177, "127.0.0.1");
http.createServer(evil).listen(5178, "127.0.0.1");
console.log("pairing spike: app :5176, control :5177, evil :5178");
