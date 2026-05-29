use std::process::ExitCode;

// Single-threaded runtime matches spec §7's single-threaded event loop and the
// interpreter's `?Send` (Rc-friendly) futures.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 3 || args[1] != "run" {
        eprintln!("usage: ascript run <file.as>");
        return ExitCode::from(2);
    }

    let path = &args[2];
    match ascript::run_file(std::path::Path::new(path)).await {
        Ok(output) => {
            print!("{}", output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}
