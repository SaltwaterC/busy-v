fn main() {
    std::process::exit(busy_v::run(std::env::args().skip(1).collect()));
}
