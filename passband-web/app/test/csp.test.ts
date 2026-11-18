// The app's CSP is written twice: as a <meta> by the build (vite.config.ts)
// and as a response header by CloudFront (deploy/aws/tofu/main/app.tf). They
// must agree, apart from the directives only a header can carry.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";
import { csp } from "../vite.config";

const HEADER_ONLY = new Set(["frame-ancestors", "upgrade-insecure-requests"]);

function tofuPolicy(): string[] {
  const tf = readFileSync(
    fileURLToPath(new URL("../../../deploy/aws/tofu/main/app.tf", import.meta.url)),
    "utf8",
  );
  const block = /content_security_policy = join\("; ", \[([\s\S]*?)\]\)/.exec(tf)?.[1];
  if (!block) throw new Error("no content_security_policy in app.tf");
  return [...block.matchAll(/"([^"]+)"/g)].map((m) => m[1]!);
}

test("the shipped header and the build's meta policy agree", () => {
  const header = tofuPolicy();
  const meta = csp("production").split("; ");
  expect(header.filter((d) => !HEADER_ONLY.has(d.split(" ")[0]!)).sort()).toEqual([...meta].sort());
  expect(header).toContain("frame-ancestors 'none'");
});

test("production never allows a loopback connection", () => {
  expect(csp("production")).not.toMatch(/127\.0\.0\.1|localhost/);
  expect(csp("e2e")).toMatch(/127\.0\.0\.1/);
});
