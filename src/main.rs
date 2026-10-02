fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = flueny::cli::main(args);
    std::process::exit(code);
}
