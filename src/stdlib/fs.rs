//! `std/fs` — filesystem access: read/write/append, metadata, directory listing
//! and recursive walking, pure path helpers, and a recursive `grep` (spec §11.3)
//! backed by the `regex` crate and the `ignore` walker (honors `.gitignore`).
//!
//! Fallible I/O returns a Tier-1 `[value, err]` pair; argument-type misuse is a
//! Tier-2 panic (per spec §11.3). Path helpers are pure and infallible.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, type_name, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("read", bi("fs.read")),
        ("readBytes", bi("fs.readBytes")),
        ("write", bi("fs.write")),
        ("append", bi("fs.append")),
        ("exists", bi("fs.exists")),
        ("stat", bi("fs.stat")),
        ("mkdir", bi("fs.mkdir")),
        ("remove", bi("fs.remove")),
        ("readDir", bi("fs.readDir")),
        ("walk", bi("fs.walk")),
        ("join", bi("fs.join")),
        ("dirname", bi("fs.dirname")),
        ("basename", bi("fs.basename")),
        ("extname", bi("fs.extname")),
        ("isAbsolute", bi("fs.isAbsolute")),
        ("grep", bi("fs.grep")),
    ]
}

fn arr(v: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(v)))
}

fn obj(m: IndexMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(m)))
}

fn bytes_val(v: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(v)))
}

/// A Tier-1 error pair `[nil, {message}]` from a `std::io::Error` (or any Display).
fn io_err(e: impl std::fmt::Display) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(e.to_string().into())))
}

