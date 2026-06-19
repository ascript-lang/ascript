//! Curated std-library signature table for the LSP and completion surface.
//!
//! This is pure STATIC DATA — no runtime, no feature-gated imports.  It builds
//! cleanly under `--no-default-features` because `std_module_exports` is only
//! called from `#[cfg(test)]` blocks (which are feature-aware by construction).
//!
//! ## Authoring conventions
//!
//! Use the `StdParam` constructors (req / req_untyped / opt / variadic /
//! with_default) together with `StdSig::new` to build rows.  The optional-ordering invariant
//! ("no required param may follow an optional or variadic one") is enforced by
//! `validate_param_order` called from the `table_ordering_invariant` test.
//!
//! The doc string for each entry is the **first sentence** of the corresponding
//! `docs/content/stdlib/*.md` prose paragraph.
//!
//! ## Coverage scope
//!
//! This file covers ALL of STD_MODULES (60 modules, batches 1–3).

// ─────────────────────────────────────────────────────────────────────────────
// Public types (spec §2.1)
// ─────────────────────────────────────────────────────────────────────────────

/// One parameter of a curated std signature.
#[derive(Debug)]
pub struct StdParam {
    pub name: &'static str,
    /// Rendered annotation, display text only (not enforced at runtime).
    pub ty: Option<&'static str>,
    /// Optional trailing parameter; never followed by a required one.
    pub optional: bool,
    /// `...rest` collector — always last when present.
    pub variadic: bool,
    /// Rendered default when documented.
    pub default: Option<&'static str>,
}

impl StdParam {
    /// Required positional parameter.
    pub const fn req(name: &'static str, ty: &'static str) -> Self {
        Self { name, ty: Some(ty), optional: false, variadic: false, default: None }
    }

    /// Required parameter with no annotated type (for untyped "any" positions).
    pub const fn req_untyped(name: &'static str) -> Self {
        Self { name, ty: None, optional: false, variadic: false, default: None }
    }

    /// Optional trailing parameter.
    pub const fn opt(name: &'static str, ty: &'static str) -> Self {
        Self { name, ty: Some(ty), optional: true, variadic: false, default: None }
    }

    /// Optional trailing parameter with a rendered default value.
    pub const fn with_default(name: &'static str, ty: &'static str, default: &'static str) -> Self {
        Self { name, ty: Some(ty), optional: true, variadic: false, default: Some(default) }
    }

    /// Variadic rest collector — always last.
    pub const fn variadic(name: &'static str, ty: &'static str) -> Self {
        Self { name, ty: Some(ty), optional: false, variadic: true, default: None }
    }
}

/// A curated std fn signature + one-line doc.
#[derive(Debug)]
pub struct StdSig {
    pub params: &'static [StdParam],
    /// Rendered return type annotation (display only).
    pub ret: Option<&'static str>,
    /// First sentence of the docs entry.
    pub doc: &'static str,
}

/// fn vs constant distinction for the completion/auto-import surface.
#[derive(Debug, Clone, Copy)]
pub enum MemberKind {
    /// A callable function; a `StdSig` row must exist.
    Fn,
    /// A non-callable constant export with its type annotation.
    Const(&'static str),
    /// A method on a native handle, not a module export.
    HandleMethod,
}

// ─────────────────────────────────────────────────────────────────────────────
// Ordering-invariant validator
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that no required parameter follows an optional or variadic one.
/// Returns `Ok(())` on success or `Err(msg)` describing the violation.
pub fn validate_param_order(sig_name: &str, params: &[StdParam]) -> Result<(), String> {
    let mut seen_optional = false;
    for p in params {
        if p.variadic {
            // variadic must be last — anything that follows would be a bug caught
            // in a subsequent iteration (there shouldn't be a subsequent iteration
            // in a well-formed sig, but we still check)
            seen_optional = true;
            continue;
        }
        if p.optional {
            seen_optional = true;
        } else if seen_optional {
            // required param after optional/variadic
            return Err(format!(
                "{}: required param '{}' follows an optional/variadic param",
                sig_name, p.name
            ));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tables
// ─────────────────────────────────────────────────────────────────────────────

// ── std/math ─────────────────────────────────────────────────────────────────

static MATH_ABS_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_FLOOR_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_CEIL_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_ROUND_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_SQRT_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_POW_PARAMS: &[StdParam] = &[
    StdParam::req("base", "number"),
    StdParam::req("exp", "number"),
];
static MATH_MIN_PARAMS: &[StdParam] = &[StdParam::variadic("nums", "number")];
static MATH_MAX_PARAMS: &[StdParam] = &[StdParam::variadic("nums", "number")];
static MATH_RANDOM_PARAMS: &[StdParam] = &[];
static MATH_SIN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_COS_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_TAN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_ASIN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_ACOS_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_ATAN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_ATAN2_PARAMS: &[StdParam] = &[
    StdParam::req("y", "number"),
    StdParam::req("x", "number"),
];
static MATH_EXP_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_LN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_LOG2_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_LOG10_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_SIGN_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_TRUNC_PARAMS: &[StdParam] = &[StdParam::req("x", "number")];
static MATH_CLAMP_PARAMS: &[StdParam] = &[
    StdParam::req("x", "number"),
    StdParam::req("lo", "number"),
    StdParam::req("hi", "number"),
];
static MATH_HYPOT_PARAMS: &[StdParam] = &[
    StdParam::req("a", "number"),
    StdParam::req("b", "number"),
];
static MATH_GCD_PARAMS: &[StdParam] = &[
    StdParam::req("a", "number"),
    StdParam::req("b", "number"),
];
static MATH_LCM_PARAMS: &[StdParam] = &[
    StdParam::req("a", "number"),
    StdParam::req("b", "number"),
];
static MATH_SUM_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static MATH_MEAN_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static MATH_MEDIAN_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static MATH_VARIANCE_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::with_default("sample", "bool", "false"),
];
static MATH_STDDEV_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::with_default("sample", "bool", "false"),
];
static MATH_RANDOM_INT_PARAMS: &[StdParam] = &[
    StdParam::req("min", "number"),
    StdParam::req("max", "number"),
];
static MATH_SHUFFLE_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static MATH_CHOICE_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static MATH_FLOORDIV_PARAMS: &[StdParam] = &[
    StdParam::req("a", "int"),
    StdParam::req("b", "int"),
];
static MATH_CEILDIV_PARAMS: &[StdParam] = &[
    StdParam::req("a", "int"),
    StdParam::req("b", "int"),
];
static MATH_DIVMOD_PARAMS: &[StdParam] = &[
    StdParam::req("a", "int"),
    StdParam::req("b", "int"),
];
static MATH_POPCOUNT_PARAMS: &[StdParam] = &[StdParam::req("x", "int")];
static MATH_LEADING_ZEROS_PARAMS: &[StdParam] = &[StdParam::req("x", "int")];
static MATH_TRAILING_ZEROS_PARAMS: &[StdParam] = &[StdParam::req("x", "int")];
static MATH_ROTL_PARAMS: &[StdParam] = &[
    StdParam::req("x", "int"),
    StdParam::req("n", "int"),
];
static MATH_ROTR_PARAMS: &[StdParam] = &[
    StdParam::req("x", "int"),
    StdParam::req("n", "int"),
];

static MATH_SIGS: &[(&str, StdSig)] = &[
    ("abs", StdSig { params: MATH_ABS_PARAMS, ret: Some("number"), doc: "Absolute value." }),
    ("floor", StdSig { params: MATH_FLOOR_PARAMS, ret: Some("int"), doc: "Round down toward negative infinity." }),
    ("ceil", StdSig { params: MATH_CEIL_PARAMS, ret: Some("int"), doc: "Round up toward positive infinity." }),
    ("round", StdSig { params: MATH_ROUND_PARAMS, ret: Some("int"), doc: "Round to the nearest integer (halves round away from zero)." }),
    ("sqrt", StdSig { params: MATH_SQRT_PARAMS, ret: Some("float"), doc: "Square root." }),
    ("pow", StdSig { params: MATH_POW_PARAMS, ret: Some("float"), doc: "Raise a base to an exponent." }),
    ("min", StdSig { params: MATH_MIN_PARAMS, ret: Some("float"), doc: "Return the smallest of one or more arguments." }),
    ("max", StdSig { params: MATH_MAX_PARAMS, ret: Some("float"), doc: "Return the largest of one or more arguments." }),
    ("random", StdSig { params: MATH_RANDOM_PARAMS, ret: Some("float"), doc: "Return a pseudo-random number in the half-open range [0, 1)." }),
    ("sin", StdSig { params: MATH_SIN_PARAMS, ret: Some("float"), doc: "Sine of an angle in radians." }),
    ("cos", StdSig { params: MATH_COS_PARAMS, ret: Some("float"), doc: "Cosine of an angle in radians." }),
    ("tan", StdSig { params: MATH_TAN_PARAMS, ret: Some("float"), doc: "Tangent of an angle in radians." }),
    ("asin", StdSig { params: MATH_ASIN_PARAMS, ret: Some("float"), doc: "Arc-sine (inverse sine)." }),
    ("acos", StdSig { params: MATH_ACOS_PARAMS, ret: Some("float"), doc: "Arc-cosine (inverse cosine)." }),
    ("atan", StdSig { params: MATH_ATAN_PARAMS, ret: Some("float"), doc: "Arc-tangent (inverse tangent)." }),
    ("atan2", StdSig { params: MATH_ATAN2_PARAMS, ret: Some("float"), doc: "Two-argument arc-tangent." }),
    ("exp", StdSig { params: MATH_EXP_PARAMS, ret: Some("float"), doc: "Euler's number raised to the power x (eˣ)." }),
    ("ln", StdSig { params: MATH_LN_PARAMS, ret: Some("float"), doc: "Natural logarithm (base e)." }),
    ("log2", StdSig { params: MATH_LOG2_PARAMS, ret: Some("float"), doc: "Base-2 logarithm." }),
    ("log10", StdSig { params: MATH_LOG10_PARAMS, ret: Some("float"), doc: "Base-10 logarithm." }),
    ("sign", StdSig { params: MATH_SIGN_PARAMS, ret: Some("float"), doc: "Return -1.0, 0.0, or 1.0 depending on the sign of x." }),
    ("trunc", StdSig { params: MATH_TRUNC_PARAMS, ret: Some("int"), doc: "Truncate toward zero (drop the fractional part)." }),
    ("clamp", StdSig { params: MATH_CLAMP_PARAMS, ret: Some("float"), doc: "Clamp x to the closed interval [lo, hi]." }),
    ("hypot", StdSig { params: MATH_HYPOT_PARAMS, ret: Some("float"), doc: "Euclidean distance — square root of the sum of squares." }),
    ("gcd", StdSig { params: MATH_GCD_PARAMS, ret: Some("number"), doc: "Greatest common divisor of two non-negative integers." }),
    ("lcm", StdSig { params: MATH_LCM_PARAMS, ret: Some("number"), doc: "Least common multiple of two non-negative integers." }),
    ("sum", StdSig { params: MATH_SUM_PARAMS, ret: Some("float"), doc: "Sum all elements of a numeric array." }),
    ("mean", StdSig { params: MATH_MEAN_PARAMS, ret: Some("number"), doc: "Arithmetic mean of a numeric array." }),
    ("median", StdSig { params: MATH_MEDIAN_PARAMS, ret: Some("number"), doc: "Median of a numeric array." }),
    ("variance", StdSig { params: MATH_VARIANCE_PARAMS, ret: Some("number"), doc: "Population or sample variance of a numeric array." }),
    ("stddev", StdSig { params: MATH_STDDEV_PARAMS, ret: Some("number"), doc: "Population or sample standard deviation." }),
    ("randomInt", StdSig { params: MATH_RANDOM_INT_PARAMS, ret: Some("float"), doc: "Return a uniformly distributed random integer-valued float in the inclusive range [min, max]." }),
    ("shuffle", StdSig { params: MATH_SHUFFLE_PARAMS, ret: Some("array"), doc: "Return a new array with the elements in a random order (Fisher-Yates)." }),
    ("choice", StdSig { params: MATH_CHOICE_PARAMS, ret: Some("any"), doc: "Return a uniformly random element from a non-empty array." }),
    ("floordiv", StdSig { params: MATH_FLOORDIV_PARAMS, ret: Some("int"), doc: "Floored integer division: the quotient rounded toward negative infinity." }),
    ("ceildiv", StdSig { params: MATH_CEILDIV_PARAMS, ret: Some("int"), doc: "Ceiling integer division: the quotient rounded toward positive infinity." }),
    ("divmod", StdSig { params: MATH_DIVMOD_PARAMS, ret: Some("[int, int]"), doc: "Combined floored quotient and matching remainder as a two-element array [q, r]." }),
    ("popcount", StdSig { params: MATH_POPCOUNT_PARAMS, ret: Some("int"), doc: "The number of set (one) bits." }),
    ("leading_zeros", StdSig { params: MATH_LEADING_ZEROS_PARAMS, ret: Some("int"), doc: "The number of leading zero bits in the 64-bit representation." }),
    ("trailing_zeros", StdSig { params: MATH_TRAILING_ZEROS_PARAMS, ret: Some("int"), doc: "The number of trailing zero bits in the 64-bit representation." }),
    ("rotl", StdSig { params: MATH_ROTL_PARAMS, ret: Some("int"), doc: "Rotate the 64-bit value left by n bits (count taken modulo 64)." }),
    ("rotr", StdSig { params: MATH_ROTR_PARAMS, ret: Some("int"), doc: "Rotate the 64-bit value right by n bits (count taken modulo 64)." }),
];

static MATH_MEMBERS: &[(&str, MemberKind)] = &[
    ("abs", MemberKind::Fn),
    ("floor", MemberKind::Fn),
    ("ceil", MemberKind::Fn),
    ("round", MemberKind::Fn),
    ("sqrt", MemberKind::Fn),
    ("pow", MemberKind::Fn),
    ("min", MemberKind::Fn),
    ("max", MemberKind::Fn),
    ("random", MemberKind::Fn),
    ("sin", MemberKind::Fn),
    ("cos", MemberKind::Fn),
    ("tan", MemberKind::Fn),
    ("asin", MemberKind::Fn),
    ("acos", MemberKind::Fn),
    ("atan", MemberKind::Fn),
    ("atan2", MemberKind::Fn),
    ("exp", MemberKind::Fn),
    ("ln", MemberKind::Fn),
    ("log2", MemberKind::Fn),
    ("log10", MemberKind::Fn),
    ("pi", MemberKind::Const("float")),
    ("e", MemberKind::Const("float")),
    ("sign", MemberKind::Fn),
    ("trunc", MemberKind::Fn),
    ("clamp", MemberKind::Fn),
    ("hypot", MemberKind::Fn),
    ("gcd", MemberKind::Fn),
    ("lcm", MemberKind::Fn),
    ("sum", MemberKind::Fn),
    ("mean", MemberKind::Fn),
    ("median", MemberKind::Fn),
    ("variance", MemberKind::Fn),
    ("stddev", MemberKind::Fn),
    ("randomInt", MemberKind::Fn),
    ("shuffle", MemberKind::Fn),
    ("choice", MemberKind::Fn),
    ("floordiv", MemberKind::Fn),
    ("divmod", MemberKind::Fn),
    ("ceildiv", MemberKind::Fn),
    ("popcount", MemberKind::Fn),
    ("leading_zeros", MemberKind::Fn),
    ("trailing_zeros", MemberKind::Fn),
    ("rotl", MemberKind::Fn),
    ("rotr", MemberKind::Fn),
];

// ── std/string ───────────────────────────────────────────────────────────────

static STRING_SPLIT_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("sep", "string"),
];
static STRING_JOIN_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("sep", "string"),
];
static STRING_SLICE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("start", "number"),
    StdParam::opt("end", "number"),
];
static STRING_TRIM_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_UPPER_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_LOWER_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_FIND_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("sub", "string"),
];
static STRING_REPLACE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("from", "string"),
    StdParam::req("to", "string"),
];
static STRING_REPLACE_ALL_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("from", "string"),
    StdParam::req("to", "string"),
];
static STRING_FORMAT_PARAMS: &[StdParam] = &[
    StdParam::req("template", "string"),
    StdParam::variadic("args", "any"),
];
static STRING_PAD_START_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("width", "number"),
    StdParam::with_default("fill", "string", "\" \""),
];
static STRING_PAD_END_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("width", "number"),
    StdParam::with_default("fill", "string", "\" \""),
];
static STRING_REPEAT_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("n", "number"),
];
static STRING_STARTS_WITH_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("prefix", "string"),
];
static STRING_ENDS_WITH_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("suffix", "string"),
];
static STRING_CONTAINS_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("sub", "string"),
];
static STRING_CHARS_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_LINES_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_REVERSE_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_COUNT_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("sub", "string"),
];
static STRING_SPLIT_N_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("sep", "string"),
    StdParam::req("n", "number"),
];
static STRING_CODEPOINTS_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static STRING_FROM_CODEPOINTS_PARAMS: &[StdParam] = &[StdParam::req("cps", "array<int>")];
static STRING_CODE_AT_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::req("i", "int"),
];

static STRING_SIGS: &[(&str, StdSig)] = &[
    ("split", StdSig { params: STRING_SPLIT_PARAMS, ret: Some("array"), doc: "Split a string into an array of substrings on a separator." }),
    ("join", StdSig { params: STRING_JOIN_PARAMS, ret: Some("string"), doc: "Join an array into a single string, inserting a separator between elements." }),
    ("slice", StdSig { params: STRING_SLICE_PARAMS, ret: Some("string"), doc: "Extract a substring between two character indices." }),
    ("trim", StdSig { params: STRING_TRIM_PARAMS, ret: Some("string"), doc: "Remove leading and trailing whitespace." }),
    ("upper", StdSig { params: STRING_UPPER_PARAMS, ret: Some("string"), doc: "Convert a string to uppercase." }),
    ("lower", StdSig { params: STRING_LOWER_PARAMS, ret: Some("string"), doc: "Convert a string to lowercase." }),
    ("find", StdSig { params: STRING_FIND_PARAMS, ret: Some("number"), doc: "Find the character index of the first occurrence of a substring." }),
    ("replace", StdSig { params: STRING_REPLACE_PARAMS, ret: Some("string"), doc: "Replace the first occurrence of a substring." }),
    ("replaceAll", StdSig { params: STRING_REPLACE_ALL_PARAMS, ret: Some("string"), doc: "Replace all occurrences of a substring." }),
    ("format", StdSig { params: STRING_FORMAT_PARAMS, ret: Some("string"), doc: "Substitute positional arguments into a template." }),
    ("padStart", StdSig { params: STRING_PAD_START_PARAMS, ret: Some("string"), doc: "Pad the start of a string with a fill string until it reaches a target character width." }),
    ("padEnd", StdSig { params: STRING_PAD_END_PARAMS, ret: Some("string"), doc: "Pad the end of a string with a fill string until it reaches a target character width." }),
    ("repeat", StdSig { params: STRING_REPEAT_PARAMS, ret: Some("string"), doc: "Concatenate n copies of a string." }),
    ("startsWith", StdSig { params: STRING_STARTS_WITH_PARAMS, ret: Some("bool"), doc: "Test whether a string begins with a given prefix." }),
    ("endsWith", StdSig { params: STRING_ENDS_WITH_PARAMS, ret: Some("bool"), doc: "Test whether a string ends with a given suffix." }),
    ("contains", StdSig { params: STRING_CONTAINS_PARAMS, ret: Some("bool"), doc: "Test whether a string contains a substring." }),
    ("chars", StdSig { params: STRING_CHARS_PARAMS, ret: Some("array"), doc: "Split a string into an array of individual characters (Unicode scalar values)." }),
    ("lines", StdSig { params: STRING_LINES_PARAMS, ret: Some("array"), doc: "Split a string into an array of lines." }),
    ("reverse", StdSig { params: STRING_REVERSE_PARAMS, ret: Some("string"), doc: "Return a string with its characters in reverse order." }),
    ("count", StdSig { params: STRING_COUNT_PARAMS, ret: Some("number"), doc: "Count the non-overlapping occurrences of a substring." }),
    ("splitN", StdSig { params: STRING_SPLIT_N_PARAMS, ret: Some("array"), doc: "Split a string on a separator, returning at most n parts." }),
    ("codepoints", StdSig { params: STRING_CODEPOINTS_PARAMS, ret: Some("array<int>"), doc: "Return the string's Unicode scalar values as an array<int>." }),
    ("from_codepoints", StdSig { params: STRING_FROM_CODEPOINTS_PARAMS, ret: Some("string"), doc: "Build a string from an array of Unicode scalar values (the inverse of codepoints)." }),
    ("code_at", StdSig { params: STRING_CODE_AT_PARAMS, ret: Some("int"), doc: "Return the Unicode scalar value (an int) at character index i." }),
];

static STRING_MEMBERS: &[(&str, MemberKind)] = &[
    ("split", MemberKind::Fn),
    ("join", MemberKind::Fn),
    ("slice", MemberKind::Fn),
    ("trim", MemberKind::Fn),
    ("upper", MemberKind::Fn),
    ("lower", MemberKind::Fn),
    ("find", MemberKind::Fn),
    ("replace", MemberKind::Fn),
    ("replaceAll", MemberKind::Fn),
    ("format", MemberKind::Fn),
    ("padStart", MemberKind::Fn),
    ("padEnd", MemberKind::Fn),
    ("repeat", MemberKind::Fn),
    ("startsWith", MemberKind::Fn),
    ("endsWith", MemberKind::Fn),
    ("contains", MemberKind::Fn),
    ("chars", MemberKind::Fn),
    ("lines", MemberKind::Fn),
    ("reverse", MemberKind::Fn),
    ("count", MemberKind::Fn),
    ("splitN", MemberKind::Fn),
    ("codepoints", MemberKind::Fn),
    ("from_codepoints", MemberKind::Fn),
    ("code_at", MemberKind::Fn),
];

// ── std/array ────────────────────────────────────────────────────────────────

