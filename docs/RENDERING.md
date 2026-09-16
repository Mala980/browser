# Rendering: gambar, video, dan kelancaran

Astra tidak menggantikan kompositor engine — ia **memastikan engine dikonfigurasi optimal** dan
**membuktikan** bahwa gambar serta video benar-benar berjalan lewat pengujian otomatis.

## Flag yang dikirim ke engine

Dipasang oleh `src/engine.c` saat peluncuran:

**Kelancaran (saat `--gpu=on`, default)**

| Flag | Efek |
|---|---|
| `--enable-gpu-rasterization` | rasterisasi di GPU, bukan CPU |
| `--ignore-gpu-blocklist` | jangan menurunkan ke software hanya karena daftar blocklist lama |
| `--enable-zero-copy` | kirim tile ke kompositor tanpa salinan memori tambahan |
| `--num-raster-threads=2` | dua thread raster (seimbang untuk perangkat kecil) |
| `--disable-checker-imaging` | hindari "kotak abu-abu" saat menggulir cepat |
| `--disable-partial-raster` | raster penuh per tile: lebih stabil untuk screenshot |
| `--force-device-scale-factor=1` | hindari biaya piksel ganda di layar HiDPI |

**Stabilitas / hemat memori**

`--disable-dev-shm-usage`, `--disable-background-timer-throttling`,
`--disable-renderer-backgrounding`, `--disable-backgrounding-occluded-windows`,
`--disable-hang-monitor`, `--disable-ipc-flooding-protection`, `--disable-breakpad`,
`--metrics-recording-only`, `--disable-background-networking`, `--disable-component-update`,
`--disable-sync`, `--disable-domain-reliability`, `--mute-audio`, `--hide-scrollbars`,
`--autoplay-policy=no-user-gesture-required` (penting agar video uji bisa diputar tanpa klik),
`--no-sandbox` otomatis saat root atau di Android/Termux.

Tambahan Anda sendiri: `--extra-flags="--force-color-profile=srgb --disable-lcd-text"`.

## Gambar

* Semua kemampuan decode (JPEG, PNG, WebP, AVIF, GIF, SVG) berasal dari engine — hasilnya
  identik dengan Chromium.
* Astra hanya mengganti byte yang dikirim (lihat [BANDWIDTH.md](BANDWIDTH.md)) dan **selalu
  memverifikasi** bahwa gambar tetap utuh: pengujian Puppeteer/go-rod memastikan
  `naturalWidth > 0`, `complete === true`, dan `img.decode()` tidak menolak.

## Video

* Pemutaran (decode, sinkronisasi A/V, DRM) sepenuhnya milik engine.
* Astra mengaktifkan `--autoplay-policy=no-user-gesture-required` sehingga video uji dapat
  diputar otomatis, dan menyediakan `--video=block` untuk memblokir unduhan video.
* Pengujian: `tests/e2e/puppeteer/test.mjs` memanggil `video.play()`, menunggu 2 detik, lalu
  memastikan `currentTime` bertambah dan `videoWidth > 0` (frame benar-benar terdecode).
  go-rod menjalankan pemeriksaan yang sama (`tests/e2e/gorod/astra_test.go`).

## Mengukur kelancaran

Pengujian menyertakan probe `requestAnimationFrame` sederhana:

```js
const fps = await page.evaluate(() => new Promise(resolve => {
  let frames = 0; const start = performance.now();
  (function tick() {
    frames++;
    if (performance.now() - start < 1000) requestAnimationFrame(tick);
    else resolve(frames * 1000 / (performance.now() - start));
  })();
}));
```

Di CI (rasterizer software SwiftShader) angka absolutnya tidak berarti; yang diuji adalah bahwa
loop animasi berjalan (fps > 5) dan tidak ada error halaman. Untuk angka nyata, ukur di perangkat
Anda dengan halaman yang sama dengan dan tanpa Astra — Astra tidak menambah proses di jalur
kritis rendering (kecuali saat mentransformasi badan respons, yang terjadi sebelum frame
digambar).

Catatan tentang screenshot: dengan `--gpu=on` dan `--disable-partial-raster`,
`Page.captureScreenshot` menghasilkan gambar penuh tanpa kotak abu-abu. Mode headless baru
(`--headless=new`) dipakai karena itulah jalur yang sama dengan Chrome berjendela (lebih
setia pada rendering nyata daripada headless lama).
