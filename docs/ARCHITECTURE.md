# Arsitektur Astra

## Tinjauan

Astra adalah **control plane browser**: satu proses native kecil yang

1. menjalankan (atau menempel pada) engine rendering Chromium-family,
2. menyajikan endpoint DevTools Protocol sendiri untuk klien otomasi,
3. dan menyisipkan pipa optimasi pada setiap request yang lewat.

```
 ┌────────────────┐        CDP over WebSocket        ┌──────────────────────────┐
 │ Puppeteer      │◀─────────────────────────────────▶│ conn_t (CONN_CLIENT)     │
 │ go-rod / curl  │   /json/version, /json/list       │  parser WS + antrian out │
 └────────────────┘                                  └────────────┬─────────────┘
                                                                  │
                                        ┌─────────────────────────▼──────────────┐
                                        │ router: metode browser-level           │
                                        │  Target.*  diproses lokal              │
                                        │  Astra.*   diproses lokal (statistik)  │
                                        │  lainnya   diteruskan ke engine        │
                                        │  sesi      id dipetakan our ↔ engine   │
                                        └─────────┬──────────────────┬───────────┘
                                                  │                  │
                   Fetch.requestPaused ───────────▼──────┐           │
                                              ┌──────────────────┐  │
                                              │ optimizer (lite) │  │
                                              │  filters         │  │
                                              │  cache  (disk)   │  │
                                              │  image  (stb)    │  │
                                              │  minify          │  │
                                              │  stats           │  │
                                              └──────────────────┘  │
                                                                    │
                                             ┌──────────────────────▼────────────┐
                                             │ conn_t (CONN_ENGINE)              │
                                             │ WS klien ke /devtools/browser/<id>│
                                             └────────────────┬──────────────────┘
                                                              │
                                                              ▼
                                                   Chromium / Chrome / headless-shell
```

## Model eksekusi

* **Satu thread, satu `poll()` loop.** Tidak ada thread pool, tidak ada async runtime. Setiap
  koneksi (`conn_t`) punya buffer masuk/keluar; frame WebSocket diurai inkremental, sehingga
  pesan besar tidak perlu dialokasi penuh sebelum diproses.
* **Non-blocking I/O** pada semua soket; penulisan di-antrekan di `conn_t.out` dan dikirim saat
  `POLLOUT`. Koneksi yang ditandai tutup baru ditutup **setelah** buffer keluar kosong (ini
  penting untuk respons HTTP pendek seperti `/json/version`).
* **Tanpa dependensi eksternal**: JSON, WebSocket, SHA-1, base64, URL, HTTP, cache — semuanya
  ditulis sendiri di `src/`. Satu-satunya kode pihak ketiga adalah `vendor/stb` (header-only,
  public domain) untuk codec gambar.
* **Backpressure**: `ws_parser.max_message` membatasi ukuran pesan (256 MiB) dan setiap alokasi
  besar (badan respons gambar) dibebaskan segera setelah ditransformasi.

## Modul

| Berkas | Tanggung jawab |
|---|---|
| `util.c` | buffer dinamis, logging, waktu monotonik, helper string, SHA-1, base64, parse URL + eTLD+1, helper berkas |
| `json.c` | parser/serializer JSON (`json_t`, `json_parse`, `json_stringify`, `json_clone`) |
| `ws.c` | RFC 6455: handshake klien/server, framing, masking, fragmentasi, ping/pong |
| `http.c` | klien HTTP (discovery `/json/version` engine, proxy `/json/list`) + parser request |
| `engine.c` | deteksi binary (PATH, `$PREFIX/bin/chromium` di Termux, lokasi absolut), fork/exec, baca `DevToolsActivePort`, probe, shutdown |
| `cdp.c` | endpoint HTTP discovery, upgrade WebSocket, routing perintah/event, pemetaan session, `Target.*` handling, `Astra.*`, event loop |
| `optimizer.c` | state machine intersepsi `Fetch.*`: keputusan request stage dan transformasi response stage |
| `filters.c` | pencocokan aturan iklan/tracker (hash + suffix match), parser gaya EasyList |
| `image.c` | decode (stb_image) → resize (stb_image_resize2) → encode JPEG/PNG (stb_image_write) + stripper metadata |
| `minify.c` | minifikasi HTML/CSS/JS yang konservatif + injeksi `loading="lazy"` |
| `cache.c` | cache disk per-URL (SHA-1), LRU by mtime, `max-age`, revalidasi kondisional |
| `stats.c` | akuntansi byte, persen penghematan, laporan |
| `config.c` | default → berkas konfigurasi → env → CLI (urutan prioritas) |
| `main.c` | CLI: `serve`, `open`, `bench`, `info`, `cache-clear` |

## Alur sebuah request (mode lite)

```
engine ── Fetch.requestPaused (Request) ──▶ optimizer
                                            ├─ cocok filter?       → Fetch.failRequest
                                            ├─ cache segar?        → Fetch.fulfillRequest (0 byte jaringan)
                                            ├─ cache kadaluarsa?   → If-None-Match / If-Modified-Since
                                            └─ teruskan + interceptResponse:true
engine ── Fetch.requestPaused (Response) ─▶ optimizer
                                            ├─ 304 + punya cache?  → fulfill dari cache (200)
                                            ├─ gambar?             → Fetch.getResponseBody → transcode → fulfill
                                            ├─ html/css/js?        → getResponseBody → minify → fulfill
                                            └─ selainnya           → Fetch.continueRequest (passthrough)
```

Setiap keputusan memperbarui `g_stats` (lihat [BANDWIDTH.md](BANDWIDTH.md)).

## Pemetaan session

Klien (Puppeteer) menggunakan `sessionId` sendiri; engine punya `sessionId` sendiri. Astra
menerjemahkan dua arah:

* `Target.attachToTarget` selalu dikirim dengan `flatten: true`, sehingga semua session berbagi
  satu koneksi WebSocket ke engine (lebih hemat dan lebih sederhana).
* Saat event `Target.attachedToTarget` datang, Astra membuat `session_t {our_id, engine_id,
  target_id, client}` dan menulis ulang `sessionId` pada event sebelum diteruskan.
* Respons `Target.attachToTarget` juga ditulis ulang di dalam `result.sessionId` — ini yang
  dipakai Puppeteer.
* Event bertarget tanpa pemilik dikirim ke klien yang mengaktifkan `setDiscoverTargets`/
  `setAutoAttach`.

## Kenapa ringan

* Binary ≈ 2 MB (sudah termasuk codec gambar), statis bila diinginkan.
* Tidak ada VM/runtime/GC; satu thread; alokasi per request dibebaskan segera.
* Memori saat idle diukur oleh proses kontrol saja; engine berjalan sebagaimana Chromium biasa.
* Loop `poll()` tanpa wakeup periodik: tidak ada polling yang menghabiskan CPU saat idle.

## Batas desain yang disengaja

* Astra tidak mengimplementasi layout/paint: itu tugas engine (lihat README §1).
* Domain `Fetch` dikuasai Astra saat `--lite=on` (klien yang butuh `Fetch.*` harus mematikan lite).
* Hanya `ws://` (bukan TLS) untuk koneksi ke engine — DevTools lokal memang plain HTTP/WS.
