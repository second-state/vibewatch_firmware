// Probe HTTP server for the watch's Tailscale page.
// Run with: bun probe-server.js
// Listens on 0.0.0.0:9090 (the watch GETs http://<tailnet-ip>:9090 every second).

const PORT = 9090;
let requestCount = 0;

Bun.serve({
  port: PORT,
  async fetch(req, server) {
    requestCount += 1;
    const url = new URL(req.url);
    const ip = server.requestIP(req);

    console.log(
      `[${new Date().toISOString()}] #${requestCount} ${req.method} ${url.pathname}`
    );
    for (const [key, value] of url.searchParams) {
      const kb = /^\d+$/.test(value)
        ? ` (${(Number(value) / 1024).toFixed(0)} KB)`
        : "";
      console.log(`  param ${key} = ${value}${kb}`);
    }
    console.log(`  from: ${ip ? `${ip.address}:${ip.port}` : "unknown"}`);
    const ua = req.headers.get("user-agent");
    const host = req.headers.get("host");
    if (host) console.log(`  host: ${host}`);
    if (ua) console.log(`  user-agent: ${ua}`);

    return new Response(`probe-server: request #${requestCount} received\n`, {
      status: 200,
      headers: { "content-type": "text/plain" },
    });
  },
});

console.log(`probe-server listening on http://0.0.0.0:${PORT}`);
