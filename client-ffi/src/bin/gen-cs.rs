//! Prints the C# bindings for the plugin (`lattice_client::csharp`), or
//! writes them to the path given:
//!
//!   cargo run -p lattice-client-ffi --bin gen-cs -- [OUT.cs]

fn main() {
    let cs = lattice_client::csharp::generate();
    match std::env::args().nth(1) {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, cs) {
                eprintln!("error: {path}: {e}");
                std::process::exit(1);
            }
        }
        None => print!("{cs}"),
    }
}
