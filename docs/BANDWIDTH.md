# Pipa hemat bandwidth

## Metrik

| Metrik | Arti |
|---|---|
| `requests` | jumlah request yang diinspeksi |
| `blocked` | request yang digagalkan (`Fetch.failRequest`) oleh filter iklan/tracker atau kebijakan video |
| `cacheHits` | request yang dijawab penuh dari disk (0 byte jaringan) |
| `revalidated` | request yang dijawab 304 oleh server lalu disajikan dari cache |
| `imagesOptimized` | gambar yang di-decode ulang dan di-encode lebih kecil |
| `textMinified` | dokumen HTML/CSS yang dirapatkan |
| `bytesOriginal` | byte yang **seharusnya** diunduh (ukuran asli tiap respons) |
| `bytesDelivered` | byte yang benar-benar diserahkan ke renderer setelah optimasi |
| `bytesSaved*` | rincian penghematan per kategori (gambar, teks, cache, metadata) |
| `savingPct` | `(bytesOriginal - bytesDelivered + bytesSavedCache + bytesBlocked) / (bytesOriginal + bytesBlocked)` |

Catatan jujur: untuk request yang **diblokir** ukurannya tidak diketahui (tidak pernah diunduh),
maka `blocked` dihitung sebagai jumlah request, bukan byte. Karena itu persen penghematan yang
dilaporkan bersifat **konservatif**.

## Teknik yang dipakai

### 1. Blokir iklan & tracker

* 142 aturan bawaan (`src/filters_builtin.h`) + berkas gaya EasyList lewat `--filter-list`.
* Bentuk yang didukung: `||domain^`, `||domain^$opsi`, `domain`. Opsi yang dipahami:
  `third-party`, `script`, `image`, `stylesheet`, `xmlhttprequest`, `font`, `media`.
* Aturan `@@` (pengecualian) selalu menang.
* Aturan dengan *wildcard*, regex, atau path spesifik **dilewati** dan dihitung di
  `skipped_lines` — sengaja, karena menurunkannya menjadi aturan domain akan menghasilkan
  pengecualian yang terlalu luas (berbahaya).

### 2. Cache disk dengan revalidasi

* Kunci = SHA-1(url); badan + meta (status, content-type, ETag, Last-Modified, expiry).
* Segar (`max-age` belum lewat) → dijawab dari disk.
* Kadaluarsa → dikirim dengan `If-None-Match`/`If-Modified-Since`; bila server menjawab 304,
  Astra menjawab 200 dari cache (header `X-Astra-Cache: REVALIDATED`).
* LRU berdasarkan mtime, eviction saat total melebihi `--cache-max-bytes` (default 256 MB).

### 3. Optimasi gambar

* `Accept: image/avif,image/webp,…` dikirim lebih dulu: CDN (Cloudflare, WordPress, dsb) akan
  membalas AVIF/WebP dengan sendirinya — penghematan tanpa biaya CPU di sisi klien.
* Bila respons tetap JPEG/PNG: decode → (downscale ke `--max-image-width` dengan filter linear
  stb_image_resize2) → encode JPEG progresif kualitas `--image-quality`.
* Hasil re-encode **hanya dipakai bila lebih kecil** dari aslinya; kalau tidak, gambar asli
  tetap dikirim (metadata yang dibuang bila `--strip-metadata=on`).
* Gambar ber-alpha (PNG transparan) tetap PNG. SVG/WebP/AVIF tidak di-encode ulang.
* Gambar di bawah `--image-min-bytes` (default 8 KB) dilewatkan.

### 4. Minifikasi teks

* HTML: komentar dibuang (kecuali conditional comment), whitespace dirapatkan, isi
  `<pre>/<textarea>/<script>/<style>` dipertahankan apa adanya, `<!DOCTYPE>` dijaga.
* CSS: komentar dibuang, whitespace dirapatkan, `#ffffff` → `#fff`, `0px/0em/0rem/0pt` → `0`,
  titik koma sebelum `}` dihapus.
* JS: hanya pembuangan komentar dengan state machine yang paham string, template literal, dan
  regex — **mati secara default** (`--minify-js=on` bila diperlukan).

### 5. Header hemat

* `Save-Data: on` hanya untuk dokumen dan gambar (bukan XHR/fetch, supaya tidak memicu
  preflight CORS).
* Cookie pihak ketiga dibuang (`--block-third-party-cookies`).
* `Accept-Encoding` dinormalisasi ke `gzip, deflate, br, zstd`.

### 6. Lazy loading

`loading="lazy" decoding="async"` disuntikkan ke `<img>`/`<iframe>` yang belum memilikinya.

### 7. Kebijakan video

`--video=block` menggagalkan request `Media` (hemat sangat besar di jaringan mahal). Default
`auto` = video dilewatkan apa adanya; Astra tidak mentranscode video.

## Angka yang terukur

`make measure` (scripts: `tests/e2e/measure_savings.py`) mendorong aset uji asli melalui jalur
optimizer yang sama persis dengan yang dipakai saat menjelajah:

| Aset | Asli | Terkirim | Hemat |
|---|---|---|---|
| photo-1.png 2400×1600 | 1216,0 KB | 283,9 KB | 76,7 % |
| photo-2.png 2000×1400 | 1054,8 KB | 275,1 KB | 73,9 % |
| photo-3.png 1600×1200 | 894,0 KB | 262,4 KB | 70,6 % |
| page.html | 6,7 KB | 6,3 KB | 5,7 % |
| theme.css | 2,0 KB | 0,2 KB | 92,5 % |
| **Total** | **3173,6 KB** | **827,9 KB** | **73,9 %** |

Penghematan HTML tampak kecil karena minifier Astra sengaja konservatif (tidak pernah mengubah
semantik markup): ia hanya membuang komentar, merapatkan whitespace, dan menyuntikkan atribut
lazy-load. Pada halaman berita sungguhan (banyak komentar template dan indentasi) hasilnya
biasanya 10–25 %.

## Mengukur sendiri

```bash
astra bench https://example.com          # lite on vs off, halaman sama, cache berbeda
astra open https://example.com --stats   # laporan sekali jalan
curl -s http://127.0.0.1:9222/stats | jq
```

`astra bench` menjalankan dua_pass pada engine yang sama:

1. lite **on** → navigasi → rekam statistik
2. `Astra` mematikan lite (`Fetch.disable` pada sesi yang ada) → `Page.reload{ignoreCache:true}`
   → rekam statistik

yang membandingkan byte yang benar-benar berpindah untuk konten yang identik.

## Menyetel

| Skenario | Saran |
|---|---|
| Foto berat, kuota mahal | `--max-image-width=720 --image-quality=55 --image-min-bytes=2048` |
| Artikel/berita teks | `--minify-css=on --minify-html=on --block-ads=on` |
| Otomasi scraping | `--lite=off --video=block` |
| Perangkat sangat lambat | `--image-min-bytes=32768` (kurangi kerja CPU re-encode) |
| Termux/Android | `--video=block --max-image-width=640 --cache-max-bytes=64M` |
