// Offline service worker for webcad {{VERSION}}. Template: `cargo xtask dist` fills in the double-brace placeholders.
// Every deploy gets its own cache name: a new build changes these bytes, so the browser installs the new
// worker, which precaches the new files and then deletes the caches of older builds.
const CACHE = "webcad-{{BUILD_ID}}";
const PRECACHE = [
  "./",
  "./index.html",
  {{PRECACHE}}
];

self.addEventListener("install", (event) => {
  // cache: "reload" bypasses the HTTP cache (GitHub Pages sends max-age=600).
  event.waitUntil(
    caches.open(CACHE)
      .then((c) => c.addAll(PRECACHE.map((u) => new Request(u, { cache: "reload" }))))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("webcad-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET" || new URL(req.url).origin !== self.location.origin) return;
  if (req.mode === "navigate") {
    // Network first for the page so a new deploy is picked up; the cached shell when offline.
    event.respondWith(
      fetch(req, { cache: "no-cache" }).catch(() =>
        caches.match("./index.html", { ignoreSearch: true }).then((hit) => hit || Response.error()))
    );
    return;
  }
  // Cache first for everything else: the .js/.wasm names are content-hashed and never go stale.
  event.respondWith(caches.match(req).then((hit) => hit || fetch(req)));
});
