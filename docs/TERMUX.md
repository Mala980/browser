# Android / Termux (aarch64)

Astra berjalan di Termux sebagai binary bionic biasa. Rendering tetap dilakukan oleh engine
Chromium-family yang dipasang dari repositori Termux.

## 1. Pasang

```bash
pkg update
pkg install chromium            # engine rendering (wajib)
pkg install clang make git      # hanya bila build dari source
```

## 2. Dapatkan binary

**A. Unduh artifact CI** — workflow *Termux (Android aarch64)* → artifact `astra-termux-aarch64`
berisi `astra_0.1.0_aarch64.deb` dan `astra-0.1.0-termux-aarch64.tar.xz`.

```bash
# dari Termux
dpkg -i astra_0.1.0_aarch64.deb
# atau manual
tar xf astra-0.1.0-termux-aarch64.tar.xz
chmod +x astra && mv astra $PREFIX/bin/
```

**B. Build di HP (paling andal)**

```bash
git clone https://github.com/Mala980/browser.git && cd browser
./scripts/build-termux.sh --package
./build/astra-test          # verifikasi unit test di perangkat
```

**C. Cross compile (di CI atau PC)** — `scripts/build-termux-ndk.sh` dengan `ANDROID_NDK_HOME`
(API 24+, target `aarch64-linux-android`). Binary statis bila memungkinkan; bila tidak, ia
terhubung dinamis ke libc/libm bionic yang tersedia di perangkat Android mana pun.

## 3. Jalankan

```bash
# kontrol plane headless (Puppeteer/go-rod/curl menyambung ke sini)
astra serve --port 9222

# sekali jalan dengan laporan hemat bandwidth
astra open https://example.com --wait 2000 --stats --screenshot /sdcard/shot.png

# mode berjendela (butuh X server: pkg install x11-repo && pkg install termux-x11)
astra serve --mode=full --port 9222
```

## 4. Hal khusus Android

* Astra menambahkan `--no-sandbox` otomatis bila mendeteksi Android (ada `$PREFIX` atau
  `/system/bin/app_process`) atau bila berjalan sebagai root.
* Deteksi engine memeriksa `$PREFIX/bin/chromium` terlebih dahulu, baru PATH.
* Default port 9222 sering bentrok dengan hal lain di HP; gunakan `--port` bila perlu.
* Simpan cache di penyimpanan internal (`--cache-dir ~/.cache/astra`); hindari /sdcard untuk
  performa.
* Baterai & kuota: `astra serve --video=block --max-image-width=640 --cache-max-bytes=64M`.

## 5. Troubleshooting

| Gejala | Solusi |
|---|---|
| `no Chromium-family engine found` | `pkg install chromium`, atau `astra serve --engine /path/to/chrome` |
| engine kelip di awal | jalankan dengan `--no-sandbox`, atau setel `ASTRA_ENGINE_LOG=/tmp/engine.log` lalu lihat isinya |
| `/json/version` tidak bisa diakses | pastikan `curl http://127.0.0.1:9222/json/version` dari Termux (bukan dari aplikasi lain yang terisolasi) |
| mode berjendela gagal | butuh X server (`termux-x11` / VNC) dan `DISPLAY` yang benar |
| video tidak diputar | engine dari Termux mungkin tanpa codec berpatenan; gunakan WebM/VP9 atau Chrome resmi |
| lambat saat membuka halaman foto besar | naikkan `--image-min-bytes` agar lebih sedikit gambar yang di-encode ulang |

## 6. Verifikasi di perangkat

```bash
./build/astra-test                       # 141 unit check
python3 tests/e2e/test_local.py          # 40 check control plane (mock engine)
astra bench http://example.com           # ukur penghematan di jaringan Anda
```
