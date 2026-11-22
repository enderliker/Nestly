#![forbid(unsafe_code)]

use std::process::ExitCode;

#[tokio::main(worker_threads = 2)]
async fn main() -> ExitCode {
    match rsbot_edge::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("rsbot-edge failed: {source}");
            ExitCode::FAILURE
        }
    }
}
