//! `std/cli` — declarative command-line argument parser.
//!
//! `cli.parse(spec, args?) -> [result, err]` (Tier-1).
//!
//! `spec` is an AScript object describing the CLI; `args` defaults to
//! `env.args()` when omitted. This is a hand-written parser — no external
//! crate dependency. The module has no Cargo feature gate (arg parsing is
//! dependency-free and core).
//!
//! # Spec shape
//! ```text
//! {
//!   name: "mytool",
//!   flags:   [{ name: "verbose", short: "v", help: "..." }],
//!   options: [{ name: "output",  short: "o", default: "out", help: "..." }],
//!   positionals: [{ name: "input", required: true, help: "..." }],
//!   subcommands: [{ name: "build", flags: [...], options: [...], positionals: [...] }]
//! }
//! ```
//!
//! # Result shape (on success)
//! ```text
//! { flags: {verbose: true}, options: {output: "x"},
//!   positionals: {input: "f"},
//!   subcommand: nil | { name: "build", flags:{}, options:{}, positionals:{} },
//!   help: nil | "<usage text>" }
//! ```

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use indexmap::IndexMap;

// ── public exports ────────────────────────────────────────────────────────────

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("parse", bi("cli.parse"))]
}

// ── internal spec representation ──────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct FlagSpec {
    name: String,
    short: Option<String>,
    help: String,
}

#[derive(Debug, Clone, Default)]
struct OptionSpec {
    name: String,
    short: Option<String>,
    default: Option<String>,
    help: String,
}

#[derive(Debug, Clone, Default)]
struct PositionalSpec {
    name: String,
    required: bool,
    help: String,
}

#[derive(Debug, Clone, Default)]
struct CliSpec {
    name: String,
    flags: Vec<FlagSpec>,
    options: Vec<OptionSpec>,
    positionals: Vec<PositionalSpec>,
    subcommands: Vec<SubcommandSpec>,
}

#[derive(Debug, Clone, Default)]
struct SubcommandSpec {
    name: String,
    flags: Vec<FlagSpec>,
    options: Vec<OptionSpec>,
    positionals: Vec<PositionalSpec>,
}

// ── spec parsing (AScript Value → internal Rust structs) ─────────────────────

/// Extract a string field from an object. Returns `""` when absent/nil. Tier-2
/// panics if the field is present but not a string.
type ObjCell = gcmodule::Cc<crate::value::ObjectCell>;

fn str_field(obj: &ObjCell, key: &str, ctx: &str, span: Span) -> Result<String, Control> {
    let got = obj.get(key);
    match got.as_ref().map(|v| v.kind()) {
        None | Some(ValueKind::Nil) => Ok(String::new()),
        Some(ValueKind::Str(s)) => Ok(s.to_string()),
        Some(_) => Err(AsError::at(
            format!(
                "cli.parse spec: {}.{} must be a string, got {}",
                ctx,
                key,
                crate::interp::type_name(got.as_ref().unwrap())
            ),
            span,
        )
        .into()),
    }
}

/// Extract a bool field from an object. Returns `false` when absent/nil. Tier-2
/// panic if present but not a bool.
fn bool_field(obj: &ObjCell, key: &str, ctx: &str, span: Span) -> Result<bool, Control> {
    let got = obj.get(key);
    match got.as_ref().map(|v| v.kind()) {
        None | Some(ValueKind::Nil) => Ok(false),
        Some(ValueKind::Bool(b)) => Ok(b),
        Some(_) => Err(AsError::at(
            format!(
                "cli.parse spec: {}.{} must be a bool, got {}",
                ctx,
                key,
                crate::interp::type_name(got.as_ref().unwrap())
            ),
            span,
        )
        .into()),
    }
}

fn parse_flag_spec(v: &Value, span: Span) -> Result<FlagSpec, Control> {
    let obj = match v.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "cli.parse spec: flags entry must be an object, got {}",
                    crate::interp::type_name(v)
                ),
                span,
            )
            .into())
        }
    };
    let name = str_field(&obj, "name", "flag", span)?;
    if name.is_empty() {
        return Err(AsError::at(
            "cli.parse spec: flag entry must have a non-empty 'name'",
            span,
        )
        .into());
    }
    let short_raw = str_field(&obj, "short", "flag", span)?;
    let short = if short_raw.is_empty() {
        None
    } else {
        Some(short_raw)
    };
    let help = str_field(&obj, "help", "flag", span)?;
    Ok(FlagSpec { name, short, help })
}

