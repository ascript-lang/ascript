/* EMBED Unit E — the c-host embedding example (spec §12, Gate 9).
 *
 * Drives an embedded AScript isolate through the C ABI, FULLY error-checked: every
 * status code is tested and a C-side die() aborts (non-zero exit) on any mismatch.
 * It exercises the §12 surface:
 *
 *   - as_isolate_new (deny-all caps, captured output);
 *   - as_register_host_fn with userdata + the fallible (tier 1) tier;
 *   - as_eval (load plugin.as) + as_call (call its globals);
 *   - the fallible tier producing the [value, err] pair + as_last_error on a forced
 *     error;
 *   - the JSON bridge (as_json_parse / as_value_to_json) round-trip;
 *   - as_take_output draining the capture buffer;
 *   - the full free discipline (every owned handle/string freed; NULL-safe frees).
 *
 * Prints `EMBED-C-HOST-OK` and exits 0 on success. This is the SAME source the
 * Makefile builds for humans AND the capi test compiles+links+runs for CI.
 *
 * Build (human): `make` in this directory (see Makefile).
 */
#include "ascript.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#define CHECK(cond, msg)                                                       \
  do {                                                                         \
    if (!(cond)) {                                                             \
      fprintf(stderr, "c-host FAIL: %s (line %d)\n", (msg), __LINE__);         \
      exit(1);                                                                 \
    }                                                                          \
  } while (0)

/* A host callback: returns arg0 (int) * a userdata factor (int). */
static as_status scale(void *userdata, as_isolate *iso,
                       const as_value *const *args, size_t nargs,
                       as_value **out, char **err_utf8) {
  (void)iso;
  (void)err_utf8;
  if (nargs < 1) return AS_ERR_CONFIG;
  int64_t factor = *(int64_t *)userdata;
  int64_t n = 0;
  if (as_value_int(args[0], &n) != AS_OK) return AS_ERR_TYPE;
  *out = as_int(n * factor);
  return AS_OK;
}

/* A FALLIBLE host callback (registered tier 1): errors when arg0 == 0, else *3. */
static as_status checked(void *userdata, as_isolate *iso,
                         const as_value *const *args, size_t nargs,
                         as_value **out, char **err_utf8) {
  (void)userdata;
  (void)iso;
  if (nargs < 1) return AS_ERR_CONFIG;
  int64_t n = 0;
  as_value_int(args[0], &n);
  if (n == 0) {
    /* a heap message the engine bridge frees via as_string_free */
    const char *m = "argument was zero";
    char *buf = (char *)malloc(strlen(m) + 1);
    CHECK(buf != NULL, "malloc err string");
    strcpy(buf, m);
    *err_utf8 = buf;
    return AS_ERR_PANIC;
  }
  *out = as_int(n * 3);
  return AS_OK;
}

