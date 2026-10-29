//! Finds an announced receiver through mDNS (needs multicast on the local
//! machine, so not run by default: `cargo test -- --ignored`).

use bl_airplay::mdns::{browse, Responder, Service};
use std::time::Duration;

#[test]
#[ignore]
fn finds_announced_service() {
    let _r = Responder::start(
        Service {
            instance: "0A1B2C3D4E5F@Test Speaker".into(),
            service_type: "_raop._tcp".into(),
            port: 5099,
            txt: vec!["et=0,1".into(), "cn=0,1".into(), "md=0,1,2".into()],
        },
        "bl-browse-test".into(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let found = browse("_raop._tcp", Duration::from_secs(3)).unwrap();
    let f = found
        .iter()
        .find(|f| f.instance == "0A1B2C3D4E5F@Test Speaker")
        .unwrap_or_else(|| panic!("{found:?}"));
    assert_eq!(f.port, 5099);
    assert_eq!(f.txt_value("et"), Some("0,1"));
}
