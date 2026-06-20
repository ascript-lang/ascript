/* EMBED §8.3 — the compiled C smoke test.
 *
 * Exercises the C ABI end-to-end against the built cdylib: version/ABI guard, isolate
 * new/eval/call, a host fn with userdata, an error + last_error, the JSON bridge, output
 * capture, and the free discipline. Compiled + linked + run by c_smoke.rs at test time
 * (cc::Build). Prints "OK" and exits 0 on success; prints a diagnostic + exits non-zero
 * on any failure.
 */
#include "ascript.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#define CHECK(cond, msg)                                                       \
  do {                                                                         \
    if (!(cond)) {                                                             \
      fprintf(stderr, "smoke FAIL: %s (line %d)\n", (msg), __LINE__);          \
      return 1;                                                                \
    }                                                                          \
  } while (0)

/* A host callback: returns arg0 (int) + a userdata bias (int). */
static as_status add_bias(void *userdata, as_isolate *iso,
                          const as_value *const *args, size_t nargs,
                          as_value **out, char **err_utf8) {
  (void)iso;
  (void)err_utf8;
  if (nargs < 1) return AS_ERR_CONFIG;
  int64_t bias = *(int64_t *)userdata;
  int64_t n = 0;
  if (as_value_int(args[0], &n) != AS_OK) return AS_ERR_TYPE;
  *out = as_int(n + bias);
  return AS_OK;
}

/* A fallible host callback: errors when arg0 == 0. */
static as_status fail_on_zero(void *userdata, as_isolate *iso,
                              const as_value *const *args, size_t nargs,
                              as_value **out, char **err_utf8) {
  (void)userdata;
  (void)iso;
  if (nargs < 1) return AS_ERR_CONFIG;
  int64_t n = 0;
  as_value_int(args[0], &n);
  if (n == 0) {
    /* a heap message the engine bridge frees via as_string_free */
    const char *m = "zero!";
    char *buf = (char *)malloc(strlen(m) + 1);
    strcpy(buf, m);
    *err_utf8 = buf;
    return AS_ERR_PANIC;
  }
  *out = as_int(n * 2);
  return AS_OK;
}

int main(void) {
  /* 1. version / ABI guard. */
  CHECK(ascript_abi_version() == ASCRIPT_CAPI_ABI, "abi version mismatch");
  CHECK(ascript_version() > 0, "version zero");

  /* 2. new + eval scalar. */
  as_isolate *iso = as_isolate_new();
  CHECK(iso != NULL, "isolate new");

  const char *e1 = "40 + 2";
  as_value *v = NULL;
  CHECK(as_eval(iso, e1, strlen(e1), &v) == AS_OK, "eval 40+2");
  int64_t n = 0;
  CHECK(as_value_int(v, &n) == AS_OK && n == 42, "read 42");
  as_value_free(v);

  /* 3. host fn with userdata + a call into it via a defined global fn. */
  int64_t bias = 1000;
  CHECK(as_register_host_fn(iso, "host:app", strlen("host:app"),
                            "addBias", strlen("addBias"), add_bias, &bias, 0) == AS_OK,
        "register addBias");
  CHECK(as_register_host_fn(iso, "host:app", strlen("host:app"),
                            "checked", strlen("checked"), fail_on_zero, NULL, 1) == AS_OK,
        "register checked");

  const char *prog =
      "import * as app from \"host:app\"\n"
      "fn run(x) { return app.addBias(x) }\n"
      "let [r, err] = app.checked(0)\n"
      "print(\"checked:\", r == nil, err.message)";
  as_value *pv = NULL;
  CHECK(as_eval(iso, prog, strlen(prog), &pv) == AS_OK, "eval host program");
  as_value_free(pv);

  /* call the global `run`. */
  as_value *arg = as_int(5);
  const as_value *call_args[1] = {arg};
  as_value *res = NULL;
  CHECK(as_call(iso, "run", strlen("run"), call_args, 1, &res) == AS_OK, "call run");
  int64_t rn = 0;
  CHECK(as_value_int(res, &rn) == AS_OK && rn == 1005, "run(5)==1005");
  as_value_free(arg);
  as_value_free(res);

  /* 4. output capture (drained). */
  char *out = NULL;
  size_t out_len = 0;
  CHECK(as_take_output(iso, &out, &out_len) == AS_OK, "take_output");
  CHECK(out_len > 0 && strstr(out, "checked: true zero!") != NULL, "output content");
  as_string_free(out);

  /* 5. error + last_error. */
  const char *bad = "let z = 1\nz.nope()";
  as_value *bv = NULL;
  as_status st = as_eval(iso, bad, strlen(bad), &bv);
  CHECK(st == AS_ERR_PANIC || st == AS_ERR_COMPILE, "expected error status");
  const char *msg = NULL;
  size_t msg_len = 0;
  CHECK(as_last_error(iso, &msg, &msg_len) == AS_OK && msg_len > 0, "last_error non-empty");

  /* 6. JSON bridge round-trip. */
  const char *json = "{\"a\":1,\"b\":[2,3]}";
  as_value *jv = NULL;
  CHECK(as_json_parse(iso, json, strlen(json), &jv) == AS_OK, "json parse");
  int kind = -1;
  CHECK(as_value_kind(jv, &kind) == AS_OK && kind == AS_KIND_OBJECT, "json kind");
  char *jout = NULL;
  size_t jlen = 0;
  CHECK(as_value_to_json(iso, jv, &jout, &jlen) == AS_OK, "to_json");
  CHECK(strcmp(jout, "{\"a\":1,\"b\":[2,3]}") == 0, "json roundtrip");
  as_string_free(jout);
  as_value_free(jv);

  /* 7. NULL-safety + free. */
  as_value_free(NULL);
  as_string_free(NULL);
  as_isolate_free(iso);
  as_isolate_free(NULL);

  printf("OK\n");
  return 0;
}