static ARRAY_MAP_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_FILTER_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_REDUCE_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(acc, item)"),
    StdParam::req_untyped("init"),
];
static ARRAY_PUSH_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req_untyped("item"),
];
static ARRAY_POP_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static ARRAY_SLICE_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("start", "number"),
    StdParam::opt("end", "number"),
];
static ARRAY_SORT_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::opt("cmp", "fn(a, b)"),
];
static ARRAY_CONTAINS_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req_untyped("needle"),
];
static ARRAY_GET_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("i", "number"),
];
static ARRAY_FIND_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_FIND_INDEX_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_SOME_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_EVERY_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_INDEX_OF_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req_untyped("needle"),
];
static ARRAY_FLAT_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::with_default("depth", "number", "1"),
];
static ARRAY_FLAT_MAP_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_REVERSE_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static ARRAY_CONCAT_PARAMS: &[StdParam] = &[StdParam::variadic("arrays", "array")];
static ARRAY_FIRST_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static ARRAY_LAST_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static ARRAY_UNIQUE_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static ARRAY_TAKE_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("n", "number"),
];
static ARRAY_DROP_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("n", "number"),
];
static ARRAY_CHUNK_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("size", "number"),
];
static ARRAY_ZIP_PARAMS: &[StdParam] = &[
    StdParam::req("a", "array"),
    StdParam::req("b", "array"),
];
static ARRAY_GROUP_BY_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];
static ARRAY_PARTITION_PARAMS: &[StdParam] = &[
    StdParam::req("arr", "array"),
    StdParam::req("f", "fn(item)"),
];

static ARRAY_SIGS: &[(&str, StdSig)] = &[
    ("map", StdSig { params: ARRAY_MAP_PARAMS, ret: Some("array"), doc: "Apply a function to every element, producing a new array." }),
    ("filter", StdSig { params: ARRAY_FILTER_PARAMS, ret: Some("array"), doc: "Keep only the elements for which the predicate returns a truthy value." }),
    ("reduce", StdSig { params: ARRAY_REDUCE_PARAMS, ret: None, doc: "Fold an array into a single accumulated value, left to right." }),
    ("push", StdSig { params: ARRAY_PUSH_PARAMS, ret: Some("number"), doc: "Append an element to an array, mutating it in place." }),
    ("pop", StdSig { params: ARRAY_POP_PARAMS, ret: None, doc: "Remove and return the last element, mutating the array in place." }),
    ("slice", StdSig { params: ARRAY_SLICE_PARAMS, ret: Some("array"), doc: "Extract a subrange between two indices." }),
    ("sort", StdSig { params: ARRAY_SORT_PARAMS, ret: Some("array"), doc: "Return a new sorted array." }),
    ("contains", StdSig { params: ARRAY_CONTAINS_PARAMS, ret: Some("bool"), doc: "Test whether an array contains a value, using structural equality." }),
    ("get", StdSig { params: ARRAY_GET_PARAMS, ret: None, doc: "Read the element at an index." }),
    ("find", StdSig { params: ARRAY_FIND_PARAMS, ret: None, doc: "Return the first element for which the predicate returns truthy." }),
    ("findIndex", StdSig { params: ARRAY_FIND_INDEX_PARAMS, ret: Some("number"), doc: "Return the index of the first element for which the predicate returns truthy." }),
    ("some", StdSig { params: ARRAY_SOME_PARAMS, ret: Some("bool"), doc: "Return true if the predicate returns truthy for at least one element." }),
    ("every", StdSig { params: ARRAY_EVERY_PARAMS, ret: Some("bool"), doc: "Return true if the predicate returns truthy for every element." }),
    ("indexOf", StdSig { params: ARRAY_INDEX_OF_PARAMS, ret: Some("number"), doc: "Return the index of the first element equal to needle (structural equality)." }),
    ("flat", StdSig { params: ARRAY_FLAT_PARAMS, ret: Some("array"), doc: "Flatten nested arrays by depth levels (default 1)." }),
    ("flatMap", StdSig { params: ARRAY_FLAT_MAP_PARAMS, ret: Some("array"), doc: "Apply f to every element and flatten the result one level." }),
    ("reverse", StdSig { params: ARRAY_REVERSE_PARAMS, ret: Some("array"), doc: "Return a new array with the elements in reversed order." }),
    ("concat", StdSig { params: ARRAY_CONCAT_PARAMS, ret: Some("array"), doc: "Concatenate any number of arrays into a single new array." }),
    ("first", StdSig { params: ARRAY_FIRST_PARAMS, ret: None, doc: "Return the first element of the array, or nil if the array is empty." }),
    ("last", StdSig { params: ARRAY_LAST_PARAMS, ret: None, doc: "Return the last element of the array, or nil if the array is empty." }),
    ("unique", StdSig { params: ARRAY_UNIQUE_PARAMS, ret: Some("array"), doc: "Return a new array with duplicate elements removed, preserving the first occurrence order." }),
    ("take", StdSig { params: ARRAY_TAKE_PARAMS, ret: Some("array"), doc: "Return the first n elements." }),
    ("drop", StdSig { params: ARRAY_DROP_PARAMS, ret: Some("array"), doc: "Return all elements after skipping the first n." }),
    ("chunk", StdSig { params: ARRAY_CHUNK_PARAMS, ret: Some("array"), doc: "Split an array into consecutive chunks of size size." }),
    ("zip", StdSig { params: ARRAY_ZIP_PARAMS, ret: Some("array"), doc: "Interleave two arrays element by element into an array of [a, b] pairs." }),
    ("groupBy", StdSig { params: ARRAY_GROUP_BY_PARAMS, ret: Some("map"), doc: "Group elements by the return value of a key function." }),
    ("partition", StdSig { params: ARRAY_PARTITION_PARAMS, ret: Some("[array, array]"), doc: "Split an array into two arrays: elements that pass the predicate and elements that do not." }),
];

static ARRAY_MEMBERS: &[(&str, MemberKind)] = &[
    ("map", MemberKind::Fn),
    ("filter", MemberKind::Fn),
    ("reduce", MemberKind::Fn),
    ("push", MemberKind::Fn),
    ("pop", MemberKind::Fn),
    ("slice", MemberKind::Fn),
    ("sort", MemberKind::Fn),
    ("contains", MemberKind::Fn),
    ("get", MemberKind::Fn),
    ("find", MemberKind::Fn),
    ("findIndex", MemberKind::Fn),
    ("some", MemberKind::Fn),
    ("every", MemberKind::Fn),
    ("indexOf", MemberKind::Fn),
    ("flat", MemberKind::Fn),
    ("flatMap", MemberKind::Fn),
    ("reverse", MemberKind::Fn),
    ("concat", MemberKind::Fn),
    ("first", MemberKind::Fn),
    ("last", MemberKind::Fn),
    ("unique", MemberKind::Fn),
    ("take", MemberKind::Fn),
    ("drop", MemberKind::Fn),
    ("chunk", MemberKind::Fn),
    ("zip", MemberKind::Fn),
    ("groupBy", MemberKind::Fn),
    ("partition", MemberKind::Fn),
];

// ── std/object ───────────────────────────────────────────────────────────────

static OBJECT_KEYS_PARAMS: &[StdParam] = &[StdParam::req("o", "object")];
static OBJECT_VALUES_PARAMS: &[StdParam] = &[StdParam::req("o", "object")];
static OBJECT_ENTRIES_PARAMS: &[StdParam] = &[StdParam::req("o", "object")];
static OBJECT_HAS_PARAMS: &[StdParam] = &[
    StdParam::req("o", "object"),
    StdParam::req("key", "string"),
];
static OBJECT_DELETE_PARAMS: &[StdParam] = &[
    StdParam::req("o", "object"),
    StdParam::req("key", "string"),
];
static OBJECT_MERGE_PARAMS: &[StdParam] = &[StdParam::variadic("objects", "object")];
static OBJECT_FROM_ENTRIES_PARAMS: &[StdParam] = &[StdParam::req("pairs", "array")];
static OBJECT_PICK_PARAMS: &[StdParam] = &[
    StdParam::req("o", "object"),
    StdParam::req("keys", "array"),
];
static OBJECT_OMIT_PARAMS: &[StdParam] = &[
    StdParam::req("o", "object"),
    StdParam::req("keys", "array"),
];
static OBJECT_DEEP_CLONE_PARAMS: &[StdParam] = &[StdParam::req("o", "object")];
static OBJECT_DEEP_EQUAL_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static OBJECT_MAP_VALUES_PARAMS: &[StdParam] = &[
    StdParam::req("o", "object"),
    StdParam::req("f", "fn(value, key)"),
];
static OBJECT_FREEZE_PARAMS: &[StdParam] = &[StdParam::req_untyped("x")];
static OBJECT_IS_FROZEN_PARAMS: &[StdParam] = &[StdParam::req_untyped("x")];

static OBJECT_SIGS: &[(&str, StdSig)] = &[
    ("keys", StdSig { params: OBJECT_KEYS_PARAMS, ret: Some("array"), doc: "Return an array of the object's keys, in insertion order." }),
    ("values", StdSig { params: OBJECT_VALUES_PARAMS, ret: Some("array"), doc: "Return an array of the object's values, in insertion order." }),
    ("entries", StdSig { params: OBJECT_ENTRIES_PARAMS, ret: Some("array"), doc: "Return an array of [key, value] pairs, in insertion order." }),
    ("has", StdSig { params: OBJECT_HAS_PARAMS, ret: Some("bool"), doc: "Test whether the object contains a key." }),
    ("delete", StdSig { params: OBJECT_DELETE_PARAMS, ret: Some("bool"), doc: "Remove a key, mutating the object in place." }),
    ("merge", StdSig { params: OBJECT_MERGE_PARAMS, ret: Some("object"), doc: "Merge any number of objects left to right into a new object; later keys overwrite earlier ones." }),
    ("fromEntries", StdSig { params: OBJECT_FROM_ENTRIES_PARAMS, ret: Some("object"), doc: "Construct an object from an array of [key, value] pairs." }),
    ("pick", StdSig { params: OBJECT_PICK_PARAMS, ret: Some("object"), doc: "Return a new object containing only the specified keys." }),
    ("omit", StdSig { params: OBJECT_OMIT_PARAMS, ret: Some("object"), doc: "Return a new object with the specified keys removed." }),
    ("deepClone", StdSig { params: OBJECT_DEEP_CLONE_PARAMS, ret: Some("object"), doc: "Recursively clone an object (and any nested objects, arrays, or maps) into a fully independent copy." }),
    ("deepEqual", StdSig { params: OBJECT_DEEP_EQUAL_PARAMS, ret: Some("bool"), doc: "Recursively compare two values for structural equality." }),
    ("mapValues", StdSig { params: OBJECT_MAP_VALUES_PARAMS, ret: Some("object"), doc: "Return a new object with each value transformed by f." }),
    ("freeze", StdSig { params: OBJECT_FREEZE_PARAMS, ret: None, doc: "Shallow-freeze a mutable container in place and return it." }),
    ("isFrozen", StdSig { params: OBJECT_IS_FROZEN_PARAMS, ret: Some("bool"), doc: "Whether the value is a frozen container." }),
];

static OBJECT_MEMBERS: &[(&str, MemberKind)] = &[
    ("keys", MemberKind::Fn),
    ("values", MemberKind::Fn),
    ("entries", MemberKind::Fn),
    ("has", MemberKind::Fn),
    ("delete", MemberKind::Fn),
    ("merge", MemberKind::Fn),
    ("fromEntries", MemberKind::Fn),
    ("pick", MemberKind::Fn),
    ("omit", MemberKind::Fn),
    ("deepClone", MemberKind::Fn),
    ("deepEqual", MemberKind::Fn),
    ("mapValues", MemberKind::Fn),
    ("freeze", MemberKind::Fn),
    ("isFrozen", MemberKind::Fn),
];

// ── std/map ──────────────────────────────────────────────────────────────────

static MAP_NEW_PARAMS: &[StdParam] = &[StdParam::opt("seed", "array")];
static MAP_GET_PARAMS: &[StdParam] = &[
    StdParam::req("m", "map"),
    StdParam::req_untyped("key"),
];
static MAP_SET_PARAMS: &[StdParam] = &[
    StdParam::req("m", "map"),
    StdParam::req_untyped("key"),
    StdParam::req_untyped("value"),
];
static MAP_HAS_PARAMS: &[StdParam] = &[
    StdParam::req("m", "map"),
    StdParam::req_untyped("key"),
];
static MAP_DELETE_PARAMS: &[StdParam] = &[
    StdParam::req("m", "map"),
    StdParam::req_untyped("key"),
];
static MAP_KEYS_PARAMS: &[StdParam] = &[StdParam::req("m", "map")];
static MAP_VALUES_PARAMS: &[StdParam] = &[StdParam::req("m", "map")];
static MAP_ENTRIES_PARAMS: &[StdParam] = &[StdParam::req("m", "map")];

static MAP_SIGS: &[(&str, StdSig)] = &[
    ("new", StdSig { params: MAP_NEW_PARAMS, ret: Some("map"), doc: "Create a new map, optionally seeded from an array of [key, value] pairs." }),
    ("get", StdSig { params: MAP_GET_PARAMS, ret: None, doc: "Read the value for a key; returns nil if the key is absent." }),
    ("set", StdSig { params: MAP_SET_PARAMS, ret: Some("map"), doc: "Insert or update a key/value pair, mutating the map in place." }),
    ("has", StdSig { params: MAP_HAS_PARAMS, ret: Some("bool"), doc: "Test whether the map contains a key." }),
    ("delete", StdSig { params: MAP_DELETE_PARAMS, ret: Some("bool"), doc: "Remove a key, mutating the map in place." }),
    ("keys", StdSig { params: MAP_KEYS_PARAMS, ret: Some("array"), doc: "Return an array of the map's keys, in insertion order." }),
    ("values", StdSig { params: MAP_VALUES_PARAMS, ret: Some("array"), doc: "Return an array of the map's values, in insertion order." }),
    ("entries", StdSig { params: MAP_ENTRIES_PARAMS, ret: Some("array"), doc: "Return an array of [key, value] pairs, in insertion order." }),
];

static MAP_MEMBERS: &[(&str, MemberKind)] = &[
    ("new", MemberKind::Fn),
    ("get", MemberKind::Fn),
    ("set", MemberKind::Fn),
    ("has", MemberKind::Fn),
    ("delete", MemberKind::Fn),
    ("keys", MemberKind::Fn),
    ("values", MemberKind::Fn),
    ("entries", MemberKind::Fn),
];

// ── std/set ──────────────────────────────────────────────────────────────────

static SET_NEW_PARAMS: &[StdParam] = &[];
static SET_FROM_PARAMS: &[StdParam] = &[StdParam::opt("arr", "array")];
static SET_ADD_PARAMS: &[StdParam] = &[
    StdParam::req("s", "set"),
    StdParam::req_untyped("value"),
];
static SET_HAS_PARAMS: &[StdParam] = &[
    StdParam::req("s", "set"),
    StdParam::req_untyped("value"),
];
static SET_DELETE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "set"),
    StdParam::req_untyped("value"),
];
static SET_SIZE_PARAMS: &[StdParam] = &[StdParam::req("s", "set")];
static SET_VALUES_PARAMS: &[StdParam] = &[StdParam::req("s", "set")];
static SET_UNION_PARAMS: &[StdParam] = &[
    StdParam::req("a", "set"),
    StdParam::req("b", "set"),
];
static SET_INTERSECTION_PARAMS: &[StdParam] = &[
    StdParam::req("a", "set"),
    StdParam::req("b", "set"),
];
static SET_DIFFERENCE_PARAMS: &[StdParam] = &[
    StdParam::req("a", "set"),
    StdParam::req("b", "set"),
];

static SET_SIGS: &[(&str, StdSig)] = &[
    ("new", StdSig { params: SET_NEW_PARAMS, ret: Some("set"), doc: "Create an empty set." }),
    ("from", StdSig { params: SET_FROM_PARAMS, ret: Some("set"), doc: "Build a set from an array, deduplicating elements." }),
    ("add", StdSig { params: SET_ADD_PARAMS, ret: Some("set"), doc: "Insert a value into the set; returns the set itself for chaining." }),
    ("has", StdSig { params: SET_HAS_PARAMS, ret: Some("bool"), doc: "Test whether a value is in the set." }),
    ("delete", StdSig { params: SET_DELETE_PARAMS, ret: Some("bool"), doc: "Remove a value from the set, mutating it in place." }),
    ("size", StdSig { params: SET_SIZE_PARAMS, ret: Some("number"), doc: "Return the number of elements in the set." }),
    ("values", StdSig { params: SET_VALUES_PARAMS, ret: Some("array"), doc: "Return an array of the set's elements, in insertion order." }),
    ("union", StdSig { params: SET_UNION_PARAMS, ret: Some("set"), doc: "Return a new set containing all elements from a and all elements from b not already in a." }),
    ("intersection", StdSig { params: SET_INTERSECTION_PARAMS, ret: Some("set"), doc: "Return a new set of elements that appear in both a and b." }),
    ("difference", StdSig { params: SET_DIFFERENCE_PARAMS, ret: Some("set"), doc: "Return a new set of elements that are in a but not in b." }),
];

static SET_MEMBERS: &[(&str, MemberKind)] = &[
    ("new", MemberKind::Fn),
    ("from", MemberKind::Fn),
    ("add", MemberKind::Fn),
    ("has", MemberKind::Fn),
    ("delete", MemberKind::Fn),
    ("size", MemberKind::Fn),
    ("values", MemberKind::Fn),
    ("union", MemberKind::Fn),
    ("intersection", MemberKind::Fn),
    ("difference", MemberKind::Fn),
];

// ── std/bytes ────────────────────────────────────────────────────────────────

static BYTES_ALLOC_PARAMS: &[StdParam] = &[StdParam::req("n", "number")];
static BYTES_FROM_ARRAY_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
static BYTES_TO_ARRAY_PARAMS: &[StdParam] = &[StdParam::req("b", "bytes")];
static BYTES_GET_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("i", "number"),
];
static BYTES_SET_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("i", "number"),
    StdParam::req("v", "number"),
];
static BYTES_SLICE_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("start", "number"),
    StdParam::opt("end", "number"),
];
static BYTES_CONCAT_PARAMS: &[StdParam] = &[StdParam::variadic("buffers", "bytes")];
static BYTES_READ_UINT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("n", "number"),
    StdParam::opt("endian", "string"),
];
static BYTES_WRITE_UINT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("value", "number"),
    StdParam::req("n", "number"),
    StdParam::opt("endian", "string"),
];
static BYTES_READ_INT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("n", "number"),
    StdParam::opt("endian", "string"),
];
static BYTES_WRITE_INT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("value", "number"),
    StdParam::req("n", "number"),
    StdParam::opt("endian", "string"),
];

static BYTES_SIGS: &[(&str, StdSig)] = &[
    ("alloc", StdSig { params: BYTES_ALLOC_PARAMS, ret: Some("bytes"), doc: "Allocate a zero-filled byte buffer of a given length." }),
    ("fromArray", StdSig { params: BYTES_FROM_ARRAY_PARAMS, ret: Some("bytes"), doc: "Build a byte buffer from an array of integers, each in 0..=255." }),
    ("toArray", StdSig { params: BYTES_TO_ARRAY_PARAMS, ret: Some("array"), doc: "Convert a byte buffer to an array of numbers." }),
    ("get", StdSig { params: BYTES_GET_PARAMS, ret: None, doc: "Read the byte at an index; returns nil for out-of-bounds indices." }),
    ("set", StdSig { params: BYTES_SET_PARAMS, ret: None, doc: "Write a single byte at an index, mutating the buffer in place." }),
    ("slice", StdSig { params: BYTES_SLICE_PARAMS, ret: Some("bytes"), doc: "Extract a subrange of bytes." }),
    ("concat", StdSig { params: BYTES_CONCAT_PARAMS, ret: Some("bytes"), doc: "Concatenate any number of byte buffers into a new buffer." }),
    ("readUint", StdSig { params: BYTES_READ_UINT_PARAMS, ret: Some("int"), doc: "Read an unsigned integer of n bytes from an offset, using the given endianness." }),
    ("writeUint", StdSig { params: BYTES_WRITE_UINT_PARAMS, ret: None, doc: "Write a non-negative integer of n bytes at an offset, using the given endianness." }),
    ("readInt", StdSig { params: BYTES_READ_INT_PARAMS, ret: Some("int"), doc: "Read a signed integer of n bytes from an offset, using the given endianness." }),
    ("writeInt", StdSig { params: BYTES_WRITE_INT_PARAMS, ret: None, doc: "Write a signed integer of n bytes at an offset, using the given endianness." }),
];

static BYTES_MEMBERS: &[(&str, MemberKind)] = &[
    ("alloc", MemberKind::Fn),
    ("fromArray", MemberKind::Fn),
    ("toArray", MemberKind::Fn),
    ("get", MemberKind::Fn),
    ("set", MemberKind::Fn),
    ("slice", MemberKind::Fn),
    ("concat", MemberKind::Fn),
    ("readUint", MemberKind::Fn),
    ("writeUint", MemberKind::Fn),
    ("readInt", MemberKind::Fn),
    ("writeInt", MemberKind::Fn),
];

// ── std/convert ──────────────────────────────────────────────────────────────

static CONVERT_PARSE_NUMBER_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static CONVERT_PARSE_INT_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::with_default("radix", "number", "10"),
];
static CONVERT_TO_STRING_PARAMS: &[StdParam] = &[StdParam::req_untyped("v")];
static CONVERT_TO_NUMBER_PARAMS: &[StdParam] = &[StdParam::req_untyped("v")];
static CONVERT_TO_BOOL_PARAMS: &[StdParam] = &[StdParam::req_untyped("v")];

static CONVERT_SIGS: &[(&str, StdSig)] = &[
    ("parseNumber", StdSig { params: CONVERT_PARSE_NUMBER_PARAMS, ret: Some("[float, err]"), doc: "Parse a string as a floating-point number; accepts scientific notation and IEEE-754 specials." }),
    ("parseInt", StdSig { params: CONVERT_PARSE_INT_PARAMS, ret: Some("[int, err]"), doc: "Parse a string as an integer in a given radix (2–36, default 10)." }),
    ("toString", StdSig { params: CONVERT_TO_STRING_PARAMS, ret: Some("string"), doc: "Convert any value to its display string form." }),
    ("toNumber", StdSig { params: CONVERT_TO_NUMBER_PARAMS, ret: Some("float"), doc: "Coerce a value to a float; numbers pass through, booleans and nil are converted, strings are parsed." }),
    ("toBool", StdSig { params: CONVERT_TO_BOOL_PARAMS, ret: Some("bool"), doc: "Coerce any value to a boolean using AScript's truthiness rules." }),
];

