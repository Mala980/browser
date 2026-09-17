/**
 * Diagnostic helper: a WebSocket proxy that logs every CDP message in both
 * directions.  Point Puppeteer at it and you can see exactly where a session
 * stalls, against Astra or against the engine's own endpoint.
 *
 *   SNIFF_UPSTREAM=ws://127.0.0.1:9222/devtools/browser/xxx \
 *   SNIFF_PORT=9335 SNIFF_LOG=/tmp/sniff.log node cdp_sniffer.mjs
 */
import fs from 'fs';
import { WebSocketServer, WebSocket } from 'ws';

const upstream = process.env.SNIFF_UPSTREAM;
const port = Number(process.env.SNIFF_PORT || 9335);
const logPath = process.env.SNIFF_LOG || '/tmp/sniff.log';
if (!upstream) {
  console.error('SNIFF_UPSTREAM is required');
  process.exit(1);
}
const t0 = Date.now();
const fd = fs.openSync(logPath, 'w');

function log(dir, obj) {
  const ms = String(Date.now() - t0).padStart(6);
  let what;
  if (obj.method) what = `${obj.method} id=${obj.id ?? '-'} sid=${obj.sessionId ?? '-'}`;
  else if (obj.id !== undefined) what = `response id=${obj.id}${obj.error ? ' ERROR ' + JSON.stringify(obj.error) : ''}`;
  else what = '(unknown)';
  fs.writeSync(fd, `${ms}ms ${dir} ${what}\n`);
}

const wss = new WebSocketServer({ port, host: '127.0.0.1' });
wss.on('connection', (client) => {
  const up = new WebSocket(upstream, { perMessageDeflate: false, maxPayload: 512 * 1024 * 1024 });
  const queue = []; /* the upstream may still be handshaking when the client talks */
  up.on('open', () => {
    fs.writeSync(fd, `${Date.now() - t0}ms -- upstream open\n`);
    for (const pending of queue) up.send(pending);
    queue.length = 0;
  });
  up.on('error', (e) => fs.writeSync(fd, `${Date.now() - t0}ms -- upstream error: ${e.message}\n`));
  up.on('close', (c) => fs.writeSync(fd, `${Date.now() - t0}ms -- upstream closed (${c})\n`));
  client.on('message', (data) => {
    try { log('CLI->ENG', JSON.parse(data.toString())); } catch { /* ignore */ }
    const text = data.toString();
    if (up.readyState === WebSocket.OPEN) up.send(text);
    else queue.push(text); // the upstream may still be handshaking
  });
  up.on('message', (data) => {
    try { log('ENG->CLI', JSON.parse(data.toString())); } catch { /* ignore */ }
    if (client.readyState === WebSocket.OPEN) client.send(data.toString());
  });
  client.on('close', () => { try { up.close(); } catch { /* ignore */ } });
});
console.log(`cdp sniffer on ws://127.0.0.1:${port} -> ${upstream} (log: ${logPath})`);
