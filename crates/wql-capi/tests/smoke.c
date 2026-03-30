#include "../include/wql.h"
#include <stdio.h>
#include <string.h>
#include <assert.h>

/* Hand-built protobuf: { 1: varint(42), 2: LEN("hello"), 3: varint(99) } */
static const uint8_t INPUT[] = {
    0x08, 0x2a,                         /* field 1, varint, value 42 */
    0x12, 0x05, 'h','e','l','l','o',    /* field 2, LEN, "hello" */
    0x18, 0x63,                         /* field 3, varint, value 99 */
};
static const size_t INPUT_LEN = sizeof(INPUT);

static int has_tag(const uint8_t *buf, size_t len, uint8_t tag) {
    for (size_t i = 0; i < len; i++) {
        if (buf[i] == tag) return 1;
    }
    return 0;
}

static void test_compile_and_inspect(void) {
    char *err = NULL;
    struct wql_bytes_t bc = wql_compile("{ #1, #2 }", &err);
    assert(bc.data != NULL && "compile failed");
    assert(err == NULL);
    assert(bc.len > 0);

    /* Verify magic bytes: "WQL\0" */
    assert(bc.data[0] == 'W');
    assert(bc.data[1] == 'Q');
    assert(bc.data[2] == 'L');
    assert(bc.data[3] == '\0');

    wql_bytes_free(bc);
    printf("  PASS test_compile_and_inspect\n");
}

static void test_compile_error(void) {
    char *err = NULL;
    struct wql_bytes_t bc = wql_compile("{ unclosed", &err);
    assert(bc.data == NULL && "expected compile error");
    assert(err != NULL);
    assert(strlen(err) > 0);

    wql_errmsg_free(err);
    printf("  PASS test_compile_error\n");
}

static void test_project(void) {
    char *err = NULL;

    /* Compile: keep fields 1 and 2 */
    struct wql_bytes_t bc = wql_compile("{ #1, #2 }", &err);
    assert(bc.data != NULL);

    /* Load program */
    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    /* Project into caller-owned buffer */
    uint8_t output[256];
    int64_t n = wql_project(prog, INPUT, INPUT_LEN, output, sizeof(output), &err);
    assert(n >= 0 && "project failed");
    assert(err == NULL);

    /* Output should have field 1 (tag 0x08) and field 2 (tag 0x12) */
    assert(has_tag(output, (size_t)n, 0x08));
    assert(has_tag(output, (size_t)n, 0x12));
    /* Field 3 (tag 0x18) should be stripped */
    assert(!has_tag(output, (size_t)n, 0x18));

    wql_program_free(prog);
    printf("  PASS test_project\n");
}

static void test_filter_pass(void) {
    char *err = NULL;

    struct wql_bytes_t bc = wql_compile("#1 > 10", &err);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    /* field 1 = 42, should pass #1 > 10 */
    int result = wql_filter(prog, INPUT, INPUT_LEN, &err);
    assert(result == 1);
    assert(err == NULL);

    wql_program_free(prog);
    printf("  PASS test_filter_pass\n");
}

static void test_filter_fail(void) {
    char *err = NULL;

    struct wql_bytes_t bc = wql_compile("#1 > 100", &err);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    /* field 1 = 42, should fail #1 > 100 */
    int result = wql_filter(prog, INPUT, INPUT_LEN, &err);
    assert(result == 0);
    assert(err == NULL);

    wql_program_free(prog);
    printf("  PASS test_filter_fail\n");
}

static void test_project_and_filter_pass(void) {
    char *err = NULL;

    struct wql_bytes_t bc = wql_compile("WHERE #1 > 10 SELECT { #2 }", &err);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    uint8_t output[256];
    int64_t n = wql_project_and_filter(prog, INPUT, INPUT_LEN, output, sizeof(output), &err);
    assert(n >= 0 && "expected pass");
    assert(err == NULL);

    /* Output should have field 2 only */
    assert(has_tag(output, (size_t)n, 0x12));
    assert(!has_tag(output, (size_t)n, 0x08));
    assert(!has_tag(output, (size_t)n, 0x18));

    wql_program_free(prog);
    printf("  PASS test_project_and_filter_pass\n");
}

static void test_project_and_filter_reject(void) {
    char *err = NULL;

    struct wql_bytes_t bc = wql_compile("WHERE #1 > 100 SELECT { #2 }", &err);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    uint8_t output[256];
    int64_t n = wql_project_and_filter(prog, INPUT, INPUT_LEN, output, sizeof(output), &err);
    assert(n == -1 && "expected filtered out");
    assert(err == NULL);

    wql_program_free(prog);
    printf("  PASS test_project_and_filter_reject\n");
}

static void test_null_errmsg(void) {
    /* All functions should accept NULL errmsg without crashing */
    struct wql_bytes_t bc = wql_compile("#1 > 0", NULL);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, NULL);
    assert(prog != NULL);
    wql_bytes_free(bc);

    int result = wql_filter(prog, INPUT, INPUT_LEN, NULL);
    assert(result == 1 || result == 0);

    wql_program_free(prog);

    /* Error case with NULL errmsg should not crash */
    struct wql_bytes_t bad = wql_compile("{ unclosed", NULL);
    assert(bad.data == NULL);

    printf("  PASS test_null_errmsg\n");
}

static void test_free_null(void) {
    /* Freeing NULL should be safe */
    wql_program_free(NULL);
    wql_errmsg_free(NULL);
    struct wql_bytes_t null_bytes = { .data = NULL, .len = 0 };
    wql_bytes_free(null_bytes);
    printf("  PASS test_free_null\n");
}

static void test_output_buffer_too_small(void) {
    char *err = NULL;
    struct wql_bytes_t bc = wql_compile("{ #1, #2 }", &err);
    assert(bc.data != NULL);

    struct wql_program_t *prog = wql_program_load(bc.data, bc.len, &err);
    assert(prog != NULL);
    wql_bytes_free(bc);

    /* Provide a buffer that's too small */
    uint8_t tiny[1];
    int64_t n = wql_project(prog, INPUT, INPUT_LEN, tiny, sizeof(tiny), &err);
    assert(n == -1 && "expected error for small buffer");
    assert(err != NULL);

    wql_errmsg_free(err);
    wql_program_free(prog);
    printf("  PASS test_output_buffer_too_small\n");
}

int main(void) {
    printf("wql-capi C smoke tests:\n");
    test_compile_and_inspect();
    test_compile_error();
    test_project();
    test_filter_pass();
    test_filter_fail();
    test_project_and_filter_pass();
    test_project_and_filter_reject();
    test_null_errmsg();
    test_free_null();
    test_output_buffer_too_small();
    printf("All C smoke tests passed.\n");
    return 0;
}