static CONVERT_MEMBERS: &[(&str, MemberKind)] = &[
    ("parseNumber", MemberKind::Fn),
    ("parseInt", MemberKind::Fn),
    ("toString", MemberKind::Fn),
    ("toNumber", MemberKind::Fn),
    ("toBool", MemberKind::Fn),
];

// ── std/decimal ──────────────────────────────────────────────────────────────

static DECIMAL_FROM_PARAMS: &[StdParam] = &[StdParam::req_untyped("x")];
static DECIMAL_PARSE_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static DECIMAL_TO_STRING_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];
static DECIMAL_TO_NUMBER_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];
static DECIMAL_ROUND_PARAMS: &[StdParam] = &[
    StdParam::req("d", "decimal"),
    StdParam::with_default("places", "number", "0"),
];
static DECIMAL_ABS_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];
static DECIMAL_FLOOR_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];
static DECIMAL_CEIL_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];
static DECIMAL_TRUNC_PARAMS: &[StdParam] = &[StdParam::req("d", "decimal")];

static DECIMAL_SIGS: &[(&str, StdSig)] = &[
    ("from", StdSig { params: DECIMAL_FROM_PARAMS, ret: Some("decimal"), doc: "Construct a decimal from a string or number; panics on invalid input." }),
    ("parse", StdSig { params: DECIMAL_PARSE_PARAMS, ret: Some("[decimal, err]"), doc: "Safely parse a string into a decimal, returning a [decimal, err] pair." }),
    ("toString", StdSig { params: DECIMAL_TO_STRING_PARAMS, ret: Some("string"), doc: "Convert a decimal to its string representation, preserving scale." }),
    ("toNumber", StdSig { params: DECIMAL_TO_NUMBER_PARAMS, ret: Some("number"), doc: "Convert a decimal to a floating-point number (lossy)." }),
    ("round", StdSig { params: DECIMAL_ROUND_PARAMS, ret: Some("decimal"), doc: "Round a decimal to a given number of decimal places using half-away-from-zero." }),
    ("abs", StdSig { params: DECIMAL_ABS_PARAMS, ret: Some("decimal"), doc: "Return the absolute value." }),
    ("floor", StdSig { params: DECIMAL_FLOOR_PARAMS, ret: Some("decimal"), doc: "Return the largest integer decimal that is ≤ d." }),
    ("ceil", StdSig { params: DECIMAL_CEIL_PARAMS, ret: Some("decimal"), doc: "Return the smallest integer decimal that is ≥ d." }),
    ("trunc", StdSig { params: DECIMAL_TRUNC_PARAMS, ret: Some("decimal"), doc: "Return the integer part of d, truncating toward zero." }),
];

static DECIMAL_MEMBERS: &[(&str, MemberKind)] = &[
    ("from", MemberKind::Fn),
    ("parse", MemberKind::Fn),
    ("toString", MemberKind::Fn),
    ("toNumber", MemberKind::Fn),
    ("round", MemberKind::Fn),
    ("abs", MemberKind::Fn),
    ("floor", MemberKind::Fn),
    ("ceil", MemberKind::Fn),
    ("trunc", MemberKind::Fn),
];

// ── std/json ─────────────────────────────────────────────────────────────────

static JSON_PARSE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static JSON_STRINGIFY_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("value"),
    StdParam::opt("pretty", "bool"),
];

static JSON_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: JSON_PARSE_PARAMS, ret: Some("[value, err]"), doc: "Parses a JSON string into an AScript value." }),
    ("stringify", StdSig { params: JSON_STRINGIFY_PARAMS, ret: Some("[string, err]"), doc: "Serializes an AScript value to a JSON string." }),
];

static JSON_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
    ("stringify", MemberKind::Fn),
];

// ── std/csv ──────────────────────────────────────────────────────────────────

static CSV_PARSE_PARAMS: &[StdParam] = &[
    StdParam::req("text", "string"),
    StdParam::opt("options", "object"),
];
static CSV_STRINGIFY_PARAMS: &[StdParam] = &[StdParam::req("rows", "array")];

static CSV_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: CSV_PARSE_PARAMS, ret: Some("[array, err]"), doc: "Parses CSV text into an array of rows." }),
    ("stringify", StdSig { params: CSV_STRINGIFY_PARAMS, ret: Some("[string, err]"), doc: "Serializes an array of rows to CSV text." }),
];

static CSV_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
    ("stringify", MemberKind::Fn),
];

// ── std/regex ────────────────────────────────────────────────────────────────

static REGEX_COMPILE_PARAMS: &[StdParam] = &[StdParam::req("pattern", "string")];
static REGEX_TEST_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "regex | string"),
    StdParam::req("text", "string"),
];
static REGEX_FIND_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "regex | string"),
    StdParam::req("text", "string"),
];
static REGEX_FIND_ALL_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "regex | string"),
    StdParam::req("text", "string"),
];
static REGEX_REPLACE_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "regex | string"),
    StdParam::req("text", "string"),
    StdParam::req("replacement", "string"),
];
static REGEX_SPLIT_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "regex | string"),
    StdParam::req("text", "string"),
];

static REGEX_SIGS: &[(&str, StdSig)] = &[
    ("compile", StdSig { params: REGEX_COMPILE_PARAMS, ret: Some("[regex, err]"), doc: "Compiles a pattern string into a reusable Regex value." }),
    ("test", StdSig { params: REGEX_TEST_PARAMS, ret: Some("bool"), doc: "Reports whether the pattern matches anywhere in the string." }),
    ("find", StdSig { params: REGEX_FIND_PARAMS, ret: None, doc: "Finds the first match and its capture groups." }),
    ("findAll", StdSig { params: REGEX_FIND_ALL_PARAMS, ret: Some("array"), doc: "Finds every non-overlapping match." }),
    ("replace", StdSig { params: REGEX_REPLACE_PARAMS, ret: Some("string"), doc: "Replaces every match with a replacement string." }),
    ("split", StdSig { params: REGEX_SPLIT_PARAMS, ret: Some("array"), doc: "Splits a string on every match of the pattern." }),
];

static REGEX_MEMBERS: &[(&str, MemberKind)] = &[
    ("compile", MemberKind::Fn),
    ("test", MemberKind::Fn),
    ("find", MemberKind::Fn),
    ("findAll", MemberKind::Fn),
    ("replace", MemberKind::Fn),
    ("split", MemberKind::Fn),
];

// ── std/encoding ─────────────────────────────────────────────────────────────

static ENCODING_BASE64_ENCODE_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static ENCODING_BASE64_DECODE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static ENCODING_HEX_ENCODE_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static ENCODING_HEX_DECODE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static ENCODING_URL_ENCODE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static ENCODING_URL_DECODE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static ENCODING_UTF8_ENCODE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static ENCODING_UTF8_DECODE_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];

static ENCODING_SIGS: &[(&str, StdSig)] = &[
    ("base64Encode", StdSig { params: ENCODING_BASE64_ENCODE_PARAMS, ret: Some("string"), doc: "Encodes bytes or a string as a standard base64 string." }),
    ("base64Decode", StdSig { params: ENCODING_BASE64_DECODE_PARAMS, ret: Some("[bytes, err]"), doc: "Decodes a standard base64 string into bytes." }),
    ("hexEncode", StdSig { params: ENCODING_HEX_ENCODE_PARAMS, ret: Some("string"), doc: "Encodes bytes or a string as a lowercase hexadecimal string." }),
    ("hexDecode", StdSig { params: ENCODING_HEX_DECODE_PARAMS, ret: Some("[bytes, err]"), doc: "Decodes a hexadecimal string into bytes." }),
    ("urlEncode", StdSig { params: ENCODING_URL_ENCODE_PARAMS, ret: Some("string"), doc: "Percent-encodes a string for use in a URL." }),
    ("urlDecode", StdSig { params: ENCODING_URL_DECODE_PARAMS, ret: Some("[string, err]"), doc: "Decodes a percent-encoded string." }),
    ("utf8Encode", StdSig { params: ENCODING_UTF8_ENCODE_PARAMS, ret: Some("bytes"), doc: "Encodes a string into its UTF-8 bytes." }),
    ("utf8Decode", StdSig { params: ENCODING_UTF8_DECODE_PARAMS, ret: Some("[string, err]"), doc: "Decodes a byte array into a string, validating UTF-8." }),
];

static ENCODING_MEMBERS: &[(&str, MemberKind)] = &[
    ("base64Encode", MemberKind::Fn),
    ("base64Decode", MemberKind::Fn),
    ("hexEncode", MemberKind::Fn),
    ("hexDecode", MemberKind::Fn),
    ("urlEncode", MemberKind::Fn),
    ("urlDecode", MemberKind::Fn),
    ("utf8Encode", MemberKind::Fn),
    ("utf8Decode", MemberKind::Fn),
];

// ── std/toml ─────────────────────────────────────────────────────────────────

static TOML_PARSE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static TOML_STRINGIFY_PARAMS: &[StdParam] = &[StdParam::req("value", "object")];

static TOML_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: TOML_PARSE_PARAMS, ret: Some("[value, err]"), doc: "Parses a TOML string into an AScript value." }),
    ("stringify", StdSig { params: TOML_STRINGIFY_PARAMS, ret: Some("[string, err]"), doc: "Serializes an AScript value to TOML text." }),
];

static TOML_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
    ("stringify", MemberKind::Fn),
];

// ── std/yaml ─────────────────────────────────────────────────────────────────

static YAML_PARSE_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static YAML_STRINGIFY_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];

static YAML_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: YAML_PARSE_PARAMS, ret: Some("[value, err]"), doc: "Parses a YAML string into an AScript value." }),
    ("stringify", StdSig { params: YAML_STRINGIFY_PARAMS, ret: Some("[string, err]"), doc: "Serializes an AScript value to YAML text." }),
];

static YAML_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
    ("stringify", MemberKind::Fn),
];

// ── std/url ──────────────────────────────────────────────────────────────────

static URL_PARSE_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static URL_PARSE_QUERY_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static URL_BUILD_QUERY_PARAMS: &[StdParam] = &[StdParam::req("obj", "object")];
static URL_BUILD_PARAMS: &[StdParam] = &[StdParam::req("obj", "object")];
static URL_ENCODE_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static URL_DECODE_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];

static URL_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: URL_PARSE_PARAMS, ret: Some("[object, err]"), doc: "Parses a URL string into a component object." }),
    ("parseQuery", StdSig { params: URL_PARSE_QUERY_PARAMS, ret: Some("object"), doc: "Parses an application/x-www-form-urlencoded query string into an object." }),
    ("buildQuery", StdSig { params: URL_BUILD_QUERY_PARAMS, ret: Some("string"), doc: "Serializes an object into an application/x-www-form-urlencoded query string." }),
    ("build", StdSig { params: URL_BUILD_PARAMS, ret: Some("[string, err]"), doc: "Assembles a URL string from a component object." }),
    ("encode", StdSig { params: URL_ENCODE_PARAMS, ret: Some("string"), doc: "Percent-encodes a single URL component." }),
    ("decode", StdSig { params: URL_DECODE_PARAMS, ret: Some("[string, err]"), doc: "Percent-decodes a URL component." }),
];

static URL_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
    ("parseQuery", MemberKind::Fn),
    ("buildQuery", MemberKind::Fn),
    ("build", MemberKind::Fn),
    ("encode", MemberKind::Fn),
    ("decode", MemberKind::Fn),
];

// ── std/uuid ─────────────────────────────────────────────────────────────────

static UUID_V4_PARAMS: &[StdParam] = &[];
static UUID_V7_PARAMS: &[StdParam] = &[];

static UUID_SIGS: &[(&str, StdSig)] = &[
    ("v4", StdSig { params: UUID_V4_PARAMS, ret: Some("string"), doc: "Generates a random (version 4) UUID." }),
    ("v7", StdSig { params: UUID_V7_PARAMS, ret: Some("string"), doc: "Generates a time-ordered (version 7) UUID based on the current timestamp." }),
];

static UUID_MEMBERS: &[(&str, MemberKind)] = &[
    ("v4", MemberKind::Fn),
    ("v7", MemberKind::Fn),
];

// ── std/msgpack ──────────────────────────────────────────────────────────────

static MSGPACK_ENCODE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static MSGPACK_DECODE_PARAMS: &[StdParam] = &[
    StdParam::req("bytes", "bytes"),
    StdParam::opt("schema", "class | schema"),
];

static MSGPACK_SIGS: &[(&str, StdSig)] = &[
    ("encode", StdSig { params: MSGPACK_ENCODE_PARAMS, ret: Some("bytes"), doc: "Serialize any data value to MessagePack bytes." }),
    ("decode", StdSig { params: MSGPACK_DECODE_PARAMS, ret: Some("[value, err]"), doc: "Deserialize MessagePack bytes into an AScript value." }),
];

static MSGPACK_MEMBERS: &[(&str, MemberKind)] = &[
    ("encode", MemberKind::Fn),
    ("decode", MemberKind::Fn),
];

// ── std/cbor ─────────────────────────────────────────────────────────────────

static CBOR_ENCODE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static CBOR_DECODE_PARAMS: &[StdParam] = &[
    StdParam::req("bytes", "bytes"),
    StdParam::opt("schema", "class | schema"),
];

static CBOR_SIGS: &[(&str, StdSig)] = &[
    ("encode", StdSig { params: CBOR_ENCODE_PARAMS, ret: Some("bytes"), doc: "Serialize any data value to CBOR bytes." }),
    ("decode", StdSig { params: CBOR_DECODE_PARAMS, ret: Some("[value, err]"), doc: "Deserialize CBOR bytes into an AScript value." }),
];

static CBOR_MEMBERS: &[(&str, MemberKind)] = &[
    ("encode", MemberKind::Fn),
    ("decode", MemberKind::Fn),
];

// ─────────────────────────────────────────────────────────────────────────────
// System modules
// ─────────────────────────────────────────────────────────────────────────────

// ── std/fs ───────────────────────────────────────────────────────────────────

static FS_READ_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_READ_BYTES_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_WRITE_PARAMS: &[StdParam] = &[
    StdParam::req("path", "string"),
    StdParam::req_untyped("data"),
];
static FS_APPEND_PARAMS: &[StdParam] = &[
    StdParam::req("path", "string"),
    StdParam::req_untyped("data"),
];
static FS_EXISTS_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_STAT_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_MKDIR_PARAMS: &[StdParam] = &[
    StdParam::req("path", "string"),
    StdParam::opt("recursive", "bool"),
];
static FS_REMOVE_PARAMS: &[StdParam] = &[
    StdParam::req("path", "string"),
    StdParam::opt("recursive", "bool"),
];
static FS_READ_DIR_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_WALK_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_JOIN_PARAMS: &[StdParam] = &[StdParam::variadic("parts", "string")];
static FS_DIRNAME_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_BASENAME_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_EXTNAME_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_IS_ABSOLUTE_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FS_GREP_PARAMS: &[StdParam] = &[
    StdParam::req("pattern", "string"),
    StdParam::req("dir", "string"),
    StdParam::opt("opts", "object"),
];

static FS_SIGS: &[(&str, StdSig)] = &[
    ("read", StdSig { params: FS_READ_PARAMS, ret: Some("[string, err]"), doc: "Reads a file as UTF-8 text." }),
    ("readBytes", StdSig { params: FS_READ_BYTES_PARAMS, ret: Some("[bytes, err]"), doc: "Reads a file as raw bytes." }),
    ("write", StdSig { params: FS_WRITE_PARAMS, ret: Some("[nil, err]"), doc: "Writes data to a file, creating or truncating it." }),
    ("append", StdSig { params: FS_APPEND_PARAMS, ret: Some("[nil, err]"), doc: "Appends data to a file, creating it if it does not exist." }),
    ("exists", StdSig { params: FS_EXISTS_PARAMS, ret: Some("bool"), doc: "Reports whether a path exists." }),
    ("stat", StdSig { params: FS_STAT_PARAMS, ret: Some("[{size, isFile, isDir, modifiedMs}, err]"), doc: "Reads metadata for a path." }),
    ("mkdir", StdSig { params: FS_MKDIR_PARAMS, ret: Some("[nil, err]"), doc: "Creates a directory." }),
    ("remove", StdSig { params: FS_REMOVE_PARAMS, ret: Some("[nil, err]"), doc: "Removes a file or directory." }),
    ("readDir", StdSig { params: FS_READ_DIR_PARAMS, ret: Some("[array, err]"), doc: "Lists the immediate entries of a directory." }),
    ("walk", StdSig { params: FS_WALK_PARAMS, ret: Some("[array, err]"), doc: "Recursively walks a directory tree." }),
    ("join", StdSig { params: FS_JOIN_PARAMS, ret: Some("string"), doc: "Joins path segments into a single path. Pure and infallible." }),
    ("dirname", StdSig { params: FS_DIRNAME_PARAMS, ret: Some("string"), doc: "Returns the parent path of a path. Pure and infallible." }),
    ("basename", StdSig { params: FS_BASENAME_PARAMS, ret: Some("string"), doc: "Returns the final component of a path. Pure and infallible." }),
    ("extname", StdSig { params: FS_EXTNAME_PARAMS, ret: Some("string"), doc: "Returns the extension of a path, including the leading dot. Pure and infallible." }),
    ("isAbsolute", StdSig { params: FS_IS_ABSOLUTE_PARAMS, ret: Some("bool"), doc: "Reports whether a path is absolute. Pure and infallible." }),
    ("grep", StdSig { params: FS_GREP_PARAMS, ret: Some("[array, err]"), doc: "Searches a directory tree for a regular-expression pattern, line by line." }),
];

static FS_MEMBERS: &[(&str, MemberKind)] = &[
    ("read", MemberKind::Fn),
    ("readBytes", MemberKind::Fn),
    ("write", MemberKind::Fn),
    ("append", MemberKind::Fn),
    ("exists", MemberKind::Fn),
    ("stat", MemberKind::Fn),
    ("mkdir", MemberKind::Fn),
    ("remove", MemberKind::Fn),
    ("readDir", MemberKind::Fn),
    ("walk", MemberKind::Fn),
    ("join", MemberKind::Fn),
    ("dirname", MemberKind::Fn),
    ("basename", MemberKind::Fn),
    ("extname", MemberKind::Fn),
    ("isAbsolute", MemberKind::Fn),
    ("grep", MemberKind::Fn),
];

// ── std/env ──────────────────────────────────────────────────────────────────

static ENV_GET_PARAMS: &[StdParam] = &[StdParam::req("name", "string")];
static ENV_SET_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("value", "string"),
];
static ENV_UNSET_PARAMS: &[StdParam] = &[StdParam::req("name", "string")];
static ENV_VARS_PARAMS: &[StdParam] = &[];
static ENV_LOAD_DOTENV_PARAMS: &[StdParam] = &[StdParam::opt("path", "string")];
static ENV_ARGS_PARAMS: &[StdParam] = &[];

static ENV_SIGS: &[(&str, StdSig)] = &[
    ("get", StdSig { params: ENV_GET_PARAMS, ret: Some("string | nil"), doc: "Reads an environment variable." }),
    ("set", StdSig { params: ENV_SET_PARAMS, ret: None, doc: "Sets an environment variable. Mutates the process-global environment." }),
    ("unset", StdSig { params: ENV_UNSET_PARAMS, ret: None, doc: "Removes an environment variable. Mutates the process-global environment." }),
    ("vars", StdSig { params: ENV_VARS_PARAMS, ret: Some("object"), doc: "Snapshots all current environment variables." }),
    ("loadDotenv", StdSig { params: ENV_LOAD_DOTENV_PARAMS, ret: Some("[number, err]"), doc: "Loads a `.env` file into the process environment." }),
    ("args", StdSig { params: ENV_ARGS_PARAMS, ret: Some("array<string>"), doc: "Returns the script's trailing CLI arguments." }),
];

static ENV_MEMBERS: &[(&str, MemberKind)] = &[
    ("get", MemberKind::Fn),
    ("set", MemberKind::Fn),
    ("unset", MemberKind::Fn),
    ("vars", MemberKind::Fn),
    ("loadDotenv", MemberKind::Fn),
    ("args", MemberKind::Fn),
];

// ── std/io ───────────────────────────────────────────────────────────────────

static IO_READ_LINE_PARAMS: &[StdParam] = &[];
static IO_READ_ALL_PARAMS: &[StdParam] = &[];
static IO_READ_LINES_PARAMS: &[StdParam] = &[];

static IO_SIGS: &[(&str, StdSig)] = &[
    ("readLine", StdSig { params: IO_READ_LINE_PARAMS, ret: Some("string | nil"), doc: "Reads one line from stdin, stripping the trailing newline." }),
    ("readAll", StdSig { params: IO_READ_ALL_PARAMS, ret: Some("string"), doc: "Reads all remaining stdin as a single UTF-8 string (lossy)." }),
    ("readLines", StdSig { params: IO_READ_LINES_PARAMS, ret: Some("array<string>"), doc: "Reads every remaining line of stdin and returns them as an array." }),
];

static IO_MEMBERS: &[(&str, MemberKind)] = &[
    ("readLine", MemberKind::Fn),
    ("readAll", MemberKind::Fn),
    ("readLines", MemberKind::Fn),
];

// ── std/process ──────────────────────────────────────────────────────────────

static PROCESS_RUN_PARAMS: &[StdParam] = &[
    StdParam::req("cmd", "string"),
    StdParam::opt("args", "array"),
    StdParam::opt("opts", "object"),
];
static PROCESS_SPAWN_PARAMS: &[StdParam] = &[
    StdParam::req("cmd", "string"),
    StdParam::opt("args", "array"),
    StdParam::opt("opts", "object"),
];
static PROCESS_ON_PARAMS: &[StdParam] = &[
    StdParam::req("signalName", "string"),
    StdParam::req("handler", "fn"),
];
static PROCESS_OFF_PARAMS: &[StdParam] = &[StdParam::req("signalName", "string")];

