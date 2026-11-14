// Turns a download the page pulls through iroh into a real browser
// download, streamed to disk: the page posts { download: { id, name, size } }
// with a MessagePort, then opens save/<id>/<name> in a hidden frame; the
// bytes come over the port, one chunk per "pull".
//
// Also answers the page's audio player, which asks for play/<song id> (with a
// Range when it seeks): the page is asked { stream: { path, range } } with a
// MessagePort, makes the request over iroh, and sends back { head: { status,
// headers } } and then the body, the same way as a download.
"use strict";

const pending = new Map();

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  const job = e.data && e.data.download;
  if (job && e.ports[0]) pending.set(job.id, { ...job, port: e.ports[0] });
});

self.addEventListener("fetch", (e) => {
  const prefix = new URL("save/", self.registration.scope).pathname;
  const play = new URL("play/", self.registration.scope).pathname;
  const url = new URL(e.request.url);
  if (url.pathname.startsWith(play)) {
    e.respondWith(stream(e, url.pathname.slice(play.length)));
    return;
  }
  if (!url.pathname.startsWith(prefix)) return;
  const id = url.pathname.slice(prefix.length).split("/")[0];
  const job = pending.get(id);
  if (!job) {
    e.respondWith(new Response("This download has expired.", { status: 404 }));
    return;
  }
  pending.delete(id);
  const headers = {
    "Content-Type": "application/octet-stream",
    "Content-Disposition": disposition(job.name),
    "Cache-Control": "no-store",
  };
  if (job.size) headers["Content-Length"] = String(job.size);
  e.respondWith(new Response(pulled(job.port), { headers }));
});

// A body whose chunks come over `port`, one per "pull".
function pulled(port) {
  let waiting = null;
  return new ReadableStream({
    start(controller) {
      port.onmessage = ({ data }) => {
        if (data.chunk) controller.enqueue(data.chunk);
        else if (data.done) controller.close();
        else if (data.error) controller.error(new Error(data.error));
        if (waiting) { waiting(); waiting = null; }
      };
    },
    // One chunk at a time: the page reads the next only when asked.
    pull() {
      return new Promise((resolve) => {
        waiting = resolve;
        port.postMessage("pull");
      });
    },
    cancel() {
      port.postMessage("cancel");
    },
  });
}

// A song (or a range of it), asked of the page that plays it.
async function stream(e, id) {
  const client = (e.clientId && (await self.clients.get(e.clientId))) ||
    (await self.clients.matchAll({ type: "window" }))[0];
  if (!client) return new Response("No page to play it through.", { status: 503 });
  const { port1: port, port2 } = new MessageChannel();
  const reply = await new Promise((resolve) => {
    port.onmessage = ({ data }) => resolve(data);
    client.postMessage({ stream: { path: "play/" + id, range: e.request.headers.get("range") } }, [port2]);
  });
  if (!reply.head) return new Response(reply.error || "Failed", { status: 502 });
  const { status, headers } = reply.head;
  const body = status === 200 || status === 206 ? pulled(port) : null;
  return new Response(body, { status, headers });
}

// `attachment` with an ASCII fallback name and the real one in UTF-8.
function disposition(name) {
  const fallback = name.replace(/[^\x20-\x7e]|["\\]/g, "_");
  return "attachment; filename=\"" + fallback + "\"; filename*=UTF-8''" + encodeURIComponent(name);
}
