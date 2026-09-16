# DevTools Protocol (CDP) di Astra

## Endpoint

| Endpoint | Keterangan |
|---|---|
| `GET /json/version` | versi, `Browser`, `Protocol-Version`, dan `webSocketDebuggerUrl` milik Astra |
| `GET /json/list` | daftar target (diproksi dari engine, port ditulis ulang ke port Astra) |
| `PUT /json/new?url=` | buat target baru (diproksi) |
| `GET /json/protocol` | definisi protokol (diproksi, untuk tooling) |
| `GET /stats` | statistik penghematan bandwidth (JSON) |
| `GET /healthz` | `{"ok":true}` |
| `GET /` | halaman info sederhana |
| `ws://host:port/devtools/browser/<id>` | endpoint CDP utama |

`Browser` pada `/json/version` berbentuk `Astra/<versi> (<Browser engine>)`, misalnya
`Astra/0.1.0 (Chrome/128.0.0.0)` — kompatibel dengan Puppeteer dan go-rod.

## Domain yang diproses sendiri oleh Astra

| Domain | Perilaku |
|---|---|
| `Target.*` | `setDiscoverTargets`, `setAutoAttach`, `attachToTarget` (dipaksa `flatten`), `createTarget`, `closeTarget` — diteruskan ke engine sambil memelihara pemetaan session |
| `Astra.*` | ekstensi Astra (lihat bawah) |
| `Fetch.*` | saat `--lite=on`: `enable`/`disable` dijawab OK tanpa diteruskan (Astra yang menguasai intersepsi). Saat `--lite=off`: diteruskan apa adanya |
| lain-lain | diteruskan ke engine (browser-level atau per-session) |

## Ekstensi `Astra.*`

```js
const cdp = await page.createCDPSession();
await cdp.send('Astra.getStats');    // {requests, blocked, cacheHits, revalidated, imagesOptimized,
                                     //  textMinified, bytesOriginal, bytesDelivered, bytesSaved*,
                                     //  savingPct, imageMs, minifyMs, uptimeMs}
await cdp.send('Astra.resetStats');  // {}
await cdp.send('Astra.getConfig');   // {version, lite, blockAds, optimizeImages, maxImageWidth,
                                     //  imageQuality, cache, rules}
await cdp.send('Astra.getVersion');  // {version, engine, browser}
```

Go (go-rod):

```go
stats, err := page.Client().Call(nil, "", "Astra.getStats", nil)
```

## Menyambung dari Puppeteer

```js
import puppeteer from 'puppeteer-core';
const v = await (await fetch('http://127.0.0.1:9222/json/version')).json();
const browser = await puppeteer.connect({ browserWSEndpoint: v.webSocketDebuggerUrl });
const page = await browser.newPage();
await page.goto('https://example.com', { waitUntil: 'load' });
await page.screenshot({ path: 'shot.png' });
```

## Menyambung dari go-rod

```go
browser := rod.New().ControlURL("ws://127.0.0.1:9222/devtools/browser/astra").MustConnect()
page := browser.MustPage("https://example.com").MustWaitLoad()
```

go-rod juga bisa memakai endpoint HTTP: `rod.New().ControlURL("http://127.0.0.1:9222")` (go-rod
akan membaca `/json/version` sendiri).

## Pemetaan session (detail)

```
klien                          astra                         engine
─────                          ─────                         ──────
Target.attachToTarget ────────▶ id baru ++ ─────────────────▶ id (flatten)
                               pending{id→klien}
                      ◀── result.sessionId = "astra-session-N"
                               session{our:astra-session-N, engine:S1}
Runtime.evaluate(sessionId=astra-session-N) ─▶ sessionId=S1 ─▶
                      ◀──── result (sessionId ditulis ulang ke astra-session-N)
Page.loadEventFired(sessionId=S1) ◀──────────────  dikirim ke klien pemilik
```

## Hal yang perlu diketahui skrip otomasi

1. **Intersepsi milik klien vs Astra.** Saat `--lite=on`, event `Fetch.requestPaused` **tidak**
   diteruskan ke klien (Astra menangani semua request). Bila skrip Anda butuh
   `page.setRequestInterception(true)`, jalankan Astra dengan `--lite=off`.
2. **Ukuran pesan.** Frame masuk dibatasi 256 MiB; badan respons yang diintersepsi hanya hidup
   selama transformasi berlangsung.
3. **Batas 256 koneksi** per iterasi `poll()` (konstanta internal). Untuk beban besar, jalankan
   beberapa instance Astra di port berbeda.
4. **Idle exit.** `--idle-exit-ms=N` menutup Astra bila tidak ada aktivitas klien selama N ms —
   berguna untuk worker, jangan dipakai untuk server panjang kecuali memang diinginkan.
