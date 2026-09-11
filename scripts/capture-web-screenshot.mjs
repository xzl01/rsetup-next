#!/usr/bin/env node
/**
 * Drive the rsetup-next web control center in headless Chrome over the
 * Chrome DevTools Protocol and capture a screenshot of a hardware tool drawer.
 *
 * Self-contained: the CDP transport is a minimal WebSocket client built on
 * node:net / node:crypto, so the script needs no third-party packages.
 *
 * Usage:
 *   node scripts/capture-web-screenshot.mjs \
 *     --url "http://127.0.0.1:19088/#hardware" \
 *     --out docs/testing/screenshots/web-nvme-rock5b-desktop.png \
 *     --width 1440 --height 900 \
 *     [--tool nvme] [--settle 1500] [--mobile] [--devtools-port 9333]
 *     [--no-click] [--scroll-drawer-bottom] [--eval "<js>"] [--rect-selector "<css>"]
 *
 * Each capture writes only the PNG. Pass --meta to additionally write a .json
 * sidecar recording the DOM state at capture time; that sidecar is a debugging
 * aid for exact field values and is deliberately NOT part of the committed
 * evidence.
 *
 * IMPORTANT: a fresh capture exposes the real disk serial. Run
 * scripts/redact-screenshots.py afterwards, before committing any capture:
 *     node scripts/capture-web-screenshot.mjs ...
 *     python3 scripts/redact-screenshots.py
 *     python3 scripts/verify-web-screenshots.py
 */

import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdtempSync, writeFileSync } from "node:fs";
import http from "node:http";
import { connect as netConnect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith("--")) continue;
    const key = token.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.startsWith("--")) {
      args[key] = true;
    } else {
      args[key] = next;
      i += 1;
    }
  }
  return args;
}

function getJson(port, path) {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: "127.0.0.1", port, path }, (res) => {
      let body = "";
      res.on("data", (chunk) => (body += chunk));
      res.on("end", () => {
        try {
          resolve(JSON.parse(body));
        } catch (error) {
          reject(new Error(`bad JSON from ${path}: ${body.slice(0, 200)}`));
        }
      });
    });
    req.on("error", reject);
  });
}

async function waitForDevTools(port, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await getJson(port, "/json/version");
    } catch (error) {
      lastError = error;
      await new Promise((r) => setTimeout(r, 250));
    }
  }
  throw lastError ?? new Error("devtools endpoint never came up");
}

/** Encode a client-to-server WebSocket text frame (always masked). */
function encodeTextFrame(text) {
  const payload = Buffer.from(text, "utf8");
  const mask = randomBytes(4);
  const len = payload.length;
  const OPCODE_TEXT_FIN = 0x81; // FIN set + opcode 1 (text)

  let header;
  if (len < 126) {
    header = Buffer.alloc(6);
    header[0] = OPCODE_TEXT_FIN;
    header[1] = 0x80 | len;
    mask.copy(header, 2);
  } else if (len < 65536) {
    header = Buffer.alloc(8);
    header[0] = OPCODE_TEXT_FIN;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(len, 2);
    mask.copy(header, 4);
  } else {
    header = Buffer.alloc(14);
    header[0] = OPCODE_TEXT_FIN;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(len), 2);
    mask.copy(header, 10);
  }

  const masked = Buffer.allocUnsafe(len);
  for (let i = 0; i < len; i += 1) masked[i] = payload[i] ^ mask[i & 3];
  return Buffer.concat([header, masked]);
}

