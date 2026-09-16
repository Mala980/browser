#!/usr/bin/env python3
"""Minimal CDP engine (WebSocket + HTTP discovery) used to test Astra end-to-end
without a real Chromium binary.  It speaks just enough of the DevTools protocol:

  * HTTP GET /json/version, /json/list
  * Browser.getVersion, Target.getTargets/setDiscoverTargets/attachToTarget
  * Page.enable / Page.navigate (emits Page.frameNavigated + Page.loadEventFired)
  * Runtime.evaluate, Emulation.setDeviceMetricsOverride
  * Fetch.enable -> records it; Fetch.failRequest/continueRequest/fulfillRequest
    -> recorded in the command log; Fetch.getResponseBody -> canned body
  * Mock.* control commands used by the test driver:
      Mock.emitFetchPaused {sessionId,url,resourceType,bodyB64,stage}
      Mock.getLog {}
      Mock.reset {}

Run: python3 mock_engine.py --port 19222
"""
import argparse
import base64
import hashlib
import json
import socket
import struct
import sys
import threading
import zlib

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def make_png(w=400, h=300):
    import random
    random.seed(7)
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # filter type 0
        for x in range(w):
            raw += bytes((random.randrange(256), random.randrange(256), random.randrange(256)))

    def chunk(t, d):
        return (struct.pack('>I', len(d)) + t + d +
                struct.pack('>I', zlib.crc32(t + d) & 0xffffffff))

    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) +
            chunk(b'IDAT', zlib.compress(bytes(raw), 6)) + chunk(b'IEND', b''))


class WS:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b''
        self.lock = threading.Lock()

    def send(self, obj):
        data = json.dumps(obj).encode()
        hdr = bytearray([0x81])
        n = len(data)
        if n < 126:
            hdr.append(n)
        elif n < 65536:
            hdr.append(126)
            hdr += struct.pack('>H', n)
        else:
            hdr.append(127)
            hdr += struct.pack('>Q', n)
        with self.lock:
            self.sock.sendall(bytes(hdr) + data)

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self):
        b = self._read(2)
        opcode = b[0] & 0x0F
        masked = b[1] & 0x80
        n = b[1] & 0x7F
        if n == 126:
            n = struct.unpack('>H', self._read(2))[0]
        elif n == 127:
            n = struct.unpack('>Q', self._read(8))[0]
        mask = self._read(4) if masked else b''
        payload = bytearray(self._read(n))
        if masked:
            for i in range(n):
                payload[i] ^= mask[i & 3]
        if opcode == 0x8:
            raise EOFError
        if opcode == 0x9:  # ping
            self.sock.sendall(bytes([0x8A, 0]))
            return None
        return json.loads(payload.decode('utf-8', 'replace'))