fn parse_option_spec(v: &Value, span: Span) -> Result<OptionSpec, Control> {
    let obj = match v.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "cli.parse spec: options entry must be an object, got {}",
                    crate::interp::type_name(v)
                ),
                span,
            )
            .into())
        }
    };
    let name = str_field(&obj, "name", "option", span)?;
    if name.is_empty() {
        return Err(AsError::at(
            "cli.parse spec: option entry must have a non-empty 'name'",
            span,
        )
        .into());
    }
    let short_raw = str_field(&obj, "short", "option", span)?;
    let short = if short_raw.is_empty() {
        None
    } else {
        Some(short_raw)
    };
    let default_raw = str_field(&obj, "default", "option", span)?;
    let default = if default_raw.is_empty() {
        // also allow explicit nil → no default
        match obj.get("default").as_ref().map(|v| v.kind()) {
            None | Some(ValueKind::Nil) => None,
            _ => Some(default_raw),
        }
    } else {
        Some(default_raw)
    };
    let help = str_field(&obj, "help", "option", span)?;
    Ok(OptionSpec {
        name,
        short,
        default,
        help,
    })
}

fn parse_positional_spec(v: &Value, span: Span) -> Result<PositionalSpec, Control> {
    let obj = match v.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "cli.parse spec: positionals entry must be an object, got {}",
                    crate::interp::type_name(v)
                ),
                span,
            )
            .into())
        }
    };
    let name = str_field(&obj, "name", "positional", span)?;
    if name.is_empty() {
        return Err(AsError::at(
            "cli.parse spec: positional entry must have a non-empty 'name'",
            span,
        )
        .into());
    }
    let required = bool_field(&obj, "required", "positional", span)?;
    let help = str_field(&obj, "help", "positional", span)?;
    Ok(PositionalSpec {
        name,
        required,
        help,
    })
}

fn parse_array_of<T>(
    obj: &ObjCell,
    key: &str,
    span: Span,
    parse_fn: impl Fn(&Value, Span) -> Result<T, Control>,
) -> Result<Vec<T>, Control> {
    let got = obj.get(key);
    match got.as_ref().map(|v| v.kind()) {
        None | Some(ValueKind::Nil) => Ok(Vec::new()),
        Some(ValueKind::Array(a)) => {
            let items = a.borrow().clone();
            items.iter().map(|v| parse_fn(v, span)).collect()
        }
        Some(_) => Err(AsError::at(
            format!(
                "cli.parse spec: '{}' must be an array, got {}",
                key,
                crate::interp::type_name(got.as_ref().unwrap())
            ),
            span,
        )
        .into()),
    }
}