/** Minimal WebSocket client sufficient for the DevTools Protocol. */
class DevToolsSocket {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.fragments = [];
    this.fragmentOpcode = 0;
    this.onMessage = () => {};
    this.onClose = () => {};
    socket.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.drain();
    });
    socket.on("close", () => this.onClose());
    socket.on("error", () => this.onClose());
  }

  static connect(wsUrl) {
    return new Promise((resolve, reject) => {
      const url = new URL(wsUrl);
      const key = randomBytes(16).toString("base64");
      const socket = netConnect({ host: url.hostname, port: Number(url.port || 80) });
      const fail = (error) => {
        socket.destroy();
        reject(error);
      };
      socket.once("error", fail);
      socket.once("connect", () => {
        socket.write(
          [
            `GET ${url.pathname}${url.search} HTTP/1.1`,
            `Host: ${url.host}`,
            "Upgrade: websocket",
            "Connection: Upgrade",
            `Sec-WebSocket-Key: ${key}`,
            "Sec-WebSocket-Version: 13",
            "",
            "",
          ].join("\r\n"),
        );
      });

      let handshake = Buffer.alloc(0);
      const onHandshakeData = (chunk) => {
        handshake = Buffer.concat([handshake, chunk]);
        const end = handshake.indexOf("\r\n\r\n");
        if (end === -1) return;
        const head = handshake.subarray(0, end).toString("latin1");
        if (!/^HTTP\/1\.1 101/.test(head)) return fail(new Error(`upgrade rejected: ${head.split("\r\n")[0]}`));
        const expected = createHash("sha1").update(key + GUID).digest("base64");
        const match = /sec-websocket-accept:\s*(\S+)/i.exec(head);
        if (!match || match[1] !== expected) return fail(new Error("bad Sec-WebSocket-Accept"));

        socket.off("data", onHandshakeData);
        socket.off("error", fail);
        const client = new DevToolsSocket(socket);
        const rest = handshake.subarray(end + 4);
        if (rest.length) {
          client.buffer = rest;
          client.drain();
        }
        resolve(client);
      };
      socket.on("data", onHandshakeData);
    });
  }

  /** Parse as many complete frames as the buffer currently holds. */
  drain() {
    for (;;) {
      const buf = this.buffer;
      if (buf.length < 2) return;
      const fin = (buf[0] & 0x80) !== 0;
      const opcode = buf[0] & 0x0f;
      const masked = (buf[1] & 0x80) !== 0;
      let len = buf[1] & 0x7f;
      let offset = 2;

      if (len === 126) {
        if (buf.length < offset + 2) return;
        len = buf.readUInt16BE(offset);
        offset += 2;
      } else if (len === 127) {
        if (buf.length < offset + 8) return;
        len = Number(buf.readBigUInt64BE(offset));
        offset += 8;
      }

      let maskKey = null;
      if (masked) {
        if (buf.length < offset + 4) return;
        maskKey = buf.subarray(offset, offset + 4);
        offset += 4;
      }
      if (buf.length < offset + len) return;

      let payload = Buffer.from(buf.subarray(offset, offset + len));
      if (maskKey) for (let i = 0; i < payload.length; i += 1) payload[i] ^= maskKey[i & 3];
      this.buffer = buf.subarray(offset + len);

      if (opcode === 0x8) {
        this.onClose();
        this.socket.end();
        return;
      }
      if (opcode === 0x9) {
        const pong = Buffer.concat([Buffer.from([0x8a, payload.length]), payload]);
        this.socket.write(pong);
        continue;
      }
      if (opcode === 0xa) continue;

      this.fragments.push(payload);
      // Only the first frame of a fragmented message carries the real opcode;
      // continuation frames use opcode 0 and must not overwrite it.
      if (opcode !== 0x0) this.fragmentOpcode = opcode;
      if (!fin) continue;

      const full = this.fragments.length === 1 ? payload : Buffer.concat(this.fragments);
      this.fragments = [];
      if (this.fragmentOpcode === 0x1) this.onMessage(full.toString("utf8"));
    }
  }

  sendText(text) {
    this.socket.write(encodeTextFrame(text));
  }

  close() {
    try {
      this.socket.end();
    } catch {
      /* already closed */
    }
  }
}

class CdpSession {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Map();
    socket.onMessage = (text) => {
      let msg;
      try {
        msg = JSON.parse(text);
      } catch {
        return;
      }
      if (msg.id !== undefined) {
        const entry = this.pending.get(msg.id);
        if (!entry) return;
        this.pending.delete(msg.id);
        msg.error ? entry.reject(new Error(JSON.stringify(msg.error))) : entry.resolve(msg.result);
        return;
      }
      for (const handler of this.listeners.get(msg.method) ?? []) handler(msg.params);
    };
  }

  send(method, params = {}, timeoutMs = 30000) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP timeout: ${method}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.socket.sendText(JSON.stringify({ id, method, params }));
    });
  }

  close() {
    this.socket.close();
  }
}

