// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

/* Stage B proof: a native host driving the Mersey engine through the C ABI
 * (crates/mersey_capi) — the same boundary Chromium's //components/mersey
 * wraps. No V8, no WASM: this is the engine running beside a host, the way
 * it will sit beside Blink.
 *
 * Build & run: ./native/build-and-test.sh
 */
#define _GNU_SOURCE /* memmem */
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

/* ---- a handle-based fake DOM, to exercise the web_apply batch path ----- */
#define MAX_NODES 64
typedef struct {
    int kind; /* 0 document, 1 element */
    char tag[32];
    char text[256];
    int64_t children[MAX_NODES];
    int n_children;
} WNode;
typedef struct {
    WNode nodes[MAX_NODES];
    int n_nodes; /* handle 0 unused, 1 = document */
} WebDom;

static void str16_to_c(const msy_str16 *s, char *out, size_t cap) {
    size_t n = s->len < cap - 1 ? s->len : cap - 1;
    for (size_t i = 0; i < n; i++) out[i] = (char)(s->ptr[i] & 0xff); /* ASCII test data */
    out[n] = 0;
}

static int64_t webdom_global(void *data, const char *name, size_t len) {
    (void)name;
    (void)len;
    WebDom *w = data;
    if (w->n_nodes < 2) {
        w->n_nodes = 2;
        w->nodes[1].kind = 0; /* the document */
    }
    return 1;
}

