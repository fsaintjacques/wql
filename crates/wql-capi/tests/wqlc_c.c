/*
 * wqlc_c — minimal WQL evaluator in C, using the wql-capi library.
 *
 * Usage:
 *   wqlc_c eval -q <query> [-s <schema.bin> -m <message>] [--delimited]
 *
 * Reads protobuf from stdin, writes to stdout.
 *   Single mode:    one message in, one result out. Exit 0=pass, 1=filtered, 2=error.
 *   Delimited mode: varint length-prefixed stream in/out.
 */

#include "../include/wql.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ── Varint codec ── */

static int read_varint(const uint8_t *buf, size_t buf_len, uint64_t *out, size_t *consumed) {
    uint64_t val = 0;
    unsigned shift = 0;
    for (size_t i = 0; i < buf_len; i++) {
        val |= (uint64_t)(buf[i] & 0x7F) << shift;
        if ((buf[i] & 0x80) == 0) {
            *out = val;
            *consumed = i + 1;
            return 0;
        }
        shift += 7;
        if (shift >= 64) return -1;
    }
    return -1; /* incomplete */
}

static size_t encode_varint(uint64_t val, uint8_t *buf) {
    size_t i = 0;
    do {
        uint8_t byte = val & 0x7F;
        val >>= 7;
        if (val) byte |= 0x80;
        buf[i++] = byte;
    } while (val);
    return i;
}

/* ── Read all of stdin ── */

static uint8_t *read_all_stdin(size_t *out_len) {
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
            if (!tmp) { free(buf); return NULL; }
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

/* ── Classify query mode ── */

typedef enum { MODE_FILTER, MODE_PROJECT, MODE_COMBINED } query_mode_t;

static query_mode_t classify(const char *query) {
    if (strstr(query, "WHERE") && strstr(query, "SELECT"))
        return MODE_COMBINED;
    if (strchr(query, '{'))
        return MODE_PROJECT;
    return MODE_FILTER;
}

/* ── Single message eval ── */

static int eval_single(const wql_program_t *prog, query_mode_t mode,
                       const uint8_t *input, size_t input_len) {
    char *err = NULL;

    if (mode == MODE_FILTER) {
        int r = wql_filter(prog, input, input_len, &err);
        if (r < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); return 2; }
        return r == 1 ? 0 : 1;
    }

    uint8_t *output = malloc(input_len * 2 + 256);
    if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); return 2; }
    size_t out_cap = input_len * 2 + 256;

    if (mode == MODE_COMBINED) {
        int64_t n = wql_project_and_filter(prog, input, input_len, output, out_cap, &err);
        if (n == -2) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); free(output); return 2; }
        if (n == -1) { free(output); return 1; } /* filtered out */
        fwrite(output, 1, (size_t)n, stdout);
        free(output);
        return 0;
    }

    /* PROJECT */
    int64_t n = wql_project(prog, input, input_len, output, out_cap, &err);
    if (n < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); free(output); return 2; }
    fwrite(output, 1, (size_t)n, stdout);
    free(output);
    return 0;
}

/* ── Delimited stream eval ── */

static int eval_delimited(const wql_program_t *prog, query_mode_t mode,
                          const uint8_t *data, size_t data_len) {
    size_t pos = 0;
    uint8_t varint_buf[10];
    uint8_t *output = NULL;
    size_t out_cap = 0;

    while (pos < data_len) {
        uint64_t rec_len;
        size_t consumed;
        if (read_varint(data + pos, data_len - pos, &rec_len, &consumed) < 0) {
            fprintf(stderr, "wqlc_c: malformed varint\n");
            free(output);
            return 2;
        }
        pos += consumed;
        if (pos + (size_t)rec_len > data_len) {
            fprintf(stderr, "wqlc_c: truncated record\n");
            free(output);
            return 2;
        }

        const uint8_t *record = data + pos;
        size_t record_len = (size_t)rec_len;
        pos += record_len;

        /* Ensure output buffer is big enough */
        size_t needed = record_len * 2 + 256;
        if (needed > out_cap) {
            free(output);
            out_cap = needed;
            output = malloc(out_cap);
            if (!output) { fprintf(stderr, "wqlc_c: out of memory\n"); return 2; }
        }

        char *err = NULL;

        if (mode == MODE_FILTER) {
            int r = wql_filter(prog, record, record_len, &err);
            if (r < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); free(output); return 2; }
            if (r == 1) {
                /* Pass: write original record */
                size_t vn = encode_varint(record_len, varint_buf);
                fwrite(varint_buf, 1, vn, stdout);
                fwrite(record, 1, record_len, stdout);
            }
        } else if (mode == MODE_COMBINED) {
            int64_t n = wql_project_and_filter(prog, record, record_len, output, out_cap, &err);
            if (n == -2) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); free(output); return 2; }
            if (n >= 0) {
                size_t vn = encode_varint((uint64_t)n, varint_buf);
                fwrite(varint_buf, 1, vn, stdout);
                fwrite(output, 1, (size_t)n, stdout);
            }
        } else {
            int64_t n = wql_project(prog, record, record_len, output, out_cap, &err);
            if (n < 0) { fprintf(stderr, "wqlc_c: %s\n", err); wql_errmsg_free(err); free(output); return 2; }
            size_t vn = encode_varint((uint64_t)n, varint_buf);
            fwrite(varint_buf, 1, vn, stdout);
            fwrite(output, 1, (size_t)n, stdout);
        }
    }

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

    /* Read stdin */
    size_t input_len;
    uint8_t *input = read_all_stdin(&input_len);
    if (!input && input_len > 0) {
        fprintf(stderr, "wqlc_c: failed to read stdin\n");
        wql_program_free(prog);
        return 2;
    }

    query_mode_t mode = classify(query);
    int rc;
    if (delimited) {
        rc = eval_delimited(prog, mode, input, input_len);
    } else {
        rc = eval_single(prog, mode, input, input_len);
    }

    free(input);
    wql_program_free(prog);
    return rc;
}
