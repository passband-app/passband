// Steps film.html one frame at a time in the system Chrome and screenshots
// each. Playwright rather than a screen recorder: the film runs on a virtual
// clock, so a frame that takes 200ms to shoot still lands exactly 1/30s after
// the last one and the video never drops or stutters.
//
//   bun run render.ts <dir> video <out.mp4> [audio.wav]
//   bun run render.ts <dir> stills <outdir> 3.5 12 20.25 ...
import { chromium } from "playwright-core";
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { DURATION, FPS } from "./timeline";

const [dir, mode, out, ...rest] = process.argv.slice(2);
const server = Bun.serve({
  port: 0,
  fetch: (req) => {
    const file = Bun.file(`${dir}${new URL(req.url).pathname}`);
    return file.size ? new Response(file) : new Response(null, { status: 404 });
  },
});
const browser = await chromium.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
page.on("pageerror", (e) => console.error("page error:", e.message));
await page.goto(`http://localhost:${server.port}/film.html`);
await page.waitForFunction(() => typeof (window as any).boot === "function");
await page.evaluate(() => (window as any).boot());

const total = Math.round(DURATION * FPS);

if (mode === "stills") {
  mkdirSync(out, { recursive: true });
  const want = new Map(rest.map((s) => [Math.round(Number(s) * FPS), s]));
  const last = Math.max(...want.keys());
  for (let f = 0; f <= last; f++) {
    await page.evaluate((f) => (window as any).seek(f), f);
    if (want.has(f)) await page.screenshot({ path: `${out}/t${want.get(f)}.png` });
  }
} else {
  const [audio] = rest;
  const args = ["-y", "-loglevel", "error", "-f", "image2pipe", "-framerate", String(FPS), "-c:v", "mjpeg", "-i", "-"];
  if (audio) args.push("-i", audio);
  args.push("-c:v", "libx264", "-preset", "slow", "-crf", "16", "-pix_fmt", "yuv420p", "-tune", "film",
    "-movflags", "+faststart");
  // The score arrives already at its loudness (build.sh): encode only, so no
  // limiter ever touches it.
  if (audio) args.push("-c:a", "aac", "-b:a", "256k", "-shortest");
  args.push(out);
  const ff = spawn("ffmpeg", args, { stdio: ["pipe", "inherit", "inherit"] });
  const started = Date.now();
  for (let f = 0; f < total; f++) {
    await page.evaluate((f) => (window as any).seek(f), f);
    const jpg = await page.screenshot({ type: "jpeg", quality: 95 });
    if (!ff.stdin.write(jpg)) await new Promise((r) => ff.stdin.once("drain", r));
    if (f % 150 === 0) console.error(`frame ${f}/${total} (${((Date.now() - started) / 1000).toFixed(0)}s)`);
  }
  ff.stdin.end();
  await new Promise((r) => ff.on("close", r));
}
await browser.close();
server.stop();
