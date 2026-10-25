fn main() {
    let code = telegram_mcp_stdio::entry();
    if code != 0 {
        std::process::exit(code);
    }
}
