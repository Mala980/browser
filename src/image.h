/* astra/image.h - image re-encoding / downscaling (powered by stb) */
#ifndef ASTRA_IMAGE_H
#define ASTRA_IMAGE_H

#include "util.h"

typedef struct {
    int ok; /* 1 = caller should use data/len/mime, 0 = keep original */
    uint8_t *data;
    size_t len;
    char mime[32];
    int w, h;      /* output size */
    int ow, oh;    /* original size */
    size_t in_len; /* original byte size */
    double ms;
} img_out_t;

typedef struct {
    int max_width;   /* 0 = don't downscale */
    int quality;     /* jpeg quality 1..100 */
    size_t min_bytes;/* ignore images smaller than this */
    int to_webp;     /* reserved: 0 = jpeg/png output */
} img_opt_cfg;

int image_optimize(const uint8_t *in, size_t in_len, const char *in_mime,
                   const img_opt_cfg *cfg, img_out_t *out);
void image_out_free(img_out_t *o);
int image_strip_metadata(const uint8_t *in, size_t in_len, const char *in_mime, buf_t *out);
int image_probe(const uint8_t *in, size_t in_len, int *w, int *h);

#endif /* ASTRA_IMAGE_H */