/// Resolve a string-or-bytes argument to a byte buffer (string → utf8 bytes).
/// Used by `write`/`append`. Anything else is a Tier-2 panic.
fn want_data(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.borrow().clone()),
        _ => Err(AsError::at(
            format!("{} expects a string or bytes, got {}", ctx, type_name(v)),
            span,
        )
        .into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("fs.{}", f);
    match func {
        // read(path) -> [string, err]  (utf8)
        "read" => {
            let path = want_string(&arg(args, 0), span, &ctx("read"))?;
            match std::fs::read(path.as_ref()) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
                    Err(_) => Ok(io_err("file is not valid UTF-8")),
                },
                Err(e) => Ok(io_err(e)),
            }
        }
        // readBytes(path) -> [bytes, err]
        "readBytes" => {
            let path = want_string(&arg(args, 0), span, &ctx("readBytes"))?;
            match std::fs::read(path.as_ref()) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(e) => Ok(io_err(e)),
            }
        }
        // write(path, data) -> [nil, err]  (data string or bytes)
        "write" => {
            let path = want_string(&arg(args, 0), span, &ctx("write"))?;
            let data = want_data(&arg(args, 1), span, &ctx("write"))?;
            match std::fs::write(path.as_ref(), data) {
                Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                Err(e) => Ok(io_err(e)),
            }
        }
        // append(path, data) -> [nil, err]
        "append" => {
            let path = want_string(&arg(args, 0), span, &ctx("append"))?;
            let data = want_data(&arg(args, 1), span, &ctx("append"))?;
            use std::io::Write;
            let result = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path.as_ref())
                .and_then(|mut f| f.write_all(&data));
            match result {
                Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                Err(e) => Ok(io_err(e)),
            }
        }
        // exists(path) -> bool
        "exists" => {
            let path = want_string(&arg(args, 0), span, &ctx("exists"))?;
            Ok(Value::Bool(Path::new(path.as_ref()).exists()))
        }
        // stat(path) -> [{size, isFile, isDir, modifiedMs}, err]
        "stat" => {
            let path = want_string(&arg(args, 0), span, &ctx("stat"))?;
            match std::fs::metadata(path.as_ref()) {
                Ok(md) => {
                    let modified = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| Value::Number(d.as_millis() as f64))
                        .unwrap_or(Value::Nil);
                    let mut m = IndexMap::new();
                    m.insert("size".to_string(), Value::Number(md.len() as f64));
                    m.insert("isFile".to_string(), Value::Bool(md.is_file()));
                    m.insert("isDir".to_string(), Value::Bool(md.is_dir()));
                    m.insert("modifiedMs".to_string(), modified);
                    Ok(make_pair(obj(m), Value::Nil))
                }
                Err(e) => Ok(io_err(e)),
            }
        }
        // mkdir(path, recursive?) -> [nil, err]
        "mkdir" => {
            let path = want_string(&arg(args, 0), span, &ctx("mkdir"))?;
            let recursive = arg(args, 1).is_truthy();
            let result = if recursive {
                std::fs::create_dir_all(path.as_ref())
            } else {
                std::fs::create_dir(path.as_ref())
            };
            match result {
                Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                Err(e) => Ok(io_err(e)),
            }
        }
        // remove(path, recursive?) -> [nil, err]  (file or dir; recursive removes trees)
        "remove" => {
            let path = want_string(&arg(args, 0), span, &ctx("remove"))?;
            let recursive = arg(args, 1).is_truthy();
            let p = Path::new(path.as_ref());
            let result = if p.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(p)
                } else {
                    std::fs::remove_dir(p)
                }
            } else {
                std::fs::remove_file(p)
            };
            match result {
                Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                Err(e) => Ok(io_err(e)),
            }
        }
        // readDir(path) -> [array of entry names, err]
        "readDir" => {
            let path = want_string(&arg(args, 0), span, &ctx("readDir"))?;
            match std::fs::read_dir(path.as_ref()) {
                Ok(entries) => {
                    let mut names = Vec::new();
                    for entry in entries {
                        match entry {
                            Ok(e) => names.push(Value::Str(
                                e.file_name().to_string_lossy().into_owned().into(),
                            )),
                            Err(e) => return Ok(io_err(e)),
                        }
                    }
                    Ok(make_pair(arr(names), Value::Nil))
                }
                Err(e) => Ok(io_err(e)),
            }
        }
        // walk(path) -> [array of full paths, err]  (recursive, walkdir)
        "walk" => {
            let path = want_string(&arg(args, 0), span, &ctx("walk"))?;
            let mut paths = Vec::new();
            for entry in walkdir::WalkDir::new(path.as_ref()) {
                match entry {
                    Ok(e) => paths.push(Value::Str(e.path().to_string_lossy().into_owned().into())),
                    Err(e) => return Ok(io_err(e)),
                }
            }
            Ok(make_pair(arr(paths), Value::Nil))
        }
        // ---- pure path helpers (infallible) ----
        // join(...parts) -> string
        "join" => {
            let mut p = PathBuf::new();
            for (i, a) in args.iter().enumerate() {
                let part = want_string(a, span, &format!("fs.join (argument {})", i + 1))?;
                p.push(part.as_ref());
            }
            Ok(Value::Str(p.to_string_lossy().into_owned().into()))
        }
        // dirname(p) -> string (parent path, or "" if none)
        "dirname" => {
            let path = want_string(&arg(args, 0), span, &ctx("dirname"))?;
            let d = Path::new(path.as_ref())
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Value::Str(d.into()))
        }
        // basename(p) -> string (final component, or "" if none)
        "basename" => {
            let path = want_string(&arg(args, 0), span, &ctx("basename"))?;
            let b = Path::new(path.as_ref())
                .file_name()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Value::Str(b.into()))
        }
        // extname(p) -> string (e.g. ".txt" or "")
        "extname" => {
            let path = want_string(&arg(args, 0), span, &ctx("extname"))?;
            let e = Path::new(path.as_ref())
                .extension()
                .map(|p| format!(".{}", p.to_string_lossy()))
                .unwrap_or_default();
            Ok(Value::Str(e.into()))
        }
        // isAbsolute(p) -> bool
        "isAbsolute" => {
            let path = want_string(&arg(args, 0), span, &ctx("isAbsolute"))?;
            Ok(Value::Bool(Path::new(path.as_ref()).is_absolute()))
        }
        // grep(pattern, dir, opts?) -> [matches, err]  (spec §11.3)
        "grep" => grep(args, span),
        _ => Err(AsError::at(format!("std/fs has no function '{}'", func), span).into()),
    }
}

