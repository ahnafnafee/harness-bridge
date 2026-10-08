mod cli;
mod family;
mod ir;
mod providers;
mod resume;
mod util;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
