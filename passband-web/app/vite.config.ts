import { defineConfig, type Plugin } from "vite";

// The app's Content-Security-Policy. Production serves it as a response header
// from CloudFront (with frame-ancestors, which a <meta> cannot carry); the
// <meta> written here is the same policy for every other way the build is
// served (preview, e2e), so tests run under the policy that ships.
//
//   - script-src 'self': only our own bundled modules. No inline, no eval.
//   - require-trusted-types-for 'script' + one named policy: the mail frame's
//     srcdoc is the only HTML sink in the app, and it must go through
//     `passband-mail` (src/mail/trusted.ts).
//   - connect-src: hosted tenants only (plus the local daemon in e2e builds).
//   - img-src blob:/data:: remote mail images arrive as blob: URLs fetched
//     through the daemon's proxy; the browser never contacts a sender's host.
//   - frame-src 'self': the reading frame is srcdoc, and this also stops the
//     frame navigating itself.
export function csp(mode: string): string {
  const connect = ["https://*.passband.email"];
  if (mode === "e2e" || mode === "development") connect.push("http://127.0.0.1:*", "http://localhost:*");
  return [
    "default-src 'none'",
    "script-src 'self'",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self'",
    `connect-src ${connect.join(" ")}`,
    "frame-src 'self'",
    "base-uri 'none'",
    "form-action 'none'",
    "object-src 'none'",
    "trusted-types passband-mail",
    "require-trusted-types-for 'script'",
  ].join("; ");
}

function cspMeta(mode: string): Plugin {
  return {
    name: "passband-csp",
    apply: "build",
    transformIndexHtml(html) {
      return html.replace(
        "<head>",
        `<head>\n    <meta http-equiv="Content-Security-Policy" content="${csp(mode)}">`,
      );
    },
  };
}

export default defineConfig(({ mode }) => ({
  plugins: [cspMeta(mode)],
  define: {
    // Loopback daemons are allowed only in dev and e2e builds; production
    // compiles this to `false` and the branch disappears.
    __ALLOW_LOOPBACK__: JSON.stringify(mode === "e2e" || mode === "development" || mode === "test"),
  },
  build: {
    sourcemap: true,
    target: "es2022",
    // The test harnesses (mail frame, sitrep design) are extra pages in e2e
    // builds ONLY. A
    // production build has the app's one entry and nothing else.
    rollupOptions:
      mode === "e2e"
        ? {
            input: {
              main: "index.html",
              "mail-harness": "e2e/mail-harness.html",
              "sitrep-harness": "e2e/sitrep-harness.html",
            },
          }
        : undefined,
  },
  server: { port: 5190, strictPort: true },
  preview: { port: 5191, strictPort: true },
}));
