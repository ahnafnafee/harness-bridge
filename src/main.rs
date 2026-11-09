fn main() {
    if let Err(error) = harness_bridge::cli::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