/// Recursive regex search across a directory tree (spec §11.3). Each match is
/// `{path, line, column, text}` with 1-based line/column. Walks with the `ignore`
/// crate; `respectGitignore` (default true) honors `.gitignore` (only inside a git
/// repo). Hidden/dotfiles are ALWAYS searched. Reads each file as UTF-8 and applies
/// the regex per line. Non-UTF-8/binary files are skipped silently so one bad file
/// doesn't fail the whole grep. `maxResults` > 0 caps the count; absent/<=0 = no limit.
fn grep(args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = "fs.grep";
    let pattern = want_string(&arg(args, 0), span, ctx)?;
    let dir = want_string(&arg(args, 1), span, ctx)?;

    // ---- parse opts ----
    let mut glob: Option<String> = None;
    let mut ignore_case = false;
    let mut max_results: Option<usize> = None;
    let mut respect_gitignore = true;
    if let Value::Object(o) = arg(args, 2) {
        let o = o.borrow();
        if let Some(Value::Str(g)) = o.get("glob") {
            glob = Some(g.to_string());
        }
        if let Some(v) = o.get("ignoreCase") {
            ignore_case = v.is_truthy();
        }
        // maxResults semantics: a value > 0 caps the result count at exactly that
        // many; absent or <= 0 means NO limit (return all matches).
        if let Some(Value::Number(n)) = o.get("maxResults") {
            if *n > 0.0 {
                max_results = Some(*n as usize);
            }
        }
        if let Some(v) = o.get("respectGitignore") {
            respect_gitignore = v.is_truthy();
        }
    }

    // ---- compile regex (reuse the `regex` crate; Tier-1 err on a bad pattern) ----
    let re = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .build()
    {
        Ok(re) => re,
        Err(e) => return Ok(io_err(format!("invalid regex: {}", e))),
    };

    // ---- build the walker ----
    // `respectGitignore` controls ONLY gitignore semantics (`.gitignore`,
    // `.git/info/exclude`, global excludes, parent ignores, and `.ignore` files).
    // NOTE: the `ignore` crate applies `.gitignore` only within a git repository;
    // a loose `.gitignore` in a non-repo directory is NOT honored.
    // Hidden/dotfiles are ALWAYS searched (`.hidden(false)`) — the intuitive grep
    // default — regardless of `respectGitignore`, so a content search still finds
    // matches in files like `.env` or `.config`.
    let mut builder = ignore::WalkBuilder::new(dir.as_ref());
    builder
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .ignore(respect_gitignore)
        .parents(respect_gitignore)
        .hidden(false);

    // glob filename filter via `ignore`'s overrides: a single allow-glob means
    // "only files matching this glob are searched".
    if let Some(g) = &glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(dir.as_ref());
        if let Err(e) = ob.add(g) {
            return Ok(io_err(format!("invalid glob: {}", e)));
        }
        match ob.build() {
            Ok(ov) => {
                builder.overrides(ov);
            }
            Err(e) => return Ok(io_err(format!("invalid glob: {}", e))),
        }
    }

    let mut matches: Vec<Value> = Vec::new();
    'walk: for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries silently
        };
        // only regular files
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        // read as UTF-8; skip binary / non-UTF-8 files gracefully
        let content = match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => continue, // non-UTF-8 → skip
            },
            Err(_) => continue, // unreadable → skip
        };
        let path_str = path.to_string_lossy().into_owned();
        for (line_idx, line) in content.lines().enumerate() {
            if let Some(m) = re.find(line) {
                // Enforce the cap BEFORE pushing so `maxResults: N` yields at most N.
                if let Some(max) = max_results {
                    if matches.len() >= max {
                        break 'walk;
                    }
                }
                let column = line[..m.start()].chars().count() + 1;
                let mut entry_obj = IndexMap::new();
                entry_obj.insert("path".to_string(), Value::Str(path_str.clone().into()));
                entry_obj.insert("line".to_string(), Value::Number((line_idx + 1) as f64));
                entry_obj.insert("column".to_string(), Value::Number(column as f64));
                entry_obj.insert("text".to_string(), Value::Str(line.into()));
                matches.push(obj(entry_obj));
            }
        }
    }

    Ok(make_pair(arr(matches), Value::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    /// A unique temp dir for a test, created fresh and returned. Caller cleans up.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ascript_fs_{}_{}", tag, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn path_str(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn unwrap_pair_ok(v: &Value) -> Value {
        match v {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a[1], Value::Nil, "expected ok pair, got err: {:?}", a[1]);
                a[0].clone()
            }
            other => panic!("expected a pair, got {:?}", other),
        }
    }

    #[test]
    fn write_read_string_roundtrip() {
        let dir = temp_dir("rw_string");
        let f = path_str(&dir.join("hello.txt"));
        let w = call("write", &[s(&f), s("hello world")], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&w), Value::Nil);
        let r = call("read", &[s(&f)], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&r), s("hello world"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_read_bytes_roundtrip() {
        let dir = temp_dir("rw_bytes");
        let f = path_str(&dir.join("data.bin"));
        let data = bytes_val(vec![0, 1, 2, 255, 254]);
        let w = call("write", &[s(&f), data], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&w), Value::Nil);
        let r = call("readBytes", &[s(&f)], sp()).unwrap();
        let got = unwrap_pair_ok(&r);
        match got {
            Value::Bytes(b) => assert_eq!(*b.borrow(), vec![0u8, 1, 2, 255, 254]),
            other => panic!("expected bytes, got {:?}", other),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_concatenates() {
        let dir = temp_dir("append");
        let f = path_str(&dir.join("log.txt"));
        call("append", &[s(&f), s("a")], sp()).unwrap();
        call("append", &[s(&f), s("b")], sp()).unwrap();
        let r = call("read", &[s(&f)], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&r), s("ab"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exists_true_and_false() {
        let dir = temp_dir("exists");
        let f = path_str(&dir.join("present.txt"));
        call("write", &[s(&f), s("x")], sp()).unwrap();
        assert_eq!(call("exists", &[s(&f)], sp()).unwrap(), Value::Bool(true));
        let missing = path_str(&dir.join("absent.txt"));
        assert_eq!(
            call("exists", &[s(&missing)], sp()).unwrap(),
            Value::Bool(false)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stat_reports_size_kind_and_mtime() {
        let dir = temp_dir("stat");
        let f = path_str(&dir.join("sized.txt"));
        call("write", &[s(&f), s("12345")], sp()).unwrap();
        let st = call("stat", &[s(&f)], sp()).unwrap();
        let o = unwrap_pair_ok(&st);
        let o = match &o {
            Value::Object(o) => o.borrow(),
            other => panic!("expected object, got {:?}", other),
        };
        assert_eq!(o.get("size"), Some(&Value::Number(5.0)));
        assert_eq!(o.get("isFile"), Some(&Value::Bool(true)));
        assert_eq!(o.get("isDir"), Some(&Value::Bool(false)));
        assert!(matches!(o.get("modifiedMs"), Some(Value::Number(_))));
        // and a directory
        let st_dir = call("stat", &[s(&path_str(&dir))], sp()).unwrap();
        let od = unwrap_pair_ok(&st_dir);
        if let Value::Object(o) = &od {
            assert_eq!(o.borrow().get("isDir"), Some(&Value::Bool(true)));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stat_missing_is_tier1_err() {
        let dir = temp_dir("stat_missing");
        let missing = path_str(&dir.join("nope.txt"));
        let st = call("stat", &[s(&missing)], sp()).unwrap();
        match &st {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a[0], Value::Nil);
                assert!(matches!(a[1], Value::Object(_)));
            }
            other => panic!("expected pair, got {:?}", other),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mkdir_recursive_and_remove_recursive() {
        let dir = temp_dir("mkdir");
        let nested = path_str(&dir.join("a").join("b").join("c"));
        // non-recursive on a deep path fails (Tier-1)
        let fail = call("mkdir", &[s(&nested)], sp()).unwrap();
        if let Value::Array(a) = &fail {
            assert!(matches!(a.borrow()[1], Value::Object(_)));
        }
        // recursive succeeds
        let ok = call("mkdir", &[s(&nested), Value::Bool(true)], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&ok), Value::Nil);
        assert_eq!(
            call("exists", &[s(&nested)], sp()).unwrap(),
            Value::Bool(true)
        );
        // recursive remove of the top dir clears the tree
        let top = path_str(&dir.join("a"));
        let rm = call("remove", &[s(&top), Value::Bool(true)], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&rm), Value::Nil);
        assert_eq!(
            call("exists", &[s(&top)], sp()).unwrap(),
            Value::Bool(false)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_file() {
        let dir = temp_dir("remove_file");
        let f = path_str(&dir.join("gone.txt"));
        call("write", &[s(&f), s("x")], sp()).unwrap();
        let rm = call("remove", &[s(&f)], sp()).unwrap();
        assert_eq!(unwrap_pair_ok(&rm), Value::Nil);
        assert_eq!(call("exists", &[s(&f)], sp()).unwrap(), Value::Bool(false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_dir_lists_entries() {
        let dir = temp_dir("readdir");
        call("write", &[s(&path_str(&dir.join("one.txt"))), s("1")], sp()).unwrap();
        call("write", &[s(&path_str(&dir.join("two.txt"))), s("2")], sp()).unwrap();
        let rd = call("readDir", &[s(&path_str(&dir))], sp()).unwrap();
        let listing = unwrap_pair_ok(&rd);
        let mut names: Vec<String> = match &listing {
            Value::Array(a) => a.borrow().iter().map(|v| v.to_string()).collect(),
            other => panic!("expected array, got {:?}", other),
        };
        names.sort();
        assert_eq!(names, vec!["one.txt".to_string(), "two.txt".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walk_finds_nested_files() {
        let dir = temp_dir("walk");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        call("write", &[s(&path_str(&dir.join("top.txt"))), s("t")], sp()).unwrap();
        call(
            "write",
            &[s(&path_str(&sub.join("deep.txt"))), s("d")],
            sp(),
        )
        .unwrap();
        let w = call("walk", &[s(&path_str(&dir))], sp()).unwrap();
        let paths = unwrap_pair_ok(&w);
        let joined = paths.to_string();
        assert!(
            joined.contains("top.txt"),
            "walk missing top.txt: {}",
            joined
        );
        assert!(
            joined.contains("deep.txt"),
            "walk missing deep.txt: {}",
            joined
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_helpers() {
        // join
        assert_eq!(
            call("join", &[s("a"), s("b"), s("c")], sp()).unwrap(),
            s("a/b/c")
        );
        // dirname / basename / extname
        assert_eq!(
            call("dirname", &[s("/x/y/z.txt")], sp()).unwrap(),
            s("/x/y")
        );
        assert_eq!(
            call("basename", &[s("/x/y/z.txt")], sp()).unwrap(),
            s("z.txt")
        );
        assert_eq!(
            call("extname", &[s("/x/y/z.txt")], sp()).unwrap(),
            s(".txt")
        );
        assert_eq!(call("extname", &[s("/x/y/noext")], sp()).unwrap(), s(""));
        // isAbsolute
        assert_eq!(
            call("isAbsolute", &[s("/abs/path")], sp()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("isAbsolute", &[s("rel/path")], sp()).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn grep_finds_matches_with_location() {
        let dir = temp_dir("grep_basic");
        call(
            "write",
            &[
                s(&path_str(&dir.join("a.txt"))),
                s("alpha\nTODO: fix\nbeta"),
            ],
            sp(),
        )
        .unwrap();
        call(
            "write",
            &[
                s(&path_str(&dir.join("b.txt"))),
                s("nothing here\nanother TODO line"),
            ],
            sp(),
        )
        .unwrap();
        let g = call("grep", &[s("TODO"), s(&path_str(&dir))], sp()).unwrap();
        let matches = unwrap_pair_ok(&g);
        let v = match &matches {
            Value::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {:?}", other),
        };
        assert_eq!(v.len(), 2, "expected 2 matches, got {:?}", v);
        // verify the shape of one match (from a.txt: line 2, "TODO" at column 1)
        let mut by_text: Vec<(String, f64, f64, String)> = v
            .iter()
            .map(|m| match m {
                Value::Object(o) => {
                    let o = o.borrow();
                    let line = if let Some(Value::Number(n)) = o.get("line") {
                        *n
                    } else {
                        -1.0
                    };
                    let col = if let Some(Value::Number(n)) = o.get("column") {
                        *n
                    } else {
                        -1.0
                    };
                    let text = o.get("text").map(|t| t.to_string()).unwrap_or_default();
                    let path = o.get("path").map(|t| t.to_string()).unwrap_or_default();
                    (path, line, col, text)
                }
                other => panic!("expected match object, got {:?}", other),
            })
            .collect();
        by_text.sort_by(|a, b| a.3.cmp(&b.3));
        // "TODO: fix"  -> line 2, column 1
        let fix = by_text.iter().find(|m| m.3.contains("fix")).unwrap();
        assert_eq!(fix.1, 2.0);
        assert_eq!(fix.2, 1.0);
        // "another TODO line" -> column 9 (1-based; "another " = 8 chars)
        let another = by_text.iter().find(|m| m.3.contains("another")).unwrap();
        assert_eq!(another.1, 2.0);
        assert_eq!(another.2, 9.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_max_results_semantics() {
        // Chosen semantics: maxResults > 0 → at most exactly N; absent or <= 0 → no limit.
        let dir = temp_dir("grep_max");
        call(
            "write",
            &[s(&path_str(&dir.join("m.txt"))), s("hit\nhit\nhit\nhit")],
            sp(),
        )
        .unwrap();
        let count = |opts: Option<Value>| -> usize {
            let mut args = vec![s("hit"), s(&path_str(&dir))];
            if let Some(o) = opts {
                args.push(o);
            }
            let g = call("grep", &args, sp()).unwrap();
            match unwrap_pair_ok(&g) {
                Value::Array(a) => a.borrow().len(),
                other => panic!("expected array, got {:?}", other),
            }
        };
        // maxResults: 2 → exactly 2
        let mut o2 = IndexMap::new();
        o2.insert("maxResults".to_string(), Value::Number(2.0));
        assert_eq!(count(Some(obj(o2))), 2);
        // maxResults: 0 → NO limit (all 4)
        let mut o0 = IndexMap::new();
        o0.insert("maxResults".to_string(), Value::Number(0.0));
        assert_eq!(count(Some(obj(o0))), 4);
        // absent → NO limit (all 4)
        assert_eq!(count(None), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_searches_dotfiles_by_default() {
        // With respectGitignore at its default (true), grep must STILL search
        // hidden/dotfiles — a content search shouldn't silently skip `.config`.
        let dir = temp_dir("grep_dotfile");
        call(
            "write",
            &[s(&path_str(&dir.join(".config"))), s("secret=findme")],
            sp(),
        )
        .unwrap();
        let g = call("grep", &[s("findme"), s(&path_str(&dir))], sp()).unwrap();
        let matches = unwrap_pair_ok(&g);
        match &matches {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a.len(), 1, "dotfile should be searched: {:?}", a);
                if let Value::Object(o) = &a[0] {
                    assert!(o
                        .borrow()
                        .get("path")
                        .unwrap()
                        .to_string()
                        .contains(".config"));
                }
            }
            other => panic!("expected array, got {:?}", other),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_ignore_case() {
        let dir = temp_dir("grep_case");
        call(
            "write",
            &[s(&path_str(&dir.join("c.txt"))), s("Hello WORLD")],
            sp(),
        )
        .unwrap();
        // case-sensitive: no match for "world"
        let g1 = call("grep", &[s("world"), s(&path_str(&dir))], sp()).unwrap();
        if let Value::Array(a) = &unwrap_pair_ok(&g1) {
            assert_eq!(a.borrow().len(), 0);
        }
        // case-insensitive: matches
        let mut opts = IndexMap::new();
        opts.insert("ignoreCase".to_string(), Value::Bool(true));
        let g2 = call("grep", &[s("world"), s(&path_str(&dir)), obj(opts)], sp()).unwrap();
        if let Value::Array(a) = &unwrap_pair_ok(&g2) {
            assert_eq!(a.borrow().len(), 1);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_glob_filter_restricts_files() {
        let dir = temp_dir("grep_glob");
        call(
            "write",
            &[s(&path_str(&dir.join("keep.rs"))), s("needle here")],
            sp(),
        )
        .unwrap();
        call(
            "write",
            &[s(&path_str(&dir.join("skip.txt"))), s("needle here too")],
            sp(),
        )
        .unwrap();
        let mut opts = IndexMap::new();
        opts.insert("glob".to_string(), s("*.rs"));
        let g = call("grep", &[s("needle"), s(&path_str(&dir)), obj(opts)], sp()).unwrap();
        let matches = unwrap_pair_ok(&g);
        let v = match &matches {
            Value::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {:?}", other),
        };
        assert_eq!(v.len(), 1, "glob should restrict to .rs only: {:?}", v);
        if let Value::Object(o) = &v[0] {
            assert!(o
                .borrow()
                .get("path")
                .unwrap()
                .to_string()
                .contains("keep.rs"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_skips_non_utf8_files() {
        let dir = temp_dir("grep_binary");
        // a binary file with invalid UTF-8 bytes
        std::fs::write(dir.join("bin.dat"), [0xFF, 0xFE, 0x00, 0x01]).unwrap();
        call(
            "write",
            &[s(&path_str(&dir.join("text.txt"))), s("findme")],
            sp(),
        )
        .unwrap();
        let g = call("grep", &[s("findme"), s(&path_str(&dir))], sp()).unwrap();
        // should not error, and finds the one text match
        let matches = unwrap_pair_ok(&g);
        if let Value::Array(a) = &matches {
            assert_eq!(a.borrow().len(), 1);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grep_bad_pattern_is_tier1_err() {
        let dir = temp_dir("grep_badpat");
        let g = call("grep", &[s("("), s(&path_str(&dir))], sp()).unwrap();
        match &g {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a[0], Value::Nil);
                assert!(matches!(a[1], Value::Object(_)));
            }
            other => panic!("expected pair, got {:?}", other),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_non_string_path_is_tier2_panic() {
        assert!(matches!(
            call("read", &[Value::Number(1.0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn write_non_data_is_tier2_panic() {
        let dir = temp_dir("write_panic");
        let f = path_str(&dir.join("x.txt"));
        assert!(matches!(
            call("write", &[s(&f), Value::Number(1.0)], sp()),
            Err(Control::Panic(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Interpreter-level end-to-end tests (write a file, read it, grep it, print).
#[cfg(test)]
mod e2e {
    #[tokio::test]
    async fn fs_write_read_grep_e2e() {
        let dir = std::env::temp_dir().join(format!("ascript_fs_e2e_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("note.txt");
        let fp = f.to_string_lossy().into_owned();
        let dp = dir.to_string_lossy().into_owned();

        let src = format!(
            r#"
import {{ write, read, grep }} from "std/fs"
let [_, werr] = write("{fp}", "first line\nTODO: ship it\nlast line")
print(werr)
let [text, rerr] = read("{fp}")
print(text)
let [matches, gerr] = grep("TODO", "{dp}")
print(len(matches))
print(matches[0].line)
print(matches[0].text)
"#,
            fp = fp,
            dp = dp,
        );

        let out = crate::run_source(&src).await.expect("program should run");
        assert_eq!(
            out,
            "nil\nfirst line\nTODO: ship it\nlast line\n1\n2\nTODO: ship it\n"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