static size_t webdom_apply(void *data, const int32_t *ops, size_t nops,
                           const int64_t *nodes, size_t nnodes,
                           const msy_str16 *strs, size_t nstrs,
                           int64_t *created_out, size_t created_cap) {
    (void)nnodes;
    (void)nstrs;
    WebDom *w = data;
    int64_t created[MAX_NODES];
    size_t nc = 0;
#define RESOLVE(r) \
    ((r) == MSY_DOM_NULL ? (int64_t)0 : ((r) >= 0 ? created[(r)] : nodes[-(r) - 1]))
    for (size_t g = 0; g < nops; g++) {
        int32_t op = ops[g * 4], a = ops[g * 4 + 1], b = ops[g * 4 + 2], c = ops[g * 4 + 3];
        if (op == MSY_DOM_CREATE) {
            (void)c; /* c = document operand; this demo has a single document */
            int64_t h = w->n_nodes++;
            w->nodes[h].kind = 1;
            str16_to_c(&strs[a], w->nodes[h].tag, sizeof w->nodes[h].tag);
            created[b] = h;
            if ((size_t)b < created_cap) created_out[b] = h;
            nc++;
        } else if (op == MSY_DOM_SET_TEXT) {
            int64_t t = RESOLVE(a);
            str16_to_c(&strs[b], w->nodes[t].text, sizeof w->nodes[t].text);
        } else if (op == MSY_DOM_APPEND || op == MSY_DOM_INSERT) {
            int64_t p = RESOLVE(a), ch = RESOLVE(b);
            w->nodes[p].children[w->nodes[p].n_children++] = ch;
        } else if (op == MSY_DOM_REMOVE) {
            int64_t p = RESOLVE(a), ch = RESOLVE(b);
            WNode *pn = &w->nodes[p];
            for (int i = 0; i < pn->n_children; i++)
                if (pn->children[i] == ch) {
                    pn->children[i] = pn->children[--pn->n_children];
                    break;
                }
        }
    }
#undef RESOLVE
    return nc;
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
    /* The first thing any embedder does: refuse a mismatched engine. */
    if (msy_abi_version() != MSY_ABI_VERSION) {
        printf("FAIL  ABI version: header %u, engine %u\n", MSY_ABI_VERSION,
               msy_abi_version());
        return 1;
    }
    printf("PASS  ABI version %u\n", MSY_ABI_VERSION);

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

    /* The module-graph loader: scan tells the host what to fetch; the host
     * hands the assembled graph back. Same payload the browser loader builds. */
    Dom dom3 = {0};
    msy_host_table t3 = table;
    t3.data = &dom3;
    msy_context *ctx3 = msy_context_new(&t3);
    const char *entry =
        "import { console } from \"std:console\";\n"
        "import { triple } from \"./lib.mersey\";\n"
        "console.log(\"tripled:\", triple(14));";
    size_t scan_len = 0;
    const char *scan =
        msy_context_scan_imports(ctx3, entry, strlen(entry), &scan_len);
    printf("%s  scan finds the import\n",
           scan && memmem(scan, scan_len, "lib.mersey", 10) ? "PASS" : "FAIL");
    if (!(scan && memmem(scan, scan_len, "lib.mersey", 10))) failures++;

    const char *payload =
        "{\"entry\":\"main.mersey\",\"modules\":["
        "{\"spec\":\"lib.mersey\",\"source\":\"export function triple(x: "
        "int32): int32 { return x * 3; }\"},"
        "{\"spec\":\"main.mersey\",\"source\":\"import { console } from "
        "\\\"std:console\\\";\\nimport { triple } from "
        "\\\"./lib.mersey\\\";\\nconsole.log(\\\"tripled:\\\", "
        "triple(14));\"}]}";
    uint32_t g = msy_context_run_graph(ctx3, payload, strlen(payload));
    printf("%s  graph status (%u)\n", g == 0 ? "PASS" : "FAIL", g);
    if (g != 0) {
        printf("      errors: %s\n", dom3.errors);
        failures++;
    }
    expect_str("graph output", dom3.log, "tripled: 42\n");
    msy_context_free(ctx3);

    /* The jitless configuration a locked-down sandbox needs. */
    Dom dom4 = {0};
    msy_host_table t4 = table;
    t4.data = &dom4;
    msy_context *ctx4 = msy_context_new_ex(&t4, MSY_FLAG_NO_JIT);
    const char *hot =
        "import { console } from \"std:console\";\n"
        "function fib(n: int32): int32 { if (n < 2) { return n; } return "
        "fib(n - 1) + fib(n - 2); }\n"
        "console.log(\"fib:\", fib(18));";
    uint32_t j = msy_context_run(ctx4, hot, strlen(hot));
    printf("%s  jitless status (%u)\n", j == 0 ? "PASS" : "FAIL", j);
    if (j != 0) failures++;
    expect_str("jitless output", dom4.log, "fib: 2584\n");
    msy_context_free(ctx4);

    /* Batched DOM mutation (web_apply, ABI v10): one crossing applies a whole
     * op stream. Proves the mersey_capi marshalling (ops/nodes/UTF-16 pool ->
     * created handles back) end to end, the shared path every fork host reuses. */
    WebDom web = {0};
    msy_host_table t5 = {
        .data = &web,
        .web_global = webdom_global,
        .web_apply = webdom_apply,
    };
    msy_context *ctx5 = msy_context_new(&t5);
    const char *batch =
        "import { document } from \"browser:dom\";\n"
        "import { dom } from \"std:dom\";\n"
        "const ops: int32[] = [\n"
        "  0,0,0,-1, 1,0,1,0, 2,-1,0,0,\n"  /* create div temp0, text hello, append */
        "  0,0,1,-1, 1,1,2,0, 2,-1,1,0,\n"  /* create div temp1, text world, append */
        "  4,-1,0,0\n"                        /* remove temp0 from document */
        "];\n"
        "const created = dom.apply(ops, [document], [\"div\",\"hello\",\"world\"]);\n"
        "if (created.length != 2) { throw new Error(\"bad created count\"); }\n";
    uint32_t bstat = msy_context_run(ctx5, batch, strlen(batch));
    printf("%s  web_apply run status (%u)\n", bstat == 0 ? "PASS" : "FAIL", bstat);
    if (bstat != 0) failures++;
    /* The document (handle 1) should hold exactly one child — temp1, whose text
     * is "world" — after temp0 was appended then removed. */
    int doc_kids = web.nodes[1].n_children;
    printf("%s  web_apply: document has one child (got %d)\n", doc_kids == 1 ? "PASS" : "FAIL",
           doc_kids);
    if (doc_kids != 1) failures++;
    if (doc_kids == 1) {
        const char *kid_text = web.nodes[web.nodes[1].children[0]].text;
        expect_str("web_apply: surviving child's text", kid_text, "world");
    }
    /* Two elements created -> handles 2 and 3 (document is 1). */
    printf("%s  web_apply: created two element nodes\n", web.n_nodes == 4 ? "PASS" : "FAIL");
    if (web.n_nodes != 4) failures++;
    msy_context_free(ctx5);

    msy_context_free(ctx);

    if (failures) {
        printf("\n%d assertion(s) failed\n", failures);
        return 1;
    }
    printf("\nStage B native embedding: all assertions passed\n");
    return 0;
}
