/* One-shot still-image encode on top of SVT-AV1 (Tritium fork).
 *
 * The encoder configuration struct differs between SVT-AV1 forks, so Rust
 * never touches it: this shim owns it and applies every setting by name
 * through svt_av1_enc_parse_parameter. */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "EbSvtAv1Enc.h"

typedef struct JxlallSvtParam {
    const char *key;
    const char *value;
} JxlallSvtParam;

/* Luma-only film grain table (photon noise). */
typedef struct JxlallSvtGrain {
    int32_t num_y_points;
    int32_t scaling_points_y[14][2];
    int32_t scaling_shift;
    int32_t ar_coeff_shift;
    int32_t overlap_flag;
    int32_t chroma_scaling_from_luma;
    int32_t clip_to_restricted_range;
    int32_t bit_depth;
    uint16_t random_seed;
} JxlallSvtGrain;

typedef struct JxlallSvtPicture {
    uint32_t width;
    uint32_t height;
    uint32_t bit_depth;    /* 8 or 10; 10-bit samples are 16-bit little endian */
    uint32_t color_format; /* EbColorFormat: 1 = 4:2:0 */
    const void *y;
    const void *u;
    const void *v;
    uint32_t y_stride; /* in samples */
    uint32_t uv_stride;
    uint32_t uv_height;
} JxlallSvtPicture;

enum {
    JXLALL_SVT_OK = 0,
    JXLALL_SVT_INIT_HANDLE = 1,
    JXLALL_SVT_BAD_PARAMETER = 2,
    JXLALL_SVT_SET_PARAMETER = 3,
    JXLALL_SVT_INIT = 4,
    JXLALL_SVT_SEND = 5,
    JXLALL_SVT_RECEIVE = 6,
    JXLALL_SVT_OUT_OF_MEMORY = 7,
};

typedef void (*JxlallSvtLogFn)(int level, const char *message);
static JxlallSvtLogFn g_log;

static void log_callback(void *context, SvtAv1LogLevel level, const char *tag, const char *fmt, va_list args) {
    (void)context;
    (void)tag;
    if (!g_log || level > SVT_AV1_LOG_WARN) {
        return;
    }
    char buf[1024];
    vsnprintf(buf, sizeof buf, fmt, args);
    g_log((int)level, buf);
}

/* Global: call once before the first encode. */
void jxlall_svt_set_logger(JxlallSvtLogFn f) {
    g_log = f;
    svt_av1_set_log_callback(log_callback, NULL);
}

void jxlall_svt_free(uint8_t *p) { free(p); }

static int append(uint8_t **out, size_t *len, size_t *cap, const uint8_t *data, size_t n) {
    if (*len + n > *cap) {
        size_t c = *cap ? *cap * 2 : 1 << 16;
        while (c < *len + n) {
            c *= 2;
        }
        uint8_t *p = (uint8_t *)realloc(*out, c);
        if (!p) {
            return 0;
        }
        *out = p;
        *cap = c;
    }
    memcpy(*out + *len, data, n);
    *len += n;
    return 1;
}

