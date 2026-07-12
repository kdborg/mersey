/* Stage B proof: a native host driving the Mersey engine through the C ABI
 * (crates/mersey_capi) — the same boundary Chromium's //components/mersey
 * wraps. No V8, no WASM: this is the engine running beside a host, the way
 * it will sit beside Blink.
 *
 * Build & run: ./native/build-and-test.sh
 */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../crates/mersey_capi/include/mersey.h"

/* ---- a 4-element fake DOM ------------------------------------------- */
#define MAX_ELEMS 4
#define MAX_CBS 8

typedef struct {
    char id[32];
    char text[256];
    uint32_t cbs[MAX_CBS];
    int n_cbs;
} Elem;

typedef struct {
    Elem elems[MAX_ELEMS];
    int n_elems;
    char log[4096];
    char errors[4096];
} Dom;

static Elem *elem(Dom *dom, const char *id, size_t len) {
    for (int i = 0; i < dom->n_elems; i++) {
        if (strlen(dom->elems[i].id) == len && !memcmp(dom->elems[i].id, id, len))
            return &dom->elems[i];
    }
    Elem *e = &dom->elems[dom->n_elems++];
    snprintf(e->id, sizeof e->id, "%.*s", (int)len, id);
    return e;
}

/* ---- host table implementation --------------------------------------- */
static void host_print(void *data, const char *s, size_t len) {
    Dom *dom = data;
    snprintf(dom->log + strlen(dom->log), sizeof dom->log - strlen(dom->log),
             "%.*s\n", (int)len, s);
}

static void host_error(void *data, const char *s, size_t len) {
    Dom *dom = data;
    snprintf(dom->errors + strlen(dom->errors),
             sizeof dom->errors - strlen(dom->errors), "%.*s\n", (int)len, s);
}

static void host_set_text(void *data, const char *id, size_t il,
                          const char *text, size_t tl) {
    Elem *e = elem(data, id, il);
    snprintf(e->text, sizeof e->text, "%.*s", (int)tl, text);
}

static const char *host_get_text(void *data, const char *id, size_t il,
                                 size_t *out_len) {
    Elem *e = elem(data, id, il);
    *out_len = strlen(e->text);
    return e->text;
}

static void host_add_listener(void *data, const char *id, size_t il,
                              const char *event, size_t el, uint32_t cb) {
    (void)event;
    (void)el; /* the demo shell fires whatever it registered */
    Elem *e = elem(data, id, il);
    e->cbs[e->n_cbs++] = cb;
}

/* ---- driver ------------------------------------------------------------ */
static int failures = 0;
static void expect_str(const char *what, const char *actual, const char *want) {
    int ok = strcmp(actual, want) == 0;
    printf("%s  %s\n", ok ? "PASS" : "FAIL", what);
    if (!ok) {
        printf("      actual:   %s\n      expected: %s\n", actual, want);
        failures++;
    }
}

static char *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    assert(f && "source file");
    fseek(f, 0, SEEK_END);
    *len = (size_t)ftell(f);
    fseek(f, 0, SEEK_SET);
    char *buf = malloc(*len + 1);
    assert(fread(buf, 1, *len, f) == *len);
    buf[*len] = 0;
    fclose(f);
    return buf;
}

int main(void) {
    Dom dom = {0};
    msy_host_table table = {
        .data = &dom,
        .print = host_print,
        .error = host_error,
        .dom_set_text = host_set_text,
        .dom_get_text = host_get_text,
        .dom_add_listener = host_add_listener,
    };
    msy_context *ctx = msy_context_new(&table);
    assert(ctx);

    /* The same counter app the browser demo uses. */
    size_t len;
    char *src = read_file("web/demo/app.mersey", &len);
    uint32_t status = msy_context_run(ctx, src, len);
    free(src);

    printf("%s  run status (%u)\n", status == 0 ? "PASS" : "FAIL", status);
    if (status != 0) {
        printf("      errors: %s\n", dom.errors);
        failures++;
    }
    expect_str("console output", dom.log, "Mersey is running in your browser \xF0\x9F\x8C\x8A\n");
    expect_str("initial render", elem(&dom, "out", 3)->text, "Clicks: 0");

    Elem *btn = elem(&dom, "btn", 3);
    for (int i = 0; i < 3; i++)
        for (int c = 0; c < btn->n_cbs; c++)
            msy_context_invoke(ctx, btn->cbs[c]);
    expect_str("after 3 clicks", elem(&dom, "out", 3)->text, "Clicks: 3");

    for (int i = 0; i < 2; i++)
        for (int c = 0; c < btn->n_cbs; c++)
            msy_context_invoke(ctx, btn->cbs[c]);
    expect_str("after 5 clicks (UTF-32 wave)", elem(&dom, "out", 3)->text,
               "Clicks: 5 \xF0\x9F\x8C\x8A");

    /* A compile error must surface through the error callback, not crash. */
    Dom dom2 = {0};
    msy_host_table t2 = table;
    t2.data = &dom2;
    msy_context *ctx2 = msy_context_new(&t2);
    const char *bad = "let x: int32 = \"oops\";";
    uint32_t bad_status = msy_context_run(ctx2, bad, strlen(bad));
    printf("%s  type error status (%u)\n", bad_status == 1 ? "PASS" : "FAIL", bad_status);
    if (bad_status != 1) failures++;
    printf("%s  type error reported\n", strstr(dom2.errors, "E0401") ? "PASS" : "FAIL");
    if (!strstr(dom2.errors, "E0401")) failures++;

    msy_context_free(ctx2);
    msy_context_free(ctx);

    if (failures) {
        printf("\n%d assertion(s) failed\n", failures);
        return 1;
    }
    printf("\nStage B native embedding: all assertions passed\n");
    return 0;
}
