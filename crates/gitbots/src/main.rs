use clap::Parser;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = gitbots::cli::Cli::parse();
    match gitbots::cli::run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gitbots: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
