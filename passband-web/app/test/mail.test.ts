// The string-level rules of the reading frame (no DOM needed).
// Tracker and quote rules mirror Lib/Trackers.swift and Lib/Quotes.swift.
import { describe, expect, test } from "vitest";
import { capturedLink, isAttribution } from "../src/mail/frame";
import { Limiter, cidIndex } from "../src/mail/images";
import {
  MAIL_CSP,
  absoluteRemote,
  escapeHtml,
  frameHead,
  isNetworkSrc,
  isTracker,
  linkTarget,
  normalizeCid,
  pxDim,
  remoteToken,
  rewriteCssUrls,
  stripFetchingAtRules,
  styleProp,
} from "../src/mail/prepare";

describe("the frame policy", () => {
  test("no script, no fetch except inline and blob images, base and forms pinned", () => {
    expect(MAIL_CSP).toBe(
      "default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; base-uri 'none'; form-action 'none'",
    );
    expect(MAIL_CSP).not.toMatch(/script-src|https?:/);
  });
  test("the CSP meta is the very first thing in <head>", () => {
    expect(frameHead().startsWith(`<meta http-equiv="Content-Security-Policy" content="${MAIL_CSP}">`)).toBe(true);
  });
  test("a stylesheet cannot close its own element", () => {
    const head = frameHead(["a{}</style><img src=x>", "b{}</STYLE ><p>"]);
    expect(head).not.toMatch(/<\/style><img/i);
    expect(head).not.toMatch(/<\/STYLE ><p>/);
    expect(head.match(/<\/style>/g)).toHaveLength(3); // ours + the two mail sheets
  });
});

describe("where a link may open on its own (Safari's path)", () => {
  test("http(s) only, normalized", () => {
    expect(linkTarget(" https://links.example/a ")).toBe("https://links.example/a");
    expect(linkTarget("http://LINKS.example")).toBe("http://links.example/");
  });
  test("never another scheme, a relative path or credentials", () => {
    for (const h of [
      "javascript:alert(1)",
      "data:text/html,x",
      "ftp://files.example/x",
      "mailto:dee@mail.example",
      "/relative",
      "#top",
      "https://user:pw@links.example/",
      "https://user@links.example/",
      "",
    ]) {
      expect(linkTarget(h)).toBeNull();
    }
  });
});

describe("remote URLs", () => {
  test("isNetworkSrc", () => {
    for (const s of ["http://a.example/x", "https://a.example/x", "//a.example/x", "  HTTPS://A.EXAMPLE "]) {
      expect(isNetworkSrc(s), s).toBe(true);
    }
    for (const s of ["cid:logo", "data:image/png;base64,x", "/relative", "x.png", "blob:x", ""]) {
      expect(isNetworkSrc(s), s).toBe(false);
    }
  });
  test("protocol-relative becomes https", () => {
    expect(absoluteRemote("//cdn.example/a.png")).toBe("https://cdn.example/a.png");
    expect(absoluteRemote("http://cdn.example/a.png")).toBe("http://cdn.example/a.png");
  });
});

describe("rewriteCssUrls", () => {
  test("remote urls become numbered inert tokens and are recorded", () => {
    const remotes: string[] = [];
    const out = rewriteCssUrls(
      "a{background:url('https://img.example/a.png')} b{background:url(//img.example/b.png)}",
      remotes,
    );
    expect(remotes).toEqual(["https://img.example/a.png", "https://img.example/b.png"]);
    expect(out).toBe(`a{background:url("${remoteToken(0)}")} b{background:url("${remoteToken(1)}")}`);
    expect(out).not.toContain("img.example");
  });
  test("inline data images stay, everything else becomes none", () => {
    const remotes: string[] = [];
    expect(rewriteCssUrls("a{background:url(data:image/png;base64,AAAA)}", remotes)).toBe(
      'a{background:url("data:image/png;base64,AAAA")}',
    );
    expect(rewriteCssUrls("a{background:url(cid:x)} b{background:url(/rel.png)}", remotes)).toBe(
      "a{background:none} b{background:none}",
    );
    expect(remotes).toEqual([]);
  });
  test("tokens are inert data URLs that no network load can come from", () => {
    expect(remoteToken(3)).toBe("data:,pb-remote-3");
  });
});

describe("stripFetchingAtRules", () => {
  test("@import and @font-face go", () => {
    const css =
      "@import url(https://evil.example/a.css);@import 'https://evil.example/b.css';" +
      "@font-face{font-family:x;src:url(https://evil.example/f.woff2)} p{color:red}";
    const out = stripFetchingAtRules(css);
    expect(out).not.toContain("evil.example");
    expect(out).toContain("p{color:red}");
  });
});

