/*
 * wqlc_c — minimal WQL evaluator in C, using the wql-capi library.
 *
 * Usage:
 *   wqlc_c eval -q <query> [-s <schema.bin> -m <message>] [--delimited]
 *
 * Reads protobuf from stdin, writes to stdout.
 *   Single mode:    reads all of stdin as one message. Exit 0=pass, 1=filtered, 2=error.
 *   Delimited mode: streams varint length-prefixed records one at a time.
 */

#include "../include/wql.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ── Varint I/O ── */

/* Read a varint from FILE*, one byte at a time. Returns 0 on success,
   -1 on clean EOF (no bytes read), -2 on truncated/malformed varint. */
static int read_varint(FILE *f, uint64_t *out) {
    uint64_t val = 0;
    unsigned shift = 0;
    for (;;) {
        int c = fgetc(f);
        if (c == EOF) {
            return shift == 0 ? -1 : -2; /* clean EOF vs truncated */
        }
        val |= (uint64_t)(c & 0x7F) << shift;
        if ((c & 0x80) == 0) {
            *out = val;
            return 0;
        }
        shift += 7;
        if (shift >= 64) return -2;
    }
}

static void write_varint(FILE *f, uint64_t val) {
    do {
        uint8_t byte = val & 0x7F;
        val >>= 7;
        if (val) byte |= 0x80;
        fputc(byte, f);
    } while (val);
}

/* Read exactly n bytes from FILE*. Returns 0 on success, -1 on short read. */
static int read_exact(FILE *f, uint8_t *buf, size_t n) {
    size_t got = fread(buf, 1, n, f);
    return got == n ? 0 : -1;
}

/* ── Read all of stdin (for single-message mode) ── */

static uint8_t *read_all_stdin(size_t *out_len) {
    *out_len = 0;
    size_t cap = 4096, len = 0;
    uint8_t *buf = malloc(cap);
    if (!buf) return NULL;
    for (;;) {
        size_t n = fread(buf + len, 1, cap - len, stdin);
        len += n;
        if (n == 0) break;
        if (len == cap) {
            cap *= 2;
            uint8_t *tmp = realloc(buf, cap);
            if (!tmp) { free(buf); *out_len = 0; return NULL; }
            buf = tmp;
        }
    }
    *out_len = len;
    return buf;
}

/* ── Read file into buffer ── */

static uint8_t *read_file(const char *path, size_t *out_len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc((size_t)sz);
    if (!buf) { fclose(f); return NULL; }
    size_t n = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    *out_len = n;
    return buf;
}

/* ── Process a single record ──
 * Returns:
 *   0 = ok (matched / projection done)
 *   1 = filtered out
 *   2 = error
 */
static int process_record(const wql_program_t *prog,
                          const uint8_t *input, size_t input_len,
                          uint8_t *output, size_t output_cap,
                          struct wql_eval_result_t *result) {
    char *err = NULL;
    int rc = wql_eval(prog, input, input_len, output, output_cap, result, &err);
    if (rc < 0) {
        fprintf(stderr, "wqlc_c: %s\n", err);
        wql_errmsg_free(err);
        return 2;
    }
    return result->matched ? 0 : 1;
}

/* ── Single message eval ── */

static int has_projection(const wql_program_t *prog) {
    struct wql_program_info_t info;
    memset(&info, 0, sizeof(info));
    wql_program_info(prog, &info);
    return (info.program_type & WQL_PROGRAM_PROJECT) != 0;
}

static int eval_single(const wql_program_t *prog) {
    size_t input_len = 0;
    uint8_t *input = read_all_stdin(&input_len);
    if (!input && input_len > 0) {
        fprintf(stderr, "wqlc_c: failed to read stdin\n");
        return 2;
    }

    if (input_len > SIZE_MAX / 2 - 128) {
        fprintf(stderr, "wqlc_c: input too large\n");
        free(input);
        return 2;
    }
    size_t out_cap = input_len * 2 + 256;
    uint8_t *output = malloc(out_cap);
    if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); free(input); return 2; }

    struct wql_eval_result_t result;
    memset(&result, 0, sizeof(result));
    int rc = process_record(prog, input, input_len, output, out_cap, &result);
    if (rc == 0) {
        if (has_projection(prog)) {
            fwrite(output, 1, result.output_len, stdout);
        }
    }

    free(output);
    free(input);
    return rc;
}

