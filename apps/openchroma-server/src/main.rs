//! `openchroma-server` command line entry point.

use openchroma_server::config::{self, Action};

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
            println!("openchroma-server {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Action::Run(cfg) => *cfg,
    };
    match openchroma_server::start(cfg) {
        Ok(server) => server.join(),
        Err(e) => {
            openchroma_server::error!("{e}");
            std::process::exit(1);
        }
    }
}
