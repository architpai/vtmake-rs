/* Fuzz test: vtmake-zlib (Z_PREFIX, symbols vtmake_z_*) against the system zlib 1.3.1.
 * Each case compresses the same data with the same calls in both libraries and
 * compares the output bytes. Usage: zh fuzz CASES [SEED]. See run.sh. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "zlib.h"            /* vtmake-zlib, with Z_PREFIX */

/* The system zlib functions, with other C names (asm labels). */
typedef struct z_stream_s sys_stream;
extern int sys_deflateInit2_(z_streamp, int, int, int, int, int, const char *, int) __asm__("deflateInit2_");
extern int sys_deflate(z_streamp, int) __asm__("deflate");
extern int sys_deflateEnd(z_streamp) __asm__("deflateEnd");
extern int sys_deflateReset(z_streamp) __asm__("deflateReset");
extern int sys_deflateParams(z_streamp, int, int) __asm__("deflateParams");
extern int sys_deflateSetDictionary(z_streamp, const Bytef *, uInt) __asm__("deflateSetDictionary");

static unsigned long long rng = 88172645463325252ULL;
static unsigned rnd(void) { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; return (unsigned)(rng >> 11); }


/* Compress in with the given stream functions, in random input/output chunks if chunk. */
static size_t run(int sys, z_streamp s, const unsigned char *in, size_t n, unsigned char *out, size_t cap, int chunk, unsigned seed, int params_at, int newlevel) {
    unsigned long long save = rng; rng = seed * 2654435761ULL + 1;
    size_t ip = 0, op = 0; int rc;
    s->total_out = 0;
    for (;;) {
        size_t ia = n - ip, oa = cap - op;
        if (chunk) { size_t a = 1 + rnd() % (chunk); if (a < ia) ia = a; size_t b = 1 + rnd() % (chunk); if (b < oa) oa = b; }
        s->next_in = (Bytef *)in + ip; s->avail_in = (uInt)ia;
        s->next_out = out + op; s->avail_out = (uInt)oa;
        int fl = (ip + ia == n) ? Z_FINISH : ((chunk && rnd() % 7 == 0) ? Z_SYNC_FLUSH : Z_NO_FLUSH);
        rc = sys ? sys_deflate(s, fl) : deflate(s, fl);
        ip += ia - s->avail_in; op += oa - s->avail_out;
        if (params_at && ip >= (size_t)params_at && params_at > 0) {
            if (sys) sys_deflateParams(s, newlevel, Z_DEFAULT_STRATEGY); else deflateParams(s, newlevel, Z_DEFAULT_STRATEGY);
            params_at = -1;
        }
        if (rc == Z_STREAM_END) break;
        if (rc != Z_OK && rc != Z_BUF_ERROR) { fprintf(stderr, "rc %d\n", rc); exit(1); }
    }
    rng = save;
    return op;
}

static void gen(unsigned char *b, size_t n, int kind) {
    size_t i;
    switch (kind) {
    case 0: for (i = 0; i < n; i++) b[i] = rnd(); break;                 /* random */
    case 1: for (i = 0; i < n; i++) b[i] = rnd() % 4; break;             /* small alphabet */
    case 2: for (i = 0; i < n; i++) b[i] = (i % 4 == 0) ? rnd() % 3 : 'a'; break;
    case 3: memset(b, rnd() & 255, n); break;                            /* runs */
    default:                                                              /* copies of earlier data */
        for (i = 0; i < n && i < 64; i++) b[i] = rnd();
        while (i < n) {
            if (rnd() % 3 == 0) { b[i] = rnd() % 16; i++; continue; }
            size_t d = 1 + rnd() % (i < 40000 ? i : 40000), l = 3 + rnd() % 300;
            for (; l && i < n; l--, i++) b[i] = b[i - d];
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (!strcmp(argv[1], "fuzz")) {       /* fuzz N: random data, levels, chunks, resets */
        long iters = atol(argv[2]);
        if (argc > 3) rng = 88172645463325252ULL ^ (unsigned long long)atol(argv[3]) * 0x9E3779B97F4A7C15ULL;
        unsigned char *in = malloc(300000), *o1 = malloc(400000), *o2 = malloc(400000);
        z_stream a, b; memset(&a, 0, sizeof a); memset(&b, 0, sizeof b);
        int lvl = 6, ml = 8, wb = 31;
        deflateInit2(&a, lvl, Z_DEFLATED, wb, ml, Z_DEFAULT_STRATEGY);
        sys_deflateInit2_(&b, lvl, Z_DEFLATED, wb, ml, Z_DEFAULT_STRATEGY, ZLIB_VERSION, sizeof b);
        for (long it = 0; it < iters; it++) {
            if (rnd() % 50 == 0) {   /* new stream with other parameters */
                deflateEnd(&a); sys_deflateEnd(&b);
                lvl = (rnd() % 4 == 0) ? (int)(rnd() % 10) : 6;
                ml = (rnd() % 4 == 0) ? 1 + (int)(rnd() % 9) : 8;
                int wbits = (rnd() % 4 == 0) ? 9 + (int)(rnd() % 7) : 15;
                wb = (rnd() % 3 == 0) ? -wbits : (rnd() % 2 ? wbits + 16 : wbits);
                memset(&a, 0, sizeof a); memset(&b, 0, sizeof b);
                if (deflateInit2(&a, lvl, Z_DEFLATED, wb, ml, Z_DEFAULT_STRATEGY) != Z_OK) exit(3);
                if (sys_deflateInit2_(&b, lvl, Z_DEFLATED, wb, ml, Z_DEFAULT_STRATEGY, ZLIB_VERSION, sizeof b) != Z_OK) exit(3);
            } else {
                deflateReset(&a); sys_deflateReset(&b);
            }
            size_t n;
            unsigned r = rnd() % 10;
            n = r < 5 ? rnd() % 4000 : r < 8 ? rnd() % 40000 : rnd() % 300000;
            gen(in, n, rnd() % 6);
            int chunk = (rnd() % 3 == 0) ? 1 + (int)(rnd() % 5000) : 0;
            unsigned seed = rnd();
            if (rnd() % 20 == 0 && n > 16) {
                uInt dl = 1 + rnd() % (n / 2);
                deflateSetDictionary(&a, in, dl); sys_deflateSetDictionary(&b, in, dl);
            }
            int pa = (rnd() % 20 == 0 && n > 10) ? (int)(1 + rnd() % n) : 0, nl = rnd() % 10;
            size_t l1 = run(0, &a, in, n, o1, 400000, chunk, seed, pa, nl);
            size_t l2 = run(1, &b, in, n, o2, 400000, chunk, seed, pa, nl);
            if (l1 != l2 || memcmp(o1, o2, l1)) {
                fprintf(stderr, "DIFF it=%ld n=%zu lvl=%d ml=%d wb=%d chunk=%d pa=%d\n", it, n, lvl, ml, wb, chunk, pa);
                return 1;
            }
            if (pa) { /* deflateParams changes the level; restore */
                deflateParams(&a, lvl, Z_DEFAULT_STRATEGY); sys_deflateParams(&b, lvl, Z_DEFAULT_STRATEGY);
            }
        }
        printf("fuzz ok: %ld cases\n", iters);
        return 0;
    }
    return 2;
}
