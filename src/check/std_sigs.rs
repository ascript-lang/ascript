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
//! `docs/content/stdlib/collections.md` prose paragraph.
//!
//! ## Coverage scope
//!
//! This file covers the three modules listed in `IMPLEMENTED_MODULES`.
//! Task 1.2 fills the rest of STD_MODULES and deletes the partial-coverage
//! marker below.
//!
// SIG Task 1.2 fills the remainder

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
static SET_FROM_PARAMS: &[StdParam] = &[StdParam::req("arr", "array")];
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
    StdParam::req("endian", "string"),
];
static BYTES_WRITE_UINT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("value", "number"),
    StdParam::req("n", "number"),
    StdParam::req("endian", "string"),
];
static BYTES_READ_INT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("n", "number"),
    StdParam::req("endian", "string"),
];
static BYTES_WRITE_INT_PARAMS: &[StdParam] = &[
    StdParam::req("b", "bytes"),
    StdParam::req("offset", "number"),
    StdParam::req("value", "number"),
    StdParam::req("n", "number"),
    StdParam::req("endian", "string"),
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
// SIG Task 1.2 fills the remainder
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

static NET_TCP_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: NET_TCP_CONNECT_PARAMS, ret: Some("[stream, err]"), doc: "Opens a client TCP connection. Async." }),
    ("listen", StdSig { params: NET_TCP_LISTEN_PARAMS, ret: Some("[listener, err]"), doc: "Binds a TCP listener. Async." }),
];

static NET_TCP_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
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

static HTTP_SERVER_SIGS: &[(&str, StdSig)] = &[
    ("create", StdSig { params: HTTP_SERVER_CREATE_PARAMS, ret: None, doc: "Creates and returns a new HTTP server handle." }),
    ("serve", StdSig { params: HTTP_SERVER_SERVE_PARAMS, ret: Some("[nil, err]"), doc: "Multi-isolate REUSEPORT serve: spreads the accept loop across N shared-nothing isolates. Async." }),
];

static HTTP_SERVER_MEMBERS: &[(&str, MemberKind)] = &[
    ("create", MemberKind::Fn),
    ("serve", MemberKind::Fn),
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

static DOCKER_SIGS: &[(&str, StdSig)] = &[
    ("connect", StdSig { params: DOCKER_CONNECT_PARAMS, ret: Some("[client, err]"), doc: "Connects to the local Docker Engine over its Unix-domain socket, negotiating the API version. Async." }),
];

static DOCKER_MEMBERS: &[(&str, MemberKind)] = &[
    ("connect", MemberKind::Fn),
];

// ─────────────────────────────────────────────────────────────────────────────
// Master index (batch 1 + batch 2 — 37 modules total)
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
];

/// Modules covered in Task 1.1 + Task 1.2 (batches 1 and 2).
/// Task 1.3 (final batch) deletes this const and the `table_is_still_partial_pending_task_1_2`
/// test once ALL of STD_MODULES is filled.
pub const IMPLEMENTED_MODULES: &[&str] = &[
    "std/math",
    "std/string",
    "std/array",
    "std/object",
    "std/map",
    "std/set",
    "std/bytes",
    "std/convert",
    "std/decimal",
    "std/json",
    "std/csv",
    "std/regex",
    "std/encoding",
    "std/toml",
    "std/yaml",
    "std/url",
    "std/uuid",
    "std/msgpack",
    "std/cbor",
    // batch 2
    "std/fs",
    "std/env",
    "std/io",
    "std/process",
    "std/os",
    "std/crypto",
    "std/compress",
    "std/net",
    "std/net/tcp",
    "std/net/udp",
    "std/net/unix",
    "std/net/ws",
    "std/net/http",
    "std/http/server",
    "std/sqlite",
    "std/postgres",
    "std/redis",
    "std/docker",
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
        _ => return None,
    };
    sigs.iter().find(|(n, _)| *n == name).map(|(_, s)| s)
}

/// Look up the curated signature for a global builtin function.
/// Returns `None` for everything in Task 1.1 — builtins are filled in Task 1.2.
pub fn builtin_sig(_name: &str) -> Option<&'static StdSig> {
    None
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
        ];
        for (module, sigs) in all_sigs {
            for (name, sig) in sigs.iter() {
                let key = format!("{module}::{name}");
                validate_param_order(&key, sig.params).unwrap_or_else(|e| panic!("{e}"));
            }
        }
    }

    /// §2.3 drift (a), direction 1: every export of every buildable module has a
    /// table row, kind-consistent with the export's Value kind. SCOPED to IMPLEMENTED_MODULES for now.
    #[test]
    fn every_export_has_a_table_row_with_consistent_kind() {
        for module in IMPLEMENTED_MODULES {
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

    /// While the table is partial (Task 1.1), IMPLEMENTED_MODULES must be a strict subset.
    /// Task 1.2 deletes IMPLEMENTED_MODULES + this test, flipping coverage to ALL of STD_MODULES.
    /// The `// SIG Task 1.2 fills the remainder` marker below MUST exist while this holds.
    #[test]
    fn table_is_still_partial_pending_task_1_2() {
        assert!(IMPLEMENTED_MODULES.len() < crate::stdlib::STD_MODULES.len(),
            "once full, delete IMPLEMENTED_MODULES + the marker and let the completeness test cover all modules (Task 1.2)");
    }
}
