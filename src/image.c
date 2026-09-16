/* astra/image.c - stb-backed image optimizer */
#include "image.h"

#include <math.h>
#include <stdlib.h>

#define STB_IMAGE_IMPLEMENTATION
#define STBI_NO_STDIO
#define STBI_NO_HDR
#define STBI_NO_PSD
#define STBI_NO_GIF
#define STBI_NO_PIC
#define STBI_NO_PNM
#include "../vendor/stb/stb_image.h"

#define STB_IMAGE_WRITE_IMPLEMENTATION
#define STBI_WRITE_NO_STDIO
#include "../vendor/stb/stb_image_write.h"

#define STB_IMAGE_RESIZE_IMPLEMENTATION
#define STBIR_MALLOC(sz, ctx) malloc(sz)
#define STBIR_FREE(p, ctx) free(p)
#include "../vendor/stb/stb_image_resize2.h"

static void write_cb(void *context, void *data, int size) {
    buf_t *b = (buf_t *)context;
    buf_append(b, data, (size_t)size);
}

void image_out_free(img_out_t *o) {
    if (!o) return;
    free(o->data);
    o->data = NULL;
    o->len = 0;
    o->ok = 0;
}

int image_probe(const uint8_t *in, size_t in_len, int *w, int *h) {
    int c = 0;
    return stbi_info_from_memory(in, (int)in_len, w, h, &c);
}

int image_optimize(const uint8_t *in, size_t in_len, const char *in_mime,
                   const img_opt_cfg *cfg, img_out_t *out) {
    memset(out, 0, sizeof(*out));
    out->in_len = in_len;
    if (!in || in_len < 64) return 0;
    if (in_mime && (astr_contains_ci(in_mime, "svg") || astr_contains_ci(in_mime, "webp") ||
                    astr_contains_ci(in_mime, "avif")))
        return 0; /* already compact or vector: never re-encode */
    if (cfg && cfg->min_bytes && in_len < cfg->min_bytes) return 0;

    int ow = 0, oh = 0, comp = 0;
    uint8_t *pix = stbi_load_from_memory(in, (int)in_len, &ow, &oh, &comp, 4);
    if (!pix || ow <= 0 || oh <= 0) {
        free(pix);
        return 0; /* unsupported / broken: metadata strip will be tried by caller */
    }
    out->ow = ow;
    out->oh = oh;

    int max_w = cfg && cfg->max_width > 0 ? cfg->max_width : 0;
    int quality = cfg && cfg->quality > 0 ? cfg->quality : 72;

    int tw = ow, th = oh;
    if (max_w && ow > max_w) {
        tw = max_w;
        th = (int)((double)oh * (double)max_w / (double)ow);
        if (th < 1) th = 1;
    }

    /* alpha present? */
    int has_alpha = 0;
    for (int i = 3; i < ow * oh * 4; i += 4) {
        if (pix[i] != 255) {
            has_alpha = 1;
            break;
        }
    }

    uint8_t *src = pix;
    uint8_t *resized = NULL;
    if (tw != ow) {
        resized = (uint8_t *)malloc((size_t)tw * (size_t)th * 4);
        if (!resized) {
            stbi_image_free(pix);
            return 0;
        }
        if (!stbir_resize_uint8_linear(pix, ow, oh, ow * 4, resized, tw, th, tw * 4, STBIR_RGBA)) {
            free(resized);
            stbi_image_free(pix);
            return 0;
        }
        src = resized;
    }

    buf_t enc;
    buf_init(&enc);
    uint64_t t0 = now_ms();
    if (has_alpha) {
        /* keep transparency: lossless PNG */
        if (!stbi_write_png_to_func(write_cb, &enc, tw, th, 4, src, tw * 4)) {
            buf_free(&enc);
            free(resized);
            stbi_image_free(pix);
            return 0;
        }
        snprintf(out->mime, sizeof(out->mime), "image/png");
    } else {
        /* drop alpha, encode progressive JPEG */
        uint8_t *rgb = src;
        uint8_t *packed = NULL;
        if (src == pix || src == resized) {
            packed = (uint8_t *)malloc((size_t)tw * (size_t)th * 3);
            if (!packed) {
                buf_free(&enc);
                free(resized);
                stbi_image_free(pix);
                return 0;
            }
            for (int i = 0, j = 0; i < tw * th * 4; i += 4, j += 3) {
                packed[j] = src[i];
                packed[j + 1] = src[i + 1];
                packed[j + 2] = src[i + 2];
            }
            rgb = packed;
        }
        stbi_write_jpg_to_func(write_cb, &enc, tw, th, 3, rgb, quality);
        free(packed);
        snprintf(out->mime, sizeof(out->mime), "image/jpeg");
    }
    out->ms = (double)(now_ms() - t0);
    out->w = tw;
    out->h = th;

    free(resized);
    stbi_image_free(pix);

    if (enc.len == 0 || enc.len >= in_len) {
        /* re-encoding did not help: keep the original bytes */
        buf_free(&enc);
        return 0;
    }
    out->data = enc.data;
    out->len = enc.len;
    out->ok = 1;
    return 1;
}