describe("isTracker (Lib/Trackers.swift)", () => {
  test("tiny declared size", () => {
    expect(isTracker({ src: "https://a.example/p.gif", width: "1", height: "1" })).toBe(true);
    expect(isTracker({ src: "https://a.example/p.gif", style: "width:2px;height:2px" })).toBe(true);
    expect(isTracker({ src: "https://a.example/p.gif", width: "1" })).toBe(false);
  });
  test("CSS-hidden", () => {
    expect(isTracker({ src: "https://a.example/p.gif", style: "display: none" })).toBe(true);
    expect(isTracker({ src: "https://a.example/p.gif", style: "visibility:hidden" })).toBe(true);
  });
  test("a known endpoint, at any size", () => {
    expect(isTracker({ src: "https://us1.list-manage.com/track/open.php?u=1" })).toBe(true);
    expect(isTracker({ src: "https://ct.sendgrid.net/wf/open?upn=x", width: "600", height: "300" })).toBe(true);
  });
  test("an ordinary image is kept", () => {
    expect(isTracker({ src: "https://images.example/hero.png", width: "600", height: "300" })).toBe(false);
  });
  test("pxDim and styleProp", () => {
    expect(pxDim("12px")).toBe(12);
    expect(pxDim(" 1.5 ")).toBe(1.5);
    expect(pxDim("50%")).toBeNull();
    expect(styleProp("color:red; WIDTH : 3px", "width")).toBe("3px");
  });
});

describe("cid matching", () => {
  test("content ids compare without brackets, scheme or case", () => {
    expect(normalizeCid("cid:Logo1")).toBe("logo1");
    expect(normalizeCid("<logo1>")).toBe("logo1");
    const idx = cidIndex([
      { id: 7, filename: "a.png", mime: "image/png", size: 1, content_id: "<Logo1>" },
      { id: 8, filename: "b.pdf", mime: "application/pdf", size: 1 },
    ]);
    expect(idx.get(normalizeCid("cid:LOGO1"))).toBe(7);
    expect(idx.size).toBe(1);
  });
});

describe("quote attribution (Lib/Quotes.swift)", () => {
  test("recognised shapes", () => {
    expect(isAttribution("On Tue, Mar 3, 2026, at 10:02, Cy Example wrote:")).toBe(true);
    expect(isAttribution("---------- Forwarded message ---------")).toBe(true);
    expect(isAttribution("-----Original Message-----")).toBe(true);
    expect(isAttribution("Begin forwarded message:")).toBe(true);
  });
  test("ordinary text is not an attribution", () => {
    expect(isAttribution("On Tuesday we wrote the plan together.")).toBe(false);
    expect(isAttribution("")).toBe(false);
    expect(isAttribution(null)).toBe(false);
  });
});

describe("link capture", () => {
  test("only http(s) reaches the app", () => {
    expect(capturedLink("https://links.example/a")).toBe("https://links.example/a");
    expect(capturedLink(" http://links.example/b ")).toBe("http://links.example/b");
    for (const h of ["mailto:a@example.com", "javascript:alert(1)", "data:text/html,x", "/rel", "#top", "", null]) {
      expect(capturedLink(h), String(h)).toBeNull();
    }
  });
});

describe("escapeHtml", () => {
  test("markup becomes text", () => {
    expect(escapeHtml(`<img src="x" onerror='y'>&`)).toBe("&lt;img src=&quot;x&quot; onerror=&#39;y&#39;&gt;&amp;");
  });
});

describe("Limiter", () => {
  test("never runs more than its cap at once", async () => {
    const lim = new Limiter(4);
    let peak = 0;
    let live = 0;
    const task = () =>
      lim.run(async () => {
        live++;
        peak = Math.max(peak, live);
        await new Promise((r) => setTimeout(r, 5));
        live--;
      });
    await Promise.all(Array.from({ length: 20 }, task));
    expect(peak).toBe(4);
    expect(lim.inFlight).toBe(0);
  });
  test("a queued task that is aborted never runs and frees nothing it did not take", async () => {
    const lim = new Limiter(1);
    let release!: () => void;
    const first = lim.run(() => new Promise<void>((r) => (release = r)));
    const ctl = new AbortController();
    let ran = false;
    const second = lim.run(async () => {
      ran = true;
    }, ctl.signal);
    ctl.abort();
    await expect(second).rejects.toThrow();
    release();
    await first;
    expect(ran).toBe(false);
    expect(lim.inFlight).toBe(0);
    await lim.run(async () => {});
  });
});
