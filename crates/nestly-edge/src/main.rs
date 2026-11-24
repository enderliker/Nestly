#![forbid(unsafe_code)]

use std::process::ExitCode;

#[tokio::main(worker_threads = 2)]
async fn main() -> ExitCode {
    match nestly_edge::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("nestly-edge failed: {source}");
            ExitCode::FAILURE
        }
    }
}