fn parse_spec(spec_val: &Value, span: Span) -> Result<CliSpec, Control> {
    let obj = match spec_val.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "cli.parse: spec must be an object, got {}",
                    crate::interp::type_name(spec_val)
                ),
                span,
            )
            .into())
        }
    };

    let name = str_field(&obj, "name", "spec", span)?;
    let flags = parse_array_of(&obj, "flags", span, parse_flag_spec)?;
    let options = parse_array_of(&obj, "options", span, parse_option_spec)?;
    let positionals = parse_array_of(&obj, "positionals", span, parse_positional_spec)?;

    // Parse subcommands
    let subcommands_field = obj.get("subcommands");
    let subcommands = match subcommands_field.as_ref().map(|v| v.kind()) {
        None | Some(ValueKind::Nil) => Vec::new(),
        Some(ValueKind::Array(a)) => {
            let items = a.borrow().clone();
            items
                .iter()
                .map(|v| -> Result<SubcommandSpec, Control> {
                    let sub_obj = match v.kind() {
                        ValueKind::Object(o) => o.clone(),
                        _ => {
                            return Err(AsError::at(
                                format!(
                                    "cli.parse spec: subcommands entry must be an object, got {}",
                                    crate::interp::type_name(v)
                                ),
                                span,
                            )
                            .into())
                        }
                    };
                    let sub_name = str_field(&sub_obj, "name", "subcommand", span)?;
                    if sub_name.is_empty() {
                        return Err(AsError::at(
                            "cli.parse spec: subcommand entry must have a non-empty 'name'",
                            span,
                        )
                        .into());
                    }
                    let sub_flags = parse_array_of(&sub_obj, "flags", span, parse_flag_spec)?;
                    let sub_options = parse_array_of(&sub_obj, "options", span, parse_option_spec)?;
                    let sub_positionals =
                        parse_array_of(&sub_obj, "positionals", span, parse_positional_spec)?;
                    Ok(SubcommandSpec {
                        name: sub_name,
                        flags: sub_flags,
                        options: sub_options,
                        positionals: sub_positionals,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        Some(_) => {
            return Err(AsError::at(
                format!(
                    "cli.parse spec: 'subcommands' must be an array, got {}",
                    crate::interp::type_name(subcommands_field.as_ref().unwrap())
                ),
                span,
            )
            .into())
        }
    };

    Ok(CliSpec {
        name,
        flags,
        options,
        positionals,
        subcommands,
    })
}

// ── help text generation ──────────────────────────────────────────────────────

fn generate_help(
    prog_name: &str,
    flags: &[FlagSpec],
    options: &[OptionSpec],
    positionals: &[PositionalSpec],
    subcommands: &[SubcommandSpec],
) -> String {
    let mut out = String::new();

    // Usage line
    out.push_str("Usage: ");
    if !prog_name.is_empty() {
        out.push_str(prog_name);
    } else {
        out.push_str("<program>");
    }
    if !flags.is_empty() || !options.is_empty() {
        out.push_str(" [options]");
    }
    for pos in positionals {
        if pos.required {
            out.push_str(&format!(" <{}>", pos.name));
        } else {
            out.push_str(&format!(" [{}]", pos.name));
        }
    }
    if !subcommands.is_empty() {
        out.push_str(" [subcommand]");
    }
    out.push('\n');

    // Flags
    if !flags.is_empty() {
        out.push_str("\nFlags:\n");
        for f in flags {
            let short_part = if let Some(s) = &f.short {
                format!("-{}, ", s)
            } else {
                "    ".to_string()
            };
            out.push_str(&format!("  {}--{}", short_part, f.name));
            if !f.help.is_empty() {
                out.push_str(&format!("  {}", f.help));
            }
            out.push('\n');
        }
    }

    // Options
    if !options.is_empty() {
        out.push_str("\nOptions:\n");
        for o in options {
            let short_part = if let Some(s) = &o.short {
                format!("-{}, ", s)
            } else {
                "    ".to_string()
            };
            out.push_str(&format!("  {}--{} <{}>", short_part, o.name, o.name));
            if let Some(d) = &o.default {
                out.push_str(&format!(" (default: {})", d));
            }
            if !o.help.is_empty() {
                out.push_str(&format!("  {}", o.help));
            }
            out.push('\n');
        }
    }

    // Positionals
    if !positionals.is_empty() {
        out.push_str("\nArguments:\n");
        for p in positionals {
            let req = if p.required { " (required)" } else { "" };
            out.push_str(&format!("  <{}>{}", p.name, req));
            if !p.help.is_empty() {
                out.push_str(&format!("  {}", p.help));
            }
            out.push('\n');
        }
    }

    // Subcommands
    if !subcommands.is_empty() {
        out.push_str("\nSubcommands:\n");
        for s in subcommands {
            out.push_str(&format!("  {}\n", s.name));
        }
    }

    out
}

// ── core argument parsing logic ───────────────────────────────────────────────

struct ParseResult {
    flags: IndexMap<String, Value>,
    options: IndexMap<String, Value>,
    positionals_map: IndexMap<String, Value>,
}

/// Parse a slice of string args against flags/options/positionals specs.
/// Returns `Ok(ParseResult)` on success or `Err(String)` with an error message
/// (which the caller wraps into a Tier-1 pair).
fn parse_args_against_spec(
    args: &[String],
    flags: &[FlagSpec],
    options: &[OptionSpec],
    positionals: &[PositionalSpec],
) -> Result<ParseResult, String> {
    let mut flag_map: IndexMap<String, Value> = IndexMap::new();
    let mut option_map: IndexMap<String, Value> = IndexMap::new();
    let mut positional_values: Vec<String> = Vec::new();

    // Initialize all flags to false
    for f in flags {
        flag_map.insert(f.name.clone(), Value::bool_(false));
    }
    // Initialize all options to their default (or nil)
    for o in options {
        let val = match &o.default {
            Some(d) => Value::str(d.as_str()),
            None => Value::nil(),
        };
        option_map.insert(o.name.clone(), val);
    }

    let mut i = 0;
    let mut after_double_dash = false;

    while i < args.len() {
        let tok = &args[i];

        if after_double_dash {
            positional_values.push(tok.clone());
            i += 1;
            continue;
        }

        if tok == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        if tok.starts_with("--") {
            // Long option or flag: --name or --name=value
            let rest = tok.strip_prefix("--").unwrap();
            let (key, inline_val) = if let Some(eq) = rest.find('=') {
                (&rest[..eq], Some(rest[eq + 1..].to_string()))
            } else {
                (rest, None)
            };

            // Check if it's a flag
            if let Some(f) = flags.iter().find(|f| f.name == key) {
                if inline_val.is_some() {
                    return Err(format!("flag '--{}' does not take a value", f.name));
                }
                flag_map.insert(f.name.clone(), Value::bool_(true));
                i += 1;
            } else if let Some(o) = options.iter().find(|o| o.name == key) {
                let val = if let Some(v) = inline_val {
                    v
                } else {
                    i += 1;
                    if i >= args.len() {
                        return Err(format!("option '--{}' requires a value", o.name));
                    }
                    args[i].clone()
                };
                option_map.insert(o.name.clone(), Value::str(val));
                i += 1;
            } else {
                return Err(format!("unknown option '--{}'", key));
            }
        } else if tok.starts_with('-') && tok.len() > 1 {
            // Short option: -x or -x val
            let short_key = &tok[1..];

            if let Some(f) = flags.iter().find(|f| f.short.as_deref() == Some(short_key)) {
                flag_map.insert(f.name.clone(), Value::bool_(true));
                i += 1;
            } else if let Some(o) = options
                .iter()
                .find(|o| o.short.as_deref() == Some(short_key))
            {
                i += 1;
                if i >= args.len() {
                    return Err(format!("option '-{}' requires a value", short_key));
                }
                let val = args[i].clone();
                option_map.insert(o.name.clone(), Value::str(val));
                i += 1;
            } else {
                return Err(format!("unknown option '-{}'", short_key));
            }
        } else {
            // Positional
            positional_values.push(tok.clone());
            i += 1;
        }
    }

    // Map positional values to declared positional names
    let mut positionals_map: IndexMap<String, Value> = IndexMap::new();
    for (idx, pos_spec) in positionals.iter().enumerate() {
        if let Some(v) = positional_values.get(idx) {
            positionals_map.insert(pos_spec.name.clone(), Value::str(v.as_str()));
        } else if pos_spec.required {
            return Err(format!("missing required argument <{}>", pos_spec.name));
        } else {
            positionals_map.insert(pos_spec.name.clone(), Value::nil());
        }
    }

    // Extra positionals beyond declared → error
    if positional_values.len() > positionals.len() {
        return Err(format!(
            "unexpected positional argument '{}'",
            positional_values[positionals.len()]
        ));
    }

    Ok(ParseResult {
        flags: flag_map,
        options: option_map,
        positionals_map,
    })
}

/// A pre-scan of `raw_args` that walks the token stream the same way the main
/// parse does — honoring the `--` terminator and pairing value-taking options
/// with their value token — and reports two facts the dispatch needs *before*
/// committing to a parse strategy:
///   - `help`: whether a real `--help`/`-h` flag appeared *before* `--`
///     (a `--help` after `--` is a positional, not a help request).
///   - `first_positional`: the index in `raw_args` of the first genuine
///     positional token (a token that is neither an option/flag nor an option's
///     consumed value), considering only the region before `--`. `None` if no
///     such token exists before `--`.
///
/// Using one walk for both detections keeps them in agreement with the real
/// `parse_args_against_spec` walk, so `--output build` never misfires as a
/// subcommand and `-- --help` never misfires as help mode.
struct PreScan {
    help: bool,
    first_positional: Option<usize>,
}

fn pre_scan(raw_args: &[String], flags: &[FlagSpec], options: &[OptionSpec]) -> PreScan {
    let mut help = false;
    let mut first_positional = None;
    let mut i = 0;
    while i < raw_args.len() {
        let tok = &raw_args[i];
        // The `--` terminator ends the option region; nothing after it is a
        // subcommand candidate or a help flag (per design), so stop scanning.
        if tok == "--" {
            break;
        }
        if tok == "--help" || tok == "-h" {
            help = true;
            i += 1;
            continue;
        }
        if let Some(rest) = tok.strip_prefix("--") {
            // Long option/flag. `--name=value` consumes no separate token.
            let key = match rest.find('=') {
                Some(eq) => &rest[..eq],
                None => rest,
            };
            let has_inline = rest.contains('=');
            if !has_inline && options.iter().any(|o| o.name == key) {
                // `--opt value` form: skip the value token too.
                i += 2;
            } else {
                // A flag, an unknown `--x`, or `--opt=value` — consumes one token.
                let _ = flags; // (flags consume no separate value either)
                i += 1;
            }
        } else if tok.starts_with('-') && tok.len() > 1 {
            // Short option/flag. `-o value` consumes the next token; `-v` (flag)
            // or an unknown short consumes one.
            let short_key = &tok[1..];
            if options
                .iter()
                .any(|o| o.short.as_deref() == Some(short_key))
            {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            // Genuine positional (or subcommand candidate).
            if first_positional.is_none() {
                first_positional = Some(i);
            }
            i += 1;
        }
    }
    PreScan {
        help,
        first_positional,
    }
}

/// Build an AScript Object from the ParseResult.
fn result_to_value(pr: ParseResult, subcommand: Value, help: Value) -> Value {
    let mut map = IndexMap::new();
    map.insert(
        "flags".to_string(),
        Value::Object(crate::value::ObjectCell::new(pr.flags)),
    );
    map.insert(
        "options".to_string(),
        Value::Object(crate::value::ObjectCell::new(pr.options)),
    );
    map.insert(
        "positionals".to_string(),
        Value::Object(crate::value::ObjectCell::new(pr.positionals_map)),
    );
    map.insert("subcommand".to_string(), subcommand);
    map.insert("help".to_string(), help);
    Value::Object(crate::value::ObjectCell::new(map))
}

// ── the impl Interp dispatch ──────────────────────────────────────────────────

impl Interp {
    pub(crate) async fn call_cli(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "parse" => {
                let spec_val = arg(args, 0);
                // args defaults to env.args() when omitted/nil
                let args_val = match args.get(1).map(|v| v.kind()) {
                    None | Some(ValueKind::Nil) => self.get_cli_args(),
                    Some(_) => args.get(1).unwrap().clone(),
                };

                // Parse the spec (Tier-2 panic on malformed spec)
                let spec = parse_spec(&spec_val, span)?;

                // Extract the args array (Tier-2 panic if wrong type)
                let raw_args: Vec<String> = match args_val.kind() {
                    ValueKind::Array(a) => a
                        .borrow()
                        .iter()
                        .map(|v| match v.kind() {
                            ValueKind::Str(s) => Ok(s.to_string()),
                            _ => Err(AsError::at(
                                format!(
                                    "cli.parse: args must be an array of strings, got {}",
                                    crate::interp::type_name(v)
                                ),
                                span,
                            )
                            .into()),
                        })
                        .collect::<Result<Vec<_>, Control>>()?,
                    ValueKind::Nil => Vec::new(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "cli.parse: args must be an array, got {}",
                                crate::interp::type_name(&args_val)
                            ),
                            span,
                        )
                        .into())
                    }
                };

                // One walk used for both --help detection and subcommand
                // detection, so they agree with the main parse: it honors the
                // `--` terminator and pairs value-options with their value.
                let scan = pre_scan(&raw_args, &spec.flags, &spec.options);

                // Handle --help/-h before regular parsing (only a real flag
                // before `--` counts; `-- --help` is a positional).
                if scan.help {
                    let help_text = generate_help(
                        &spec.name,
                        &spec.flags,
                        &spec.options,
                        &spec.positionals,
                        &spec.subcommands,
                    );
                    // Return a "success" result with help populated, no error
                    let pr = ParseResult {
                        flags: spec
                            .flags
                            .iter()
                            .map(|f| (f.name.clone(), Value::bool_(false)))
                            .collect(),
                        options: spec
                            .options
                            .iter()
                            .map(|o| {
                                (
                                    o.name.clone(),
                                    o.default
                                        .as_deref()
                                        .map(Value::str)
                                        .unwrap_or(Value::nil()),
                                )
                            })
                            .collect(),
                        positionals_map: IndexMap::new(),
                    };
                    let result = result_to_value(pr, Value::nil(), Value::str(help_text));
                    return Ok(make_pair(result, Value::nil()));
                }

                // Detect subcommand: only the first *genuine* positional token
                // (per the pre-scan, which skips option values and stops at `--`)
                // is a subcommand candidate.
                let sub_tok_idx = scan.first_positional;
                let matched_sub = sub_tok_idx.and_then(|idx| {
                    let tok = &raw_args[idx];
                    spec.subcommands.iter().find(|s| &s.name == tok)
                });

                if let Some(sub) = matched_sub {
                    let sub_name = sub.name.clone();
                    // Split at the detected subcommand token index: args before it
                    // go to the top-level; the token itself is consumed; the rest
                    // are parsed under the subcommand spec.
                    let sub_tok_idx = sub_tok_idx.unwrap();
                    let top_args: Vec<String> = raw_args[..sub_tok_idx].to_vec();
                    let sub_args: Vec<String> = raw_args[sub_tok_idx + 1..].to_vec();

                    // Parse top-level (without positionals — the subcommand consumed that slot)
                    let top_pr =
                        match parse_args_against_spec(&top_args, &spec.flags, &spec.options, &[]) {
                            Ok(pr) => pr,
                            Err(msg) => {
                                return Ok(make_pair(
                                    Value::nil(),
                                    make_error(Value::str(msg)),
                                ));
                            }
                        };

                    // Parse subcommand
                    let sub_pr = match parse_args_against_spec(
                        &sub_args,
                        &sub.flags,
                        &sub.options,
                        &sub.positionals,
                    ) {
                        Ok(pr) => pr,
                        Err(msg) => {
                            return Ok(make_pair(Value::nil(), make_error(Value::str(msg))));
                        }
                    };

                    // Build subcommand result object
                    let mut sub_map = IndexMap::new();
                    sub_map.insert("name".to_string(), Value::str(sub_name.as_str()));
                    sub_map.insert(
                        "flags".to_string(),
                        Value::Object(crate::value::ObjectCell::new(sub_pr.flags)),
                    );
                    sub_map.insert(
                        "options".to_string(),
                        Value::Object(crate::value::ObjectCell::new(sub_pr.options)),
                    );
                    sub_map.insert(
                        "positionals".to_string(),
                        Value::Object(crate::value::ObjectCell::new(sub_pr.positionals_map)),
                    );
                    let sub_val = Value::Object(crate::value::ObjectCell::new(sub_map));

                    let result = result_to_value(top_pr, sub_val, Value::nil());
                    return Ok(make_pair(result, Value::nil()));
                }

                // Regular parse (no subcommand)
                match parse_args_against_spec(
                    &raw_args,
                    &spec.flags,
                    &spec.options,
                    &spec.positionals,
                ) {
                    Ok(pr) => {
                        let result = result_to_value(pr, Value::nil(), Value::nil());
                        Ok(make_pair(result, Value::nil()))
                    }
                    Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
                }
            }
            _ => Err(AsError::at(format!("std/cli has no function '{}'", func), span).into()),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;
    use crate::span::Span;
    use crate::value::{Value, ValueKind};
    use indexmap::IndexMap;
    

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn s(x: &str) -> Value {
        Value::str(x)
    }

    fn b(x: bool) -> Value {
        Value::bool_(x)
    }

    fn obj(fields: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in fields {
            m.insert(k.to_string(), v);
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(crate::value::ArrayCell::new(items))
    }

    /// Build a simple spec with flags, options, positionals.
    fn make_spec(
        name: &str,
        flags: Vec<Value>,
        options: Vec<Value>,
        positionals: Vec<Value>,
        subcommands: Vec<Value>,
    ) -> Value {
        let mut fields = vec![
            ("name", s(name)),
            ("flags", arr(flags)),
            ("options", arr(options)),
            ("positionals", arr(positionals)),
        ];
        if !subcommands.is_empty() {
            fields.push(("subcommands", arr(subcommands)));
        }
        obj(fields)
    }

    fn flag_spec(name: &str, short: &str) -> Value {
        obj(vec![
            ("name", s(name)),
            ("short", s(short)),
            ("help", s("")),
        ])
    }

    fn option_spec(name: &str, short: &str, default: Option<&str>) -> Value {
        let default_val = match default {
            Some(d) => s(d),
            None => Value::nil(),
        };
        obj(vec![
            ("name", s(name)),
            ("short", s(short)),
            ("default", default_val),
            ("help", s("")),
        ])
    }

    fn pos_spec(name: &str, required: bool) -> Value {
        obj(vec![
            ("name", s(name)),
            ("required", b(required)),
            ("help", s("")),
        ])
    }

    // ── helper: get a nested field from a parse result ──────────────────────

    fn get_field(v: &Value, key: &str) -> Value {
        match v.kind() {
            ValueKind::Object(o) => o.get(key).unwrap_or(Value::nil()),
            _ => Value::nil(),
        }
    }

    fn pair_val(pair: &Value) -> Value {
        match pair.kind() {
            ValueKind::Array(a) => a.borrow()[0].clone(),
            _ => panic!("expected pair array"),
        }
    }

    fn pair_err(pair: &Value) -> Value {
        match pair.kind() {
            ValueKind::Array(a) => a.borrow()[1].clone(),
            _ => panic!("expected pair array"),
        }
    }

    fn is_err(pair: &Value) -> bool {
        pair_err(pair) != Value::nil()
    }

    // ── tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn flag_long_present() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![flag_spec("verbose", "v")],
            vec![],
            vec![],
            vec![],
        );
        let args = arr(vec![s("--verbose")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "unexpected error: {}", pair_err(&result));
        let val = pair_val(&result);
        let flags = get_field(&val, "flags");
        assert_eq!(get_field(&flags, "verbose"), b(true));
    }

    #[tokio::test]
    async fn flag_short_present() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![flag_spec("verbose", "v")],
            vec![],
            vec![],
            vec![],
        );
        let args = arr(vec![s("-v")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let flags = get_field(&pair_val(&result), "flags");
        assert_eq!(get_field(&flags, "verbose"), b(true));
    }

    #[tokio::test]
    async fn flag_absent_is_false() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![flag_spec("verbose", "v")],
            vec![],
            vec![],
            vec![],
        );
        let args = arr(vec![]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let flags = get_field(&pair_val(&result), "flags");
        assert_eq!(get_field(&flags, "verbose"), b(false));
    }

    #[tokio::test]
    async fn option_long_space() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", Some("out.txt"))],
            vec![],
            vec![],
        );
        let args = arr(vec![s("--output"), s("result.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "output"), s("result.txt"));
    }

    #[tokio::test]
    async fn option_long_equals() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", Some("out.txt"))],
            vec![],
            vec![],
        );
        let args = arr(vec![s("--output=result.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "output"), s("result.txt"));
    }

    #[tokio::test]
    async fn option_short() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", None)],
            vec![],
            vec![],
        );
        let args = arr(vec![s("-o"), s("myfile.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "output"), s("myfile.txt"));
    }

    #[tokio::test]
    async fn option_absent_uses_default() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", Some("out.txt"))],
            vec![],
            vec![],
        );
        let args = arr(vec![]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "output"), s("out.txt"));
    }

    #[tokio::test]
    async fn option_absent_no_default_is_nil() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", None)],
            vec![],
            vec![],
        );
        let args = arr(vec![]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "output"), Value::nil());
    }

    #[tokio::test]
    async fn required_positional_present() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("input", true)],
            vec![],
        );
        let args = arr(vec![s("myfile.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let positionals = get_field(&pair_val(&result), "positionals");
        assert_eq!(get_field(&positionals, "input"), s("myfile.txt"));
    }

    #[tokio::test]
    async fn required_positional_missing_is_tier1_err() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("input", true)],
            vec![],
        );
        let args = arr(vec![]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(
            is_err(&result),
            "expected error for missing required positional"
        );
        let err_msg = get_field(&pair_err(&result), "message");
        assert!(
            err_msg.to_string().contains("missing"),
            "err msg: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn help_flag_returns_help_text_no_error() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![flag_spec("verbose", "v")],
            vec![option_spec("output", "o", Some("out.txt"))],
            vec![pos_spec("input", false)],
            vec![],
        );
        let args = arr(vec![s("--help")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "help should not produce an error");
        let val = pair_val(&result);
        let help = get_field(&val, "help");
        assert!(matches!(help.kind(), ValueKind::Str(_)), "help should be a string");
        let help_str = match help.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => unreachable!(),
        };
        assert!(!help_str.is_empty(), "help text should not be empty");
        assert!(
            help_str.contains("verbose") || help_str.contains("Usage"),
            "help: {}",
            help_str
        );
    }

    #[tokio::test]
    async fn short_help_flag() {
        let interp = Interp::new();
        let spec = make_spec("mytool", vec![], vec![], vec![], vec![]);
        let args = arr(vec![s("-h")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let help = get_field(&pair_val(&result), "help");
        assert!(matches!(help.kind(), ValueKind::Str(_)));
    }

    #[tokio::test]
    async fn double_dash_terminator() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![flag_spec("verbose", "v")],
            vec![],
            vec![pos_spec("file", true)],
            vec![],
        );
        // --verbose -- --not-a-flag: the "--not-a-flag" should be treated as a positional
        // Actually in this test let's use a normal-looking positional after --
        let args = arr(vec![s("--verbose"), s("--"), s("some-file.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let val = pair_val(&result);
        let flags = get_field(&val, "flags");
        assert_eq!(get_field(&flags, "verbose"), b(true));
        let positionals = get_field(&val, "positionals");
        assert_eq!(get_field(&positionals, "file"), s("some-file.txt"));
    }

    #[tokio::test]
    async fn double_dash_routes_dash_prefix_as_positional() {
        // after --, even "--not-a-flag" is treated as a positional
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("file", true)],
            vec![],
        );
        let args = arr(vec![s("--"), s("--not-a-flag")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let positionals = get_field(&pair_val(&result), "positionals");
        assert_eq!(get_field(&positionals, "file"), s("--not-a-flag"));
    }

    #[tokio::test]
    async fn subcommand_dispatch() {
        let interp = Interp::new();
        let sub = obj(vec![
            ("name", s("build")),
            ("flags", arr(vec![])),
            ("options", arr(vec![])),
            ("positionals", arr(vec![pos_spec("target", false)])),
        ]);
        let spec = make_spec("mytool", vec![], vec![], vec![], vec![sub]);
        let args = arr(vec![s("build"), s("myapp")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let val = pair_val(&result);
        let subcommand = get_field(&val, "subcommand");
        assert_ne!(subcommand, Value::nil(), "subcommand should not be nil");
        let sub_name = get_field(&subcommand, "name");
        assert_eq!(sub_name, s("build"));
        let sub_pos = get_field(&subcommand, "positionals");
        assert_eq!(get_field(&sub_pos, "target"), s("myapp"));
    }

    #[tokio::test]
    async fn unknown_flag_is_tier1_err() {
        let interp = Interp::new();
        let spec = make_spec("mytool", vec![], vec![], vec![], vec![]);
        let args = arr(vec![s("--unknown")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(is_err(&result), "expected error for unknown flag");
        let err_msg = get_field(&pair_err(&result), "message");
        assert!(err_msg.to_string().contains("unknown"), "err: {}", err_msg);
    }

    #[tokio::test]
    async fn no_args_no_required_positional_ok() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("file", false)],
            vec![],
        );
        let args = arr(vec![]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result));
        let positionals = get_field(&pair_val(&result), "positionals");
        assert_eq!(get_field(&positionals, "file"), Value::nil());
    }

    #[tokio::test]
    async fn too_many_positionals_is_tier1_err() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("file", true)],
            vec![],
        );
        let args = arr(vec![s("a.txt"), s("b.txt")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(is_err(&result), "expected error for too many positionals");
    }

    #[tokio::test]
    async fn missing_option_value_is_tier1_err() {
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", None)],
            vec![],
            vec![],
        );
        let args = arr(vec![s("--output")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(is_err(&result), "expected error for missing option value");
    }

    #[tokio::test]
    async fn bad_spec_is_tier2_panic() {
        let interp = Interp::new();
        // Passing a non-object spec → Tier-2 panic (Control::Panic)
        let result = interp
            .call_cli("parse", &[s("not-an-object"), arr(vec![])], sp())
            .await;
        assert!(
            matches!(result, Err(Control::Panic(_))),
            "expected Tier-2 panic"
        );
    }

    #[tokio::test]
    async fn option_value_matching_subcommand_name_is_not_dispatched() {
        // `--output build x`: `build` is the *value* of --output, NOT a
        // subcommand dispatch. It must parse as top-level with output="build"
        // and x as... well, there are no top-level positionals, so the result
        // is top-level option output="build", subcommand nil.
        let interp = Interp::new();
        let sub = obj(vec![
            ("name", s("build")),
            ("flags", arr(vec![])),
            ("options", arr(vec![])),
            ("positionals", arr(vec![pos_spec("target", false)])),
        ]);
        // Give the top-level a positional slot so `x` lands somewhere valid.
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("output", "o", None)],
            vec![pos_spec("rest", false)],
            vec![sub],
        );
        let args = arr(vec![s("--output"), s("build"), s("x")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let val = pair_val(&result);
        // No subcommand dispatch.
        assert_eq!(
            get_field(&val, "subcommand"),
            Value::nil(),
            "should NOT dispatch subcommand"
        );
        // output is the literal "build".
        let options = get_field(&val, "options");
        assert_eq!(get_field(&options, "output"), s("build"));
        // x is the top-level positional.
        let positionals = get_field(&val, "positionals");
        assert_eq!(get_field(&positionals, "rest"), s("x"));
    }

    #[tokio::test]
    async fn double_dash_then_help_is_not_help_mode() {
        // `-- --help`: after the terminator, `--help` is a positional, NOT a
        // help request.
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("file", true)],
            vec![],
        );
        let args = arr(vec![s("--"), s("--help")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let val = pair_val(&result);
        assert_eq!(
            get_field(&val, "help"),
            Value::nil(),
            "should NOT be help mode"
        );
        let positionals = get_field(&val, "positionals");
        assert_eq!(get_field(&positionals, "file"), s("--help"));
    }

    #[tokio::test]
    async fn double_dash_then_subcommand_name_is_positional() {
        // `-- build` with a `build` subcommand: `build` is a positional after
        // the terminator, NOT a subcommand dispatch.
        let interp = Interp::new();
        let sub = obj(vec![
            ("name", s("build")),
            ("flags", arr(vec![])),
            ("options", arr(vec![])),
            ("positionals", arr(vec![])),
        ]);
        let spec = make_spec(
            "mytool",
            vec![],
            vec![],
            vec![pos_spec("file", true)],
            vec![sub],
        );
        let args = arr(vec![s("--"), s("build")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let val = pair_val(&result);
        assert_eq!(
            get_field(&val, "subcommand"),
            Value::nil(),
            "should NOT dispatch subcommand"
        );
        let positionals = get_field(&val, "positionals");
        assert_eq!(get_field(&positionals, "file"), s("build"));
    }

    #[tokio::test]
    async fn option_equals_value_with_equals_signs() {
        // `--opt=a=b=c`: only the first `=` splits key/value, so the value is
        // the literal `a=b=c`.
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("opt", "o", None)],
            vec![],
            vec![],
        );
        let args = arr(vec![s("--opt=a=b=c")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "opt"), s("a=b=c"));
    }

    #[tokio::test]
    async fn short_option_value_can_look_like_a_flag() {
        // `-o --x`: `--x` is consumed as the *value* of `-o`, not parsed as an
        // option.
        let interp = Interp::new();
        let spec = make_spec(
            "mytool",
            vec![],
            vec![option_spec("opt", "o", None)],
            vec![],
            vec![],
        );
        let args = arr(vec![s("-o"), s("--x")]);
        let result = interp.call_cli("parse", &[spec, args], sp()).await.unwrap();
        assert!(!is_err(&result), "err: {}", pair_err(&result));
        let options = get_field(&pair_val(&result), "options");
        assert_eq!(get_field(&options, "opt"), s("--x"));
    }
}
