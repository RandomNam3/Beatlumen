//! Damaged UPnP XML from devices on the network: device descriptions,
//! DIDL-Lite metadata and SOAP answers end with a value or an error, in
//! linear time.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_upnp::renderer::Meta;
use bl_upnp::{xml, Renderer};

const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <URLBase>http://192.168.1.20:8080/</URLBase>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>Living Room &amp; Kitchen</friendlyName>
    <manufacturer>Example</manufacturer><modelName>Speaker</modelName>
    <UDN>uuid:12345678-1234-1234-1234-123456789abc</UDN>
    <serviceList>
      <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>/upnp/control/avt</controlURL></service>
      <service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <controlURL>rc</controlURL></service>
    </serviceList>
    <deviceList><device><friendlyName>inner</friendlyName></device></deviceList>
  </device>
</root>"#;

const DIDL: &str = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"><item id="1" parentID="0" restricted="1"><dc:title>Song &lt;live&gt;</dc:title><upnp:artist>Someone</upnp:artist><upnp:album>Album</upnp:album><upnp:albumArtURI>http://x/cover.jpg</upnp:albumArtURI><res protocolInfo="http-get:*:audio/flac:*" duration="0:03:35.500">http://x/song.flac</res></item></DIDL-Lite>"#;

const SOAP: &str = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetPositionInfoResponse xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><Track>1</Track><TrackDuration>0:03:35</TrackDuration><RelTime>0:01:02.250</RelTime><Sink>http-get:*:audio/mpeg:*,http-get:*:audio/flac:*</Sink></u:GetPositionInfoResponse></s:Body></s:Envelope>"#;

#[test]
fn damaged_upnp_xml_ends_cleanly() {
    let samples = vec![
        DESCRIPTION.as_bytes().to_vec(),
        DIDL.as_bytes().to_vec(),
        SOAP.as_bytes().to_vec(),
    ];
    let tokens: &[&[u8]] = &[
        b"<",
        b">",
        b"</",
        b"/>",
        b"<device>",
        b"</device>",
        b"<service>",
        b"</service>",
        b"<s:Body>",
        b"&",
        b"&amp;",
        b"&#x110000;",
        b"&#99999999999;",
        b":",
        b"\"",
        b"<?",
        b"<!",
        b"\xc3\xa4",
        b" ",
    ];
    fuzz::run("upnp-xml", &samples, tokens, 20_000, |d| {
        let text = String::from_utf8_lossy(d);
        let _ = Renderer::parse("http://192.168.1.20:49152/description.xml", &text);
        let _ = Renderer::parse("not a url", &text);
        let m = Meta::parse(&text);
        let _ = bl_upnp::didl(&m.title, &m.artist, &m.art_url, &m.mime);
        for tag in ["RelTime", "TrackDuration", "Sink", "device", "b"] {
            if let Some(t) = xml::text(&text, tag) {
                let _ = bl_upnp::parse_time(&t);
                assert_eq!(xml::unescape(&xml::escape(&t)), t);
            }
        }
    });
}