/* ------------------------------------------------- metadata / chunk stripper */

static int strip_jpeg(const uint8_t *in, size_t len, buf_t *out) {
    if (len < 4 || in[0] != 0xFF || in[1] != 0xD8) return 0;
    size_t i = 2;
    int changed = 0;
    buf_appendc(out, (char)0xFF);
    buf_appendc(out, (char)0xD8);
    while (i + 4 <= len) {
        if (in[i] != 0xFF) break;
        uint8_t marker = in[i + 1];
        if (marker == 0xD8 || marker == 0x01 || (marker >= 0xD0 && marker <= 0xD7)) {
            i += 2;
            continue;
        }
        if (marker == 0xD9) { /* EOI */
            buf_appendc(out, (char)0xFF);
            buf_appendc(out, (char)0xD9);
            i += 2;
            break;
        }
        if (marker == 0xDA) { /* start of scan: copy the rest verbatim */
            buf_append(out, in + i, len - i);
            return changed;
        }
        size_t seglen = ((size_t)in[i + 2] << 8) | in[i + 3];
        if (seglen < 2 || i + 2 + seglen > len) break;
        int keep = 1;
        /* drop APP1 (EXIF/XMP), APP2 (ICC), APP13 (IPTC), COM */
        if ((marker >= 0xE1 && marker <= 0xEF) || marker == 0xFE) keep = 0;
        if (marker == 0xE0) keep = 1; /* JFIF / JFXX keep (small, often needed) */
        if (keep) {
            buf_append(out, in + i, 2 + seglen);
        } else {
            changed = 1;
        }
        i += 2 + seglen;
    }
    return changed && out->len > 4;
}

static int strip_png(const uint8_t *in, size_t len, buf_t *out) {
    static const uint8_t sig[8] = {0x89, 'P', 'N', 'G', 0x0D, 0x0A, 0x1A, 0x0A};
    if (len < 8 || memcmp(in, sig, 8) != 0) return 0;
    static const char *keep[] = {"IHDR", "PLTE", "IDAT", "IEND", "tRNS", "cHRM", "gAMA",
                                 "iCCP", "sBIT", "sRGB", "bKGD", "pHYs", "acTL", "fcTL",
                                 "fdAT", NULL};
    buf_append(out, sig, 8);
    size_t i = 8;
    int changed = 0;
    while (i + 8 <= len) {
        uint32_t clen = ((uint32_t)in[i] << 24) | ((uint32_t)in[i + 1] << 16) |
                        ((uint32_t)in[i + 2] << 8) | in[i + 3];
        char type[5];
        memcpy(type, in + i + 4, 4);
        type[4] = 0;
        if (clen > len || i + 12 + clen > len) break;
        int k = 0;
        for (int j = 0; keep[j]; j++)
            if (strcmp(type, keep[j]) == 0) {
                k = 1;
                break;
            }
        if (k)
            buf_append(out, in + i, 12 + clen);
        else
            changed = 1;
        i += 12 + clen;
        if (strcmp(type, "IEND") == 0) break;
    }
    return changed && out->len > 8;
}

int image_strip_metadata(const uint8_t *in, size_t in_len, const char *in_mime, buf_t *out) {
    if (!in || in_len < 16) return 0;
    if (in_mime && astr_contains_ci(in_mime, "png")) return strip_png(in, in_len, out);
    return strip_jpeg(in, in_len, out);
}
