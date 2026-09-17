# Astra Browser — browser ringan dengan control plane CDP

**Astra** adalah *browser control plane* native yang sangat ringan: satu binary C11 (≈2 MB, tanpa
runtime, tanpa dependensi eksternal) yang menjalankan dan mengendalikan engine rendering
Chromium-family lewat **Chrome DevTools Protocol**, sambil mengoptimasi **setiap request** yang
lewat supaya halaman terasa ringan dan hemat bandwidth.

```
Puppeteer / go-rod / curl
        │  CDP  (ws://127.0.0.1:9222)
        ▼
┌─────────────────────────────────────────────────────────────────┐
│  astra  (satu proses, satu thread, poll() loop)                 │
│  ┌───────────┐ ┌────────────┐ ┌──────────┐ ┌───────────────┐   │
│  │ HTTP /json│ │ WS proxy   │ │optimizer │ │ cache + stats │   │
│  │ discovery │ │ + sessions │ │(lite)    │ │ (disk, LRU)   │   │
│  └───────────┘ └────────────┘ └──────────┘ └───────────────┘   │
└───────────────────────────┬─────────────────────────────────────┘
                            │  CDP (Fetch.* interception)
                            ▼
                 Chromium / Chrome / Brave / Edge / headless-shell
                 (rendering, gambar, video, kompositor GPU)
```

---

## 1. Apa yang Astra lakukan (dan apa yang tidak)

**Yang Astra lakukan sendiri (kode kami, C11 murni):**

| Bagian | Keterangan |
|---|---|
| Control plane CDP | endpoint `/json/version`, `/json/list`, `/json/new`, `/json/protocol`, WebSocket, pemetaan session, routing event |
| Pipa hemat bandwidth | blokir iklan/tracker, re-encode + downscale gambar, minifikasi HTML/CSS, lazy-load injection, `Save-Data`, hapus metadata EXIF/ICC |
| Cache | cache disk dengan LRU, `max-age`, dan revalidasi kondisional (`If-None-Match` → 304 → sajikan dari cache) |
| Launcher engine | deteksi & jalankan Chromium-family (headless *new* atau berjendela), baca `DevToolsActivePort`, tuning flag kompositor |
| Metrik | hitungan byte asli vs byte terkirim, persen penghematan, `/stats`, `Astra.getStats` via CDP, `astra bench` |
| CLI | `serve`, `open` (screenshot/pdf/dump-dom/eval), `bench`, `info`, `cache-clear` |

**Yang dikerjakan engine rendering** (Chromium/Chrome/dll): layout, paint, kompositor, decode
gambar/video di GPU, WebGL, DRM, dll. Astra **bukan** engine HTML dari nol — menulis engine
`setara Chromium` (layout, paint, kompositor, video) adalah proyek puluhan ribu jam (Servo saja
butuh ~1 jam build di mesin 16-core dan belum punya CDP). Yang Astra lakukan adalah membuat
**lapisan browser** yang ringan, bisa diotomasi, dan jauh lebih hemat bandwidth, persis seperti
cara Brave/Edge membangun di atas Chromium — tetapi dengan binary 2 MB yang tidak perlu
meng-compile ulang Chromium.

> Perbandingan jujur: Chromium penuh ≈ 150 MB binary + ratusan MB RSS. Astra ≈ **2 MB** binary dan
> RSS yang dipakai hanyalah proses kontrolnya (engine tetap berjalan, sama seperti Chromium
> headless). Keuntungan utama Astra: **byte yang diunduh jauh lebih sedikit** dan kontrol otomasi
> yang seragam di Linux/macOS/Android-Termux.

## 2. Fitur utama

* **Ringan**: satu binary statis, tanpa runtime/VM, satu thread event loop (`poll()`), alokasi
  minimal. Cocok untuk perangkat lemah dan Termux.
* **Mode headless dan mode penuh (berjendela)** — `--mode=headless` (default) dan `--mode=full`.
* **Rendering halus**: flag kompositor di-tuning (`--enable-gpu-rasterization`,
  `--ignore-gpu-blocklist`, `--enable-zero-copy`, `--num-raster-threads=2`,
  `--disable-checker-imaging`, `--disable-partial-raster`) + "pemanasan" rAF; lihat
  [docs/RENDERING.md](docs/RENDERING.md).
