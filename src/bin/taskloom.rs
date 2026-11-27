#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(error) = taskloom::runtime::app::cli::run(std::env::args().skip(1)).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
