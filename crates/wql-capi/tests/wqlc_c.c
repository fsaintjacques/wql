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

/* ── Classify query mode ──
 *
 * Uses string heuristics since the C API doesn't expose the parsed AST.
 * This is sufficient for the controlled e2e test data; a production C
 * consumer should expose mode from the compiler or bytecode header.
 */

typedef enum { MODE_FILTER, MODE_PROJECT, MODE_COMBINED } query_mode_t;

static query_mode_t classify(const char *query) {
    if (strstr(query, "WHERE") && strstr(query, "SELECT"))
        return MODE_COMBINED;
    if (strchr(query, '{'))
        return MODE_PROJECT;
    return MODE_FILTER;
}

/* ── Process a single record ──
 * Returns: bytes written to *output (via *out_written), or:
 *   0 = ok (projection/filter pass)
 *   1 = filtered out
 *   2 = error
 */
static int process_record(const wql_program_t *prog, query_mode_t mode,
                          const uint8_t *input, size_t input_len,
                          uint8_t *output, size_t output_cap,
                          size_t *out_written) {
    char *err = NULL;

    if (mode == MODE_FILTER) {
        int r = wql_filter(prog, input, input_len, &err);
        if (r < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); return 2; }
        *out_written = 0;
        return r == 1 ? 0 : 1;
    }

    if (mode == MODE_COMBINED) {
        int64_t n = wql_project_and_filter(prog, input, input_len, output, output_cap, &err);
        if (n == -2) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); return 2; }
        if (n == -1) { *out_written = 0; return 1; }
        *out_written = (size_t)n;
        return 0;
    }

    /* PROJECT */
    int64_t n = wql_project(prog, input, input_len, output, output_cap, &err);
    if (n < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); return 2; }
    *out_written = (size_t)n;
    return 0;
}

/* ── Single message eval ── */

static int eval_single(const wql_program_t *prog, query_mode_t mode) {
    size_t input_len = 0;
    uint8_t *input = read_all_stdin(&input_len);
    if (!input && input_len > 0) {
        fprintf(stderr, "wqlc_c: failed to read stdin\n");
        return 2;
    }

    /* Projection output <= input size. Generous headroom for test data. */
    size_t out_cap = input_len * 2 + 256;
    uint8_t *output = malloc(out_cap);
    if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); free(input); return 2; }

    size_t written = 0;
    int rc = process_record(prog, mode, input, input_len, output, out_cap, &written);
    if (rc == 0 && mode != MODE_FILTER) {
        fwrite(output, 1, written, stdout);
    } else if (rc == 0 && mode == MODE_FILTER) {
        /* pass — nothing to write */
    }

    free(output);
    free(input);
    return rc;
}

/* ── Streaming delimited eval ── */

static int eval_delimited(const wql_program_t *prog, query_mode_t mode) {
    uint8_t *record = NULL;
    uint8_t *output = NULL;
    size_t rec_cap = 0, out_cap = 0;

    for (;;) {
        /* Read record length */
        uint64_t rec_len;
        int vr = read_varint(stdin, &rec_len);
        if (vr == -1) break; /* clean EOF */
        if (vr == -2) {
            fprintf(stderr, "wqlc_c: malformed varint\n");
            free(record); free(output);
            return 2;
        }

        /* Grow record buffer if needed */
        if ((size_t)rec_len > rec_cap) {
            rec_cap = (size_t)rec_len;
            free(record);
            record = malloc(rec_cap);
            if (!record) { fprintf(stderr, "wqlc_c: out of memory\n"); free(output); return 2; }
        }

        /* Read record body */
        if (read_exact(stdin, record, (size_t)rec_len) < 0) {
            fprintf(stderr, "wqlc_c: truncated record\n");
            free(record); free(output);
            return 2;
        }

        /* Grow output buffer if needed */
        size_t needed = (size_t)rec_len * 2 + 256;
        if (needed > out_cap) {
            out_cap = needed;
            free(output);
            output = malloc(out_cap);
            if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); free(record); return 2; }
        }

        size_t written = 0;
        int rc = process_record(prog, mode, record, (size_t)rec_len, output, out_cap, &written);
        if (rc == 2) { free(record); free(output); return 2; }

        if (rc == 0) {
            if (mode == MODE_FILTER) {
                /* Pass: emit original record */
                write_varint(stdout, rec_len);
                fwrite(record, 1, (size_t)rec_len, stdout);
            } else {
                /* Emit projected output */
                write_varint(stdout, (uint64_t)written);
                fwrite(output, 1, written, stdout);
            }
        }
        /* rc == 1: filtered out, emit nothing */
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

    int rc = delimited ? eval_delimited(prog, classify(query))
                       : eval_single(prog, classify(query));

    wql_program_free(prog);
    return rc;
}
