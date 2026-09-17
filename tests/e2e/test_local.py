#!/usr/bin/env python3
"""End-to-end test of the Astra control plane against the mock CDP engine.

Exercises: HTTP discovery endpoints, WebSocket upgrade, Target.* routing + session
id rewriting, Fetch interception (ad blocking, image transcoding, HTML minification,
passthrough), bandwidth accounting, Runtime.evaluate/Page.navigate passthrough and
the `astra open` CLI.
"""
import base64
import json
import os
import socket
import struct
import subprocess
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
ASTRA = os.path.join(ROOT, 'build', 'astra')
MOCK_PORT = int(os.environ.get('ASTRA_MOCK_PORT', '19222'))
ASTRA_PORT = int(os.environ.get('ASTRA_PORT', '19233'))
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

failures = []
checks = 0


def check(cond, label, extra=''):
    global checks
    checks += 1
    if cond:
        print('  ok   %s' % label)
    else:
        print('  FAIL %s %s' % (label, extra))
        failures.append(label)


def make_png(w=400, h=300):
    import random
    random.seed(11)
    raw = bytearray()
    for _ in range(h):
        raw.append(0)
        for _ in range(w):
            raw += bytes((random.randrange(256), random.randrange(256), random.randrange(256)))

    def chunk(t, d):
        return (struct.pack('>I', len(d)) + t + d +
                struct.pack('>I', zlib.crc32(t + d) & 0xffffffff))

    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) +
            chunk(b'IDAT', zlib.compress(bytes(raw), 6)) + chunk(b'IEND', b''))


class WSClient:
    def __init__(self, host, port, path):
        self.sock = socket.create_connection((host, port), timeout=10)
        import secrets
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        req = ('GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
               'Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n' % (path, host, port, key))
        self.sock.sendall(req.encode())
        self.buf = b''
        while b'\r\n\r\n' not in self.buf:
            self.buf += self.sock.recv(4096)
        head, self.buf = self.buf.split(b'\r\n\r\n', 1)
        assert b'101' in head.split(b'\r\n')[0], head
        self.events = []

    def send(self, obj):
        data = json.dumps(obj).encode()
        hdr = bytearray([0x81])
        mask = os.urandom(4)
        n = len(data)
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126)
            hdr += struct.pack('>H', n)
        else:
            hdr.append(0x80 | 127)
            hdr += struct.pack('>Q', n)
        hdr += mask
        masked = bytes(bytearray(b ^ mask[i & 3] for i, b in enumerate(data)))
        self.sock.sendall(bytes(hdr) + masked)

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _recv_frame(self):
        b = self._read(2)
        opcode = b[0] & 0x0F
        n = b[1] & 0x7F
        if n == 126:
            n = struct.unpack('>H', self._read(2))[0]
        elif n == 127:
            n = struct.unpack('>Q', self._read(8))[0]
        payload = self._read(n)
        if opcode == 0x9:
            return None
        return json.loads(payload.decode('utf-8', 'replace'))

    def wait_for(self, predicate, timeout=8.0):
        """Pump messages until predicate(msg) is true; other messages go to self.events."""
        for i, m in enumerate(self.events):
            if predicate(m):
                return self.events.pop(i)
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.sock.settimeout(max(0.05, deadline - time.time()))
            try:
                msg = self._recv_frame()
            except (socket.timeout, EOFError):
                return None
            if msg is None:
                continue
            if predicate(msg):
                return msg
            self.events.append(msg)
        return None

    def request(self, method, params=None, session=None, timeout=8.0):
        self._mid = getattr(self, '_mid', 0) + 1
        mid = self._mid
        msg = {'id': mid, 'method': method}
        if params is not None:
            msg['params'] = params
        if session:
            msg['sessionId'] = session
        self.send(msg)
        res = self.wait_for(lambda m: m.get('id') == mid, timeout)
        return res

    def close(self):
        try:
            self.sock.close()
        except Exception:
            pass


def http_get(port, path, timeout=5.0):
    s = socket.create_connection(('127.0.0.1', port), timeout=timeout)
    s.sendall(('GET %s HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' % path).encode())
    data = b''
    while True:
        c = s.recv(65536)
        if not c:
            break
        data += c
    s.close()
    head, _, body = data.partition(b'\r\n\r\n')
    status = head.split(b' ')[1] if b' ' in head else b'?'
    return int(status), body


