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
    #[cfg(unix)]
    stop_on_signals();
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

/// Stops the server on SIGTERM and SIGINT. As process 1 of a container the
/// kernel ignores signals that have no handler, so without this the server
/// would only end when killed; with it the container needs no init process.
#[cfg(unix)]
fn stop_on_signals() {
    // SAFETY: plain libc calls on a local signal set. The signals are blocked
    // before any other thread starts, so all threads inherit the mask and
    // only the waiting thread takes them.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        let _ = std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                let mut sig = 0;
                if libc::sigwait(&set, &mut sig) == 0 {
                    beatlumen_server::info!("stopping (signal {sig})");
                    std::process::exit(0);
                }
            });
    }
}