static PROCESS_SIGS: &[(&str, StdSig)] = &[
    ("run", StdSig { params: PROCESS_RUN_PARAMS, ret: Some("[result, err]"), doc: "Runs a command to completion and captures its output. Async — must be awaited." }),
    ("spawn", StdSig { params: PROCESS_SPAWN_PARAMS, ret: Some("[child, err]"), doc: "Spawns a command and returns a live ChildProcess handle for streaming I/O. Async — must be awaited." }),
    ("on", StdSig { params: PROCESS_ON_PARAMS, ret: None, doc: "Registers a handler for an inbound OS signal." }),
    ("off", StdSig { params: PROCESS_OFF_PARAMS, ret: None, doc: "Removes a previously-registered signal handler." }),
];

static PROCESS_MEMBERS: &[(&str, MemberKind)] = &[
    ("run", MemberKind::Fn),
    ("spawn", MemberKind::Fn),
    ("on", MemberKind::Fn),
    ("off", MemberKind::Fn),
];

// ── std/os ───────────────────────────────────────────────────────────────────

static OS_PID_PARAMS: &[StdParam] = &[];
static OS_PLATFORM_PARAMS: &[StdParam] = &[];
static OS_ARCH_PARAMS: &[StdParam] = &[];
static OS_CPU_COUNT_PARAMS: &[StdParam] = &[];
static OS_HOSTNAME_PARAMS: &[StdParam] = &[];
static OS_TEMP_DIR_PARAMS: &[StdParam] = &[];
static OS_IN_CONTAINER_PARAMS: &[StdParam] = &[];
static OS_MEMORY_PARAMS: &[StdParam] = &[];
static OS_SWAP_PARAMS: &[StdParam] = &[];
static OS_CPU_USAGE_PARAMS: &[StdParam] = &[];
static OS_LOAD_AVG_PARAMS: &[StdParam] = &[];
static OS_DISKS_PARAMS: &[StdParam] = &[];
static OS_UPTIME_PARAMS: &[StdParam] = &[];
static OS_NETWORK_INTERFACES_PARAMS: &[StdParam] = &[];
static OS_LOCAL_IP_PARAMS: &[StdParam] = &[];

static OS_SIGS: &[(&str, StdSig)] = &[
    ("pid", StdSig { params: OS_PID_PARAMS, ret: Some("number"), doc: "Returns the current process ID." }),
    ("platform", StdSig { params: OS_PLATFORM_PARAMS, ret: Some("string"), doc: "Returns the OS name: \"macos\", \"linux\", \"windows\", etc." }),
    ("arch", StdSig { params: OS_ARCH_PARAMS, ret: Some("string"), doc: "Returns the CPU architecture: \"aarch64\", \"x86_64\", etc." }),
    ("cpuCount", StdSig { params: OS_CPU_COUNT_PARAMS, ret: Some("number"), doc: "Returns the number of logical CPUs available to the process." }),
    ("hostname", StdSig { params: OS_HOSTNAME_PARAMS, ret: Some("string"), doc: "Returns the machine hostname. Returns \"unknown\" if the OS call fails." }),
    ("tempDir", StdSig { params: OS_TEMP_DIR_PARAMS, ret: Some("string"), doc: "Returns the OS temporary directory path." }),
    ("inContainer", StdSig { params: OS_IN_CONTAINER_PARAMS, ret: Some("bool"), doc: "Heuristic container detection; returns true when running inside Docker, Podman, or Kubernetes." }),
    ("memory", StdSig { params: OS_MEMORY_PARAMS, ret: Some("{total, used, free, available}"), doc: "Snapshots the current RAM allocation from the OS." }),
    ("swap", StdSig { params: OS_SWAP_PARAMS, ret: Some("{total, used, free}"), doc: "Snapshots the current swap-space allocation from the OS." }),
    ("cpuUsage", StdSig { params: OS_CPU_USAGE_PARAMS, ret: Some("number"), doc: "Samples the CPU twice (~200 ms apart) and returns the average utilization percentage. Async." }),
    ("loadAvg", StdSig { params: OS_LOAD_AVG_PARAMS, ret: Some("{one, five, fifteen}"), doc: "Returns the 1-, 5-, and 15-minute load averages." }),
    ("disks", StdSig { params: OS_DISKS_PARAMS, ret: Some("array"), doc: "Returns one entry per disk with mount, total, free, and available fields." }),
    ("uptime", StdSig { params: OS_UPTIME_PARAMS, ret: Some("number"), doc: "Returns the system uptime in seconds." }),
    ("networkInterfaces", StdSig { params: OS_NETWORK_INTERFACES_PARAMS, ret: Some("array"), doc: "Returns one entry per network interface with name and addresses fields." }),
    ("localIp", StdSig { params: OS_LOCAL_IP_PARAMS, ret: Some("[string, err]"), doc: "Returns the first non-loopback, non-link-local IPv4 address found across all interfaces." }),
];

static OS_MEMBERS: &[(&str, MemberKind)] = &[
    ("pid", MemberKind::Fn),
    ("platform", MemberKind::Fn),
    ("arch", MemberKind::Fn),
    ("cpuCount", MemberKind::Fn),
    ("hostname", MemberKind::Fn),
    ("tempDir", MemberKind::Fn),
    ("inContainer", MemberKind::Fn),
    ("memory", MemberKind::Fn),
    ("swap", MemberKind::Fn),
    ("cpuUsage", MemberKind::Fn),
    ("loadAvg", MemberKind::Fn),
    ("disks", MemberKind::Fn),
    ("uptime", MemberKind::Fn),
    ("networkInterfaces", MemberKind::Fn),
    ("localIp", MemberKind::Fn),
];

// ── std/crypto ───────────────────────────────────────────────────────────────

static CRYPTO_SHA256_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static CRYPTO_SHA512_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static CRYPTO_MD5_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static CRYPTO_HMAC_SHA256_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("key"),
    StdParam::req_untyped("data"),
];
static CRYPTO_HMAC_SHA384_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("key"),
    StdParam::req_untyped("data"),
];
static CRYPTO_HMAC_SHA512_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("key"),
    StdParam::req_untyped("data"),
];
static CRYPTO_TIMING_SAFE_EQUAL_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static CRYPTO_RANDOM_BYTES_PARAMS: &[StdParam] = &[StdParam::req("n", "number")];
static CRYPTO_HASH_PASSWORD_PARAMS: &[StdParam] = &[StdParam::req_untyped("password")];
static CRYPTO_VERIFY_PASSWORD_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("password"),
    StdParam::req("phc", "string"),
];
static CRYPTO_BCRYPT_HASH_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("password"),
    StdParam::opt("cost", "number"),
];
static CRYPTO_BCRYPT_VERIFY_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("password"),
    StdParam::req("hash", "string"),
];
static CRYPTO_CRC32_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static CRYPTO_XXHASH_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];

static CRYPTO_SIGS: &[(&str, StdSig)] = &[
    ("sha256", StdSig { params: CRYPTO_SHA256_PARAMS, ret: Some("string"), doc: "Computes the SHA-256 digest of the input." }),
    ("sha512", StdSig { params: CRYPTO_SHA512_PARAMS, ret: Some("string"), doc: "Computes the SHA-512 digest of the input." }),
    ("md5", StdSig { params: CRYPTO_MD5_PARAMS, ret: Some("string"), doc: "Computes the MD5 digest of the input." }),
    ("hmacSha256", StdSig { params: CRYPTO_HMAC_SHA256_PARAMS, ret: Some("string"), doc: "Computes an HMAC-SHA256 tag." }),
    ("hmacSha384", StdSig { params: CRYPTO_HMAC_SHA384_PARAMS, ret: Some("string"), doc: "Computes an HMAC-SHA384 tag." }),
    ("hmacSha512", StdSig { params: CRYPTO_HMAC_SHA512_PARAMS, ret: Some("string"), doc: "Computes an HMAC-SHA512 tag." }),
    ("timingSafeEqual", StdSig { params: CRYPTO_TIMING_SAFE_EQUAL_PARAMS, ret: Some("bool"), doc: "Compares two byte sequences in constant time." }),
    ("randomBytes", StdSig { params: CRYPTO_RANDOM_BYTES_PARAMS, ret: Some("bytes"), doc: "Generates cryptographically secure random bytes." }),
    ("hashPassword", StdSig { params: CRYPTO_HASH_PASSWORD_PARAMS, ret: Some("[string, err]"), doc: "Hashes a password with Argon2, returning a self-describing PHC string." }),
    ("verifyPassword", StdSig { params: CRYPTO_VERIFY_PASSWORD_PARAMS, ret: Some("bool"), doc: "Verifies a password against an Argon2 PHC string." }),
    ("bcryptHash", StdSig { params: CRYPTO_BCRYPT_HASH_PARAMS, ret: Some("[string, err]"), doc: "Hashes a password with bcrypt." }),
    ("bcryptVerify", StdSig { params: CRYPTO_BCRYPT_VERIFY_PARAMS, ret: Some("bool"), doc: "Verifies a password against a bcrypt hash." }),
    ("crc32", StdSig { params: CRYPTO_CRC32_PARAMS, ret: Some("number"), doc: "CRC-32 checksum (IEEE polynomial). Fast, non-cryptographic." }),
    ("xxhash", StdSig { params: CRYPTO_XXHASH_PARAMS, ret: Some("string"), doc: "xxHash-64 (XXH64) with seed 0. Extremely fast, non-cryptographic." }),
];

static CRYPTO_MEMBERS: &[(&str, MemberKind)] = &[
    ("sha256", MemberKind::Fn),
    ("sha512", MemberKind::Fn),
    ("md5", MemberKind::Fn),
    ("hmacSha256", MemberKind::Fn),
    ("hmacSha384", MemberKind::Fn),
    ("hmacSha512", MemberKind::Fn),
    ("timingSafeEqual", MemberKind::Fn),
    ("randomBytes", MemberKind::Fn),
    ("hashPassword", MemberKind::Fn),
    ("verifyPassword", MemberKind::Fn),
    ("bcryptHash", MemberKind::Fn),
    ("bcryptVerify", MemberKind::Fn),
    ("crc32", MemberKind::Fn),
    ("xxhash", MemberKind::Fn),
];

// ── std/compress ─────────────────────────────────────────────────────────────

static COMPRESS_GZIP_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static COMPRESS_GUNZIP_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static COMPRESS_DEFLATE_PARAMS: &[StdParam] = &[StdParam::req_untyped("data")];
static COMPRESS_INFLATE_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static COMPRESS_ZIP_CREATE_PARAMS: &[StdParam] = &[StdParam::req("entries", "array")];
static COMPRESS_ZIP_EXTRACT_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static COMPRESS_ZSTD_COMPRESS_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("data"),
    StdParam::opt("level", "number"),
];
static COMPRESS_ZSTD_DECOMPRESS_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static COMPRESS_BROTLI_COMPRESS_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("data"),
    StdParam::opt("quality", "number"),
];
static COMPRESS_BROTLI_DECOMPRESS_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static COMPRESS_TAR_CREATE_PARAMS: &[StdParam] = &[StdParam::req("entries", "array")];
static COMPRESS_TAR_EXTRACT_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];

static COMPRESS_SIGS: &[(&str, StdSig)] = &[
    ("gzip", StdSig { params: COMPRESS_GZIP_PARAMS, ret: Some("bytes"), doc: "Compresses data with gzip." }),
    ("gunzip", StdSig { params: COMPRESS_GUNZIP_PARAMS, ret: Some("[bytes, err]"), doc: "Decompresses gzip data." }),
    ("deflate", StdSig { params: COMPRESS_DEFLATE_PARAMS, ret: Some("bytes"), doc: "Compresses data with raw deflate." }),
    ("inflate", StdSig { params: COMPRESS_INFLATE_PARAMS, ret: Some("[bytes, err]"), doc: "Decompresses raw deflate data." }),
    ("zipCreate", StdSig { params: COMPRESS_ZIP_CREATE_PARAMS, ret: Some("[bytes, err]"), doc: "Builds an in-memory zip archive." }),
    ("zipExtract", StdSig { params: COMPRESS_ZIP_EXTRACT_PARAMS, ret: Some("[array, err]"), doc: "Extracts an in-memory zip archive." }),
    ("zstdCompress", StdSig { params: COMPRESS_ZSTD_COMPRESS_PARAMS, ret: Some("bytes"), doc: "Compresses data with zstd (Zstandard)." }),
    ("zstdDecompress", StdSig { params: COMPRESS_ZSTD_DECOMPRESS_PARAMS, ret: Some("[bytes, err]"), doc: "Decompresses zstd-compressed data." }),
    ("brotliCompress", StdSig { params: COMPRESS_BROTLI_COMPRESS_PARAMS, ret: Some("bytes"), doc: "Compresses data with brotli." }),
    ("brotliDecompress", StdSig { params: COMPRESS_BROTLI_DECOMPRESS_PARAMS, ret: Some("[bytes, err]"), doc: "Decompresses brotli-compressed data." }),
    ("tarCreate", StdSig { params: COMPRESS_TAR_CREATE_PARAMS, ret: Some("[bytes, err]"), doc: "Builds an in-memory tar archive from an array of {name, data} entries." }),
    ("tarExtract", StdSig { params: COMPRESS_TAR_EXTRACT_PARAMS, ret: Some("[array, err]"), doc: "Extracts a tar archive into an array of {name, data} entries." }),
];

static COMPRESS_MEMBERS: &[(&str, MemberKind)] = &[
    ("gzip", MemberKind::Fn),
    ("gunzip", MemberKind::Fn),
    ("deflate", MemberKind::Fn),
    ("inflate", MemberKind::Fn),
    ("zipCreate", MemberKind::Fn),
    ("zipExtract", MemberKind::Fn),
    ("zstdCompress", MemberKind::Fn),
    ("zstdDecompress", MemberKind::Fn),
    ("brotliCompress", MemberKind::Fn),
    ("brotliDecompress", MemberKind::Fn),
    ("tarCreate", MemberKind::Fn),
    ("tarExtract", MemberKind::Fn),
];

// ── std/archive (BATT B1 §6) ──────────────────────────────────────────────────

static ARCHIVE_TAR_WRITER_PARAMS: &[StdParam] = &[StdParam::opt("opts", "object")];
static ARCHIVE_TAR_ENTRIES_PARAMS: &[StdParam] = &[StdParam::req("data", "bytes")];
static ARCHIVE_TAR_APPEND_PARAMS: &[StdParam] = &[
    StdParam::req("data", "bytes"),
    StdParam::req("additions", "array"),
];
// archiveWriter handle methods (MemberKind::HandleMethod in ARCHIVE_MEMBERS).
static ARCHIVE_ADD_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req_untyped("data"),
    StdParam::opt("opts", "object"),
];
static ARCHIVE_FINISH_PARAMS: &[StdParam] = &[];

static ARCHIVE_SIGS: &[(&str, StdSig)] = &[
    ("tarWriter", StdSig { params: ARCHIVE_TAR_WRITER_PARAMS, ret: Some("archiveWriter"), doc: "Opens a streaming tar writer handle. opts: {gzip?, deterministic?}." }),
    ("tarEntries", StdSig { params: ARCHIVE_TAR_ENTRIES_PARAMS, ret: Some("generator"), doc: "Lazily decodes a (optionally gzipped) tar, yielding one {name, size, mode, isDir, data} per entry; a corrupt entry yields a [nil, err] pair." }),
    ("tarAppend", StdSig { params: ARCHIVE_TAR_APPEND_PARAMS, ret: Some("[bytes, err]"), doc: "Decodes a tar (preserving its entries) and appends {name, data, mode?, dir?} additions, returning the new archive bytes." }),
    // archiveWriter handle methods (BATT B1 §6).
    ("add", StdSig { params: ARCHIVE_ADD_PARAMS, ret: None, doc: "Appends one entry to a tar writer. opts: {dir?, mode?, mtime?}." }),
    ("finish", StdSig { params: ARCHIVE_FINISH_PARAMS, ret: Some("bytes"), doc: "Finalizes a tar writer and returns the assembled bytes (gzip-wrapped if the writer was opened with {gzip:true})." }),
];
static ARCHIVE_MEMBERS: &[(&str, MemberKind)] = &[
    ("tarWriter", MemberKind::Fn),
    ("tarEntries", MemberKind::Fn),
    ("tarAppend", MemberKind::Fn),
    // archiveWriter handle methods — not module exports.
    ("add", MemberKind::HandleMethod),
    ("finish", MemberKind::HandleMethod),
];

// ── std/net ───────────────────────────────────────────────────────────────────

static NET_LOOKUP_PARAMS: &[StdParam] = &[StdParam::req("host", "string")];
static NET_LOOKUP_ONE_PARAMS: &[StdParam] = &[StdParam::req("host", "string")];

static NET_SIGS: &[(&str, StdSig)] = &[
    ("lookup", StdSig { params: NET_LOOKUP_PARAMS, ret: Some("[array<string>, err]"), doc: "Resolves a hostname to a de-duplicated list of IP-address strings. Async." }),
    ("lookupOne", StdSig { params: NET_LOOKUP_ONE_PARAMS, ret: Some("[string, err]"), doc: "Resolves a hostname and returns only the first IP address. Async." }),
];

static NET_MEMBERS: &[(&str, MemberKind)] = &[
    ("lookup", MemberKind::Fn),
    ("lookupOne", MemberKind::Fn),
];

// ── std/net/tcp ───────────────────────────────────────────────────────────────

static NET_TCP_CONNECT_PARAMS: &[StdParam] = &[
    StdParam::req("host", "string"),
    StdParam::req("port", "number"),
];
static NET_TCP_LISTEN_PARAMS: &[StdParam] = &[
    StdParam::req("host", "string"),
    StdParam::req("port", "number"),
];

static NET_TCP_CONNECT_TLS_PARAMS: &[StdParam] = &[
    StdParam::req("host", "string"),
    StdParam::req("port", "number"),
    StdParam::opt("opts", "object"),
];

static NET_TCP_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: NET_TCP_CONNECT_PARAMS, ret: Some("[stream, err]"), doc: "Opens a client TCP connection. Async." }),
    // BATT A1 — TLS client connect (feature `tls`); the row is unconditional (the export
    // is present wherever `std/net/tcp` is buildable in the test configs that exercise it).
    ("connectTls", StdSig { params: NET_TCP_CONNECT_TLS_PARAMS, ret: Some("[stream, err]"), doc: "Opens a client TCP connection and performs a TLS handshake. opts: {caCert?, serverName?, alpn?}. Async." }),
    ("listen", StdSig { params: NET_TCP_LISTEN_PARAMS, ret: Some("[listener, err]"), doc: "Binds a TCP listener. Async." }),
];

static NET_TCP_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
    ("connectTls", MemberKind::Fn),
    ("listen", MemberKind::Fn),
];

// ── std/net/udp ───────────────────────────────────────────────────────────────

static NET_UDP_BIND_PARAMS: &[StdParam] = &[StdParam::req("addr", "string")];

static NET_UDP_SIGS: &[(&str, StdSig)] = &[
    ("bind", StdSig { params: NET_UDP_BIND_PARAMS, ret: Some("[socket, err]"), doc: "Binds a UDP socket to a local address." }),
];

static NET_UDP_MEMBERS: &[(&str, MemberKind)] = &[
    ("bind", MemberKind::Fn),
];

// ── std/net/unix ──────────────────────────────────────────────────────────────

static NET_UNIX_CONNECT_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static NET_UNIX_LISTEN_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];

static NET_UNIX_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: NET_UNIX_CONNECT_PARAMS, ret: Some("[stream, err]"), doc: "Opens a client stream to the Unix-domain socket at path. Async." }),
    ("listen", StdSig { params: NET_UNIX_LISTEN_PARAMS, ret: Some("[listener, err]"), doc: "Binds a Unix-domain listener at the filesystem path; unlinks the socket on close. Async." }),
];

static NET_UNIX_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
    ("listen", MemberKind::Fn),
];

// ── std/net/ws ────────────────────────────────────────────────────────────────

static NET_WS_CONNECT_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_WS_LISTEN_PARAMS: &[StdParam] = &[
    StdParam::req("host", "string"),
    StdParam::req("port", "number"),
];

static NET_WS_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: NET_WS_CONNECT_PARAMS, ret: Some("[conn, err]"), doc: "Opens a client WebSocket to a ws:// or wss:// URL. Async." }),
    ("listen", StdSig { params: NET_WS_LISTEN_PARAMS, ret: Some("[listener, err]"), doc: "Binds a TCP listener for accepting WebSocket connections. Async." }),
];

static NET_WS_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
    ("listen", MemberKind::Fn),
];

// ── std/net/http ──────────────────────────────────────────────────────────────

static NET_HTTP_GET_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_POST_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_PUT_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_PATCH_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_DELETE_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_HEAD_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_OPTIONS_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
static NET_HTTP_REQUEST_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static NET_HTTP_CANCEL_TOKEN_PARAMS: &[StdParam] = &[];
static NET_HTTP_SSE_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];