/* ── Streaming delimited eval ── */

static int eval_delimited(const wql_program_t *prog) {
    uint8_t *record = NULL;
    uint8_t *output = NULL;
    size_t rec_cap = 0, out_cap = 0;

    for (;;) {
        uint64_t rec_len;
        int vr = read_varint(stdin, &rec_len);
        if (vr == -1) break;
        if (vr == -2) {
            fprintf(stderr, "wqlc_c: malformed varint\n");
            free(record); free(output);
            return 2;
        }

        if ((size_t)rec_len > rec_cap) {
            rec_cap = (size_t)rec_len;
            free(record);
            record = malloc(rec_cap);
            if (!record) { fprintf(stderr, "wqlc_c: out of memory\n"); free(output); return 2; }
        }

        if (read_exact(stdin, record, (size_t)rec_len) < 0) {
            fprintf(stderr, "wqlc_c: truncated record\n");
            free(record); free(output);
            return 2;
        }

        if ((size_t)rec_len > SIZE_MAX / 2 - 128) {
            fprintf(stderr, "wqlc_c: record too large\n");
            free(record); free(output);
            return 2;
        }
        size_t needed = (size_t)rec_len * 2 + 256;
        if (needed > out_cap) {
            out_cap = needed;
            free(output);
            output = malloc(out_cap);
            if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); free(record); return 2; }
        }

        struct wql_eval_result_t result;
        memset(&result, 0, sizeof(result));
        int rc = process_record(prog, record, (size_t)rec_len, output, out_cap, &result);
        if (rc == 2) { free(record); free(output); return 2; }

        if (rc == 0) {
            if (has_projection(prog)) {
                write_varint(stdout, (uint64_t)result.output_len);
                fwrite(output, 1, result.output_len, stdout);
            } else {
                write_varint(stdout, rec_len);
                fwrite(record, 1, (size_t)rec_len, stdout);
            }
        }
    }

    fflush(stdout);
    free(record);
    free(output);
    return 0;
}

/* ── main ── */

int main(int argc, char **argv) {
    const char *query = NULL;
    const char *schema_path = NULL;
    const char *message = NULL;
    int delimited = 0;

    /* Skip argv[0] and "eval" */
    int i = 1;
    if (i < argc && strcmp(argv[i], "eval") == 0) i++;

    for (; i < argc; i++) {
        if (strcmp(argv[i], "-q") == 0 && i + 1 < argc) { query = argv[++i]; }
        else if (strcmp(argv[i], "-s") == 0 && i + 1 < argc) { schema_path = argv[++i]; }
        else if (strcmp(argv[i], "-m") == 0 && i + 1 < argc) { message = argv[++i]; }
        else if (strcmp(argv[i], "--delimited") == 0) { delimited = 1; }
    }

    if (!query) {
        fprintf(stderr, "usage: wqlc_c eval -q <query> [-s schema -m msg] [--delimited]\n");
        return 2;
    }

    /* Compile */
    char *err = NULL;
    struct wql_bytes_t bc;
    if (schema_path) {
        size_t schema_len;
        uint8_t *schema = read_file(schema_path, &schema_len);
        if (!schema) { fprintf(stderr, "wqlc_c: cannot read schema %s\n", schema_path); return 2; }
        bc = wql_compile_with_schema(query, schema, schema_len, message, &err);
        free(schema);
    } else {
        bc = wql_compile(query, &err);
    }

    if (!bc.data) {
        fprintf(stderr, "wqlc_c: compile error: %s\n", err ? err : "unknown");
        wql_errmsg_free(err);
        return 2;
    }

    /* Load */
    wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    wql_bytes_free(bc);
    if (!prog) {
        fprintf(stderr, "wqlc_c: load error: %s\n", err ? err : "unknown");
        wql_errmsg_free(err);
        return 2;
    }

    int rc = delimited ? eval_delimited(prog)
                       : eval_single(prog);

    wql_program_free(prog);
    return rc;
}
