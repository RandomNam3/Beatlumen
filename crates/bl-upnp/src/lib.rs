//! A UPnP AV / DLNA control point: finds media renderers in the local
//! network (TVs, AV receivers, Sonos, …) and plays a stream on them.
//!
//! * [`ssdp::discover`] sends an M-SEARCH and collects the answers.
//! * [`Renderer::fetch`] reads a renderer's device description.
//! * [`Renderer`]'s methods call the AVTransport and RenderingControl
//!   services (SOAP).

pub mod renderer;
pub mod ssdp;
pub mod xml;

use bl_net::client::{self, Options, Url};
use std::time::Duration;

const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RENDERING_CONTROL: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const CONNECTION_MANAGER: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

/// A media renderer and the control URLs of its services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renderer {
    /// Unique device name ("uuid:…").
    pub udn: String,
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub location: String,
    av_transport: String,
    rendering_control: Option<String>,
    connection_manager: Option<String>,
}

fn opts() -> Options {
    Options {
        connect_timeout: Duration::from_secs(3),
        io_timeout: Duration::from_secs(5),
        max_body: 512 * 1024,
        max_redirects: 2,
    }
}

impl Renderer {
    /// Reads the device description at `location`; None when it is not a
    /// media renderer with an AVTransport service.
    pub fn fetch(location: &str) -> Result<Renderer, String> {
        let r = client::request("GET", location, &[], &[], &opts())?;
        if r.status != 200 {
            return Err(format!("HTTP {}", r.status));
        }
        let text = String::from_utf8_lossy(&r.body).into_owned();
        Renderer::parse(location, &text)
    }

    pub fn parse(location: &str, text: &str) -> Result<Renderer, String> {
        let base = Url::parse(location)?;
        // A URLBase element overrides the description's address.
        let base = xml::text(text, "URLBase")
            .and_then(|u| Url::parse(&u).ok())
            .unwrap_or(base);
        // The renderer may be an embedded device: take the first device
        // that has an AVTransport service.
        let devices = xml::elements(text, "device");
        let device = devices
            .iter()
            .find(|d| d.contains(AV_TRANSPORT))
            .ok_or("no AVTransport service")?;
        let control = |service: &str| {
            xml::elements(device, "service")
                .into_iter()
                .find(|s| {
                    xml::text(s, "serviceType")
                        .is_some_and(|t| t.starts_with(&service[..service.len() - 1]))
                })
                .and_then(|s| xml::text(s, "controlURL"))
                .map(|u| base.join(u.trim()))
        };
        Ok(Renderer {
            udn: xml::text(device, "UDN").unwrap_or_default(),
            name: xml::text(device, "friendlyName").unwrap_or_else(|| "Renderer".into()),
            manufacturer: xml::text(device, "manufacturer").unwrap_or_default(),
            model: xml::text(device, "modelName").unwrap_or_default(),
            location: location.to_string(),
            av_transport: control(AV_TRANSPORT).ok_or("no AVTransport control URL")?,
            rendering_control: control(RENDERING_CONTROL),
            connection_manager: control(CONNECTION_MANAGER),
        })
    }

    /// The renderer's host (for picking the local address it can reach).
    pub fn host(&self) -> Option<String> {
        Url::parse(&self.av_transport).ok().map(|u| u.host)
    }

