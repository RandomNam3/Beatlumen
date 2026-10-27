//! `beatlumen-server` command line entry point.

use beatlumen_server::config::{self, Action};

fn main() {
    let action = match config::parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let cfg = match action {
        Action::Help => {
            print!("{}", config::USAGE);
            return;
        }
        Action::Version => {
            println!("beatlumen-server {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Action::Run(cfg) => *cfg,
    };
    match beatlumen_server::start(cfg) {
        Ok(server) => server.join(),
        Err(e) => {
            beatlumen_server::error!("{e}");
            std::process::exit(1);
        }
    }
}
