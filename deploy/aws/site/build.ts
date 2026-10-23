#!/usr/bin/env bun
// Builds the landing site for S3 + CloudFront (deploy/aws/DESIGN.md, "landing
// site"). Railway keeps building passband-site/Dockerfile until cutover; this
// script does not touch that path, it only runs the same `bun run build`.
//
//   bun deploy/aws/site/build.ts [--skip-site-build] [--out <dir>]
//
// Produces, under deploy/aws/site/out/ by default:
//   site/          exactly the files server.ts serves, at their own paths
//   objects.tsv    key \t content-type \t cache-control  (one per object;
//                  the deploy uploads each with these headers, since S3
//                  would otherwise guess, and guess woff2 wrong)
// and rewrites deploy/aws/site/function.js with the newest appcast version
// baked in.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "fs";
import { dirname, join, resolve } from "path";
import {
  latestVersion,
  objectSpecs,
  parsePages,
  parseReleasesRepo,
  renderFunction,
  type ObjectSpec,
} from "./lib";

const HERE = import.meta.dir;
export const SITE_DIR = resolve(HERE, "../../../passband-site");
export const FUNCTION_PATH = join(HERE, "function.js");

export type BuildResult = {
  outDir: string;
  specs: ObjectSpec[];
  latest: string | null;
  functionSource: string;
};

function run(cmd: string[], cwd: string) {
  const p = Bun.spawnSync(cmd, { cwd, stdout: "inherit", stderr: "inherit" });
  if (p.exitCode !== 0) throw new Error(`${cmd.join(" ")} failed (${p.exitCode})`);
}

export function build(opts: { outDir: string; siteBuild: boolean; writeFunction: boolean }): BuildResult {
  if (opts.siteBuild) {
    run(["bun", "install", "--frozen-lockfile"], SITE_DIR);
    run(["bun", "run", "build"], SITE_DIR);
  }

  const serverSrc = readFileSync(join(SITE_DIR, "server.ts"), "utf8");
  const pages = parsePages(serverSrc);
  const releasesRepo = parseReleasesRepo(serverSrc);
  const appcastPath = join(SITE_DIR, "appcast.xml");
  const latest = latestVersion(existsSync(appcastPath) ? readFileSync(appcastPath, "utf8") : null);
  const specs = objectSpecs(pages);

  const siteOut = join(opts.outDir, "site");
  rmSync(opts.outDir, { recursive: true, force: true });
  mkdirSync(siteOut, { recursive: true });
  for (const s of specs) {
    const src = join(SITE_DIR, s.source);
    if (!existsSync(src)) throw new Error(`server.ts serves ${s.source} but it does not exist (did the site build run?)`);
    const dst = join(siteOut, s.key);
    mkdirSync(dirname(dst), { recursive: true });
    cpSync(src, dst);
  }
  writeFileSync(
    join(opts.outDir, "objects.tsv"),
    specs.map((s) => [s.key, s.contentType, s.cacheControl ?? ""].join("\t")).join("\n") + "\n",
  );

  const functionSource = renderFunction({ pages, latest, releasesRepo });
  if (opts.writeFunction) writeFileSync(FUNCTION_PATH, functionSource);
  return { outDir: opts.outDir, specs, latest, functionSource };
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const outIdx = args.indexOf("--out");
  const outDir = outIdx >= 0 ? resolve(args[outIdx + 1]) : join(HERE, "out");
  const r = build({ outDir, siteBuild: !args.includes("--skip-site-build"), writeFunction: true });
  console.log(`site: ${r.specs.length} objects in ${join(r.outDir, "site")}`);
  console.log(`site: function.js bakes latest version ${r.latest ?? "(none: /download goes to the releases page)"}`);
}