int main(void) {
  /* 1. version / ABI guard. */
  CHECK(ascript_abi_version() == ASCRIPT_CAPI_ABI, "abi version mismatch");
  CHECK(ascript_version() > 0, "version zero");

  /* 2. new isolate (deny-all caps, captured output). */
  as_isolate *iso = as_isolate_new();
  CHECK(iso != NULL, "isolate new");

  /* 3. register the host:plugin module: a plain fn (scale, userdata) + a fallible
   *    fn (checked, tier 1). */
  int64_t factor = 10;
  CHECK(as_register_host_fn(iso, "host:plugin", strlen("host:plugin"),
                            "scale", strlen("scale"), scale, &factor, 0) == AS_OK,
        "register scale");
  CHECK(as_register_host_fn(iso, "host:plugin", strlen("host:plugin"),
                            "checked", strlen("checked"), checked, NULL, 1) == AS_OK,
        "register checked");

  /* 4. load plugin.as (defines transform/checked over host:plugin). The script is
   *    embedded as a string literal so the binary is self-contained. It mirrors
   *    examples/embed/c-host/plugin.as. */
  const char *prog =
      "import * as plugin from \"host:plugin\"\n"
      "fn transform(x) { return plugin.scale(x) }\n"
      "fn checked(x) {\n"
      "  let [v, err] = plugin.checked(x)\n"
      "  if (err != nil) { print(`checked(${x}): err=${err.message}`); return -1 }\n"
      "  return v\n"
      "}\n";
  as_value *pv = NULL;
  CHECK(as_eval(iso, prog, strlen(prog), &pv) == AS_OK, "eval plugin program");
  as_value_free(pv);

  /* 5. call transform(7) → 7 * 10 = 70 (plain host fn + userdata). */
  as_value *arg = as_int(7);
  const as_value *call_args[1] = {arg};
  as_value *res = NULL;
  CHECK(as_call(iso, "transform", strlen("transform"), call_args, 1, &res) == AS_OK,
        "call transform");
  int64_t rn = 0;
  CHECK(as_value_int(res, &rn) == AS_OK && rn == 70, "transform(7)==70");
  as_value_free(arg);
  as_value_free(res);

  /* 6. the FALLIBLE tier: checked(0) → the script prints an err line + returns -1. */
  as_value *zero = as_int(0);
  const as_value *zero_args[1] = {zero};
  as_value *cres = NULL;
  CHECK(as_call(iso, "checked", strlen("checked"), zero_args, 1, &cres) == AS_OK,
        "call checked(0)");
  int64_t cn = 0;
  CHECK(as_value_int(cres, &cn) == AS_OK && cn == -1, "checked(0)==-1");
  as_value_free(zero);
  as_value_free(cres);

  /* the happy path of the fallible tier: checked(4) → 12. */
  as_value *four = as_int(4);
  const as_value *four_args[1] = {four};
  as_value *cres2 = NULL;
  CHECK(as_call(iso, "checked", strlen("checked"), four_args, 1, &cres2) == AS_OK,
        "call checked(4)");
  CHECK(as_value_int(cres2, &cn) == AS_OK && cn == 12, "checked(4)==12");
  as_value_free(four);
  as_value_free(cres2);

  /* 7. output capture: the checked(0) err line must be in the buffer. */
  char *out = NULL;
  size_t out_len = 0;
  CHECK(as_take_output(iso, &out, &out_len) == AS_OK, "take_output");
  CHECK(out_len > 0 && strstr(out, "checked(0): err=argument was zero") != NULL,
        "output content");
  as_string_free(out);

  /* 8. a forced runtime error + as_last_error. */
  const char *bad = "let z = 1\nz.nope()";
  as_value *bv = NULL;
  as_status st = as_eval(iso, bad, strlen(bad), &bv);
  CHECK(st == AS_ERR_PANIC || st == AS_ERR_COMPILE, "expected error status");
  const char *emsg = NULL;
  size_t emsg_len = 0;
  CHECK(as_last_error(iso, &emsg, &emsg_len) == AS_OK && emsg_len > 0,
        "last_error non-empty");

  /* 9. the JSON bridge round-trip. */
  const char *json = "{\"id\":7,\"tags\":[\"a\",\"b\"]}";
  as_value *jv = NULL;
  CHECK(as_json_parse(iso, json, strlen(json), &jv) == AS_OK, "json parse");
  int kind = -1;
  CHECK(as_value_kind(jv, &kind) == AS_OK && kind == AS_KIND_OBJECT, "json kind");
  char *jout = NULL;
  size_t jlen = 0;
  CHECK(as_value_to_json(iso, jv, &jout, &jlen) == AS_OK, "to_json");
  CHECK(strcmp(jout, "{\"id\":7,\"tags\":[\"a\",\"b\"]}") == 0, "json roundtrip");
  as_string_free(jout);
  as_value_free(jv);

  /* 10. NULL-safety + free the isolate. */
  as_value_free(NULL);
  as_string_free(NULL);
  as_isolate_free(iso);
  as_isolate_free(NULL);

  printf("EMBED-C-HOST-OK\n");
  return 0;
}
