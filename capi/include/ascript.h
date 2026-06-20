/* ascript.h — the C ABI for embedding the AScript engine (EMBED §8).
 *
 * Hand-written, checked in. A drift test (capi/tests/header_drift.rs) asserts this
 * header's `as_*`/`ascript_*` declarations match the crate's exported `#[no_mangle]`
 * symbols exactly, both directions.
 *
 * ABI CONVENTIONS
 *   - Every handle is OPAQUE (as_isolate / as_value); construct/free only via the fns
 *     here. Every value/string handle is owned by the caller until passed to the engine
 *     or freed.
 *   - Every string crossing IN is UTF-8 with an EXPLICIT length (NOT NUL-terminated);
 *     invalid UTF-8 → AS_ERR_UTF8. Every string OUT carries an explicit byte length and
 *     is also NUL-terminated for convenience; free it with as_string_free.
 *   - Every fn returns an as_status. AS_OK (0) is success; consult as_last_error for a
 *     human message after any error.
 *   - NULL pointer args are CHECKED (never dereferenced) → AS_ERR_CONFIG.
 *
 * THREADING (the honest model — EMBED §1, §8.2)
 *   - An as_isolate is !Send: it must be used ONLY from its creating thread. Every entry
 *     compares the calling thread id and returns AS_ERR_WRONG_THREAD instead of touching
 *     any internal state cross-thread (a checked error, never undefined behavior).
 *   - A host that wants N threads creates N isolates (one per thread). There is no lock,
 *     no shared isolate.
 *   - as_value handles are likewise thread-affine. The one unfixable case —
 *     as_value_free from a thread other than the value's creating thread — LEAKS the
 *     handle and returns (an off-thread refcount decrement is a data race; a documented
 *     leak beats UB). Free values on their creating thread.
 *   - as_isolate_free must be called on the isolate's creating thread.
 *
 * PANIC SAFETY
 *   - Every fn catches internal Rust panics. A caught panic POISONS the isolate (every
 *     subsequent call except as_isolate_free / as_last_error returns AS_ERR_POISONED) and
 *     returns AS_ERR_INTERNAL. An unwind never crosses the C boundary.
 */
#ifndef ASCRIPT_H
#define ASCRIPT_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The ABI version this header describes. A host MUST assert
 * `ascript_abi_version() == ASCRIPT_CAPI_ABI` at load time. Bumps only on a breaking
 * C-surface change. */
#define ASCRIPT_CAPI_ABI 1

/* Status codes. */
typedef enum {
  AS_OK = 0,
  AS_ERR_COMPILE = 1,       /* lex/parse/compile diagnostics */
  AS_ERR_PANIC = 2,         /* a Tier-2 runtime panic (session survives) */
  AS_ERR_EXIT = 3,          /* the script called exit(n) */
  AS_ERR_UTF8 = 4,          /* an input string was not valid UTF-8 */
  AS_ERR_TYPE = 5,          /* a value read on a mismatched kind */
  AS_ERR_UNDEFINED = 6,     /* as_call target not defined / not callable */
  AS_ERR_CONFIG = 7,        /* NULL arg, builder/registration misuse, bad JSON */
  AS_ERR_WRONG_THREAD = 8,  /* called from a thread other than the isolate's */
  AS_ERR_NESTED_RUNTIME = 9,/* blocking call from inside an async runtime */
  AS_ERR_POISONED = 10,     /* the isolate was poisoned by a prior internal panic */
  AS_ERR_INTERNAL = 127     /* a caught internal panic (the isolate is now poisoned) */
} as_status;

/* Value kinds (as_value_kind). */
typedef enum {
  AS_KIND_NIL = 0,
  AS_KIND_BOOL = 1,
  AS_KIND_INT = 2,
  AS_KIND_FLOAT = 3,
  AS_KIND_DECIMAL = 4,
  AS_KIND_STR = 5,
  AS_KIND_ARRAY = 6,
  AS_KIND_OBJECT = 7,
  AS_KIND_MAP = 8,
  AS_KIND_SET = 9,
  AS_KIND_BYTES = 10,
  AS_KIND_CALLABLE = 11,
  AS_KIND_FUTURE = 12,
  AS_KIND_OPAQUE = 13
} as_kind;

/* Opaque handles. */
typedef struct CIsolate as_isolate;
typedef struct CValue   as_value;

/* Version / ABI guard. */
uint32_t ascript_version(void);      /* packed crate semver: major<<16 | minor<<8 | patch */
uint32_t ascript_abi_version(void);  /* == ASCRIPT_CAPI_ABI of the loaded library */

/* Isolate lifecycle. as_isolate_new: deny-all caps, captured output; NULL on failure. */
as_isolate *as_isolate_new(void);
void        as_isolate_free(as_isolate *iso);
/* Last error message for this isolate (borrowed; valid until the next call). Works even
 * when the isolate is poisoned. */
as_status as_last_error(const as_isolate *iso, const char **msg, size_t *msg_len);

/* Compile + run `src` (src_len UTF-8 bytes) on the isolate, BLOCKING until quiescent. On
 * AS_OK, *out receives a caller-owned trailing-value handle (free with as_value_free). */
as_status as_eval(as_isolate *iso, const char *src, size_t src_len, as_value **out);
/* Call a module-scope global `name` with nargs argument handles; auto-awaits an async fn
 * result. On AS_OK, *out receives a caller-owned result handle. */
as_status as_call(as_isolate *iso, const char *name, size_t name_len,
                  const as_value *const *args, size_t nargs, as_value **out);

/* Value constructors (caller-owned until passed to the engine or freed). */
as_value *as_nil(void);
as_value *as_bool(bool b);
as_value *as_int(int64_t n);
as_value *as_float(double x);
as_value *as_string(const char *utf8, size_t len);  /* NULL on invalid UTF-8 */
void      as_value_free(as_value *v);                /* creating-thread only (see THREADING) */

/* Value readers (thread-affine to the value's creating thread). */
as_status as_value_kind(const as_value *v, int *out);            /* AS_KIND_* */
as_status as_value_int(const as_value *v, int64_t *out);         /* AS_ERR_TYPE on mismatch */
as_status as_value_float(const as_value *v, double *out);
as_status as_value_bool(const as_value *v, bool *out);
/* Borrow a string value's UTF-8 bytes (*ptr/*len; valid until the value is freed). */
as_status as_value_string(const as_value *v, const char **ptr, size_t *len);

#ifdef __cplusplus
}
#endif

#endif /* ASCRIPT_H */