static NET_HTTP_SIGS: &[(&str, StdSig)] = &[
    ("get", StdSig { params: NET_HTTP_GET_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP GET request. Async." }),
    ("post", StdSig { params: NET_HTTP_POST_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP POST request. Async." }),
    ("put", StdSig { params: NET_HTTP_PUT_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP PUT request. Async." }),
    ("patch", StdSig { params: NET_HTTP_PATCH_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP PATCH request. Async." }),
    ("delete", StdSig { params: NET_HTTP_DELETE_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP DELETE request. Async." }),
    ("head", StdSig { params: NET_HTTP_HEAD_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP HEAD request. Async." }),
    ("options", StdSig { params: NET_HTTP_OPTIONS_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP OPTIONS request. Async." }),
    ("request", StdSig { params: NET_HTTP_REQUEST_PARAMS, ret: Some("[resp, err]"), doc: "Sends an HTTP request using a full options object; opts.method selects the verb (default GET). Async." }),
    ("cancelToken", StdSig { params: NET_HTTP_CANCEL_TOKEN_PARAMS, ret: None, doc: "Returns a cancel-token handle; pass it as opts.cancel to abort an in-flight request." }),
    ("sse", StdSig { params: NET_HTTP_SSE_PARAMS, ret: Some("[stream, err]"), doc: "Opens a first-class Server-Sent Events client stream. Async." }),
];

static NET_HTTP_MEMBERS: &[(&str, MemberKind)] = &[
    ("get", MemberKind::Fn),
    ("post", MemberKind::Fn),
    ("put", MemberKind::Fn),
    ("patch", MemberKind::Fn),
    ("delete", MemberKind::Fn),
    ("head", MemberKind::Fn),
    ("options", MemberKind::Fn),
    ("request", MemberKind::Fn),
    ("cancelToken", MemberKind::Fn),
    ("sse", MemberKind::Fn),
];

// ── std/http/server ───────────────────────────────────────────────────────────

static HTTP_SERVER_CREATE_PARAMS: &[StdParam] = &[];
static HTTP_SERVER_SERVE_PARAMS: &[StdParam] = &[StdParam::opt("opts", "object")];
// BATT A8 §5.7 — signed cookies + sessions (the `auth` feature).
static HTTP_SERVER_SIGN_COOKIE_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req_untyped("value"),
    StdParam::req("secret", "string | bytes"),
];
static HTTP_SERVER_VERIFY_COOKIE_PARAMS: &[StdParam] = &[
    StdParam::req("signedValue", "string"),
    StdParam::req("secret", "string | bytes"),
];
static HTTP_SERVER_SET_COOKIE_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("value", "string"),
    StdParam::opt("opts", "object"),
];
static HTTP_SERVER_SESSION_PARAMS: &[StdParam] = &[
    StdParam::req("req", "object"),
    StdParam::req("secret", "string | bytes"),
];

static HTTP_SERVER_SIGS: &[(&str, StdSig)] = &[
    ("create", StdSig { params: HTTP_SERVER_CREATE_PARAMS, ret: None, doc: "Creates and returns a new HTTP server handle." }),
    ("serve", StdSig { params: HTTP_SERVER_SERVE_PARAMS, ret: Some("[nil, err]"), doc: "Multi-isolate REUSEPORT serve: spreads the accept loop across N shared-nothing isolates. Async." }),
    ("signCookie", StdSig { params: HTTP_SERVER_SIGN_COOKIE_PARAMS, ret: Some("string"), doc: "Signs a cookie value with HMAC-SHA256 into a tamper-evident string." }),
    ("verifyCookie", StdSig { params: HTTP_SERVER_VERIFY_COOKIE_PARAMS, ret: Some("[value, err]"), doc: "Verifies a signed cookie value in constant time, returning the original value or a Tier-1 error." }),
    ("setCookie", StdSig { params: HTTP_SERVER_SET_COOKIE_PARAMS, ret: Some("string"), doc: "Renders a Set-Cookie header value with attributes (httpOnly defaults true, sameSite defaults Lax); CR/LF in name or value is a Tier-2 panic." }),
    ("session", StdSig { params: HTTP_SERVER_SESSION_PARAMS, ret: Some("[object, err]"), doc: "Reads and verifies the signed session cookie from a request; an absent cookie yields an empty session." }),
];

static HTTP_SERVER_MEMBERS: &[(&str, MemberKind)] = &[
    ("create", MemberKind::Fn),
    ("serve", MemberKind::Fn),
    ("signCookie", MemberKind::Fn),
    ("verifyCookie", MemberKind::Fn),
    ("setCookie", MemberKind::Fn),
    ("session", MemberKind::Fn),
];

// ── std/sqlite ────────────────────────────────────────────────────────────────

static SQLITE_OPEN_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];

static SQLITE_SIGS: &[(&str, StdSig)] = &[
    ("open", StdSig { params: SQLITE_OPEN_PARAMS, ret: Some("[connection, err]"), doc: "Opens (or creates) a SQLite database file and returns a connection handle." }),
];

static SQLITE_MEMBERS: &[(&str, MemberKind)] = &[
    ("open", MemberKind::Fn),
];

// ── std/postgres ──────────────────────────────────────────────────────────────

static POSTGRES_CONNECT_PARAMS: &[StdParam] = &[StdParam::req("url", "string")];

static POSTGRES_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: POSTGRES_CONNECT_PARAMS, ret: Some("[conn, err]"), doc: "Opens an async PostgreSQL connection from a postgres:// URL. Async." }),
];

static POSTGRES_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
];

// ── std/redis ─────────────────────────────────────────────────────────────────

static REDIS_CONNECT_PARAMS: &[StdParam] = &[StdParam::req("url", "string")];

static REDIS_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: REDIS_CONNECT_PARAMS, ret: Some("[conn, err]"), doc: "Opens a multiplexed Redis connection from a redis:// URL. Async." }),
];

static REDIS_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
];

// ── std/docker ────────────────────────────────────────────────────────────────

static DOCKER_CONNECT_PARAMS: &[StdParam] = &[StdParam::opt("opts", "object")];

// Handle-method sigs for dockerClient methods — NOT module exports; added so
// the derivation in std_arity.rs can compute their min-arity (SIG §2.5).
// Each takes exactly one required arg (the container/image/exec id); `remove`
// and `removeImage` also accept an optional `{force}` opts, but the required
// id counts as the leading required param → min = 1.
static DOCKER_ID_PARAMS: &[StdParam] = &[StdParam::req("id", "string")];
// execCreate takes (containerId, opts?) — leading required param is the id.
static DOCKER_EXEC_CREATE_PARAMS: &[StdParam] = &[
    StdParam::req("containerId", "string"),
    StdParam::opt("opts", "object"),
];

static DOCKER_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: DOCKER_CONNECT_PARAMS, ret: Some("[client, err]"), doc: "Connects to the local Docker Engine over its Unix-domain socket, negotiating the API version. Async." }),
    // Handle methods on a dockerClient — not module exports; included for
    // std_arity derivation (SIG §2.5). MemberKind::HandleMethod in DOCKER_MEMBERS.
    ("inspect", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[object, err]"), doc: "Inspect a container by id, returning the full Docker inspect JSON. Async." }),
    ("start", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Start a stopped container by id. Async." }),
    ("stop", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Stop a running container by id. Async." }),
    ("restart", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Restart a container by id. Async." }),
    ("wait", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[int, err]"), doc: "Wait for a container to exit, returning its exit code. Async." }),
    ("remove", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Remove a container by id. Async." }),
    ("removeImage", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Remove an image by id or tag. Async." }),
    ("execCreate", StdSig { params: DOCKER_EXEC_CREATE_PARAMS, ret: Some("[string, err]"), doc: "Create an exec instance on a container and return its exec id. Async." }),
    ("execStart", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[nil, err]"), doc: "Start an exec instance by exec id. Async." }),
    ("execInspect", StdSig { params: DOCKER_ID_PARAMS, ret: Some("[object, err]"), doc: "Inspect an exec instance by exec id. Async." }),
    ("exec", StdSig { params: DOCKER_EXEC_CREATE_PARAMS, ret: Some("[object, err]"), doc: "Create, start, and inspect an exec on a container in one step. Async." }),
];

static DOCKER_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
    // Handle methods on dockerClient — not module exports.
    ("inspect", MemberKind::HandleMethod),
    ("start", MemberKind::HandleMethod),
    ("stop", MemberKind::HandleMethod),
    ("restart", MemberKind::HandleMethod),
    ("wait", MemberKind::HandleMethod),
    ("remove", MemberKind::HandleMethod),
    ("removeImage", MemberKind::HandleMethod),
    ("execCreate", MemberKind::HandleMethod),
    ("execStart", MemberKind::HandleMethod),
    ("execInspect", MemberKind::HandleMethod),
    ("exec", MemberKind::HandleMethod),
];

// ─────────────────────────────────────────────────────────────────────────────
// Batch 3 — 23 remaining modules
// ─────────────────────────────────────────────────────────────────────────────

// ── std/ai ────────────────────────────────────────────────────────────────────

static AI_PROVIDER_PARAMS: &[StdParam] = &[
    StdParam::req("kind", "string"),
    StdParam::opt("config", "object"),
];
static AI_GENERATE_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static AI_STREAM_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static AI_EMBED_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static AI_EMBED_MANY_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static AI_TOOL_PARAMS: &[StdParam] = &[StdParam::req("def", "object")];

static AI_SIGS: &[(&str, StdSig)] = &[
    ("provider", StdSig { params: AI_PROVIDER_PARAMS, ret: Some("provider"), doc: "Create an AI provider handle for a named backend (e.g. 'anthropic', 'openai')." }),
    ("generate", StdSig { params: AI_GENERATE_PARAMS, ret: Some("[object, err]"), doc: "Send a chat/completion request and return the full response. Async." }),
    ("stream", StdSig { params: AI_STREAM_PARAMS, ret: Some("[stream, err]"), doc: "Send a streaming chat/completion request and return an async token stream. Async." }),
    ("embed", StdSig { params: AI_EMBED_PARAMS, ret: Some("[array<float>, err]"), doc: "Compute an embedding vector for a single input. Async." }),
    ("embedMany", StdSig { params: AI_EMBED_MANY_PARAMS, ret: Some("[array<array<float>>, err]"), doc: "Compute embedding vectors for multiple inputs in one request. Async." }),
    ("tool", StdSig { params: AI_TOOL_PARAMS, ret: Some("object"), doc: "Define a tool descriptor for use with ai.generate's 'tools' option." }),
];

static AI_MEMBERS: &[(&str, MemberKind)] = &[
    ("provider", MemberKind::Fn),
    ("generate", MemberKind::Fn),
    ("stream", MemberKind::Fn),
    ("embed", MemberKind::Fn),
    ("embedMany", MemberKind::Fn),
    ("tool", MemberKind::Fn),
];

// ── std/assert ────────────────────────────────────────────────────────────────

static ASSERT_EQ_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_DEEP_EQ_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_NE_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_IS_TRUE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static ASSERT_IS_FALSE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static ASSERT_IS_NIL_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static ASSERT_NOT_NIL_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static ASSERT_GT_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_GTE_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_LT_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_LTE_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
];
static ASSERT_CONTAINS_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("container"),
    StdParam::req_untyped("item"),
];
static ASSERT_APPROX_EQ_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("a"),
    StdParam::req_untyped("b"),
    StdParam::opt("epsilon", "number"),
];
static ASSERT_THROWS_PARAMS: &[StdParam] = &[StdParam::req("f", "fn()")];
static ASSERT_THROWS_WITH_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::req("msg", "string"),
];
static ASSERT_MATCHES_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("value"),
    StdParam::req("pattern", "regex | string"),
];
static ASSERT_SNAPSHOT_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req_untyped("value"),
];

static ASSERT_SIGS: &[(&str, StdSig)] = &[
    ("eq", StdSig { params: ASSERT_EQ_PARAMS, ret: None, doc: "Assert that two values are strictly equal (===); panics with a diff on failure." }),
    ("deepEq", StdSig { params: ASSERT_DEEP_EQ_PARAMS, ret: None, doc: "Assert that two values are deeply structurally equal; panics with a diff on failure." }),
    ("ne", StdSig { params: ASSERT_NE_PARAMS, ret: None, doc: "Assert that two values are NOT equal." }),
    ("isTrue", StdSig { params: ASSERT_IS_TRUE_PARAMS, ret: None, doc: "Assert that a value is strictly true." }),
    ("isFalse", StdSig { params: ASSERT_IS_FALSE_PARAMS, ret: None, doc: "Assert that a value is strictly false." }),
    ("isNil", StdSig { params: ASSERT_IS_NIL_PARAMS, ret: None, doc: "Assert that a value is nil." }),
    ("notNil", StdSig { params: ASSERT_NOT_NIL_PARAMS, ret: None, doc: "Assert that a value is not nil." }),
    ("gt", StdSig { params: ASSERT_GT_PARAMS, ret: None, doc: "Assert that a is greater than b." }),
    ("gte", StdSig { params: ASSERT_GTE_PARAMS, ret: None, doc: "Assert that a is greater than or equal to b." }),
    ("lt", StdSig { params: ASSERT_LT_PARAMS, ret: None, doc: "Assert that a is less than b." }),
    ("lte", StdSig { params: ASSERT_LTE_PARAMS, ret: None, doc: "Assert that a is less than or equal to b." }),
    ("contains", StdSig { params: ASSERT_CONTAINS_PARAMS, ret: None, doc: "Assert that a container (string, array, object) contains an item or key." }),
    ("approxEq", StdSig { params: ASSERT_APPROX_EQ_PARAMS, ret: None, doc: "Assert that two numbers are within epsilon of each other (default 1e-9)." }),
    ("throws", StdSig { params: ASSERT_THROWS_PARAMS, ret: None, doc: "Assert that a zero-argument function throws a panic." }),
    ("throwsWith", StdSig { params: ASSERT_THROWS_WITH_PARAMS, ret: None, doc: "Assert that a function throws and the panic message contains a substring." }),
    ("matches", StdSig { params: ASSERT_MATCHES_PARAMS, ret: None, doc: "Assert that a value matches a regex pattern." }),
    ("snapshot", StdSig { params: ASSERT_SNAPSHOT_PARAMS, ret: None, doc: "Assert that a value matches a persisted snapshot, creating it on first run." }),
];

static ASSERT_MEMBERS: &[(&str, MemberKind)] = &[
    ("eq", MemberKind::Fn),
    ("deepEq", MemberKind::Fn),
    ("ne", MemberKind::Fn),
    ("isTrue", MemberKind::Fn),
    ("isFalse", MemberKind::Fn),
    ("isNil", MemberKind::Fn),
    ("notNil", MemberKind::Fn),
    ("gt", MemberKind::Fn),
    ("gte", MemberKind::Fn),
    ("lt", MemberKind::Fn),
    ("lte", MemberKind::Fn),
    ("contains", MemberKind::Fn),
    ("approxEq", MemberKind::Fn),
    ("throws", MemberKind::Fn),
    ("throwsWith", MemberKind::Fn),
    ("matches", MemberKind::Fn),
    ("snapshot", MemberKind::Fn),
];

// ── std/bench ─────────────────────────────────────────────────────────────────

static BENCH_MEASURE_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::opt("opts", "object"),
];
static BENCH_COMPARE_PARAMS: &[StdParam] = &[
    StdParam::req("fns", "array"),
    StdParam::opt("opts", "object"),
];

static BENCH_SIGS: &[(&str, StdSig)] = &[
    ("measure", StdSig { params: BENCH_MEASURE_PARAMS, ret: Some("object"), doc: "Benchmark a zero-argument function and return timing statistics (iterations, totalMs, avgMs). Async." }),
    ("compare", StdSig { params: BENCH_COMPARE_PARAMS, ret: Some("array"), doc: "Run multiple benchmark functions and return their timing results for comparison. Async." }),
];

static BENCH_MEMBERS: &[(&str, MemberKind)] = &[
    ("measure", MemberKind::Fn),
    ("compare", MemberKind::Fn),
];

// ── std/cli ───────────────────────────────────────────────────────────────────

static CLI_PARSE_PARAMS: &[StdParam] = &[StdParam::req("spec", "object")];

static CLI_SIGS: &[(&str, StdSig)] = &[
    ("parse", StdSig { params: CLI_PARSE_PARAMS, ret: Some("object"), doc: "Parse process arguments according to a CLI spec and return the parsed flags and positionals." }),
];

static CLI_MEMBERS: &[(&str, MemberKind)] = &[
    ("parse", MemberKind::Fn),
];

// ── std/color ─────────────────────────────────────────────────────────────────

static COLOR_FG_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];
static COLOR_RGB_PARAMS: &[StdParam] = &[
    StdParam::req("r", "int"),
    StdParam::req("g", "int"),
    StdParam::req("b", "int"),
];
static COLOR_BG_RGB_PARAMS: &[StdParam] = &[
    StdParam::req("r", "int"),
    StdParam::req("g", "int"),
    StdParam::req("b", "int"),
];
static COLOR_STRIP_PARAMS: &[StdParam] = &[StdParam::req("text", "string")];

