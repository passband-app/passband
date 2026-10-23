// Differential test: the CloudFront Function + uploaded objects must answer
// every route the way passband-site/server.ts does. Not a hand-written table
// of expected outputs: it boots the real server.ts under Bun and compares
// status, Location, body bytes, Content-Type and Cache-Control route by route.
//
//   bun test deploy/aws/site
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { copyFileSync, mkdtempSync, readFileSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { build, FUNCTION_PATH, SITE_DIR, type BuildResult } from "./build";
import { APPCAST_CACHE_CONTROL, latestVersion, parsePages, parseReleasesRepo, renderFunction } from "./lib";

type CfResult =
  | { statusCode: number; headers: Record<string, { value: string }> }
  | { uri: string };

function loadHandler(src: string): (event: unknown) => CfResult {
  // The runtime has no modules: the file must define a global `handler`.
  return new Function(`${src}\nreturn handler;`)() as (event: unknown) => CfResult;
}

function cfRequest(handler: (e: unknown) => CfResult, rawPath: string): CfResult {
  // CloudFront hands the function the path without the query string.
  const [uri, qs = ""] = rawPath.split("?");
  return handler({
    version: "1.0",
    context: { eventType: "viewer-request" },
    viewer: { ip: "192.0.2.1" },
    request: { method: "GET", uri, querystring: qs ? { q: { value: qs } } : {}, headers: {}, cookies: {} },
  });
}

async function startServer(cwd: string): Promise<{ base: string; stop: () => void }> {
  const proc = Bun.spawn(["bun", "run", join(cwd, "server.ts")], {
    cwd,
    env: { ...process.env, PORT: "0" },
    stdout: "pipe",
    stderr: "inherit",
  });
  const reader = proc.stdout.getReader();
  let buf = "";
  while (!/listening on (\S+)/.test(buf)) {
    const { value, done } = await reader.read();
    if (done) throw new Error(`server.ts exited before listening: ${buf}`);
    buf += new TextDecoder().decode(value);
  }
  const base = buf.match(/listening on (\S+)/)![1].replace(/\/$/, "");
  return { base, stop: () => proc.kill() };
}

// Every mapped page, plus the edges.
const EDGE_ROUTES = [
  "/",
  "/index.html",
  "/unknown",
  "/about/", // trailing slash is NOT /about
  "/about.html", // the object key itself is not a route
  "/dist/main.js",
  "/About",
  "/%61bout", // percent-encoded: not decoded by either side
  "/main.js?v=1", // query string ignored
  "/download",
  "/download/",
  "/download/latest",
  "/download/latest/",
  "/download/Passband-0.0.14.zip",
  "/download/Passband-1.2.3.zip",
  "/download/Passband-10.20.30.zip",
  "/download/Passband-1.2.zip", // malformed: index.html
  "/download/Passband-1.2.3.4.zip",
  "/download/Passband-v1.2.3.zip",
  "/download/passband-1.2.3.zip",
  "/download/Passband-1.2.3.zip/",
  "/download/Passband-1.2.3.dmg",
  "/download/x/Passband-1.2.3.zip",
  "/downloads",
  "/DOWNLOAD",
];

let built: BuildResult;
let server: { base: string; stop: () => void };
let handler: (e: unknown) => CfResult;
const serverSrc = readFileSync(join(SITE_DIR, "server.ts"), "utf8");
const pages = parsePages(serverSrc);

beforeAll(async () => {
  const out = mkdtempSync(join(tmpdir(), "passband-site-"));
  // Runs the real site build (bun install + bun run build) so server.ts has
  // dist/main.js to serve, and assembles the upload dir from it. Does not
  // rewrite the committed function.js: the test exercises what it renders.
  built = build({ outDir: out, siteBuild: true, writeFunction: false });
  handler = loadHandler(built.functionSource);
  server = await startServer(SITE_DIR);
}, 120_000);

afterAll(() => server?.stop());

describe("CloudFront Function matches server.ts", () => {
  const routes = [...Object.keys(pages), ...EDGE_ROUTES];
  for (const route of routes) {
    test(route, async () => {
      const res = await fetch(server.base + route, { redirect: "manual" });
      const cf = cfRequest(handler, route);

      if ("statusCode" in cf) {
        expect(cf.statusCode).toBe(res.status);
        expect(cf.headers.location.value).toBe(res.headers.get("location")!);
        return;
      }
      expect(res.status).toBe(200);
      const key = cf.uri.slice(1);
      const spec = built.specs.find((s) => s.key === key);
      expect(spec).toBeDefined();
      const served = new Uint8Array(await res.arrayBuffer());
      const uploaded = new Uint8Array(readFileSync(join(built.outDir, "site", key)));
      expect(Buffer.compare(served, uploaded)).toBe(0);
      expect(spec!.contentType).toBe(res.headers.get("content-type")!);
      if (key === "appcast.xml") {
        // Deliberate difference (DESIGN.md): short-cached at the edge.
        expect(res.headers.get("cache-control")).toBeNull();
        expect(spec!.cacheControl).toBe(APPCAST_CACHE_CONTROL);
        expect(spec!.contentType).toBe("application/xml");
      } else {
        expect(spec!.cacheControl).toBe(res.headers.get("cache-control"));
      }
    });
  }

  test("woff2 keeps its week-long Cache-Control", () => {
    const woff = built.specs.find((s) => s.key.endsWith(".woff2"))!;
    expect(woff.cacheControl).toBe("public, max-age=604800");
  });

  test("upload set is exactly server.ts's files plus index.html", () => {
    expect(built.specs.map((s) => s.key).sort()).toEqual(
      [...new Set([...Object.values(pages), "index.html"])].sort(),
    );
  });
});

describe("no released version", () => {
  let bare: { base: string; stop: () => void };
  let noVersion: (e: unknown) => CfResult;
  beforeAll(async () => {
    // server.ts with no appcast.xml beside it: its latestVersion() is null.
    const dir = mkdtempSync(join(tmpdir(), "passband-site-bare-"));
    copyFileSync(join(SITE_DIR, "server.ts"), join(dir, "server.ts"));
    copyFileSync(join(SITE_DIR, "index.html"), join(dir, "index.html"));
    bare = await startServer(dir);
    noVersion = loadHandler(
      renderFunction({ pages, latest: latestVersion(null), releasesRepo: parseReleasesRepo(serverSrc) }),
    );
  });
  afterAll(() => bare?.stop());

  for (const route of ["/download", "/download/latest", "/download/Passband-1.2.3.zip"]) {
    test(route, async () => {
      const res = await fetch(bare.base + route, { redirect: "manual" });
      const cf = cfRequest(noVersion, route) as { statusCode: number; headers: Record<string, { value: string }> };
      expect(cf.statusCode).toBe(res.status);
      expect(cf.headers.location.value).toBe(res.headers.get("location")!);
    });
  }

  test("an appcast with no x.y.z version reads as none, like server.ts", () => {
    expect(latestVersion("<sparkle:shortVersionString>1.2</sparkle:shortVersionString>")).toBeNull();
    expect(latestVersion("<rss></rss>")).toBeNull();
  });
});

test("committed function.js is valid for the runtime shape", () => {
  // Whatever version it bakes, the committed file must load and route.
  const h = loadHandler(readFileSync(FUNCTION_PATH, "utf8"));
  const r = cfRequest(h, "/nope") as { uri: string };
  expect(r.uri).toBe("/index.html");
});
