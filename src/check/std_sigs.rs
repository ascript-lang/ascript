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

// ─────────────────────────────────────────────────────────────────────────────
// Master index (covers ONLY the three implemented modules for Task 1.1)
// ─────────────────────────────────────────────────────────────────────────────

static ALL_MODULES: &[(&str, &[(&str, MemberKind)])] = &[
    ("std/math", MATH_MEMBERS),
    ("std/string", STRING_MEMBERS),
    ("std/array", ARRAY_MEMBERS),
];

/// The three modules covered in Task 1.1.
/// Task 1.2 deletes this const and the `table_is_still_partial_pending_task_1_2` test
/// once ALL of STD_MODULES is filled.
pub const IMPLEMENTED_MODULES: &[&str] = &["std/math", "std/string", "std/array"];

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