static COLOR_SIGS: &[(&str, StdSig)] = &[
    ("black", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI black foreground color escape codes." }),
    ("red", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI red foreground color escape codes." }),
    ("green", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI green foreground color escape codes." }),
    ("yellow", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI yellow foreground color escape codes." }),
    ("blue", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI blue foreground color escape codes." }),
    ("magenta", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI magenta foreground color escape codes." }),
    ("cyan", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI cyan foreground color escape codes." }),
    ("white", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI white foreground color escape codes." }),
    ("gray", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI gray foreground color escape codes." }),
    ("grey", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Alias for color.gray." }),
    ("bold", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI bold style escape codes." }),
    ("dim", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI dim style escape codes." }),
    ("italic", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI italic style escape codes." }),
    ("underline", StdSig { params: COLOR_FG_PARAMS, ret: Some("string"), doc: "Wrap text in ANSI underline style escape codes." }),
    ("rgb", StdSig { params: COLOR_RGB_PARAMS, ret: Some("fn(string) -> string"), doc: "Return a function that wraps text in 24-bit (truecolor) foreground ANSI escape codes." }),
    ("bgRgb", StdSig { params: COLOR_BG_RGB_PARAMS, ret: Some("fn(string) -> string"), doc: "Return a function that wraps text in 24-bit (truecolor) background ANSI escape codes." }),
    ("strip", StdSig { params: COLOR_STRIP_PARAMS, ret: Some("string"), doc: "Remove all ANSI escape sequences from a string." }),
];

static COLOR_MEMBERS: &[(&str, MemberKind)] = &[
    ("black", MemberKind::Fn),
    ("red", MemberKind::Fn),
    ("green", MemberKind::Fn),
    ("yellow", MemberKind::Fn),
    ("blue", MemberKind::Fn),
    ("magenta", MemberKind::Fn),
    ("cyan", MemberKind::Fn),
    ("white", MemberKind::Fn),
    ("gray", MemberKind::Fn),
    ("grey", MemberKind::Fn),
    ("bold", MemberKind::Fn),
    ("dim", MemberKind::Fn),
    ("italic", MemberKind::Fn),
    ("underline", MemberKind::Fn),
    ("rgb", MemberKind::Fn),
    ("bgRgb", MemberKind::Fn),
    ("strip", MemberKind::Fn),
];

// ── std/schema ────────────────────────────────────────────────────────────────

static SCHEMA_ZERO_PARAMS: &[StdParam] = &[];
static SCHEMA_ONE_VALUE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static SCHEMA_ONE_SCHEMA_PARAMS: &[StdParam] = &[StdParam::req("schema", "object")];
static SCHEMA_SCHEMA_NUM_PARAMS: &[StdParam] = &[
    StdParam::req("schema", "object"),
    StdParam::req("n", "number"),
];
static SCHEMA_SCHEMA_STR_PARAMS: &[StdParam] = &[
    StdParam::req("schema", "object"),
    StdParam::req("pattern", "string"),
];
static SCHEMA_REFINE_PARAMS: &[StdParam] = &[
    StdParam::req("schema", "object"),
    StdParam::req("f", "fn(value)"),
    StdParam::req("message", "string"),
];
static SCHEMA_DEFAULT_PARAMS: &[StdParam] = &[
    StdParam::req("schema", "object"),
    StdParam::req_untyped("default"),
];
static SCHEMA_PARSE_PARAMS: &[StdParam] = &[
    StdParam::req("schema", "object"),
    StdParam::req_untyped("value"),
    StdParam::opt("opts", "object"),
];
static SCHEMA_MAP_PARAMS: &[StdParam] = &[
    StdParam::req("keySchema", "object"),
    StdParam::req("valSchema", "object"),
];
static SCHEMA_STRICT_PARAMS: &[StdParam] = &[StdParam::req("objSchema", "object")];
static SCHEMA_FROM_CLASS_PARAMS: &[StdParam] = &[StdParam::req("Class", "class")];

static SCHEMA_SIGS: &[(&str, StdSig)] = &[
    ("string", StdSig { params: SCHEMA_ZERO_PARAMS, ret: Some("schema"), doc: "Create a schema that validates string values." }),
    ("number", StdSig { params: SCHEMA_ZERO_PARAMS, ret: Some("schema"), doc: "Create a schema that validates numeric values." }),
    ("bool", StdSig { params: SCHEMA_ZERO_PARAMS, ret: Some("schema"), doc: "Create a schema that validates boolean values." }),
    ("nilType", StdSig { params: SCHEMA_ZERO_PARAMS, ret: Some("schema"), doc: "Create a schema that validates nil values (the nil-type constructor; 'nil' is a reserved keyword)." }),
    ("any", StdSig { params: SCHEMA_ZERO_PARAMS, ret: Some("schema"), doc: "Create a schema that accepts any value." }),
    ("literal", StdSig { params: SCHEMA_ONE_VALUE_PARAMS, ret: Some("schema"), doc: "Create a schema that validates a single literal value." }),
    ("array", StdSig { params: SCHEMA_ONE_SCHEMA_PARAMS, ret: Some("schema"), doc: "Create a schema that validates an array whose every element satisfies elem." }),
    ("object", StdSig { params: SCHEMA_ONE_VALUE_PARAMS, ret: Some("schema"), doc: "Create a schema that validates an object against a fields descriptor." }),
    ("strict", StdSig { params: SCHEMA_STRICT_PARAMS, ret: Some("schema"), doc: "Clone an object schema and enable strict mode (reject unknown keys)." }),
    ("map", StdSig { params: SCHEMA_MAP_PARAMS, ret: Some("schema"), doc: "Create a schema that validates a map with the given key and value schemas." }),
    ("optional", StdSig { params: SCHEMA_ONE_SCHEMA_PARAMS, ret: Some("schema"), doc: "Create a schema that accepts nil or any value satisfying the inner schema." }),
    ("union", StdSig { params: SCHEMA_ONE_VALUE_PARAMS, ret: Some("schema"), doc: "Create a schema that accepts a value matching any schema in an array of options." }),
    ("oneOf", StdSig { params: SCHEMA_ONE_VALUE_PARAMS, ret: Some("schema"), doc: "Create a schema that accepts a value equal to one of the allowed literal values (enum discriminant)." }),
    ("parse", StdSig { params: SCHEMA_PARSE_PARAMS, ret: Some("[value, err]"), doc: "Validate a value against a schema, returning a Tier-1 [value, err] pair." }),
    ("parseAll", StdSig { params: SCHEMA_PARSE_PARAMS, ret: Some("[value, errors]"), doc: "Validate a value against a schema, collecting ALL validation errors before returning." }),
    ("min", StdSig { params: SCHEMA_SCHEMA_NUM_PARAMS, ret: Some("schema"), doc: "Add a minimum constraint to a numeric or array/string-length schema." }),
    ("max", StdSig { params: SCHEMA_SCHEMA_NUM_PARAMS, ret: Some("schema"), doc: "Add a maximum constraint to a numeric or array/string-length schema." }),
    ("minLength", StdSig { params: SCHEMA_SCHEMA_NUM_PARAMS, ret: Some("schema"), doc: "Add a minimum character/element length constraint to a string or array schema." }),
    ("maxLength", StdSig { params: SCHEMA_SCHEMA_NUM_PARAMS, ret: Some("schema"), doc: "Add a maximum character/element length constraint to a string or array schema." }),
    ("pattern", StdSig { params: SCHEMA_SCHEMA_STR_PARAMS, ret: Some("schema"), doc: "Add a regex pattern constraint to a string schema." }),
    ("refine", StdSig { params: SCHEMA_REFINE_PARAMS, ret: Some("schema"), doc: "Add a custom predicate refinement to a schema; f must return truthy or the message is reported." }),
    ("default", StdSig { params: SCHEMA_DEFAULT_PARAMS, ret: Some("schema"), doc: "Attach a default value to a schema so nil inputs are substituted before validation." }),
    ("fromClass", StdSig { params: SCHEMA_FROM_CLASS_PARAMS, ret: Some("schema"), doc: "Derive a schema from a class's declared field types." }),
];

static SCHEMA_MEMBERS: &[(&str, MemberKind)] = &[
    ("string", MemberKind::Fn),
    ("number", MemberKind::Fn),
    ("bool", MemberKind::Fn),
    ("nilType", MemberKind::Fn),
    ("any", MemberKind::Fn),
    ("literal", MemberKind::Fn),
    ("array", MemberKind::Fn),
    ("object", MemberKind::Fn),
    ("strict", MemberKind::Fn),
    ("map", MemberKind::Fn),
    ("optional", MemberKind::Fn),
    ("union", MemberKind::Fn),
    ("oneOf", MemberKind::Fn),
    ("parse", MemberKind::Fn),
    ("parseAll", MemberKind::Fn),
    ("min", MemberKind::Fn),
    ("max", MemberKind::Fn),
    ("minLength", MemberKind::Fn),
    ("maxLength", MemberKind::Fn),
    ("pattern", MemberKind::Fn),
    ("refine", MemberKind::Fn),
    ("default", MemberKind::Fn),
    ("fromClass", MemberKind::Fn),
];

// ── std/shared ────────────────────────────────────────────────────────────────

static SHARED_FREEZE_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];
static SHARED_IS_SHARED_PARAMS: &[StdParam] = &[StdParam::req_untyped("value")];

static SHARED_SIGS: &[(&str, StdSig)] = &[
    ("freeze", StdSig { params: SHARED_FREEZE_PARAMS, ret: None, doc: "Deep-convert a value into an immutable, Arc-backed shared value that can cross worker isolate boundaries." }),
    ("isShared", StdSig { params: SHARED_IS_SHARED_PARAMS, ret: Some("bool"), doc: "Return true if a value is a frozen shared node." }),
];

static SHARED_MEMBERS: &[(&str, MemberKind)] = &[
    ("freeze", MemberKind::Fn),
    ("isShared", MemberKind::Fn),
];

// ── std/lru ───────────────────────────────────────────────────────────────────

static LRU_NEW_PARAMS: &[StdParam] = &[StdParam::req("capacity", "int")];

static LRU_SIGS: &[(&str, StdSig)] = &[
    ("new", StdSig { params: LRU_NEW_PARAMS, ret: None, doc: "Create a new LRU cache handle with the given maximum capacity." }),
];

static LRU_MEMBERS: &[(&str, MemberKind)] = &[
    ("new", MemberKind::Fn),
];

// ── std/events ────────────────────────────────────────────────────────────────

static EVENTS_NEW_PARAMS: &[StdParam] = &[];

static EVENTS_SIGS: &[(&str, StdSig)] = &[
    ("new", StdSig { params: EVENTS_NEW_PARAMS, ret: None, doc: "Create a new event bus handle for pub/sub messaging within an isolate." }),
];

static EVENTS_MEMBERS: &[(&str, MemberKind)] = &[
    ("new", MemberKind::Fn),
];

// ── std/template ──────────────────────────────────────────────────────────────

static TEMPLATE_RENDER_PARAMS: &[StdParam] = &[
    StdParam::req("template", "string"),
    StdParam::req("vars", "object"),
];

static TEMPLATE_SIGS: &[(&str, StdSig)] = &[
    ("render", StdSig { params: TEMPLATE_RENDER_PARAMS, ret: Some("string"), doc: "Render a Mustache-style template string, substituting {{key}} placeholders with values from the vars object." }),
];

static TEMPLATE_MEMBERS: &[(&str, MemberKind)] = &[
    ("render", MemberKind::Fn),
];

// ── std/caps ──────────────────────────────────────────────────────────────────

static CAPS_HAS_PARAMS: &[StdParam] = &[StdParam::req("cap", "string")];
static CAPS_LIST_PARAMS: &[StdParam] = &[];
static CAPS_DROP_PARAMS: &[StdParam] = &[StdParam::req("cap", "string")];
static CAPS_DROP_ALL_PARAMS: &[StdParam] = &[];

static CAPS_SIGS: &[(&str, StdSig)] = &[
    ("has", StdSig { params: CAPS_HAS_PARAMS, ret: Some("bool"), doc: "Return whether the named capability (fs/net/process/ffi/env) is currently granted." }),
    ("list", StdSig { params: CAPS_LIST_PARAMS, ret: Some("array<string>"), doc: "Return the list of currently granted capability names." }),
    ("drop", StdSig { params: CAPS_DROP_PARAMS, ret: None, doc: "Irreversibly revoke a capability from the current isolate." }),
    ("dropAll", StdSig { params: CAPS_DROP_ALL_PARAMS, ret: None, doc: "Irreversibly revoke all capabilities from the current isolate." }),
];

static CAPS_MEMBERS: &[(&str, MemberKind)] = &[
    ("has", MemberKind::Fn),
    ("list", MemberKind::Fn),
    ("drop", MemberKind::Fn),
    ("dropAll", MemberKind::Fn),
];

// ── std/task ──────────────────────────────────────────────────────────────────

static TASK_SPAWN_PARAMS: &[StdParam] = &[StdParam::req("f", "fn()")];
static TASK_GATHER_PARAMS: &[StdParam] = &[StdParam::req("futures", "array")];
static TASK_RACE_PARAMS: &[StdParam] = &[StdParam::req("futures", "array")];
static TASK_TIMEOUT_PARAMS: &[StdParam] = &[
    StdParam::req("ms", "number"),
    StdParam::req("future", "future"),
];
static TASK_RETRY_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::opt("opts", "object"),
];
static TASK_PIPE_PARAMS: &[StdParam] = &[
    StdParam::req("gen", "generator"),
    StdParam::req("bus", "events"),
];
static TASK_PMAP_PARAMS: &[StdParam] = &[
    StdParam::req("data", "array"),
    StdParam::req("f", "worker fn"),
    StdParam::opt("opts", "object"),
];
static TASK_PREDUCE_PARAMS: &[StdParam] = &[
    StdParam::req("data", "array"),
    StdParam::req("f", "worker fn"),
    StdParam::req_untyped("init"),
    StdParam::opt("opts", "object"),
];

static TASK_SIGS: &[(&str, StdSig)] = &[
    ("spawn", StdSig { params: TASK_SPAWN_PARAMS, ret: Some("future"), doc: "Detach a future as an independent task that continues after the call site." }),
    ("gather", StdSig { params: TASK_GATHER_PARAMS, ret: Some("future<array>"), doc: "Await all futures in an array concurrently and return their results in order." }),
    ("race", StdSig { params: TASK_RACE_PARAMS, ret: Some("future"), doc: "Await the first future to complete, cancelling all losers." }),
    ("timeout", StdSig { params: TASK_TIMEOUT_PARAMS, ret: Some("[value, err]"), doc: "Race a future against a timer; return [nil, err] if the timer fires first." }),
    ("retry", StdSig { params: TASK_RETRY_PARAMS, ret: Some("future"), doc: "Retry a function up to the configured number of times with optional back-off." }),
    ("pipe", StdSig { params: TASK_PIPE_PARAMS, ret: Some("future"), doc: "Bridge a worker generator stream onto a local event bus by forwarding each yielded value." }),
    ("pmap", StdSig { params: TASK_PMAP_PARAMS, ret: Some("future<array>"), doc: "Parallel-map an array across the worker pool, chunking the work for multi-core throughput. Async." }),
    ("preduce", StdSig { params: TASK_PREDUCE_PARAMS, ret: Some("future"), doc: "Parallel-reduce an array across the worker pool, folding per-chunk then combining with init. Async." }),
];

static TASK_MEMBERS: &[(&str, MemberKind)] = &[
    ("spawn", MemberKind::Fn),
    ("gather", MemberKind::Fn),
    ("race", MemberKind::Fn),
    ("timeout", MemberKind::Fn),
    ("retry", MemberKind::Fn),
    ("pipe", MemberKind::Fn),
    ("pmap", MemberKind::Fn),
    ("preduce", MemberKind::Fn),
];

// ── std/time ──────────────────────────────────────────────────────────────────

static TIME_ZERO_PARAMS: &[StdParam] = &[];
static TIME_MS_PARAMS: &[StdParam] = &[StdParam::req("ms", "number")];
static TIME_INTERVAL_PARAMS: &[StdParam] = &[StdParam::req("ms", "number")];
static TIME_DEBOUNCE_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::req("ms", "number"),
];
static TIME_THROTTLE_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::req("ms", "number"),
];

static TIME_SIGS: &[(&str, StdSig)] = &[
    ("now", StdSig { params: TIME_ZERO_PARAMS, ret: Some("float"), doc: "Return the current wall-clock time as milliseconds since the Unix epoch." }),
    ("monotonic", StdSig { params: TIME_ZERO_PARAMS, ret: Some("float"), doc: "Return a monotonic timestamp in milliseconds since process start." }),
    ("sleep", StdSig { params: TIME_MS_PARAMS, ret: None, doc: "Suspend the current async task for at least ms milliseconds. Async." }),
    ("millis", StdSig { params: TIME_MS_PARAMS, ret: Some("float"), doc: "Return ms unchanged (identity helper for duration clarity)." }),
    ("seconds", StdSig { params: TIME_MS_PARAMS, ret: Some("float"), doc: "Convert a number of seconds to milliseconds." }),
    ("minutes", StdSig { params: TIME_MS_PARAMS, ret: Some("float"), doc: "Convert a number of minutes to milliseconds." }),
    ("hours", StdSig { params: TIME_MS_PARAMS, ret: Some("float"), doc: "Convert a number of hours to milliseconds." }),
    ("interval", StdSig { params: TIME_INTERVAL_PARAMS, ret: None, doc: "Create a periodic timer handle that ticks every ms milliseconds." }),
    ("debounce", StdSig { params: TIME_DEBOUNCE_PARAMS, ret: None, doc: "Return a debounced wrapper of f that only fires ms after the last call." }),
    ("throttle", StdSig { params: TIME_THROTTLE_PARAMS, ret: None, doc: "Return a throttled wrapper of f that fires at most once per ms window." }),
];

static TIME_MEMBERS: &[(&str, MemberKind)] = &[
    ("now", MemberKind::Fn),
    ("monotonic", MemberKind::Fn),
    ("sleep", MemberKind::Fn),
    ("millis", MemberKind::Fn),
    ("seconds", MemberKind::Fn),
    ("minutes", MemberKind::Fn),
    ("hours", MemberKind::Fn),
    ("interval", MemberKind::Fn),
    ("debounce", MemberKind::Fn),
    ("throttle", MemberKind::Fn),
];

// ── std/sync ──────────────────────────────────────────────────────────────────

static SYNC_CHANNEL_PARAMS: &[StdParam] = &[StdParam::opt("capacity", "int")];
static SYNC_SEND_PARAMS: &[StdParam] = &[
    StdParam::req("ch", "channel"),
    StdParam::req_untyped("value"),
];
static SYNC_RECV_PARAMS: &[StdParam] = &[StdParam::req("ch", "channel")];
static SYNC_TRY_RECV_PARAMS: &[StdParam] = &[StdParam::req("ch", "channel")];
static SYNC_CLOSE_PARAMS: &[StdParam] = &[StdParam::req("ch", "channel")];
static SYNC_SEMAPHORE_PARAMS: &[StdParam] = &[StdParam::req("permits", "int")];
static SYNC_ACQUIRE_PARAMS: &[StdParam] = &[StdParam::req("sem", "semaphore")];
static SYNC_RELEASE_PARAMS: &[StdParam] = &[StdParam::req("sem", "semaphore")];
static SYNC_WITH_PERMIT_PARAMS: &[StdParam] = &[
    StdParam::req("sem", "semaphore"),
    StdParam::req("f", "fn()"),
];
static SYNC_AVAILABLE_PARAMS: &[StdParam] = &[StdParam::req("sem", "semaphore")];
static SYNC_RATE_LIMITER_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];

static SYNC_SIGS: &[(&str, StdSig)] = &[
    ("channel", StdSig { params: SYNC_CHANNEL_PARAMS, ret: None, doc: "Create a channel handle for passing values between tasks; optional capacity makes it bounded." }),
    ("send", StdSig { params: SYNC_SEND_PARAMS, ret: Some("[nil, err]"), doc: "Send a value into a channel, blocking if the buffer is full. Async." }),
    ("recv", StdSig { params: SYNC_RECV_PARAMS, ret: Some("[value, err]"), doc: "Receive the next value from a channel, blocking until one is available. Async." }),
    ("tryRecv", StdSig { params: SYNC_TRY_RECV_PARAMS, ret: Some("[value, err]"), doc: "Non-blocking receive: return immediately with [nil, err] if no value is ready." }),
    ("close", StdSig { params: SYNC_CLOSE_PARAMS, ret: None, doc: "Close a channel so further sends return an error and pending recvs drain remaining values." }),
    ("semaphore", StdSig { params: SYNC_SEMAPHORE_PARAMS, ret: None, doc: "Create a semaphore with an initial permit count for limiting concurrency." }),
    ("acquire", StdSig { params: SYNC_ACQUIRE_PARAMS, ret: None, doc: "Acquire one permit from a semaphore, blocking until one is available. Async." }),
    ("release", StdSig { params: SYNC_RELEASE_PARAMS, ret: None, doc: "Release one permit back to a semaphore, waking a waiting acquirer." }),
    ("withPermit", StdSig { params: SYNC_WITH_PERMIT_PARAMS, ret: None, doc: "Acquire a permit, run a function, then release the permit on all paths. Async." }),
    ("available", StdSig { params: SYNC_AVAILABLE_PARAMS, ret: Some("int"), doc: "Return the current number of available permits in a semaphore." }),
    ("rateLimiter", StdSig { params: SYNC_RATE_LIMITER_PARAMS, ret: None, doc: "Create a token-bucket rate limiter handle from an opts object ({perSecond} or {count, windowMs})." }),
];

static SYNC_MEMBERS: &[(&str, MemberKind)] = &[
    ("channel", MemberKind::Fn),
    ("send", MemberKind::Fn),
    ("recv", MemberKind::Fn),
    ("tryRecv", MemberKind::Fn),
    ("close", MemberKind::Fn),
    ("semaphore", MemberKind::Fn),
    ("acquire", MemberKind::Fn),
    ("release", MemberKind::Fn),
    ("withPermit", MemberKind::Fn),
    ("available", MemberKind::Fn),
    ("rateLimiter", MemberKind::Fn),
];

// ── std/stream ────────────────────────────────────────────────────────────────

static STREAM_FROM_PARAMS: &[StdParam] = &[StdParam::req_untyped("source")];
static STREAM_RANGE_PARAMS: &[StdParam] = &[
    StdParam::req("start", "number"),
    StdParam::req("end", "number"),
    StdParam::opt("step", "number"),
];
static STREAM_TRANSFORM_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("f", "fn(item)"),
];
static STREAM_TAKE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("n", "number"),
];
static STREAM_DROP_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("n", "number"),
];
static STREAM_ENUMERATE_PARAMS: &[StdParam] = &[StdParam::req("s", "stream")];
static STREAM_ZIP_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("t", "stream"),
];
static STREAM_COLLECT_PARAMS: &[StdParam] = &[StdParam::req("s", "stream")];
static STREAM_REDUCE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("f", "fn(acc, item)"),
    StdParam::req_untyped("init"),
];
static STREAM_COUNT_PARAMS: &[StdParam] = &[StdParam::req("s", "stream")];
static STREAM_FIND_PARAMS: &[StdParam] = &[
    StdParam::req("s", "stream"),
    StdParam::req("f", "fn(item)"),
];
static STREAM_FIRST_PARAMS: &[StdParam] = &[StdParam::req("s", "stream")];

static STREAM_SIGS: &[(&str, StdSig)] = &[
    ("from", StdSig { params: STREAM_FROM_PARAMS, ret: None, doc: "Create a lazy stream from an array or generator." }),
    ("range", StdSig { params: STREAM_RANGE_PARAMS, ret: None, doc: "Create a lazy numeric stream from start to end with an optional step." }),
    ("map", StdSig { params: STREAM_TRANSFORM_PARAMS, ret: None, doc: "Append a map stage to a stream, applying f to each item." }),
    ("filter", StdSig { params: STREAM_TRANSFORM_PARAMS, ret: None, doc: "Append a filter stage to a stream, keeping only items for which f is truthy." }),
    ("take", StdSig { params: STREAM_TAKE_PARAMS, ret: None, doc: "Append a take stage that limits the stream to the first n items." }),
    ("drop", StdSig { params: STREAM_DROP_PARAMS, ret: None, doc: "Append a drop stage that skips the first n items." }),
    ("flatMap", StdSig { params: STREAM_TRANSFORM_PARAMS, ret: None, doc: "Append a flatMap stage that applies f and flattens one level of nested streams or arrays." }),
    ("enumerate", StdSig { params: STREAM_ENUMERATE_PARAMS, ret: None, doc: "Append an enumerate stage that pairs each item with its zero-based index as [index, value]." }),
    ("zip", StdSig { params: STREAM_ZIP_PARAMS, ret: None, doc: "Append a zip stage pairing items from two streams element-by-element." }),
    ("collect", StdSig { params: STREAM_COLLECT_PARAMS, ret: Some("future<array>"), doc: "Pull all items from a stream into an array. Async." }),
    ("forEach", StdSig { params: STREAM_TRANSFORM_PARAMS, ret: Some("future"), doc: "Pull all items from a stream and call f for each side effect. Async." }),
    ("reduce", StdSig { params: STREAM_REDUCE_PARAMS, ret: Some("future"), doc: "Fold a stream into a single value by applying f to each item with an accumulator. Async." }),
    ("count", StdSig { params: STREAM_COUNT_PARAMS, ret: Some("future<int>"), doc: "Count the total number of items in a stream. Async." }),
    ("find", StdSig { params: STREAM_FIND_PARAMS, ret: Some("future"), doc: "Return the first item for which f is truthy, or nil if none. Async." }),
    ("first", StdSig { params: STREAM_FIRST_PARAMS, ret: Some("future"), doc: "Return the first item of a stream, or nil if empty. Async." }),
];

static STREAM_MEMBERS: &[(&str, MemberKind)] = &[
    ("from", MemberKind::Fn),
    ("range", MemberKind::Fn),
    ("map", MemberKind::Fn),
    ("filter", MemberKind::Fn),
    ("take", MemberKind::Fn),
    ("drop", MemberKind::Fn),
    ("flatMap", MemberKind::Fn),
    ("enumerate", MemberKind::Fn),
    ("zip", MemberKind::Fn),
    ("collect", MemberKind::Fn),
    ("forEach", MemberKind::Fn),
    ("reduce", MemberKind::Fn),
    ("count", MemberKind::Fn),
    ("find", MemberKind::Fn),
    ("first", MemberKind::Fn),
];

// ── std/date ──────────────────────────────────────────────────────────────────

static DATE_ZERO_PARAMS: &[StdParam] = &[];
static DATE_FROM_EPOCH_PARAMS: &[StdParam] = &[StdParam::req("ms", "number")];
static DATE_PARSE_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::opt("fmt", "string"),
];
static DATE_FORMAT_PARAMS: &[StdParam] = &[
    StdParam::req("date", "object"),
    StdParam::req("fmt", "string"),
    StdParam::opt("offsetMinutes", "number"),
];
static DATE_ADD_DAYS_PARAMS: &[StdParam] = &[
    StdParam::req("date", "object"),
    StdParam::req("n", "number"),
];
static DATE_DIFF_MS_PARAMS: &[StdParam] = &[
    StdParam::req("a", "object"),
    StdParam::req("b", "object"),
];

static DATE_SIGS: &[(&str, StdSig)] = &[
    ("now", StdSig { params: DATE_ZERO_PARAMS, ret: Some("object"), doc: "Return the current local date and time as a date object." }),
    ("fromEpochMs", StdSig { params: DATE_FROM_EPOCH_PARAMS, ret: Some("object"), doc: "Create a date object from a Unix epoch timestamp in milliseconds." }),
    ("parse", StdSig { params: DATE_PARSE_PARAMS, ret: Some("[object, err]"), doc: "Parse an ISO 8601 date/time string into a date object." }),
    ("format", StdSig { params: DATE_FORMAT_PARAMS, ret: Some("string"), doc: "Format a date object using a strftime-style format string." }),
    ("addDays", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n days added." }),
    ("addHours", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n hours added." }),
    ("addMinutes", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n minutes added." }),
    ("addSeconds", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n seconds added." }),
    ("addMonths", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n months added." }),
    ("addYears", StdSig { params: DATE_ADD_DAYS_PARAMS, ret: Some("object"), doc: "Return a new date object with n years added." }),
    ("diffMs", StdSig { params: DATE_DIFF_MS_PARAMS, ret: Some("float"), doc: "Return the difference between two date objects in milliseconds (a − b)." }),
];

static DATE_MEMBERS: &[(&str, MemberKind)] = &[
    ("now", MemberKind::Fn),
    ("fromEpochMs", MemberKind::Fn),
    ("parse", MemberKind::Fn),
    ("format", MemberKind::Fn),
    ("addDays", MemberKind::Fn),
    ("addHours", MemberKind::Fn),
    ("addMinutes", MemberKind::Fn),
    ("addSeconds", MemberKind::Fn),
    ("addMonths", MemberKind::Fn),
    ("addYears", MemberKind::Fn),
    ("diffMs", MemberKind::Fn),
];

// ── std/intl ──────────────────────────────────────────────────────────────────

static INTL_FORMAT_NUMBER_PARAMS: &[StdParam] = &[
    StdParam::req("n", "number"),
    StdParam::opt("opts", "object"),
];
static INTL_FORMAT_CURRENCY_PARAMS: &[StdParam] = &[
    StdParam::req("n", "number"),
    StdParam::req("currency", "string"),
    StdParam::opt("opts", "object"),
];
static INTL_FORMAT_DATE_PARAMS: &[StdParam] = &[
    StdParam::req("date", "object"),
    StdParam::opt("opts", "object"),
];
static INTL_CASE_UPPER_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::opt("locale", "string"),
];
static INTL_CASE_LOWER_PARAMS: &[StdParam] = &[
    StdParam::req("s", "string"),
    StdParam::opt("locale", "string"),
];
static INTL_COMPARE_PARAMS: &[StdParam] = &[
    StdParam::req("a", "string"),
    StdParam::req("b", "string"),
    StdParam::opt("opts", "object"),
];

static INTL_SIGS: &[(&str, StdSig)] = &[
    ("formatNumber", StdSig { params: INTL_FORMAT_NUMBER_PARAMS, ret: Some("string"), doc: "Format a number using locale-aware formatting rules." }),
    ("formatCurrency", StdSig { params: INTL_FORMAT_CURRENCY_PARAMS, ret: Some("string"), doc: "Format a number as a currency amount with the given ISO 4217 currency code." }),
    ("formatDate", StdSig { params: INTL_FORMAT_DATE_PARAMS, ret: Some("string"), doc: "Format a date object using locale-aware date formatting." }),
    ("caseUpper", StdSig { params: INTL_CASE_UPPER_PARAMS, ret: Some("string"), doc: "Convert a string to uppercase using Unicode locale-aware case folding." }),
    ("caseLower", StdSig { params: INTL_CASE_LOWER_PARAMS, ret: Some("string"), doc: "Convert a string to lowercase using Unicode locale-aware case folding." }),
    ("compare", StdSig { params: INTL_COMPARE_PARAMS, ret: Some("int"), doc: "Compare two strings with locale-aware collation; returns -1, 0, or 1." }),
];

static INTL_MEMBERS: &[(&str, MemberKind)] = &[
    ("formatNumber", MemberKind::Fn),
    ("formatCurrency", MemberKind::Fn),
    ("formatDate", MemberKind::Fn),
    ("caseUpper", MemberKind::Fn),
    ("caseLower", MemberKind::Fn),
    ("compare", MemberKind::Fn),
];

// ── std/log ───────────────────────────────────────────────────────────────────

static LOG_MSG_PARAMS: &[StdParam] = &[
    StdParam::req_untyped("message"),
    StdParam::opt("fields", "object"),
];
static LOG_SET_LEVEL_PARAMS: &[StdParam] = &[StdParam::req("level", "string")];
static LOG_SET_FORMAT_PARAMS: &[StdParam] = &[StdParam::req("format", "string")];

static LOG_SIGS: &[(&str, StdSig)] = &[
    ("debug", StdSig { params: LOG_MSG_PARAMS, ret: None, doc: "Emit a debug-level log message with optional structured fields." }),
    ("info", StdSig { params: LOG_MSG_PARAMS, ret: None, doc: "Emit an info-level log message with optional structured fields." }),
    ("warn", StdSig { params: LOG_MSG_PARAMS, ret: None, doc: "Emit a warn-level log message with optional structured fields." }),
    ("error", StdSig { params: LOG_MSG_PARAMS, ret: None, doc: "Emit an error-level log message with optional structured fields." }),
    ("setLevel", StdSig { params: LOG_SET_LEVEL_PARAMS, ret: None, doc: "Set the minimum log level (debug/info/warn/error); messages below it are suppressed." }),
    ("setFormat", StdSig { params: LOG_SET_FORMAT_PARAMS, ret: None, doc: "Set the log output format ('text' or 'json')." }),
];

static LOG_MEMBERS: &[(&str, MemberKind)] = &[
    ("debug", MemberKind::Fn),
    ("info", MemberKind::Fn),
    ("warn", MemberKind::Fn),
    ("error", MemberKind::Fn),
    ("setLevel", MemberKind::Fn),
    ("setFormat", MemberKind::Fn),
];

// ── std/workflow ──────────────────────────────────────────────────────────────

static WORKFLOW_ACTIVITY_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("f", "fn()"),
];
static WORKFLOW_RUN_PARAMS: &[StdParam] = &[
    StdParam::req("wf", "fn(ctx, input)"),
    StdParam::req_untyped("input"),
    StdParam::opt("opts", "object"),
];
static WORKFLOW_RESUME_PARAMS: &[StdParam] = &[
    StdParam::req("wf", "fn(ctx, input)"),
    StdParam::req_untyped("input"),
    StdParam::opt("opts", "object"),
];

static WORKFLOW_SIGS: &[(&str, StdSig)] = &[
    ("activity", StdSig { params: WORKFLOW_ACTIVITY_PARAMS, ret: Some("object"), doc: "Define a named durable activity descriptor that a workflow can execute." }),
    ("run", StdSig { params: WORKFLOW_RUN_PARAMS, ret: Some("[value, err]"), doc: "Run a workflow function in Record mode, persisting events to a durable log. Async." }),
    ("resume", StdSig { params: WORKFLOW_RESUME_PARAMS, ret: Some("[value, err]"), doc: "Resume a previously started workflow from its durable log, replaying past events. Async." }),
];

static WORKFLOW_MEMBERS: &[(&str, MemberKind)] = &[
    ("activity", MemberKind::Fn),
    ("run", MemberKind::Fn),
    ("resume", MemberKind::Fn),
];

// ── std/telemetry ─────────────────────────────────────────────────────────────

static TELEMETRY_INIT_PARAMS: &[StdParam] = &[StdParam::req("config", "object")];
static TELEMETRY_EXPORTER_PARAMS: &[StdParam] = &[StdParam::req("config", "object")];
static TELEMETRY_ZERO_PARAMS: &[StdParam] = &[];
static TELEMETRY_SPAN_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::opt("opts", "object"),
];
static TELEMETRY_SCOPED_SPAN_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("f", "fn()"),
];
static TELEMETRY_INSTRUMENT_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::opt("opts", "object"),
];
static TELEMETRY_CAPTURE_PARAMS: &[StdParam] = &[
    StdParam::req("event", "string"),
    StdParam::opt("opts", "object"),
];
static TELEMETRY_IDENTIFY_PARAMS: &[StdParam] = &[
    StdParam::req("distinctId", "string"),
    StdParam::opt("props", "object"),
];

static TELEMETRY_SIGS: &[(&str, StdSig)] = &[
    ("init", StdSig { params: TELEMETRY_INIT_PARAMS, ret: Some("[nil, err]"), doc: "Initialize telemetry with a configuration object specifying exporters and service metadata. Async." }),
    ("otlp", StdSig { params: TELEMETRY_EXPORTER_PARAMS, ret: Some("object"), doc: "Build an OTLP exporter descriptor for use with telemetry.init." }),
    ("sentry", StdSig { params: TELEMETRY_EXPORTER_PARAMS, ret: Some("object"), doc: "Build a Sentry exporter descriptor for use with telemetry.init." }),
    ("posthog", StdSig { params: TELEMETRY_EXPORTER_PARAMS, ret: Some("object"), doc: "Build a PostHog analytics exporter descriptor for use with telemetry.init." }),
    ("flush", StdSig { params: TELEMETRY_ZERO_PARAMS, ret: Some("[nil, err]"), doc: "Force-export all buffered telemetry signals to the configured exporters. Async." }),
    ("shutdown", StdSig { params: TELEMETRY_ZERO_PARAMS, ret: Some("[nil, err]"), doc: "Flush and shut down the telemetry pipeline. Async." }),
    ("startSpan", StdSig { params: TELEMETRY_SPAN_PARAMS, ret: None, doc: "Open a distributed tracing span and return a handle; caller must close it." }),
    ("span", StdSig { params: TELEMETRY_SCOPED_SPAN_PARAMS, ret: Some("future<[value, err]>"), doc: "Open a span, run a scoped async callback, close the span on completion. Async." }),
    ("counter", StdSig { params: TELEMETRY_INSTRUMENT_PARAMS, ret: None, doc: "Create a monotonic counter metric instrument." }),
    ("histogram", StdSig { params: TELEMETRY_INSTRUMENT_PARAMS, ret: None, doc: "Create a histogram metric instrument for recording distributions." }),
    ("gauge", StdSig { params: TELEMETRY_INSTRUMENT_PARAMS, ret: None, doc: "Create a gauge metric instrument for recording point-in-time values." }),
    ("capture", StdSig { params: TELEMETRY_CAPTURE_PARAMS, ret: None, doc: "Emit a named analytics event to the configured analytics exporter." }),
    ("identify", StdSig { params: TELEMETRY_IDENTIFY_PARAMS, ret: None, doc: "Associate a user identity with a distinct ID and optional properties." }),
];

static TELEMETRY_MEMBERS: &[(&str, MemberKind)] = &[
    ("init", MemberKind::Fn),
    ("otlp", MemberKind::Fn),
    ("sentry", MemberKind::Fn),
    ("posthog", MemberKind::Fn),
    ("flush", MemberKind::Fn),
    ("shutdown", MemberKind::Fn),
    ("startSpan", MemberKind::Fn),
    ("span", MemberKind::Fn),
    ("counter", MemberKind::Fn),
    ("histogram", MemberKind::Fn),
    ("gauge", MemberKind::Fn),
    ("capture", MemberKind::Fn),
    ("identify", MemberKind::Fn),
];

// ── std/tui ───────────────────────────────────────────────────────────────────

static TUI_INIT_PARAMS: &[StdParam] = &[];
static TUI_BUFFER_PARAMS: &[StdParam] = &[
    StdParam::req("width", "int"),
    StdParam::req("height", "int"),
];

static TUI_SIGS: &[(&str, StdSig)] = &[
    ("init", StdSig { params: TUI_INIT_PARAMS, ret: Some("[term, err]"), doc: "Initialize the terminal for full-screen TUI rendering, returning a terminal handle." }),
    ("buffer", StdSig { params: TUI_BUFFER_PARAMS, ret: Some("buffer"), doc: "Create an off-screen character buffer of width × height cells." }),
];

static TUI_MEMBERS: &[(&str, MemberKind)] = &[
    ("init", MemberKind::Fn),
    ("buffer", MemberKind::Fn),
];

// ── std/ffi ───────────────────────────────────────────────────────────────────

static FFI_OPEN_PARAMS: &[StdParam] = &[StdParam::req("path", "string")];
static FFI_STRUCT_PARAMS: &[StdParam] = &[StdParam::req("fields", "array")];
static FFI_CSTR_PARAMS: &[StdParam] = &[StdParam::req("s", "string")];
static FFI_READ_CSTR_PARAMS: &[StdParam] = &[StdParam::req_untyped("ptr")];
static FFI_ALLOC_PARAMS: &[StdParam] = &[StdParam::req("layout", "object")];
static FFI_GET_PARAMS: &[StdParam] = &[
    StdParam::req("layout", "object"),
    StdParam::req("buf", "bytes"),
    StdParam::req("name", "string"),
];
static FFI_SET_PARAMS: &[StdParam] = &[
    StdParam::req("layout", "object"),
    StdParam::req("buf", "bytes"),
    StdParam::req("name", "string"),
    StdParam::req_untyped("value"),
];

// Handle-method sigs for `lib.symbol(name, argtypes, rettype)` and
// `sym.call(args)` — NOT module exports; added so the derivation in
// std_arity.rs can compute their min-arity via std_sig (SIG §2.5).
static FFI_SYMBOL_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("argtypes", "array"),
    StdParam::req("rettype", "object"),
];
static FFI_CALL_PARAMS: &[StdParam] = &[StdParam::req("args", "array")];

static FFI_SIGS: &[(&str, StdSig)] = &[
    ("open", StdSig { params: FFI_OPEN_PARAMS, ret: Some("[lib, err]"), doc: "Load a native shared library from the filesystem path and return a library handle." }),
    ("struct", StdSig { params: FFI_STRUCT_PARAMS, ret: Some("object"), doc: "Build a C struct layout descriptor from an array of [name, ffi.<type>] field pairs." }),
    ("cstr", StdSig { params: FFI_CSTR_PARAMS, ret: Some("bytes"), doc: "Convert a string to a NUL-terminated C string in a bytes buffer." }),
    ("read_cstr", StdSig { params: FFI_READ_CSTR_PARAMS, ret: Some("string"), doc: "Read a NUL-terminated C string from a bytes buffer or foreign pointer." }),
    ("alloc", StdSig { params: FFI_ALLOC_PARAMS, ret: Some("bytes"), doc: "Allocate a zero-filled bytes buffer sized to hold a C struct described by the layout." }),
    ("get", StdSig { params: FFI_GET_PARAMS, ret: None, doc: "Read a named field from a struct buffer using its layout descriptor." }),
    ("set", StdSig { params: FFI_SET_PARAMS, ret: None, doc: "Write a value into a named field of a struct buffer using its layout descriptor." }),
    // Handle methods — included for std_arity derivation (SIG §2.5); skipped by
    // the export cross-check (MemberKind::HandleMethod in FFI_MEMBERS).
    ("symbol", StdSig { params: FFI_SYMBOL_PARAMS, ret: Some("[sym, err]"), doc: "Resolve a symbol from an open foreign library handle by name, binding its argument and return types." }),
    ("call", StdSig { params: FFI_CALL_PARAMS, ret: None, doc: "Invoke the bound foreign symbol through the libffi trampoline, marshalling the args array." }),
];

static FFI_MEMBERS: &[(&str, MemberKind)] = &[
    ("i8", MemberKind::Const("object")),
    ("i16", MemberKind::Const("object")),
    ("i32", MemberKind::Const("object")),
    ("i64", MemberKind::Const("object")),
    ("u8", MemberKind::Const("object")),
    ("u16", MemberKind::Const("object")),
    ("u32", MemberKind::Const("object")),
    ("u64", MemberKind::Const("object")),
    ("size", MemberKind::Const("object")),
    ("f32", MemberKind::Const("object")),
    ("f64", MemberKind::Const("object")),
    ("ptr", MemberKind::Const("object")),
    ("void", MemberKind::Const("object")),
    ("open", MemberKind::Fn),
    ("struct", MemberKind::Fn),
    ("cstr", MemberKind::Fn),
    ("read_cstr", MemberKind::Fn),
    ("alloc", MemberKind::Fn),
    ("get", MemberKind::Fn),
    ("set", MemberKind::Fn),
    // Handle methods on ForeignLib / ForeignSymbol handles — not module exports.
    ("symbol", MemberKind::HandleMethod),
    ("call", MemberKind::HandleMethod),
];

// ── std/resilience ────────────────────────────────────────────────────────────

static RESIL_OPTS_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static RESIL_FALLBACK_PARAMS: &[StdParam] = &[
    StdParam::req("f", "fn()"),
    StdParam::req_untyped("fallback"),
];
static RESIL_SINGLEFLIGHT_PARAMS: &[StdParam] = &[
    StdParam::req("key", "string"),
    StdParam::req("f", "fn()"),
];
static RESIL_DEADLINE_PARAMS: &[StdParam] = &[
    StdParam::req("ms", "number"),
    StdParam::req("f", "fn()"),
];
static RESIL_DEADLINE_REMAINING_PARAMS: &[StdParam] = &[];
static RESIL_WITH_TRACE_PARAMS: &[StdParam] = &[
    StdParam::req("id", "string"),
    StdParam::req("f", "fn()"),
];
static RESIL_TRACE_ID_PARAMS: &[StdParam] = &[];
static RESIL_METRICS_HANDLER_PARAMS: &[StdParam] = &[];
static RESIL_HEALTH_PARAMS: &[StdParam] = &[StdParam::req("config", "object")];
static RESIL_HANDLER_PARAMS: &[StdParam] = &[
    StdParam::req("policies", "object"),
    StdParam::req("f", "fn(req)"),
];

static RESIL_SIGS: &[(&str, StdSig)] = &[
    ("breaker", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a circuit-breaker policy that trips after a configurable failure threshold." }),
    ("limiter", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a rate-limiter policy using a token-bucket algorithm." }),
    ("keyedLimiter", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a per-key rate-limiter policy backed by an LRU cache of token buckets." }),
    ("bulkhead", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a bulkhead policy that limits the number of concurrent in-flight calls." }),
    ("retry", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a retry policy with configurable attempts, back-off, and jitter." }),
    ("fallback", StdSig { params: RESIL_FALLBACK_PARAMS, ret: Some("future<[value, err]>"), doc: "Call f and, on failure, return the fallback value instead. Async." }),
    ("singleflight", StdSig { params: RESIL_SINGLEFLIGHT_PARAMS, ret: Some("future"), doc: "Deduplicate concurrent calls for the same key so only one runs at a time. Async." }),
    ("memoize", StdSig { params: RESIL_OPTS_PARAMS, ret: Some("object"), doc: "Create a memoize policy that caches function results by their arguments." }),
    ("deadline", StdSig { params: RESIL_DEADLINE_PARAMS, ret: Some("future<[value, err]>"), doc: "Run f with a deadline of ms milliseconds; return [nil, err] if the deadline expires. Async." }),
    ("deadlineRemaining", StdSig { params: RESIL_DEADLINE_REMAINING_PARAMS, ret: Some("float?"), doc: "Return the milliseconds remaining on the current task's deadline, or nil if no deadline is set." }),
    ("withTrace", StdSig { params: RESIL_WITH_TRACE_PARAMS, ret: Some("future"), doc: "Run f with a trace ID set in task-local context; child tasks inherit it. Async." }),
    ("traceId", StdSig { params: RESIL_TRACE_ID_PARAMS, ret: Some("string?"), doc: "Return the current task's trace ID, or nil if none is set." }),
    ("metricsHandler", StdSig { params: RESIL_METRICS_HANDLER_PARAMS, ret: None, doc: "Return an HTTP handler that serves per-isolate resilience metrics in Prometheus text format." }),
    ("health", StdSig { params: RESIL_HEALTH_PARAMS, ret: None, doc: "Return an HTTP readiness/liveness handler that runs configured health checks." }),
    ("handler", StdSig { params: RESIL_HANDLER_PARAMS, ret: None, doc: "Wrap an HTTP handler function with a set of resilience policies (retry, breaker, etc.)." }),
];

static RESIL_MEMBERS: &[(&str, MemberKind)] = &[
    ("breaker", MemberKind::Fn),
    ("limiter", MemberKind::Fn),
    ("keyedLimiter", MemberKind::Fn),
    ("bulkhead", MemberKind::Fn),
    ("retry", MemberKind::Fn),
    ("fallback", MemberKind::Fn),
    ("singleflight", MemberKind::Fn),
    ("memoize", MemberKind::Fn),
    ("deadline", MemberKind::Fn),
    ("deadlineRemaining", MemberKind::Fn),
    ("withTrace", MemberKind::Fn),
    ("traceId", MemberKind::Fn),
    ("metricsHandler", MemberKind::Fn),
    ("health", MemberKind::Fn),
    ("handler", MemberKind::Fn),
];

// ── std/jwt (BATT §5) ─────────────────────────────────────────────────────────

static JWT_HMAC_KEY_PARAMS: &[StdParam] = &[StdParam::req("secret", "string | bytes")];
static JWT_PEM_KEY_PARAMS: &[StdParam] = &[StdParam::req("pem", "string")];
static JWT_SIGN_PARAMS: &[StdParam] = &[
    StdParam::req("claims", "object"),
    StdParam::req_untyped("key"),
    StdParam::opt("opts", "object"),
];
static JWT_VERIFY_PARAMS: &[StdParam] = &[
    StdParam::req("token", "string"),
    StdParam::req_untyped("key"),
    StdParam::opt("opts", "object"),
];
static JWT_DECODE_PARAMS: &[StdParam] = &[StdParam::req("token", "string")];
static JWT_JWKS_PARAMS: &[StdParam] = &[
    StdParam::req("url", "string"),
    StdParam::opt("opts", "object"),
];
// JwksCache handle methods (BATT §5.6, MemberKind::HandleMethod in JWT_MEMBERS).
// The handle `verify` shares the name (and arity row) of the module `jwt.verify`.
static JWT_JWKS_KEYS_PARAMS: &[StdParam] = &[];
static JWT_JWKS_CLOSE_PARAMS: &[StdParam] = &[];

static JWT_SIGS: &[(&str, StdSig)] = &[
    ("hmacKey", StdSig { params: JWT_HMAC_KEY_PARAMS, ret: Some("object"), doc: "Builds a typed HMAC key for HS256/HS384/HS512." }),
    ("rsaPublicKey", StdSig { params: JWT_PEM_KEY_PARAMS, ret: Some("object"), doc: "Builds a typed RSA public key (RS256) from a PEM string, validated at construction." }),
    ("rsaPrivateKey", StdSig { params: JWT_PEM_KEY_PARAMS, ret: Some("object"), doc: "Builds a typed RSA private key (RS256) from a PEM string, validated at construction." }),
    ("ecPublicKey", StdSig { params: JWT_PEM_KEY_PARAMS, ret: Some("object"), doc: "Builds a typed EC (P-256) public key (ES256) from a PEM string, validated at construction." }),
    ("ecPrivateKey", StdSig { params: JWT_PEM_KEY_PARAMS, ret: Some("object"), doc: "Builds a typed EC (P-256) private key (ES256) from a PEM string, validated at construction." }),
    ("sign", StdSig { params: JWT_SIGN_PARAMS, ret: Some("[string, err]"), doc: "Signs claims into a compact JWT with a typed key." }),
    ("verify", StdSig { params: JWT_VERIFY_PARAMS, ret: Some("[object, err]"), doc: "Verifies a JWT against a typed key, intersecting the header alg with the key kind's algorithm set." }),
    ("decode", StdSig { params: JWT_DECODE_PARAMS, ret: Some("[object, err]"), doc: "Decodes a JWT WITHOUT verifying its signature (inspection only)." }),
    ("jwks", StdSig { params: JWT_JWKS_PARAMS, ret: Some("[jwksCache, err]"), doc: "Fetches a JWK Set over the network and returns a cache handle whose verify() resolves a token's kid to the matching key. Async; Net-gated." }),
    // jwksCache handle methods (BATT §5.6). `verify` shares the name of jwt.verify
    // (the Fn row above provides its arity); `keys`/`close` get their own rows.
    ("keys", StdSig { params: JWT_JWKS_KEYS_PARAMS, ret: Some("array<string>"), doc: "Return the kids currently cached by a jwksCache handle." }),
    ("close", StdSig { params: JWT_JWKS_CLOSE_PARAMS, ret: None, doc: "Drop a jwksCache handle's cached keys." }),
];

static JWT_MEMBERS: &[(&str, MemberKind)] = &[
    ("hmacKey", MemberKind::Fn),
    ("rsaPublicKey", MemberKind::Fn),
    ("rsaPrivateKey", MemberKind::Fn),
    ("ecPublicKey", MemberKind::Fn),
    ("ecPrivateKey", MemberKind::Fn),
    ("sign", MemberKind::Fn),
    ("verify", MemberKind::Fn),
    ("decode", MemberKind::Fn),
    ("jwks", MemberKind::Fn),
    // jwksCache handle methods — not module exports.
    ("verify", MemberKind::HandleMethod),
    ("keys", MemberKind::HandleMethod),
    ("close", MemberKind::HandleMethod),
];

// ── std/oauth (BATT §5.6) ──────────────────────────────────────────────────────

static OAUTH_PKCE_PARAMS: &[StdParam] = &[];
static OAUTH_EXCHANGE_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static OAUTH_CLIENT_CREDS_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static OAUTH_REFRESH_PARAMS: &[StdParam] = &[StdParam::req("opts", "object")];
static OAUTH_DISCOVER_PARAMS: &[StdParam] = &[StdParam::req("issuer", "string")];

static OAUTH_SIGS: &[(&str, StdSig)] = &[
    ("pkce", StdSig { params: OAUTH_PKCE_PARAMS, ret: Some("object"), doc: "Generate a PKCE verifier/challenge pair (RFC 7636 S256)." }),
    ("exchangeCode", StdSig { params: OAUTH_EXCHANGE_PARAMS, ret: Some("[object, err]"), doc: "Exchange an authorization code (with a PKCE code_verifier) for tokens. Async; Net-gated." }),
    ("clientCredentials", StdSig { params: OAUTH_CLIENT_CREDS_PARAMS, ret: Some("[object, err]"), doc: "Obtain tokens via the client_credentials grant (Basic auth). Async; Net-gated." }),
    ("refresh", StdSig { params: OAUTH_REFRESH_PARAMS, ret: Some("[object, err]"), doc: "Exchange a refresh token for a fresh access token. Async; Net-gated." }),
    ("discover", StdSig { params: OAUTH_DISCOVER_PARAMS, ret: Some("[object, err]"), doc: "Fetch an issuer's OpenID Connect discovery metadata (.well-known/openid-configuration). Async; Net-gated." }),
];

static OAUTH_MEMBERS: &[(&str, MemberKind)] = &[
    ("pkce", MemberKind::Fn),
    ("exchangeCode", MemberKind::Fn),
    ("clientCredentials", MemberKind::Fn),
    ("refresh", MemberKind::Fn),
    ("discover", MemberKind::Fn),
];

// ─────────────────────────────────────────────────────────────────────────────
// Master index (batch 1 + batch 2 + batch 3 — 60 modules total)
// ─────────────────────────────────────────────────────────────────────────────

static ALL_MODULES: &[(&str, &[(&str, MemberKind)])] = &[
    ("std/math", MATH_MEMBERS),
    ("std/string", STRING_MEMBERS),
    ("std/array", ARRAY_MEMBERS),
    ("std/object", OBJECT_MEMBERS),
    ("std/map", MAP_MEMBERS),
    ("std/set", SET_MEMBERS),
    ("std/bytes", BYTES_MEMBERS),
    ("std/convert", CONVERT_MEMBERS),
    ("std/decimal", DECIMAL_MEMBERS),
    ("std/json", JSON_MEMBERS),
    ("std/csv", CSV_MEMBERS),
    ("std/regex", REGEX_MEMBERS),
    ("std/encoding", ENCODING_MEMBERS),
    ("std/toml", TOML_MEMBERS),
    ("std/yaml", YAML_MEMBERS),
    ("std/url", URL_MEMBERS),
    ("std/uuid", UUID_MEMBERS),
    ("std/msgpack", MSGPACK_MEMBERS),
    ("std/cbor", CBOR_MEMBERS),
    // batch 2 — system + net + db + docker
    ("std/fs", FS_MEMBERS),
    ("std/env", ENV_MEMBERS),
    ("std/io", IO_MEMBERS),
    ("std/process", PROCESS_MEMBERS),
    ("std/os", OS_MEMBERS),
    ("std/crypto", CRYPTO_MEMBERS),
    ("std/compress", COMPRESS_MEMBERS),
    ("std/net", NET_MEMBERS),
    ("std/net/tcp", NET_TCP_MEMBERS),
    ("std/net/udp", NET_UDP_MEMBERS),
    ("std/net/unix", NET_UNIX_MEMBERS),
    ("std/net/ws", NET_WS_MEMBERS),
    ("std/net/http", NET_HTTP_MEMBERS),
    ("std/http/server", HTTP_SERVER_MEMBERS),
    ("std/sqlite", SQLITE_MEMBERS),
    ("std/postgres", POSTGRES_MEMBERS),
    ("std/redis", REDIS_MEMBERS),
    ("std/docker", DOCKER_MEMBERS),
    // batch 3 — ai + assert + bench + cli + color + schema + shared + lru + events + template +
    //           caps + task + time + sync + stream + date + intl + log + workflow + telemetry +
    //           tui + ffi + resilience
    ("std/ai", AI_MEMBERS),
    ("std/assert", ASSERT_MEMBERS),
    ("std/bench", BENCH_MEMBERS),
    ("std/cli", CLI_MEMBERS),
    ("std/color", COLOR_MEMBERS),
    ("std/schema", SCHEMA_MEMBERS),
    ("std/shared", SHARED_MEMBERS),
    ("std/lru", LRU_MEMBERS),
    ("std/events", EVENTS_MEMBERS),
    ("std/template", TEMPLATE_MEMBERS),
    ("std/caps", CAPS_MEMBERS),
    ("std/task", TASK_MEMBERS),
    ("std/time", TIME_MEMBERS),
    ("std/sync", SYNC_MEMBERS),
    ("std/stream", STREAM_MEMBERS),
    ("std/date", DATE_MEMBERS),
    ("std/intl", INTL_MEMBERS),
    ("std/log", LOG_MEMBERS),
    ("std/workflow", WORKFLOW_MEMBERS),
    ("std/telemetry", TELEMETRY_MEMBERS),
    ("std/tui", TUI_MEMBERS),
    ("std/ffi", FFI_MEMBERS),
    ("std/resilience", RESIL_MEMBERS),
    ("std/jwt", JWT_MEMBERS),
    ("std/oauth", OAUTH_MEMBERS),
    ("std/archive", ARCHIVE_MEMBERS),
];

// ─────────────────────────────────────────────────────────────────────────────
// Public lookup API (spec §2.1)
// ─────────────────────────────────────────────────────────────────────────────

/// Look up the curated signature for a `std/*` function.
/// Returns `None` for non-implemented modules, constants, or unknown names.
pub fn std_sig(module: &str, name: &str) -> Option<&'static StdSig> {
    let sigs: &[(&str, StdSig)] = match module {
        "std/math" => MATH_SIGS,
        "std/string" => STRING_SIGS,
        "std/array" => ARRAY_SIGS,
        "std/object" => OBJECT_SIGS,
        "std/map" => MAP_SIGS,
        "std/set" => SET_SIGS,
        "std/bytes" => BYTES_SIGS,
        "std/convert" => CONVERT_SIGS,
        "std/decimal" => DECIMAL_SIGS,
        "std/json" => JSON_SIGS,
        "std/csv" => CSV_SIGS,
        "std/regex" => REGEX_SIGS,
        "std/encoding" => ENCODING_SIGS,
        "std/toml" => TOML_SIGS,
        "std/yaml" => YAML_SIGS,
        "std/url" => URL_SIGS,
        "std/uuid" => UUID_SIGS,
        "std/msgpack" => MSGPACK_SIGS,
        "std/cbor" => CBOR_SIGS,
        // batch 2
        "std/fs" => FS_SIGS,
        "std/env" => ENV_SIGS,
        "std/io" => IO_SIGS,
        "std/process" => PROCESS_SIGS,
        "std/os" => OS_SIGS,
        "std/crypto" => CRYPTO_SIGS,
        "std/compress" => COMPRESS_SIGS,
        "std/net" => NET_SIGS,
        "std/net/tcp" => NET_TCP_SIGS,
        "std/net/udp" => NET_UDP_SIGS,
        "std/net/unix" => NET_UNIX_SIGS,
        "std/net/ws" => NET_WS_SIGS,
        "std/net/http" => NET_HTTP_SIGS,
        "std/http/server" => HTTP_SERVER_SIGS,
        "std/sqlite" => SQLITE_SIGS,
        "std/postgres" => POSTGRES_SIGS,
        "std/redis" => REDIS_SIGS,
        "std/docker" => DOCKER_SIGS,
        // batch 3
        "std/ai" => AI_SIGS,
        "std/assert" => ASSERT_SIGS,
        "std/bench" => BENCH_SIGS,
        "std/cli" => CLI_SIGS,
        "std/color" => COLOR_SIGS,
        "std/schema" => SCHEMA_SIGS,
        "std/shared" => SHARED_SIGS,
        "std/lru" => LRU_SIGS,
        "std/events" => EVENTS_SIGS,
        "std/template" => TEMPLATE_SIGS,
        "std/caps" => CAPS_SIGS,
        "std/task" => TASK_SIGS,
        "std/time" => TIME_SIGS,
        "std/sync" => SYNC_SIGS,
        "std/stream" => STREAM_SIGS,
        "std/date" => DATE_SIGS,
        "std/intl" => INTL_SIGS,
        "std/log" => LOG_SIGS,
        "std/workflow" => WORKFLOW_SIGS,
        "std/telemetry" => TELEMETRY_SIGS,
        "std/tui" => TUI_SIGS,
        "std/ffi" => FFI_SIGS,
        "std/resilience" => RESIL_SIGS,
        "std/jwt" => JWT_SIGS,
        "std/oauth" => OAUTH_SIGS,
        "std/archive" => ARCHIVE_SIGS,
        _ => return None,
    };
    sigs.iter().find(|(n, _)| *n == name).map(|(_, s)| s)
}

// ── Global builtins ───────────────────────────────────────────────────────────

static BUILTIN_PRINT_PARAMS: &[StdParam] = &[StdParam::variadic("values", "any")];
static BUILTIN_LEN_PARAMS: &[StdParam] = &[StdParam::req("value", "any")];
static BUILTIN_TYPE_PARAMS: &[StdParam] = &[StdParam::req("value", "any")];
static BUILTIN_ASSERT_PARAMS: &[StdParam] = &[
    StdParam::req("cond", "bool"),
    StdParam::opt("message", "string"),
];
static BUILTIN_RANGE_PARAMS: &[StdParam] = &[
    StdParam::req("start_or_end", "number"),
    StdParam::opt("end", "number"),
    StdParam::opt("step", "number"),
];
static BUILTIN_OK_PARAMS: &[StdParam] = &[StdParam::opt("value", "any")];
static BUILTIN_ERR_PARAMS: &[StdParam] = &[StdParam::opt("error", "any")];
static BUILTIN_RECOVER_PARAMS: &[StdParam] = &[StdParam::req("f", "fn()")];
static BUILTIN_TEST_PARAMS: &[StdParam] = &[
    StdParam::req("name", "string"),
    StdParam::req("f", "fn()"),
];
static BUILTIN_EXIT_PARAMS: &[StdParam] = &[StdParam::opt("code", "int")];

static BUILTIN_SIGS: &[(&str, StdSig)] = &[
    ("print",   StdSig { params: BUILTIN_PRINT_PARAMS,  ret: None,           doc: "Print values to stdout, separated by spaces." }),
    ("len",     StdSig { params: BUILTIN_LEN_PARAMS,    ret: Some("int"),    doc: "Return the length of a string, array, map, set, object, or bytes value." }),
    ("type",    StdSig { params: BUILTIN_TYPE_PARAMS,   ret: Some("string"), doc: "Return the runtime type name of a value as a string." }),
    ("assert",  StdSig { params: BUILTIN_ASSERT_PARAMS, ret: None,           doc: "Panic with a Tier-2 error if cond is falsy; optional message overrides the default." }),
    ("range",   StdSig { params: BUILTIN_RANGE_PARAMS,  ret: Some("array<int>"), doc: "Return an integer array. range(end), range(start,end), or range(start,end,step)." }),
    ("Ok",      StdSig { params: BUILTIN_OK_PARAMS,     ret: Some("[value, nil]"), doc: "Construct a successful Tier-1 result pair [value, nil]." }),
    ("Err",     StdSig { params: BUILTIN_ERR_PARAMS,    ret: Some("[nil, err]"),   doc: "Construct a Tier-1 error pair [nil, error]." }),
    ("recover", StdSig { params: BUILTIN_RECOVER_PARAMS, ret: None,          doc: "Call f, catching any Tier-2 panic as a Tier-1 [nil, err] result instead of aborting." }),
    ("test",    StdSig { params: BUILTIN_TEST_PARAMS,   ret: None,           doc: "Register a named test case. Collected and run by `ascript test`." }),
    ("exit",    StdSig { params: BUILTIN_EXIT_PARAMS,   ret: None,           doc: "Terminate the process immediately with the given exit code (0–255; default 0)." }),
];

/// Look up the curated signature for a global builtin function.
pub fn builtin_sig(name: &str) -> Option<&'static StdSig> {
    BUILTIN_SIGS.iter().find(|(n, _)| *n == name).map(|(_, s)| s)
}

/// The member list (name → kind) for a module, or `None` if not implemented.
pub fn module_members(module: &str) -> Option<&'static [(&'static str, MemberKind)]> {
    ALL_MODULES.iter().find(|(m, _)| *m == module).map(|(_, members)| *members)
}

/// Iterator over all implemented (module, members) pairs.
/// Used by the reverse drift test to ensure every table row is a real export.
/// Also consumed by LSP consumers (Task 1.2+).
#[allow(dead_code)]
pub(crate) fn all_modules() -> &'static [(&'static str, &'static [(&'static str, MemberKind)])] {
    ALL_MODULES
}

