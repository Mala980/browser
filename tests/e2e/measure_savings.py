#!/usr/bin/env python3
"""Measure Astra's real byte savings on the actual test assets.

This drives the *production* optimizer path (astra serve + Fetch interception)
with the same PNG/HTML/CSS payloads that the browser test page uses, so the
numbers below are the numbers Astra really produces - no browser required.

  python3 tests/e2e/measure_savings.py [--keep]

It prints a per-asset table and exits non-zero when the total saving is below
MIN_SAVING_PCT (default 25%).
"""
import base64
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
ASTRA = os.path.join(ROOT, 'build', 'astra')
MOCK_PORT = int(os.environ.get('ASTRA_MOCK_PORT', '19422'))
ASTRA_PORT = int(os.environ.get('ASTRA_PORT', '19433'))
MIN_SAVING_PCT = float(os.environ.get('MIN_SAVING_PCT', '25'))
OUT = '/tmp/astra-savings'

sys.path.insert(0, HERE)
from test_local import WSClient, http_get, wait_port  # noqa: E402


def make_png(w, h, seed=3):
    import random
    import struct
    import zlib
    random.seed(seed)
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        for x in range(w):
            raw += bytes(((x * 7 + y * 3) % 256, (x ^ y) % 256, (y * 5) % 256))

    def chunk(t, d):
        return (struct.pack('>I', len(d)) + t + d +
                struct.pack('>I', zlib.crc32(t + d) & 0xffffffff))

    ihdr = struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) +
            chunk(b'IDAT', zlib.compress(bytes(raw), 6)) + chunk(b'IEND', b''))


def make_page(n=40):
    """A realistic, chatty HTML document (lots of whitespace and comments)."""
    parts = ['<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n']
    parts.append('  <!-- generated test document for astra bandwidth measurement -->\n')
    parts.append('  <title>Astra savings probe</title>\n  <style>\n')
    parts.append('    /* layout styles */\n    body { margin : 0px ; padding : 12px ; '
                 'background : #ffffff ; }\n')
    parts.append('    .card { border : 1px solid #eeeeee ; border-radius : 4px ; }\n')
    parts.append('  </style>\n</head>\n<body>\n')
    for i in range(n):
        parts.append('  <!-- item %d -->\n  <div class="card">\n    <h3>Item %d</h3>\n'
                     '    <p>Some text that is long enough to be worth collapsing.</p>\n'
                     '    <img src="photo-%d.png">\n  </div>\n' % (i, i, (i % 3) + 1))
    parts.append('</body>\n</html>\n')
    return ''.join(parts).encode()


def make_css():
    css = (b'/* theme */\n'
           b'body {\n  margin : 0px ;\n  padding : 24px ;\n  background : #fafafa ;\n  '
           b'color : #223344 ;\n}\n'
           b'.card {\n  border : 1px solid #dddddd ;\n  border-radius : 8px ;\n  '
           b'margin-bottom : 16px ;\n}\n'
           b'a { color : #0055ff ; text-decoration : none ; }\n')
    pad = b'/* padding comment to reach a realistic stylesheet size */\n'
    while len(css) < 2048:
        css += pad
    return css


def main():
    if not os.path.exists(ASTRA):
        print('build/astra missing - run make first')
        return 1
    subprocess.run(['rm', '-rf', OUT])
    os.makedirs(OUT, exist_ok=True)

    assets = []
    for i, (w, h) in enumerate([(2400, 1600), (2000, 1400), (1600, 1200)], start=1):
        p = os.path.join(OUT, 'photo-%d.png' % i)
        data = make_png(w, h, seed=i * 31)
        with open(p, 'wb') as f:
            f.write(data)
        assets.append(('photo-%d.png (%dx%d)' % (i, w, h), data, 'Image', 'image/png'))
    page = make_page()
    assets.append(('page.html', page, 'Document', 'text/html'))
    assets.append(('theme.css', make_css(), 'Stylesheet', 'text/css'))

    print('astra savings measurement (optimizer path, no browser required)')
    for name, data, _, _ in assets:
        print('  asset %-26s %8.1f KB' % (name, len(data) / 1024.0))

    mock = subprocess.Popen([sys.executable, os.path.join(HERE, 'mock_engine.py'),
                             '--port', str(MOCK_PORT)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.8)
    astra = subprocess.Popen(
        [ASTRA, 'serve',
         '--engine-url', 'ws://127.0.0.1:%d/devtools/browser/mock' % MOCK_PORT,
         '--port', str(ASTRA_PORT), '--cache-dir', OUT + '/cache', '--log-level', '1'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    rc = 1
    try:
        if not wait_port(ASTRA_PORT):
            print('astra did not start')
            return 1
        info = json.loads(http_get(ASTRA_PORT, '/json/version')[1])
        path = info['webSocketDebuggerUrl'].split('127.0.0.1:%d' % ASTRA_PORT, 1)[1]
        c = WSClient('127.0.0.1', ASTRA_PORT, path)
        c.request('Target.attachToTarget', {'targetId': 'T1', 'flatten': True})

        total_in = total_out = 0
        rows = []
        for name, data, rtype, ctype in assets:
            res = c.request('Mock.emitFetchPaused', {
                'sessionId': 'S1', 'url': 'https://assets.example.com/' + name,
                'resourceType': rtype, 'stage': 'response', 'contentType': ctype,
                'bodyB64': base64.b64encode(data).decode()})
            rid = res['result']['requestId']
            deadline = time.time() + 10
            out = None
            while time.time() < deadline:
                log = c.request('Mock.getLog')['result']['log']
                for e in log:
                    if e['params'].get('requestId') == rid and e['method'] in (
                            'Fetch.fulfillRequest', 'Fetch.continueRequest'):
                        out = e
                        break
                if out:
                    break
                time.sleep(0.1)
            if out and out['method'] == 'Fetch.fulfillRequest':
                body = base64.b64decode(out['params']['body'])
                hdrs = {h['name']: h['value'] for h in out['params'].get('responseHeaders', [])}
                rows.append((name, len(data), len(body), hdrs.get('Content-Type', '')))
                total_in += len(data)
                total_out += len(body)
            else:
                rows.append((name, len(data), len(data), 'unchanged'))
                total_in += len(data)
                total_out += len(data)

        print('\n  %-26s %10s %10s %8s  %s' % ('asset', 'original', 'delivered', 'saved', 'type'))
        for name, a, b, ctype in rows:
            print('  %-26s %9.1fK %9.1fK %7.1f%%  %s'
                  % (name, a / 1024.0, b / 1024.0, (a - b) * 100.0 / max(a, 1), ctype))
        saved = (total_in - total_out) * 100.0 / max(total_in, 1)
        print('\n  total: %.1f KB -> %.1f KB  (%.1f%% fewer bytes for the same content)'
              % (total_in / 1024.0, total_out / 1024.0, saved))

        stats = json.loads(http_get(ASTRA_PORT, '/stats')[1])
        print('  astra counters: requests=%d blocked=%d imagesOptimized=%d textMinified=%d'
              % (stats['requests'], stats['blocked'], stats['imagesOptimized'],
                 stats['textMinified']))
        if saved < MIN_SAVING_PCT:
            print('\n  FAIL: saving %.1f%% is below the %.1f%% threshold' % (saved, MIN_SAVING_PCT))
            rc = 1
        else:
            print('\n  ok: saving %.1f%% >= %.1f%% threshold' % (saved, MIN_SAVING_PCT))
            rc = 0
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
    return rc


if __name__ == '__main__':
    sys.exit(main())