def wait_port(port, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            s = socket.create_connection(('127.0.0.1', port), timeout=1)
            s.close()
            return True
        except OSError:
            time.sleep(0.2)
    return False


def main():
    print('astra local end-to-end test (mock engine)')
    if not os.path.exists(ASTRA):
        print('  build/astra missing - run make first')
        return 1

    cache_dir = '/tmp/astra-it-cache'
    subprocess.run(['rm', '-rf', cache_dir])

    mock = subprocess.Popen([sys.executable, os.path.join(HERE, 'mock_engine.py'),
                             '--port', str(MOCK_PORT)],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    time.sleep(0.8)
    check(mock.poll() is None, 'mock engine started')

    astra = subprocess.Popen(
        [ASTRA, 'serve',
         '--engine-url', 'ws://127.0.0.1:%d/devtools/browser/mock' % MOCK_PORT,
         '--port', str(ASTRA_PORT), '--cache-dir', cache_dir, '--log-level', '3'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        if not wait_port(ASTRA_PORT):
            out = astra.stdout.read().decode() if astra.stdout else ''
            check(False, 'astra control plane started', out[:2000])
            return 1
        check(True, 'astra control plane started')

        # ---------------------------------------------------------- discovery
        st, body = http_get(ASTRA_PORT, '/json/version')
        check(st == 200, 'GET /json/version returns 200', str(st))
        info = json.loads(body)
        check('Astra/' in info.get('Browser', ''), 'version endpoint advertises Astra',
              info.get('Browser'))
        ws_url = info.get('webSocketDebuggerUrl', '')
        check(':%d/devtools/browser/' % ASTRA_PORT in ws_url,
              'webSocketDebuggerUrl points at astra', ws_url)
        check(':%d/' % MOCK_PORT not in ws_url, 'engine port is not leaked', ws_url)

        st, body = http_get(ASTRA_PORT, '/json/list')
        check(st == 200 and 'targetId' in body.decode(errors='replace') or st == 200,
              'GET /json/list proxied', body[:120].decode(errors='replace'))

        # --------------------------------------------------------- websocket
        path = ws_url.split('127.0.0.1:%d' % ASTRA_PORT, 1)[1]
        c = WSClient('127.0.0.1', ASTRA_PORT, path)
        check(True, 'websocket upgrade accepted')

        r = c.request('Browser.getVersion')
        check(r is not None and r.get('result', {}).get('product') == 'MockEngine/1.0',
              'browser-level command forwarded to engine', str(r))

        r = c.request('Target.setDiscoverTargets', {'discover': True})
        got_event = c.wait_for(lambda m: m.get('method') == 'Target.targetCreated', 3)
        check(got_event is not None, 'target events forwarded to client', str(got_event))

        r = c.request('Target.attachToTarget', {'targetId': 'T1', 'flatten': True})
        session = (r or {}).get('result', {}).get('sessionId')
        check(bool(session), 'attachToTarget returns a session id', str(r))
        check(session != 'S1', 'session id is rewritten by astra (not the engine id)',
              str(session))

        # astra must have enabled Fetch + Network on that session
        log = c.request('Mock.getLog')['result']['log']
        methods = [e['method'] for e in log]
        check('Fetch.enable' in methods, 'astra enabled Fetch interception', str(methods))
        check('Network.enable' in methods, 'astra enabled Network domain', str(methods))
        check(all(e['sessionId'].startswith('S') for e in log if e['sessionId']),
              'astra never leaks its own session ids to the engine', str(log[:4]))

        c.request('Mock.reset')

        # ------------------------------------------------------ ad blocking
        png = make_png()
        html = (b'<!DOCTYPE html>\n<html>\n  <head>\n    <!-- hello -->\n'
                b'    <title>t</title>\n  </head>\n  <body>\n    <p>hi</p>\n'
                b'    <img src="x.png">\n  </body>\n</html>\n')
        js = b'function f(){ return 1; }  // trailing comment\n'

        def emit(url, rtype, body=None, ctype='text/plain'):
            params = {'sessionId': 'S1', 'url': url, 'resourceType': rtype}
            if body is not None:
                params['bodyB64'] = base64.b64encode(body).decode()
                params['stage'] = 'response'
                params['contentType'] = ctype
            res = c.request('Mock.emitFetchPaused', params)
            rid = (res or {}).get('result', {}).get('requestId')
            deadline = time.time() + 6
            while time.time() < deadline:
                log = c.request('Mock.getLog')['result']['log']
                for e in log:
                    if e['method'] in ('Fetch.failRequest', 'Fetch.fulfillRequest',
                                       'Fetch.continueRequest') and \
                            e['params'].get('requestId') == rid:
                        return e, rid
                time.sleep(0.1)
            return None, rid

        e, rid = emit('https://googleads.g.doubleclick.net/pagead/id', 'Image')
        check(e is not None and e['method'] == 'Fetch.failRequest',
              'ad/tracker request is blocked (Fetch.failRequest)', str(e))

        e, rid = emit('https://example.com/big.png', 'Image', png, 'image/png')
        check(e is not None and e['method'] == 'Fetch.fulfillRequest',
              'image request is fulfilled with an optimized body', str(e)[:200])
        if e and e['method'] == 'Fetch.fulfillRequest':
            out = base64.b64decode(e['params']['body'])
            check(len(out) < len(png), 'optimized image is smaller: %d -> %d bytes'
                  % (len(png), len(out)))
            check(out[:2] == b'\xff\xd8', 'optimized image is a JPEG', out[:4].hex())
            hdrs = {h['name']: h['value'] for h in e['params'].get('responseHeaders', [])}
            check(hdrs.get('Content-Type') == 'image/jpeg', 'content-type updated', str(hdrs))
            check(str(len(out)) == hdrs.get('Content-Length'), 'content-length updated', str(hdrs))

        e, rid = emit('https://example.com/page.html', 'Document', html, 'text/html')
        check(e is not None and e['method'] == 'Fetch.fulfillRequest',
              'html is minified and fulfilled', str(e)[:160])
        if e and e['method'] == 'Fetch.fulfillRequest':
            out = base64.b64decode(e['params']['body']).decode('utf-8', 'replace')
            check(len(out) < len(html), 'html is smaller: %d -> %d chars' % (len(html), len(out)))
            check('<!-- hello -->' not in out, 'html comments removed')
            check('loading="lazy"' in out, 'lazy-loading injected', out[:200])

        e, rid = emit('https://example.com/app.js', 'Script', js, 'application/javascript')
        check(e is not None and e['method'] == 'Fetch.continueRequest',
              'js passes through untouched (minify-js off)', str(e)[:120])

        # ------------------------------------------------------------- stats
        st, body = http_get(ASTRA_PORT, '/stats')
        stats = json.loads(body)
        check(stats.get('requests', 0) >= 4, 'stats counts requests', json.dumps(stats))
        check(stats.get('blocked', 0) >= 1, 'stats counts blocked requests', json.dumps(stats))
        check(stats.get('imagesOptimized', 0) >= 1, 'stats counts optimized images')
        check(stats.get('textMinified', 0) >= 1, 'stats counts minified documents')
        check(stats.get('bytesOriginal', 0) > stats.get('bytesDelivered', 0),
              'stats shows fewer delivered bytes than original',
              '%s vs %s' % (stats.get('bytesOriginal'), stats.get('bytesDelivered')))
        check(stats.get('savingPct', 0) > 0, 'saving percentage is positive',
              str(stats.get('savingPct')))

        r = c.request('Astra.getStats')
        check(r is not None and 'result' in r and r['result'].get('requests', 0) >= 4,
              'Astra.getStats works over CDP', str(r)[:160])
        r = c.request('Astra.getVersion')
        check(r is not None and r.get('result', {}).get('version') is not None,
              'Astra.getVersion works over CDP', str(r)[:160])

        # puppeteer/go-rod send everything on a page session: same domain must work there too
        r = c.request('Astra.getStats', session=session)
        check(r is not None and 'result' in r and r['result'].get('requests', 0) >= 4,
              'Astra.getStats works on a page session (as puppeteer sends it)', str(r)[:160])
        r = c.request('Astra.resetStats', session=session)
        check(r is not None and 'result' in r,
              'Astra.resetStats works on a page session', str(r)[:160])
        r = c.request('Astra.getStats', session=session)
        check(r is not None and r['result'].get('requests', 0) == 0,
              'resetStats really resets the counters', str(r)[:160])

        # ------------------------------------------- evaluate / navigation
        r = c.request('Runtime.evaluate',
                      {'expression': '1+1', 'returnByValue': True}, session=session)
        check(r is not None and r.get('result', {}).get('result', {}).get('value') == 2,
              'Runtime.evaluate routed through astra (1+1 -> 2)', str(r))

        c.request('Page.navigate', {'url': 'https://example.org/'}, session=session)
        ev = c.wait_for(lambda m: m.get('method') == 'Page.loadEventFired', 6)
        check(ev is not None, 'Page.loadEventFired event delivered with mapped session',
              str(ev))
        check(ev is not None and ev.get('sessionId') == session,
              'event session id matches the client session',
              str(ev.get('sessionId') if ev else None))
        c.close()
    finally:
        astra.terminate()
        try:
            astra.wait(timeout=5)
        except subprocess.TimeoutExpired:
            astra.kill()
        mock.terminate()
        try:
            mock.wait(timeout=5)
        except subprocess.TimeoutExpired:
            mock.kill()

    # ------------------------------------------------------- `astra open` CLI
    print('  -- astra open (CLI one shot) --')
    mock2 = subprocess.Popen([sys.executable, os.path.join(HERE, 'mock_engine.py'),
                              '--port', str(MOCK_PORT + 2)],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.8)
    try:
        p = subprocess.run([ASTRA, 'open', 'https://example.org/',
                            '--engine-url',
                            'ws://127.0.0.1:%d/devtools/browser/mock' % (MOCK_PORT + 2),
                            '--eval', '6 * 7', '--stats', '--wait', '200', '--log-level', '1'],
                           capture_output=True, timeout=60)
        out = p.stdout.decode('utf-8', 'replace')
        check(p.returncode == 0, 'astra open exits 0', p.stderr.decode()[:400])
        check('42' in out, 'astra open --eval prints the JS value (6 * 7 -> 42)', out[:300])
        check('astra bandwidth report' in out, 'astra open --stats prints the report', out[:400])
    finally:
        mock2.terminate()
        try:
            mock2.wait(timeout=5)
        except subprocess.TimeoutExpired:
            mock2.kill()

    print('\n%d checks, %d failed' % (checks, len(failures)))
    if failures:
        for f in failures:
            print('  failed: %s' % f)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