/// Render a `StdSig` into a concise `(params...) -> ret` detail string suitable
/// for the LSP `CompletionItem.detail` field.  Purely formatting — no allocation
/// at lookup time when the result is cached by the caller.
/// Render ONE param for display: `...`(variadic) + name + `?`(optional) + `: ty`.
/// The single per-param renderer shared by completion detail (`render_sig_detail`)
/// and signature-help labels (`signature.rs::format_std_param`) so the two
/// consumers never diverge on optionality/variadic notation.
pub fn render_param(p: &StdParam) -> String {
    let mut s = String::new();
    if p.variadic {
        s.push_str("...");
    }
    s.push_str(p.name);
    if p.optional {
        s.push('?');
    }
    if let Some(ty) = p.ty {
        s.push_str(": ");
        s.push_str(ty);
    }
    s
}

pub fn render_sig_detail(sig: &StdSig) -> String {
    let mut s = String::from("(");
    for (i, p) in sig.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&render_param(p));
    }
    s.push(')');
    if let Some(ret) = sig.ret {
        s.push_str(" -> ");
        s.push_str(ret);
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Validate that no required param follows an optional/variadic one in any sig.
    /// This is the optional-ordering guard for the authoring invariant.
    #[test]
    fn table_ordering_invariant() {
        let all_sigs: &[(&str, &[(&str, StdSig)])] = &[
            ("std/math", MATH_SIGS),
            ("std/string", STRING_SIGS),
            ("std/array", ARRAY_SIGS),
            ("std/object", OBJECT_SIGS),
            ("std/map", MAP_SIGS),
            ("std/set", SET_SIGS),
            ("std/bytes", BYTES_SIGS),
            ("std/convert", CONVERT_SIGS),
            ("std/decimal", DECIMAL_SIGS),
            ("std/json", JSON_SIGS),
            ("std/csv", CSV_SIGS),
            ("std/regex", REGEX_SIGS),
            ("std/encoding", ENCODING_SIGS),
            ("std/toml", TOML_SIGS),
            ("std/yaml", YAML_SIGS),
            ("std/url", URL_SIGS),
            ("std/uuid", UUID_SIGS),
            ("std/msgpack", MSGPACK_SIGS),
            ("std/cbor", CBOR_SIGS),
            // batch 2
            ("std/fs", FS_SIGS),
            ("std/env", ENV_SIGS),
            ("std/io", IO_SIGS),
            ("std/process", PROCESS_SIGS),
            ("std/os", OS_SIGS),
            ("std/crypto", CRYPTO_SIGS),
            ("std/compress", COMPRESS_SIGS),
            ("std/net", NET_SIGS),
            ("std/net/tcp", NET_TCP_SIGS),
            ("std/net/udp", NET_UDP_SIGS),
            ("std/net/unix", NET_UNIX_SIGS),
            ("std/net/ws", NET_WS_SIGS),
            ("std/net/http", NET_HTTP_SIGS),
            ("std/http/server", HTTP_SERVER_SIGS),
            ("std/sqlite", SQLITE_SIGS),
            ("std/postgres", POSTGRES_SIGS),
            ("std/redis", REDIS_SIGS),
            ("std/docker", DOCKER_SIGS),
            // batch 3
            ("std/ai", AI_SIGS),
            ("std/assert", ASSERT_SIGS),
            ("std/bench", BENCH_SIGS),
            ("std/cli", CLI_SIGS),
            ("std/color", COLOR_SIGS),
            ("std/schema", SCHEMA_SIGS),
            ("std/shared", SHARED_SIGS),
            ("std/lru", LRU_SIGS),
            ("std/events", EVENTS_SIGS),
            ("std/template", TEMPLATE_SIGS),
            ("std/caps", CAPS_SIGS),
            ("std/task", TASK_SIGS),
            ("std/time", TIME_SIGS),
            ("std/sync", SYNC_SIGS),
            ("std/stream", STREAM_SIGS),
            ("std/date", DATE_SIGS),
            ("std/intl", INTL_SIGS),
            ("std/log", LOG_SIGS),
            ("std/workflow", WORKFLOW_SIGS),
            ("std/telemetry", TELEMETRY_SIGS),
            ("std/tui", TUI_SIGS),
            ("std/ffi", FFI_SIGS),
            ("std/resilience", RESIL_SIGS),
            ("std/jwt", JWT_SIGS),
            ("std/oauth", OAUTH_SIGS),
            ("std/archive", ARCHIVE_SIGS),
        ];
        for (module, sigs) in all_sigs {
            for (name, sig) in sigs.iter() {
                let key = format!("{module}::{name}");
                validate_param_order(&key, sig.params).unwrap_or_else(|e| panic!("{e}"));
            }
        }
    }

    /// §2.3 drift (a), direction 1: every export of every buildable module has a
    /// table row, kind-consistent with the export's Value kind.
    #[test]
    fn every_export_has_a_table_row_with_consistent_kind() {
        for module in crate::stdlib::STD_MODULES {
            let Some(exports) = crate::stdlib::std_module_exports(module) else {
                continue; // feature-gated out of THIS build — covered by the other config
            };
            let members = module_members(module).unwrap_or_else(|| {
                panic!("std_sigs has no member list for {module}")
            });
            for (name, value) in &exports {
                let kind = members.iter().find(|(n, _)| *n == name).map(|(_, k)| k)
                    .unwrap_or_else(|| panic!("{module}::{name} export missing from std_sigs"));
                let is_fn = matches!(value.kind(), crate::value::ValueKind::Builtin(_));
                match kind {
                    MemberKind::Fn => {
                        assert!(is_fn, "{module}::{name}: table says Fn, export is a constant");
                        assert!(std_sig(module, name).is_some(), "{module}::{name}: MemberKind::Fn but no StdSig row");
                    }
                    MemberKind::Const(_) => {
                        assert!(!is_fn, "{module}::{name}: table says Const, export is a Builtin");
                    }
                    MemberKind::HandleMethod => {}
                }
            }
        }
    }

    /// §2.3 drift (a), direction 2: every table key is a real export (handle-method rows skipped).
    #[test]
    fn every_table_row_is_a_real_export() {
        for (module, members) in all_modules() {
            let Some(exports) = crate::stdlib::std_module_exports(module) else { continue; };
            for (name, kind) in *members {
                if matches!(kind, MemberKind::HandleMethod) { continue; }
                assert!(exports.iter().any(|(n, _)| n == name),
                    "std_sigs lists {module}::{name} but it is not an export");
            }
        }
    }

    /// Every global builtin in scope has a sig row, and key invariants hold.
    #[test]
    fn global_builtins_have_sigs() {
        for b in ["print", "len", "type", "assert", "range", "Ok", "Err", "recover", "test", "exit"] {
            assert!(builtin_sig(b).is_some(), "builtin '{b}' missing from BUILTIN_SIGS");
        }
        let len = builtin_sig("len").unwrap();
        assert_eq!(len.params.len(), 1, "len() must have exactly one param");
        assert_eq!(len.params[0].name, "value", "len()'s single param must be named 'value'");
    }
}