async function evaluate(session, expression) {
  const result = await session.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) throw new Error(`page evaluation failed: ${JSON.stringify(result.exceptionDetails)}`);
  return result.result?.value;
}

async function waitForCondition(session, expression, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const ok = await evaluate(session, `(() => { try { return Boolean(${expression}); } catch { return false; } })()`);
    if (ok) return;
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`timed out waiting for: ${label ?? expression}`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const url = args.url;
  const out = args.out;
  if (!url || !out) {
    console.error("usage: capture-web-screenshot.mjs --url <url> --out <png> [--width N --height N --tool nvme]");
    process.exit(2);
  }
  const width = Number(args.width ?? 1440);
  const height = Number(args.height ?? 900);
  const tool = typeof args.tool === "string" ? args.tool : "nvme";
  const settleMs = Number(args.settle ?? 1500);
  const devtoolsPort = Number(args["devtools-port"] ?? 9333);
  const mobile = Boolean(args.mobile);
  const noClick = Boolean(args["no-click"]);
  const writeMeta = Boolean(args.meta);
  const scrollDrawerBottom = Boolean(args["scroll-drawer-bottom"]);
  const theme = typeof args.theme === "string" ? args.theme : "light";

  const profile = mkdtempSync(join(tmpdir(), "rsetup-chrome-"));
  const crashDir = mkdtempSync(join(tmpdir(), "rsetup-chrome-crash-"));
  const chrome = spawn(
    "google-chrome",
    [
      "--headless=new",
      // This host's chrome-sandbox helper is not setuid root and $HOME is not
      // writable, so run unprivileged with a throwaway profile under /tmp.
      "--no-sandbox",
      "--disable-dev-shm-usage",
      `--crash-dumps-dir=${crashDir}`,
      `--remote-debugging-port=${devtoolsPort}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "--hide-scrollbars",
      "--force-device-scale-factor=1",
      `--window-size=${width},${height}`,
      "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  const chromeLog = [];
  chrome.stderr.on("data", (d) => chromeLog.push(d.toString()));

  try {
    await waitForDevTools(devtoolsPort);

    const targets = await getJson(devtoolsPort, "/json/list");
    const page = targets.find((t) => t.type === "page");
    if (!page) throw new Error("no page target available");

    const session = new CdpSession(await DevToolsSocket.connect(page.webSocketDebuggerUrl));
    await session.send("Page.enable");
    await session.send("Runtime.enable");

    await session.send("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: mobile ? 2 : 1,
      mobile,
    });
    await session.send("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-color-scheme", value: theme }],
    });

    await session.send("Page.navigate", { url });

    await waitForCondition(session, "document.querySelector('[data-hardware-matrix]')", 20000, "hardware matrix element");
    await waitForCondition(
      session,
      "document.querySelectorAll('[data-hardware-tool]').length > 0",
      20000,
      "hardware capability cards rendered",
    );

    const card = await evaluate(
      session,
      `(() => {
         const el = document.querySelector('[data-hardware-tool="${tool}"]');
         if (!el) return { found: false };
         return { found: true, disabled: el.disabled === true, text: el.innerText.replace(/\\s+/g, ' ').trim() };
       })()`,
    );
    if (!card.found) throw new Error(`hardware card '${tool}' not present in the matrix`);

    if (noClick) {
      // Unavailable hardware renders a disabled card with no drawer to open;
      // capture the hardware matrix itself in that state. The card may sit
      // below the fold, so scroll it into view first.
      await evaluate(
        session,
        `(() => { document.querySelector('[data-hardware-tool="${tool}"]')
           .scrollIntoView({ block: 'center', inline: 'nearest' }); return true; })()`,
      );
      await new Promise((r) => setTimeout(r, settleMs));
      const shot = await session.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
      writeFileSync(out, Buffer.from(shot.data, "base64"));
      if (writeMeta) {
        const meta = {
          url,
          width,
          height,
          mobile,
          theme,
          tool,
          scrollDrawerBottom: false,
          card: card.text,
          drawerText: null,
          consoleErrors: [],
          rect: null,
        };
        writeFileSync(out.replace(/\.png$/i, ".json"), `${JSON.stringify(meta, null, 2)}\n`);
      }
      session.close();
      console.log(
        JSON.stringify({ out, width, height, mobile, theme, tool, card: card.text, drawerText: null }, null, 2),
      );
      return;
    }

    if (card.disabled) throw new Error(`hardware card '${tool}' is disabled: ${card.text}`);

    await evaluate(session, `document.querySelector('[data-hardware-tool="${tool}"]').click(); true`);

    await waitForCondition(
      session,
      `(() => {
         const drawer = document.querySelector('[data-hardware-drawer]');
         if (!drawer || !drawer.open) return false;
         const body = document.querySelector('[data-hardware-body]');
         return Boolean(body && body.innerText.trim().length > 40 && !body.querySelector('.hardware-tool-loading'));
       })()`,
      20000,
      "hardware drawer content loaded",
    );

    await new Promise((r) => setTimeout(r, settleMs));

    // A drawer taller than the viewport hides its last metrics below the fold.
    // Scroll it to the bottom so the capture can show the remaining values.
    if (scrollDrawerBottom) {
      await evaluate(
        session,
        `(() => {
           const drawer = document.querySelector('[data-hardware-drawer]');
           if (drawer) drawer.scrollTop = drawer.scrollHeight;
           return true;
         })()`,
      );
      await new Promise((r) => setTimeout(r, 600));
    }

    const drawerText = await evaluate(
      session,
      `(() => {
         const body = document.querySelector('[data-hardware-body]');
         return body ? body.innerText.replace(/\\n{3,}/g, '\\n\\n').trim() : '';
       })()`,
    );
    const consoleErrors = await evaluate(
      session,
      `(() => (window.__rsetupErrors || []).slice(0, 10))()`,
    ).catch(() => []);

    // Optionally run a probe expression in the page and report its value, so a
    // verifier can measure real rendered geometry rather than guess from pixels.
    let probe = null;
    if (typeof args.eval === "string") {
      probe = await evaluate(session, args.eval);
    }

    // Optionally report the on-screen box of a selector, so a verifier can
    // crop the captured PNG to exactly the region it wants to inspect.
    let rect = null;
    if (typeof args["rect-selector"] === "string") {
      rect = await evaluate(
        session,
        `(() => {
           const el = document.querySelector(${JSON.stringify(args["rect-selector"])});
           if (!el) return null;
           const r = el.getBoundingClientRect();
           return { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height),
                    text: el.innerText.replace(/\\s+/g, ' ').trim() };
         })()`,
      );
    }

    const shot = await session.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
    writeFileSync(out, Buffer.from(shot.data, "base64"));

    // The sidecar is a debugging aid only; opt in with --meta so captures do
    // not drop extra artefacts into the screenshots directory by default.
    if (writeMeta) {
      const meta = {
        url,
        width,
        height,
        mobile,
        theme,
        tool,
        scrollDrawerBottom,
        card: card.text,
        drawerText,
        consoleErrors,
        rect,
      };
      writeFileSync(out.replace(/\.png$/i, ".json"), `${JSON.stringify(meta, null, 2)}\n`);
    }

    session.close();
    console.log(
      JSON.stringify(
        { out, width, height, mobile, theme, tool, card: card.text, drawerText, consoleErrors, rect, probe },
        null,
        2,
      ),
    );
  } finally {
    chrome.kill("SIGKILL");
    if (chromeLog.length && process.env.RSETUP_CHROME_LOG) process.stderr.write(chromeLog.join(""));
  }
}

main().catch((error) => {
  console.error(`capture failed: ${error.message}`);
  process.exit(1);
});
