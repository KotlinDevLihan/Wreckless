#[cfg(not(target_arch = "wasm32"))]
fn main() {
    // Joined into a single command line, not one command per argument.
    //
    // `message_loop` treats each buffer entry as a whole command, so collecting
    // `args()` directly made `wreckless bench 128 1 10` four commands: `bench`
    // with no arguments -- silently taking every default -- followed by `128`,
    // `1` and `10` as unknown commands. Every documented CLI form was affected:
    // `bench <hash> <threads> <depth>`, `speedtest <threads> <hash> <seconds>`
    // and `perft <depth>`, the last visibly, since it printed its own usage line
    // and then rejected its own argument.
    //
    // The same text piped to stdin always worked, which is why this survived:
    // that path receives one line and splits it, and it is the path UCI uses.
    let arguments = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    let buffer = if arguments.trim().is_empty() {
        std::collections::VecDeque::new()
    } else {
        std::collections::VecDeque::from([arguments])
    };

    wreckless::run(buffer);
}