int jxlall_svt_encode(const JxlallSvtParam *params, size_t n_params, const JxlallSvtGrain *grain,
                      const JxlallSvtPicture *pic, uint8_t **out, size_t *out_len, size_t *failed_param) {
    *out = NULL;
    *out_len = 0;
    EbComponentType *h = NULL;
    EbSvtAv1EncConfiguration cfg;
    memset(&cfg, 0, sizeof cfg);
    if (svt_av1_enc_init_handle(&h, &cfg) != EB_ErrorNone) {
        return JXLALL_SVT_INIT_HANDLE;
    }
    int rc = JXLALL_SVT_OK;
    int initialized = 0;
    AomFilmGrain fg; /* referenced by cfg until the encoder is torn down */
    size_t cap = 0;

    cfg.source_width = pic->width;
    cfg.source_height = pic->height;
    cfg.encoder_bit_depth = (uint8_t)pic->bit_depth;
    cfg.encoder_color_format = (EbColorFormat)pic->color_format;
    for (size_t i = 0; i < n_params; i++) {
        if (svt_av1_enc_parse_parameter(&cfg, params[i].key, params[i].value) != EB_ErrorNone) {
            *failed_param = i;
            rc = JXLALL_SVT_BAD_PARAMETER;
            goto done;
        }
    }
    if (grain) {
        memset(&fg, 0, sizeof fg);
        fg.apply_grain = 1;
        fg.update_parameters = 1;
        fg.num_y_points = grain->num_y_points;
        memcpy(fg.scaling_points_y, grain->scaling_points_y, sizeof fg.scaling_points_y);
        fg.scaling_shift = grain->scaling_shift;
        fg.ar_coeff_lag = 0;
        fg.ar_coeff_shift = grain->ar_coeff_shift;
        fg.overlap_flag = grain->overlap_flag;
        fg.chroma_scaling_from_luma = grain->chroma_scaling_from_luma;
        fg.clip_to_restricted_range = grain->clip_to_restricted_range;
        fg.bit_depth = grain->bit_depth;
        fg.random_seed = grain->random_seed;
        cfg.fgs_table = &fg;
    }
    if (svt_av1_enc_set_parameter(h, &cfg) != EB_ErrorNone) {
        rc = JXLALL_SVT_SET_PARAMETER;
        goto done;
    }
    if (svt_av1_enc_init(h) != EB_ErrorNone) {
        rc = JXLALL_SVT_INIT;
        goto done;
    }
    initialized = 1;

    const size_t bytes = pic->bit_depth > 8 ? 2 : 1;
    EbSvtIOFormat io;
    memset(&io, 0, sizeof io);
    io.luma = (uint8_t *)pic->y;
    io.cb = (uint8_t *)pic->u;
    io.cr = (uint8_t *)pic->v;
    io.y_stride = pic->y_stride;
    io.cb_stride = pic->uv_stride;
    io.cr_stride = pic->uv_stride;

    EbBufferHeaderType in;
    memset(&in, 0, sizeof in);
    in.size = sizeof in;
    in.p_buffer = (uint8_t *)&io;
    in.n_filled_len = (uint32_t)(bytes * ((size_t)pic->y_stride * pic->height +
                                          2 * (size_t)pic->uv_stride * pic->uv_height));
    in.pic_type = EB_AV1_KEY_PICTURE;
    if (svt_av1_enc_send_picture(h, &in) != EB_ErrorNone) {
        rc = JXLALL_SVT_SEND;
        goto done;
    }
    EbBufferHeaderType eos;
    memset(&eos, 0, sizeof eos);
    eos.size = sizeof eos;
    eos.flags = EB_BUFFERFLAG_EOS;
    if (svt_av1_enc_send_picture(h, &eos) != EB_ErrorNone) {
        rc = JXLALL_SVT_SEND;
        goto done;
    }
    for (;;) {
        EbBufferHeaderType *pkt = NULL;
        EbErrorType res = svt_av1_enc_get_packet(h, &pkt, 1);
        if (res != EB_ErrorNone || !pkt) {
            rc = JXLALL_SVT_RECEIVE;
            break;
        }
        int eos_seen = (pkt->flags & EB_BUFFERFLAG_EOS) != 0;
        if (pkt->p_buffer && pkt->n_filled_len && !append(out, out_len, &cap, pkt->p_buffer, pkt->n_filled_len)) {
            rc = JXLALL_SVT_OUT_OF_MEMORY;
        }
        svt_av1_enc_release_out_buffer(&pkt);
        if (eos_seen || rc != JXLALL_SVT_OK) {
            break;
        }
    }

done:
    if (initialized) {
        svt_av1_enc_deinit(h);
    }
    svt_av1_enc_deinit_handle(h);
    if (rc != JXLALL_SVT_OK) {
        free(*out);
        *out = NULL;
        *out_len = 0;
    }
    return rc;
}
