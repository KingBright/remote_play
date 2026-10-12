//! Guarded synthetic-window input diagnostic using the production RemotePlay SDK.
mod guarded_input;
fn main() { guarded_input::main_cli(&std::env::args().collect::<Vec<_>>()); }
