// The admin console without page loads.
//
// EVERY PAGE STILL WORKS WITHOUT THIS FILE. The server renders whole documents
// and every action is a plain form; this script only changes HOW they are
// fetched. A same-origin link or form inside the console is fetched here, and
// the sidebar's links and the main panel are swapped for the new page's, so
// the frame never redraws. Anything this script does not recognise (another
// origin, a modified click, a page that is not the console) is left to the
// browser.
//
// SERVED FROM THIS ORIGIN AND NOTHING ELSE: the console's CSP is
// `script-src 'self'`, with no inline script and no third party. Everything
// inserted below is HTML this service rendered and escaped; nothing typed into
// the page is ever written into it as markup.
(() => {
  "use strict";

  const ADMIN = /^\/admin(\/|$|\?)/;
  let inflight = null;

  // A same-origin admin URL, or null.
  const adminUrl = (href) => {
    let url;
    try {
      url = new URL(href, location.href);
    } catch {
      return null;
    }
    if (url.origin !== location.origin || !ADMIN.test(url.pathname + url.search)) return null;
    if (url.pathname === "/admin/app.js") return null;
    return url;
  };

  const busy = (on) => {
    document.documentElement.classList.toggle("is-loading", on);
    const main = document.querySelector("main");
    if (main) main.setAttribute("aria-busy", on ? "true" : "false");
  };

  // Put a fetched page on screen. Console to console swaps the sidebar's links
  // and the main panel; anything else (the sign-in door after a session ends,
  // the question before an invite) replaces the whole document, exactly as a
  // page load would have.
  const show = (html, { push, url, scroll }) => {
    const next = new DOMParser().parseFromString(html, "text/html");
    const here = document.querySelector(".shell");
    const there = next.querySelector(".shell");
    if (here && there) {
      const nav = document.querySelector(".side-nav");
      const newNav = next.querySelector(".side-nav");
      if (nav && newNav) nav.replaceWith(document.adoptNode(newNav));
      const main = document.querySelector("main");
      const newMain = next.querySelector("main");
      main.replaceWith(document.adoptNode(newMain));
      document.title = next.title;
      if (push === "push") history.pushState({ admin: true }, "", url);
      else if (push === "replace") history.replaceState({ admin: true }, "", url);
      if (scroll) window.scrollTo(0, 0);
      // Where a screen reader lands after the swap: the top of the new page,
      // as it would after a load.
      newMain.setAttribute("tabindex", "-1");
      newMain.focus({ preventScroll: true });
    } else {
      document.replaceChild(document.adoptNode(next.documentElement), document.documentElement);
      if (push === "push" && there) history.pushState({ admin: true }, "", url);
      window.scrollTo(0, 0);
    }
  };

  // Fetch and show. On any failure the browser does it the old way: a GET is
  // simply loaded, and a POST is submitted natively by the caller.
  const go = async (url, init, push) => {
    if (inflight) inflight.abort();
    const ctl = new AbortController();
    inflight = ctl;
    busy(true);
    try {
      const resp = await fetch(url, { credentials: "same-origin", signal: ctl.signal, ...init });
      const type = resp.headers.get("content-type") || "";
      if (!type.startsWith("text/html")) throw new Error("not a page");
      const html = await resp.text();
      // Where a POST's redirect landed is the page to name in the address bar.
      const landed = resp.redirected ? resp.url : url.toString();
      const isPost = init && init.method === "POST";
      // A POST that answered without redirecting (a refusal re-rendering its
      // page, or the confirmation question) keeps the address it had: that
      // URL answers a POST, not a GET, and must not be one back or reload
      // away.
      const mode = isPost && !resp.redirected ? "none" : push;
      show(html, { push: mode, url: landed, scroll: mode !== "none" || isPost });
      return true;
    } catch (e) {
      if (e && e.name === "AbortError") return true;
      return false;
    } finally {
      if (inflight === ctl) {
        inflight = null;
        busy(false);
      }
    }
  };

  document.addEventListener("click", async (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target.closest("a[href]");
    if (!a || a.target || a.hasAttribute("download")) return;
    const url = adminUrl(a.getAttribute("href"));
    if (!url) return;
    e.preventDefault();
    if (!(await go(url, undefined, "push"))) location.assign(url);
  });

  document.addEventListener("submit", async (e) => {
    const form = e.target;
    if (e.defaultPrevented || !(form instanceof HTMLFormElement) || form.target) return;
    const url = adminUrl(form.getAttribute("action") || location.href);
    if (!url) return;
    e.preventDefault();
    const data = new FormData(form, e.submitter || undefined);
    const method = (form.getAttribute("method") || "get").toUpperCase();
    if (method === "GET") {
      url.search = new URLSearchParams(data).toString();
      if (!(await go(url, undefined, "push"))) location.assign(url);
      return;
    }
    // A button pressed twice while its page is still coming back is one
    // press: the server's guards hold either way, and this saves the round
    // trip and the second banner.
    if (form.dataset.sending) return;
    form.dataset.sending = "1";
    const ok = await go(url, { method: "POST", body: new URLSearchParams(data) }, "push");
    delete form.dataset.sending;
    if (!ok) HTMLFormElement.prototype.submit.call(form);
  });

  // SEARCH AS YOU TYPE. A pause in typing fetches the list for what is in the
  // box and swaps only the results and the filter tabs (whose links carry the
  // search), never the box itself, so the caret stays where it was. The
  // address bar follows without a history entry per keystroke. Enter still
  // submits the form, which the handler above turns into the same fetch.
  let typing = null;
  let searching = null;
  document.addEventListener("input", (e) => {
    const box = e.target;
    if (!(box instanceof HTMLInputElement) || box.type !== "search") return;
    const form = box.form;
    if (!form || !form.classList.contains("search")) return;
    clearTimeout(typing);
    typing = setTimeout(async () => {
      const url = adminUrl(form.getAttribute("action"));
      if (!url) return;
      url.search = new URLSearchParams(new FormData(form)).toString();
      if (searching) searching.abort();
      const ctl = new AbortController();
      searching = ctl;
      busy(true);
      try {
        const resp = await fetch(url, { credentials: "same-origin", signal: ctl.signal });
        const next = new DOMParser().parseFromString(await resp.text(), "text/html");
        for (const sel of ["#results", ".seg"]) {
          const now = document.querySelector(sel);
          const then = next.querySelector(sel);
          if (now && then) now.replaceWith(document.adoptNode(then));
        }
        history.replaceState({ admin: true }, "", url);
      } catch (err) {
        if (!err || err.name !== "AbortError") form.requestSubmit();
      } finally {
        if (searching === ctl) {
          searching = null;
          busy(false);
        }
      }
    }, 220);
  });

  window.addEventListener("popstate", async () => {
    const url = adminUrl(location.href);
    if (!url || !(await go(url, undefined, "none"))) location.reload();
  });

  // The page this script loaded on is a history entry it can come back to.
  history.replaceState({ admin: true }, "", location.href);
})();
