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
        Action::Invite(cfg, days) => {
            if !cfg.registration {
                eprintln!("warning: registration is off (--registration off); this code works only after turning it on");
            }
            match beatlumen_server::vault::create_invite(&cfg.data_dir, days) {
                Ok(code) => {
                    println!("{code}");
                    eprintln!("Invitation for one account, valid for {days} days. Enter it in the app when creating the account.");
                    return;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Action::WebManifest { dir, version } => {
            match beatlumen_server::release::write_manifest(&dir, &version) {
                Ok(msg) => {
                    println!("{msg}");
                    return;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Action::ReleaseKeygen => match beatlumen_server::release::keygen() {
            Ok((secret, public)) => {
                println!("BEATLUMEN_RELEASE_SIGNING_KEY={secret}");
                println!("BEATLUMEN_RELEASE_KEY={public}");
                eprintln!("Keep the signing key secret (CI secret); build official apps and servers with BEATLUMEN_RELEASE_KEY set.");
                return;
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Action::AdminLink(cfg) => {
            match beatlumen_server::admin::create_setup_token(
                &cfg.data_dir,
                beatlumen_server::admin::LINK_SETUP_SECS,
            ) {
                Ok(token) => {
                    println!("/admin/#setup={token}");
                    eprintln!("Open this path on the server's address within an hour, e.g. http://<server>:7878/admin/#setup={token}, to set the admin password.");
                    return;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Action::Users(cfg) => {
            for u in beatlumen_server::vault::list_users(&cfg.data_dir) {
                println!("{u}");
            }
            return;
        }
        Action::HealthCheck(cfg) => {
            std::process::exit(if beatlumen_server::health_check(&cfg) {
                0
            } else {
                1
            });
        }
    };
    match beatlumen_server::start(cfg) {
        Ok(server) => {
            server
                .state
                .set_restart_handler(Box::new(beatlumen_server::restart_process));
            server.join()
        }
        Err(e) => {
            beatlumen_server::error!("{e}");
            std::process::exit(1);
        }
    }
}