class MockEngine:
    def __init__(self, port):
        self.port = port
        self.ws = None
        self.log = []
        self.bodies = {}  # requestId -> raw bytes
        self.next_request_id = 1

    # ------------------------------------------------------------------ HTTP
    def http_response(self, path):
        if path.startswith('/json/version'):
            return json.dumps({
                "Browser": "MockEngine/1.0",
                "Protocol-Version": "1.3",
                "User-Agent": "MockEngine/1.0",
                "webSocketDebuggerUrl": "ws://127.0.0.1:%d/devtools/browser/mock" % self.port,
            }).encode()
        if path.startswith('/json/list'):
            return json.dumps([{
                "description": "", "id": "T1", "title": "mock", "type": "page",
                "url": "about:blank",
                "webSocketDebuggerUrl": "ws://127.0.0.1:%d/devtools/page/T1" % self.port,
            }]).encode()
        return b'{"error":"not found"}'

    def serve_conn(self, conn):
        data = b''
        while b'\r\n\r\n' not in data:
            chunk = conn.recv(65536)
            if not chunk:
                conn.close()
                return
            data += chunk
        head = data.decode('utf-8', 'replace')
        if 'Upgrade' in head and 'websocket' in head.lower():
            key = ''
            for line in head.split('\r\n'):
                if line.lower().startswith('sec-websocket-key:'):
                    key = line.split(':', 1)[1].strip()
            accept = base64.b64encode(
                hashlib.sha1((key + GUID).encode()).digest()).decode()
            conn.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                          "Connection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n" % accept).encode())
            rest = data.split(b'\r\n\r\n', 1)[1]
            ws = WS(conn)
            self.ws = ws
            if rest:
                ws.buf = rest
            self.ws_loop(ws)
            return
        path = head.split(' ')[1] if ' ' in head else '/'
        body = self.http_response(path)
        conn.sendall(b'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ' +
                     str(len(body)).encode() + b'\r\nConnection: close\r\n\r\n' + body)
        conn.close()

    # ------------------------------------------------------------------- CDP
    def handle(self, msg):
        mid = msg.get('id')
        method = msg.get('method')
        params = msg.get('params') or {}
        session = msg.get('sessionId')
        self.log.append({'method': method, 'params': params, 'sessionId': session})

        def ok(result=None):
            out = {'id': mid}
            if session:
                out['sessionId'] = session
            out['result'] = result if result is not None else {}
            self.ws.send(out)

        if method == 'Browser.getVersion':
            return ok({'protocolVersion': '1.3', 'product': 'MockEngine/1.0'})
        if method == 'Target.getTargets':
            return ok({'targetInfos': [{'targetId': 'T1', 'type': 'page', 'title': 'mock',
                                        'url': 'about:blank', 'attached': False}]})
        if method == 'Target.setDiscoverTargets':
            self.ws.send({'method': 'Target.targetCreated',
                          'params': {'targetInfo': {'targetId': 'T1', 'type': 'page',
                                                    'title': 'mock', 'url': 'about:blank'}}})
            return ok()
        if method == 'Target.attachToTarget' or method == 'Target.createTarget':
            if method == 'Target.createTarget':
                return ok({'targetId': 'T1'})
            self.ws.send({'method': 'Target.attachedToTarget',
                          'params': {'sessionId': 'S1', 'targetInfo': {
                              'targetId': 'T1', 'type': 'page', 'title': 'mock',
                              'url': 'about:blank'}}})
            return ok({'sessionId': 'S1'})
        if method == 'Page.navigate':
            self.ws.send({'method': 'Page.frameNavigated', 'sessionId': 'S1',
                          'params': {'frame': {'id': 'F1', 'url': params.get('url'),
                                               'mimeType': 'text/html'}}})
            self.ws.send({'method': 'Page.loadEventFired', 'sessionId': 'S1',
                          'params': {'timestamp': 1.0}})
            return ok({'frameId': 'F1'})
        if method == 'Runtime.evaluate':
            return ok({'result': {'type': 'string', 'value': 'mock-eval:' + params.get('expression', '')}})
        if method == 'Page.captureScreenshot':
            return ok({'data': base64.b64encode(make_png(8, 8)).decode()})
        if method == 'Fetch.getResponseBody':
            body = self.bodies.get(params.get('requestId'), b'')
            return ok({'body': base64.b64encode(body).decode(), 'base64Encoded': True})
        if method == 'Mock.emitFetchPaused':
            rid = 'R%d' % self.next_request_id
            self.next_request_id += 1
            if params.get('bodyB64'):
                self.bodies[rid] = base64.b64decode(params['bodyB64'])
            ev = {'requestId': rid,
                  'request': {'url': params['url'], 'method': 'GET',
                              'headers': {'Referer': params.get('referer', '')}},
                  'frameId': 'F1', 'resourceType': params.get('resourceType', 'Other'),
                  'networkId': rid}
            if params.get('stage') == 'response':
                ev['responseStatusCode'] = 200
                ev['responseHeaders'] = [{'name': 'Content-Type',
                                          'value': params.get('contentType', 'text/plain')}]
            self.ws.send({'method': 'Fetch.requestPaused',
                          'sessionId': params.get('sessionId', 'S1'), 'params': ev})
            return ok({'requestId': rid})
        if method == 'Mock.getLog':
            return ok({'log': self.log})
        if method == 'Mock.reset':
            self.log = []
            return ok()
        return ok()

    def ws_loop(self, ws):
        try:
            while True:
                msg = ws.recv()
                if msg is None:
                    continue
                self.handle(msg)
        except (EOFError, ConnectionResetError, OSError):
            pass

    def run(self):
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(('127.0.0.1', self.port))
        srv.listen(16)
        print('mock engine listening on 127.0.0.1:%d' % self.port, flush=True)
        while True:
            conn, _ = srv.accept()
            threading.Thread(target=self.serve_conn, args=(conn,), daemon=True).start()


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=19222)
    args = ap.parse_args()
    try:
        MockEngine(args.port).run()
    except KeyboardInterrupt:
        sys.exit(0)
