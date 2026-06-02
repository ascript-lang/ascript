//! Smoke test: resolution runs over the whole example corpus without panicking
//! and produces a frame for every file. Correctness of individual resolutions is
//! unit-tested in src/syntax/resolve.

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension().and_then(|x| x.to_str()) == Some("as") { out.push(p); }
        }
    }
    let mut v = Vec::new();
    walk(Path::new("examples"), &mut v);
    v.sort();
    v
}

#[test]
fn resolve_runs_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let r = ascript::syntax::resolve_source(&src);
        assert!(!r.frames.is_empty(), "no frames for {}", path.display());
    }
}
