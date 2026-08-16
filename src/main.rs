fn main() {
    if let Err(err) = airgap_xfer::cli::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
