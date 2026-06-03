fn main() {
    if let Err(error) = crossbow_switch::cli::run_from_env() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
