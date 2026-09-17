#!/usr/bin/env python3
"""Logging CDP websocket proxy - a sniffer that works with every client.

The node based sniffer (tests/e2e/puppeteer/cdp_sniffer.mjs) rejects the
handshake of some clients: go-rod sends a placeholder Sec-WebSocket-Key
("nil") which strict servers answer with 400.  This proxy accepts whatever
key a client offers, so it can also be used for go-rod, curl, or a raw
socket, and it only does one thing: log every message in both directions
and pass it on unchanged.

    python3 tests/e2e/cdp_proxy.py --upstream ws://127.0.0.1:9222/devtools/browser/x \
        --port 9339 --log /tmp/gorod-cdp.log

Log lines look like:

    1203ms CLI->ENG Target.attachToTarget id=7 sid=-
    1204ms ENG->CLI response id=7
"""
import argparse
import base64
import hashlib
import json
import os
import socket
import struct
import sys
import threading
import time

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _accept(key):
    return base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()


class Upstream:
    """Client side of the proxy: talks to astra (or Chrome) as a browser would."""

    def __init__(self, url):
        assert url.startswith("ws://"), url
        rest = url[len("ws://"):]
        hostport, _, path = rest.partition("/")
        host, _, port = hostport.partition(":")
        self.sock = socket.create_connection((host, int(port or 80)), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        req = ("GET /%s HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\n"
               "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
               "Sec-WebSocket-Version: 13\r\n\r\n" % (path, hostport, key))
        self.sock.sendall(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.sock.recv(4096)
        head = buf.split(b"\r\n\r\n", 1)[0].decode("latin1")
        if "101" not in head.split("\r\n")[0]:
            raise RuntimeError("upstream rejected the handshake: %s" % head.split("\r\n")[0])
        self.buf = buf.split(b"\r\n\r\n", 1)[1]

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
            n = struct.unpack(">H", self._read(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", self._read(8))[0]
        mask = self._read(4) if masked else b""
        data = bytearray(self._read(n))
        if masked:
            for i in range(n):
                data[i] ^= mask[i & 3]
        if opcode == 0x8:
            raise EOFError
        if opcode == 0x9:  # ping -> pong, nothing to log
            self.sock.sendall(struct.pack(">BB", 0x8A, 0))
            return None
        return bytes(data)

    def send_raw(self, data):
        """Forward a payload upstream, masked like any browser client."""
        mask = os.urandom(4)
        n = len(data)
        hdr = bytearray([0x81])
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126)
            hdr += struct.pack(">H", n)
        else:
            hdr.append(0x80 | 127)
            hdr += struct.pack(">Q", n)
        hdr += mask
        masked = bytes(bytearray(b ^ mask[i & 3] for i, b in enumerate(data)))
        self.sock.sendall(bytes(hdr) + masked)


def frame(payload):
    """Server -> client frames are never masked."""
    n = len(payload)
    hdr = bytearray([0x81])
    if n < 126:
        hdr.append(n)
    elif n < 65536:
        hdr.append(126)
        hdr += struct.pack(">H", n)
    else:
        hdr.append(127)
        hdr += struct.pack(">Q", n)
    return bytes(hdr) + payload


def describe(payload):
    try:
        m = json.loads(payload.decode("utf-8", "replace"))
    except Exception:
        return "<binary %d bytes>" % len(payload)
    sid = m.get("sessionId", "-")
    if "id" in m and ("result" in m or "error" in m):
        return "%s id=%s%s" % ("error" if "error" in m else "response", m["id"],
                               " sid=%s" % sid if sid != "-" else "")
    return "%s id=%s sid=%s" % (m.get("method", "?"), m.get("id", "-"), sid)


def serve(conn, upstream_url, logf, t0):
    up = Upstream(upstream_url)
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = conn.recv(4096)
        if not chunk:
            conn.close()
            return
        buf += chunk
    head, rest = buf.split(b"\r\n\r\n", 1)
    key = ""
    for line in head.decode("latin1").split("\r\n")[1:]:
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    # Accept any key: strict servers break clients with a placeholder key.
    conn.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                  "Connection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n"
                  % _accept(key)).encode())

    def pump(src, dst_send, tag):
        try:
            while True:
                payload = src.recv()
                if payload is None:
                    continue
                try:
                    logf.write("%5dms %s %s\n" % (int((time.time() - t0) * 1000), tag,
                                                  describe(payload)))
                    logf.flush()
                except Exception:
                    pass
                dst_send(payload)
        except (EOFError, OSError):
            pass

    class ClientSrc:
        """Reads unmasked client frames from the accepted socket."""

        def __init__(self, sock, buf):
            self.sock = sock
            self.buf = buf

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
                n = struct.unpack(">H", self._read(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self._read(8))[0]
            mask = self._read(4) if masked else b""
            data = bytearray(self._read(n))
            if masked:
                for i in range(n):
                    data[i] ^= mask[i & 3]
            if opcode == 0x8:
                raise EOFError
            if opcode == 0x9:
                ClientSrc.sock_send(self.sock, struct.pack(">BB", 0x8A, 0))
                return None
            return bytes(data)

        @staticmethod
        def sock_send(sock, data):
            sock.sendall(data)

    cli = ClientSrc(conn, rest)
    t1 = threading.Thread(target=pump, args=(cli, up.send_raw, "CLI->ENG"), daemon=True)
    t2 = threading.Thread(target=pump, args=(up, lambda p: conn.sendall(frame(p)), "ENG->CLI"),
                          daemon=True)
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    conn.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--port", type=int, default=9339)
    ap.add_argument("--log", default="/tmp/cdp-proxy.log")
    a = ap.parse_args()

    logf = open(a.log, "w", buffering=1)
    t0 = time.time()
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", a.port))
    srv.listen(4)
    print("cdp proxy on ws://127.0.0.1:%d -> %s (log: %s)" % (a.port, a.upstream, a.log),
          flush=True)
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=serve, args=(conn, a.upstream, logf, t0), daemon=True).start()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