    /// Calls a SOAP action of a UPnP service; returns the answer's XML.
    pub fn soap(
        url: &str,
        service: &str,
        action: &str,
        args: &[(&str, &str)],
    ) -> Result<String, String> {
        let mut body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">"
        );
        for (k, v) in args {
            body.push_str(&format!("<{k}>{}</{k}>", xml::escape(v)));
        }
        body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
        let headers = vec![
            (
                "Content-Type".to_string(),
                "text/xml; charset=\"utf-8\"".to_string(),
            ),
            ("SOAPAction".to_string(), format!("\"{service}#{action}\"")),
        ];
        let r = client::request("POST", url, &headers, body.as_bytes(), &opts())?;
        let text = String::from_utf8_lossy(&r.body).into_owned();
        if r.status != 200 {
            let detail = xml::text(&text, "errorDescription")
                .or_else(|| xml::text(&text, "errorCode"))
                .unwrap_or_else(|| format!("HTTP {}", r.status));
            return Err(format!("{action}: {detail}"));
        }
        Ok(text)
    }

    fn transport(&self, action: &str, args: &[(&str, &str)]) -> Result<String, String> {
        let mut all = vec![("InstanceID", "0")];
        all.extend_from_slice(args);
        Self::soap(&self.av_transport, AV_TRANSPORT, action, &all)
    }

    /// Sets the stream to play (`metadata`: DIDL-Lite, may be empty).
    pub fn set_uri(&self, uri: &str, metadata: &str) -> Result<(), String> {
        self.transport(
            "SetAVTransportURI",
            &[("CurrentURI", uri), ("CurrentURIMetaData", metadata)],
        )
        .map(|_| ())
    }

    pub fn play(&self) -> Result<(), String> {
        self.transport("Play", &[("Speed", "1")]).map(|_| ())
    }

    pub fn pause(&self) -> Result<(), String> {
        self.transport("Pause", &[]).map(|_| ())
    }

    pub fn stop(&self) -> Result<(), String> {
        self.transport("Stop", &[]).map(|_| ())
    }

    /// Jumps to `secs` into the current track.
    pub fn seek(&self, secs: f64) -> Result<(), String> {
        let s = secs.max(0.0) as u64;
        let target = format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
        self.transport("Seek", &[("Unit", "REL_TIME"), ("Target", &target)])
            .map(|_| ())
    }

    /// The stream to play after the current one.
    pub fn set_next_uri(&self, uri: &str, metadata: &str) -> Result<(), String> {
        self.transport(
            "SetNextAVTransportURI",
            &[("NextURI", uri), ("NextURIMetaData", metadata)],
        )
        .map(|_| ())
    }

    /// "PLAYING", "PAUSED_PLAYBACK", "STOPPED", "TRANSITIONING", …
    pub fn transport_state(&self) -> Result<String, String> {
        let r = self.transport("GetTransportInfo", &[])?;
        Ok(xml::text(&r, "CurrentTransportState").unwrap_or_default())
    }

    /// Seconds played of the current stream, if the renderer reports it.
    pub fn position(&self) -> Result<Option<f64>, String> {
        let r = self.transport("GetPositionInfo", &[])?;
        Ok(xml::text(&r, "RelTime").and_then(|t| parse_time(&t)))
    }

    /// Volume 0..100.
    pub fn set_volume(&self, volume: u32) -> Result<(), String> {
        let Some(url) = &self.rendering_control else {
            return Ok(());
        };
        let v = volume.min(100).to_string();
        Self::soap(
            url,
            RENDERING_CONTROL,
            "SetVolume",
            &[
                ("InstanceID", "0"),
                ("Channel", "Master"),
                ("DesiredVolume", &v),
            ],
        )
        .map(|_| ())
    }

    /// The formats the renderer plays ("http-get:*:audio/mpeg:*,…").
    pub fn sink_protocols(&self) -> Result<Vec<String>, String> {
        let Some(url) = &self.connection_manager else {
            return Ok(Vec::new());
        };
        let r = Self::soap(url, CONNECTION_MANAGER, "GetProtocolInfo", &[])?;
        Ok(xml::text(&r, "Sink")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }
}

/// "H:MM:SS" or "H:MM:SS.fff" in seconds ("NOT_IMPLEMENTED": None).
pub fn parse_time(t: &str) -> Option<f64> {
    let mut parts = t.trim().split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// DIDL-Lite for a stream: title and MIME type, shown by many renderers.
pub fn didl(title: &str, artist: &str, uri: &str, protocol_info: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"1\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:artist>{}</upnp:artist><upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo=\"{}\">{}</res></item></DIDL-Lite>",
        xml::escape(title),
        xml::escape(artist),
        xml::escape(protocol_info),
        xml::escape(uri)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>Wohnzimmer &amp; TV</friendlyName>
    <manufacturer>ACME</manufacturer>
    <modelName>Renderer 9000</modelName>
    <UDN>uuid:1234</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <controlURL>/upnp/control/rendercontrol1</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>upnp/control/avtransport1</controlURL>
      </service>
    </serviceList>
  </device>
</root>"#;

    #[test]
    fn parses_description() {
        let r = Renderer::parse("http://192.168.1.20:49152/description.xml", DESCRIPTION).unwrap();
        assert_eq!(r.name, "Wohnzimmer & TV");
        assert_eq!(r.model, "Renderer 9000");
        assert_eq!(r.udn, "uuid:1234");
        assert_eq!(
            r.av_transport,
            "http://192.168.1.20:49152/upnp/control/avtransport1"
        );
        assert_eq!(
            r.rendering_control.as_deref(),
            Some("http://192.168.1.20:49152/upnp/control/rendercontrol1")
        );
        assert_eq!(r.host().as_deref(), Some("192.168.1.20"));
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("0:01:02"), Some(62.0));
        assert_eq!(parse_time("1:00:00.500"), Some(3600.5));
        assert_eq!(parse_time("NOT_IMPLEMENTED"), None);
    }
}