* **Gambar & video seperti Chromium**: semua kemampuan decode/encode berasal dari engine;
  Astra memastikan gambar tetap tampil (`naturalWidth > 0`, `img.decode()` ok) dan video
  benar-benar berjalan (`currentTime` maju, `videoWidth > 0`) — keduanya **diuji dengan Puppeteer
  dan go-rod di CI** pada halaman uji sungguhan.
* **Bisa dikendalikan Puppeteer & go-rod**: `puppeteer.connect({browserWSEndpoint})` dan
  `rod.New().ControlURL(ws)` langsung menyambung ke Astra.
* **Hemat bandwidth**: lihat bagian [5](#5-hemat-bandwidth-cara-kerja-dan-hasil-ukur).
* **Build Android/Termux arm64** lewat GitHub Actions (NDK) + skrip build native di HP.

## 3. Instalasi

### 3.1 Build dari source (Linux, macOS, WSL)

Butuh: `gcc`/`clang` + `make` (tidak ada dependensi lain — JSON, WebSocket, SHA-1, base64, cache
semuanya ditulis sendiri).

```bash
git clone https://github.com/Mala980/browser.git
cd browser
make -j$(nproc)          # -> build/astra
make test                # 141 unit check + 43 e2e check (mock engine)
sudo make install        # -> /usr/local/bin/astra
```

Butuh juga engine Chromium-family:

```bash
sudo apt install chromium            # Debian/Ubuntu (atau: npx @puppeteer/browsers install chrome@stable)
astra info                           # lihat engine yang terdeteksi
```

### 3.2 Android / Termux (arm64)

**Opsi A — unduh `.deb` hasil CI** (workflow *Termux (Android aarch64)* → artifact
`astra-termux-aarch64`):

```bash
pkg install chromium
dpkg -i astra_0.1.0_aarch64.deb     # atau: apt install ./astra_0.1.0_aarch64.deb
astra serve --port 9222
```

**Opsi B — build langsung di HP** (paling sederhana, tidak perlu cross-compile):

```bash
pkg install clang make chromium git
git clone https://github.com/Mala980/browser.git && cd browser
./scripts/build-termux.sh --package   # -> build/astra + dist/astra_0.1.0_aarch64.deb
./build/astra-test                    # verifikasi: unit test jalan di HP
```

**Opsi C — cross compile di CI** dengan Android NDK (`scripts/build-termux-ndk.sh`), menghasilkan
binary statis aarch64-bionic + `.deb` + `.tar.xz`.

Catatan Termux: jalankan dengan `--no-sandbox` bila diperlukan (Astra menambahkannya otomatis di
lingkungan Android/Termux), dan mode berjendela butuh X server (`pkg install x11-repo` +
`termux-x11` / VNC).

## 4. Pemakaian

```bash
# 1) control plane (headless). Puppeteer/go-rod menyambung ke sini.
astra serve --port 9222

# 2) mode berjendela penuh (butuh DISPLAY / X server)
astra serve --mode=full --port 9222

# 3) sekali jalan: buka halaman, screenshot, cetak statistik
astra open https://example.com --screenshot /tmp/shot.png --wait 1500 --stats

# 4) ukur penghematan nyata: lite ON vs OFF pada halaman yang sama
astra bench https://example.com

# 5) pakai engine tertentu / tempel ke browser yang sudah jalan
astra serve --engine /opt/chrome/chrome
astra serve --engine-url ws://127.0.0.1:9333/devtools/browser/xxxx

# 6) utilitas
astra info
astra cache-clear
astra open https://example.com --eval "document.title" --dump-dom --pdf out.pdf
```

Contoh dengan **Puppeteer**:

```js
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({ browserWSEndpoint: 'ws://127.0.0.1:9222/devtools/browser/astra' });
const page = await browser.newPage();
await page.goto('https://example.com', { waitUntil: 'load' });
console.log(await page.title());
await page.screenshot({ path: 'shot.png' });

// statistik penghematan bandwidth milik Astra (ekstensi domain CDP)
const cdp = await page.createCDPSession();
console.log(await cdp.send('Astra.getStats'));
```

Contoh dengan **go-rod** (Go):

```go
browser := rod.New().ControlURL("ws://127.0.0.1:9222/devtools/browser/astra").MustConnect()
page := browser.MustPage("https://example.com").MustWaitLoad()
fmt.Println(page.MustEval("() => document.title"))
page.MustScreenshot("shot.png")
```

Contoh tanpa library (curl + ws):

```bash
curl -s http://127.0.0.1:9222/json/version
curl -s http://127.0.0.1:9222/stats
```

### Opsi CLI lengkap

| Opsi | Default | Keterangan |
|---|---|---|
| `--mode=headless\|full` | headless | mode engine |
| `--engine=PATH` | autodetect | binary Chromium-family |
| `--engine-url=ws://…` | – | tempel ke browser yang sudah berjalan |
| `--profile=DIR` | `~/.config/astra/profile` | user-data-dir |
| `--port=N` / `--bind=ADDR` | 9222 / 127.0.0.1 | endpoint CDP |
| `--width` / `--height` | 1280 / 800 | ukuran jendela/viewport |
| `--gpu=on\|off` | on | flag rasterisasi GPU |
| `--no-sandbox` | auto (root/Android) | jalankan engine tanpa sandbox |
| `--user-agent=UA` | – | override UA |
| `--extra-flags="…"` | – | flag tambahan untuk engine |
| `--lite=on\|off` | on | master switch pipa penghematan |
| `--block-ads=on\|off` | on | blokir iklan/tracker |
| `--filter-list=FILE` | – | berkas filter gaya EasyList |
| `--optimize-images=on\|off` | on | re-encode + downscale gambar |
| `--max-image-width=N` | 1280 | batas lebar gambar |
| `--image-quality=N` | 72 | kualitas JPEG hasil re-encode |
| `--image-min-bytes=N` | 8k | abaikan gambar lebih kecil dari ini |
| `--minify-html` / `--minify-css` / `--minify-js` | on / on / off | minifikasi |
| `--lazy-load=on\|off` | on | sisipkan `loading="lazy" decoding="async"` |
| `--save-data=on\|off` | on | kirim header `Save-Data: on` |
| `--strip-metadata=on\|off` | on | buang EXIF/ICC/chunk PNG |
| `--block-third-party-cookies` | on | buang cookie untuk request pihak ketiga |
| `--video=auto\|block` | auto | `block` menghentikan unduhan video (hemat besar) |
| `--cache=on\|off` / `--cache-dir` / `--cache-max-bytes` | on / `~/.cache/astra` / 256M | cache disk |
| `--idle-exit-ms=N` | 0 | keluar sendiri saat idle |
| `--log-level=0..4`, `-v`, `-q` | 2 | verbositas |
| `--config=FILE` | `~/.config/astra/astra.conf` | berkas konfigurasi `kunci = nilai` |

Variabel lingkungan yang setara: `ASTRA_ENGINE`, `ASTRA_ENGINE_URL`, `ASTRA_PORT`, `ASTRA_MODE`,
`ASTRA_LITE`, `ASTRA_FILTER_LIST`, `ASTRA_CACHE_DIR`, `ASTRA_USER_AGENT`, … (lihat
`config_apply_env()` di [src/config.c](src/config.c)).

## 5. Hemat bandwidth: cara kerja dan hasil ukur

Astra memasang **intersepsi CDP** (`Fetch.enable` + `Fetch.continueRequest{interceptResponse:true}`)
sehingga setiap request bisa diputuskan sebelum dan sesudah diunduh:

1. **Request stage**
   * cocokkan URL dengan daftar filter → `Fetch.failRequest` (iklan/tracker/google-analytics/…);
   * cache disk: jika masih segar → `Fetch.fulfillRequest` **tanpa menyentuh jaringan**;
   * jika kadaluarsa → kirim `If-None-Match` / `If-Modified-Since` (revalidasi kondisional);
   * rapikan header: `Save-Data: on`, `Accept: image/avif,image/webp,…` (server/CDN akan
     mengirim AVIF/WebP dengan sendirinya), buang cookie pihak ketiga.
2. **Response stage**
   * gambar: decode (stb_image) → downscale ke `--max-image-width` (filter linear berkualitas,
     stb_image_resize2) → encode JPEG progresif kualitas `--image-quality`; bila hasilnya tidak
     lebih kecil, metadata (EXIF/ICC/chunk PNG) tetap dibuang; gambar dengan alpha tetap PNG;
   * HTML: minifikasi aman (komentar dibuang, whitespace dirapatkan, isi `<pre>/<script>/<style>`
     dipertahankan) + disuntik `loading="lazy" decoding="async"` pada `<img>`/`<iframe>`;
   * CSS: komentar dibuang, whitespace dirapatkan, `#ffffff` → `#fff`, `0px` → `0`;
   * JS: opsional (`--minify-js`), hanya buang komentar dengan state machine yang paham
     string/template/regex — **mati secara default** karena bersifat heuristik;
   * hasil transformasi disimpan di cache untuk kunjungan berikutnya.

### Hasil ukur nyata

**A. Pipeline optimizer (dapat diulang di mesin mana pun, tanpa browser):**

```
$ make measure            # = python3 tests/e2e/measure_savings.py
astra savings measurement (optimizer path, no browser required)

  asset                        original  delivered    saved  type
  photo-1.png (2400x1600)       1216.0K     283.9K    76.7%  image/jpeg
  photo-2.png (2000x1400)       1054.8K     275.1K    73.9%  image/jpeg
  photo-3.png (1600x1200)        894.0K     262.4K    70.6%  image/jpeg
  page.html                        6.7K       6.3K     5.7%  text/html
  theme.css                        2.0K       0.2K    92.5%  text/css

  total: 3173.6 KB -> 827.9 KB  (73.9% fewer bytes for the same content)
  astra counters: requests=5 blocked=0 imagesOptimized=3 textMinified=2
```

Ini byte **sebelum dan sesudah melewati kode Astra yang sama** yang dipakai saat menjelajah
(aset uji riil, bukan simulasi).

**B. End-to-end dengan browser sungguhan** (`astra bench`, dijalankan di CI pada halaman uji
berisi 3 foto besar + 1 klip WebM):

```
astra benchmark: http://127.0.0.1:8123/index.html
  metric                         lite on     lite off        delta
  requests                            5            6           +1
  blocked (ads)                       0            0
  images optimized                    3            0
  bytes on the wire (chrome)    2352395      8326184
  bytes handed to the renderer  1721427          n/a
  bytes before optimizing       8324717          n/a

  => lite mode moved 71.7% fewer bytes for the same page (2297.3 KB vs 8131.0 KB)
```

Kolom `lite off` diukur dengan penghitung jaringan milik Chrome sendiri (`Network.loadingFinished`),
bukan dengan perkiraan Astra, sehingga kedua angka berasal dari sumber yang sama. Penghitung
Astra hanya ada di pass lite: saat optimizer mati ia meneruskan body apa adanya, jadi ia tidak
pernah melihat (dan tidak bisa menghitung) byte yang lewat — karenanya `n/a`.

Angka akhirnya bergerak antar run (run lain di CI: **64,5%** — 2889,5 KB vs 8131,0 KB) karena
bagian halaman yang keburu diunduh (klip video, gambar di bawah lipatan) bergantung pada
waktu. Yang stabil adalah penghematan per gambar: 70,6–76,7% (lihat tabel A).

Jalankan `astra bench <url>` atau `astra open <url> --stats` untuk angka di perangkat/jaringan
Anda sendiri; metodologi dan cara membaca metrik ada di [docs/BANDWIDTH.md](docs/BANDWIDTH.md).
Blok iklan/tracker menambah penghematan di luar angka di atas pada halaman sungguhan (ukurannya
tidak dihitung karena request yang diblokir tidak pernah diunduh — sehingga laporan Astra
bersifat konservatif).

## 6. Pengujian

Semua pengujian berjalan otomatis di GitHub Actions (lihat `.github/workflows/`), dan sebagian
besar bisa dijalankan lokal:

| Tingkat | Perintah | Isi |
|---|---|---|
| Unit | `make test` | 141 pemeriksaan: JSON, base64/SHA-1, URL/eTLD+1, filter iklan, minifier, codec gambar (encode→decode balik), cache+eviction, framing/fragmentasi WebSocket, handshake, statistik, konfigurasi, HTTP |
| Control plane (e2e) | `make integration` | 51 pemeriksaan terhadap **mock CDP engine** (`tests/e2e/mock_engine.py`): endpoint `/json/*`, upgrade WebSocket, `Target.*` + pemetaan session, blokir iklan lewat Fetch, transcode gambar nyata (PNG 360 KB → JPEG 68 KB), minifikasi HTML, passthrough JS, akuntansi statistik, `astra open` CLI |
| Penghematan nyata | `make measure` | mendorong aset uji asli (3 foto PNG + HTML + CSS) melewati pipeline optimizer Astra dan memastikan total penghematan ≥ 25% (terukur: **73,9%**) |
| Puppeteer (tanpa browser) | `make puppeteer` | 10 pemeriksaan Puppeteer terhadap **mock engine**: discovery/auto-attach, terjemahan session, navigasi + lifecycle, domain `Astra` di sesi halaman, screenshot, multi-halaman (yang butuh renderer dilewati) — jalan dalam ~6 detik |
| Browser sungguhan (e2e) | `bash tests/e2e/run_real_browser_tests.sh` | Puppeteer + go-rod terhadap Chromium asli: navigasi, **gambar ter-decode**, **video berjalan** (`currentTime` maju), screenshot PNG, probe FPS rAF, multi-tab, statistik CDP, cache pada reload, mode headless **dan** mode penuh (Xvfb) |
| Bandwidth | `astra bench <url>` | perbandingan lite ON vs OFF pada halaman yang sama, diukur dari byte yang benar-benar lewat |
| Android | workflow *Termux* | cross-compile NDK → `.deb`/`.tar.xz`, inspeksi `file`, smoke test qemu (best effort), dan build native di dalam image Termux |

Bila sebuah job CI gagal, log lengkapnya dikomit ke branch `ci-diagnostics` (`logs/*.txt`) agar
mudah dibaca lewat API GitHub dari lingkungan yang tidak bisa mengakses penyimpanan log Actions
(lihat `.github/publish-log.sh`).

## 7. Struktur repositori

```
src/         core C11
  util.c/h        buffer, log, waktu, string, SHA-1, base64, URL, file
  json.c/h        parser/serializer JSON
  ws.c/h          WebSocket RFC 6455 (handshake, framing, fragmentasi)
  http.c/h        klien HTTP (discovery engine) + parser request (endpoint /json/*)
  engine.c/h      deteksi, launch, probe, shutdown engine
  cdp.c/h         control plane: HTTP discovery, WS proxy, pemetaan session, event loop
  optimizer.c/h   pipa hemat bandwidth (intersepsi Fetch)
  filters.c/h     pencocokan aturan iklan/tracker (+ daftar bawaan)
  image.c/h       re-encode/downscale gambar (stb)
  minify.c/h      minifikasi HTML/CSS/JS
  cache.c/h       cache disk + LRU + revalidasi
  stats.c/h       akuntansi bandwidth
  config.c/h      konfigurasi (CLI/env/berkas)      main.c  CLI
tests/       unit test + e2e (mock engine, Puppeteer, go-rod) + halaman uji
vendor/stb/  stb_image, stb_image_write, stb_image_resize2 (public domain)
scripts/     build Termux (native + NDK), packaging .deb
.github/     workflows CI
docs/        arsitektur, CDP, bandwidth, rendering, Termux
```

## 8. Batasan yang jujur

* Astra butuh engine Chromium-family; tanpa itu tidak bisa merender (pesan error akan
  menyarankan `pkg install chromium` di Termux atau `apt install chromium` di Linux).
* Saat `--lite=on`, domain `Fetch` CDP dikuasai Astra (intersepsi milik klien diparkir). Matikan
  dengan `--lite=off` bila skrip otomasi Anda butuh `Fetch.*` sendiri.
* Aturan filter mendukung bentuk `||domain^`, `||domain^$opsi`, `domain`. Aturan ber-*wildcard*,
  regex, dan yang spesifik-path **dilewati** (dihitung di `skipped_lines`) daripada diturunkan
  menjadi aturan domain yang terlalu luas — pasang EasyList/EasyPrivacy lewat `--filter-list`
  untuk cakupan penuh.
* Minifikasi JS heuristik → default off. Minifikasi HTML/CSS konservatif (aman secara semantik).
* Video diblokir hanya jika `--video=block`; selain itu video dilewatkan apa adanya (Astra tidak
  mentranscode video).
* Build Termux native di CI bergantung pada image `ghcr.io/termux/termux-docker` (best effort);
  jalur NDK deterministis dan selalu menghasilkan artifact.

## 9. Referensi

Arsitektur dan penyetelan flag dipelajari dari (kode tidak disalin):

* **Servo** — https://github.com/servo/servo (desain engine paralel)
* **Lightpanda** — https://github.com/lightpanda-io/browser (browser headless Zig, CDP-first)
* **Chromium** — DevTools Protocol, flag kompositor/hemat memori
* **termux/termux-packages** — https://github.com/termux/termux-packages (packaging Android/Termux)
* **stb** — https://github.com/nothings/stb (codec gambar, public domain)

## 10. Lisensi

MIT — lihat [LICENSE](LICENSE). Kode pihak ketiga: lihat [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES).
