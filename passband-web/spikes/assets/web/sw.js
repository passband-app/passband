// Option (1): a service worker that puts the bearer on image requests bound for
// the daemon, so a plain <img src> works — in the page AND in a srcdoc frame,
// if the frame is a controlled client.
//
// Rules:
//   - ONLY the configured daemon origin, ONLY GET, ONLY /client/attachments/*
//     and /client/image. Every other request passes through untouched, so the
//     token can never ride along to a host the sender chose.
//   - the token lives in the worker's memory and in IndexedDB (a worker is
//     stopped after ~30s idle and loses memory; IDB brings it back).
//   - the outgoing request is credentials:'omit', redirect:'error',
//     referrerPolicy:'no-referrer'.

const DB = "pb-sw";
let config = null; // {origin, token}

function idb(mode, fn) {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(DB, 1);
    open.onupgradeneeded = () => open.result.createObjectStore("kv");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const tx = open.result.transaction("kv", mode);
      const req = fn(tx.objectStore("kv"));
      tx.oncomplete = () => resolve(req?.result);
      tx.onerror = () => reject(tx.error);
    };
  });
}

async function current() {
  if (!config) config = (await idb("readonly", (s) => s.get("config"))) || null;
  return config;
}

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  if (e.data?.type === "config") {
    config = { origin: e.data.origin, token: e.data.token };
    e.waitUntil(
      idb("readwrite", (s) => s.put(config, "config")).then(() => e.source?.postMessage({ type: "config-ok" })),
    );
  }
});

function guarded(url) {
  return url.pathname.startsWith("/client/attachments/") || url.pathname === "/client/image";
}

self.addEventListener("fetch", (e) => {
  if (e.request.method !== "GET") return;
  const url = new URL(e.request.url);
  // Cheap synchronous pre-filter so unrelated requests never wait on IDB.
  if (!guarded(url)) return;
  e.respondWith(
    (async () => {
      const cfg = await current();
      if (!cfg || url.origin !== cfg.origin) return fetch(e.request);
      return fetch(url.href, {
        headers: { Authorization: `Bearer ${cfg.token}` },
        mode: "cors",
        credentials: "omit",
        redirect: "error",
        referrerPolicy: "no-referrer",
      });
    })(),
  );
});
