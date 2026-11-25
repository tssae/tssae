mod call_effects;
mod calls_values;
mod cfg;
mod directives;
mod instance_identity;
mod native_adapter;
mod observe;
mod package_impl;
mod projection;
mod projects;
mod serve;

use std::{env, ffi::OsString, process::ExitCode, thread};

/// TypeScript's checker recurses deeply on real-world graphs; run analysis on a 512 MiB stack.
const WORKER_STACK_BYTES: usize = 512 << 20;

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();

    if args.len() == 1 && args[0] == "--version" {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if args.len() == 2 && args[0] == "serve" && args[1] == "--stdio" {
        let builder = thread::Builder::new()
            .name("tssae-engine-worker".to_string())
            .stack_size(WORKER_STACK_BYTES);
        let handle = match builder.spawn(serve::run_stdio) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("Failed to spawn tssae-engine worker thread: {err}");
                return ExitCode::from(2);
            }
        };
        return match handle.join() {
            Ok(Ok(())) => ExitCode::SUCCESS,
            Ok(Err(err)) => {
                eprintln!("Engine stdio server failed: {err}");
                ExitCode::from(2)
            }
            Err(_) => {
                eprintln!("Engine worker thread panicked.");
                ExitCode::from(2)
            }
        };
    }

    eprintln!("Usage: tssae-engine (--version | serve --stdio)");
    ExitCode::from(2)
}
