//! The shared way to the card on Linux: PipeWire stays in charge, its graph runs
//! at the song's rate (set by the platform when the output opens), so nothing is
//! converted and other programs stay audible. Whether the samples really arrive
//! as they left is not ours to promise: this looks at what the graph and the
//! card are doing right now, and the signal path says what it finds.

use bl_json::Value;

/// The device id of this way (the ALSA device PipeWire offers).
pub const DEVICE: &str = "pipewire";

/// What is true of the output right now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SinkFacts {
    /// A sound card and not a virtual device, Bluetooth or a filter.
    pub hardware: bool,
    /// What the card is running at, when it could be read.
    pub card_rate: Option<u32>,
    pub card_bits: Option<u8>,
    /// PipeWire's own volume on this output is not at 100 %.
    pub soft_volume: bool,
    /// Other programs connected to this output and running.
    pub others: u32,
}

fn number(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// What `pw-dump` says about the output `own_pid` plays on. `card_rate` and
/// `card_bits` are not in there, see [`read_card`].
pub(crate) fn parse_dump(dump: &Value, own_pid: u64) -> Option<(SinkFacts, Option<(u64, u64)>)> {
    let objects = dump.as_array()?;
    let of_type = |t: &'static str| {
        objects
            .iter()
            .filter(move |o| o.str_or("type", "").ends_with(t))
    };
    let links: Vec<(u64, u64)> = of_type(":Link")
        .filter_map(|l| {
            let i = l.get("info");
            Some((
                i.get("output-node-id").as_u64()?,
                i.get("input-node-id").as_u64()?,
            ))
        })
        .collect();
    let class_of = |n: &Value| {
        n.get("info")
            .get("props")
            .str_or("media.class", "")
            .to_string()
    };
    // A stream names its client, the client the process.
    let own_clients: Vec<u64> = of_type(":Client")
        .filter(|c| {
            number(c.get("info").get("props").get("application.process.id")) == Some(own_pid)
        })
        .filter_map(|c| c.get("id").as_u64())
        .collect();
    let own = of_type(":Node")
        .find(|n| {
            class_of(n) == "Stream/Output/Audio"
                && number(n.get("info").get("props").get("client.id"))
                    .is_some_and(|c| own_clients.contains(&c))
        })
        .and_then(|n| n.get("id").as_u64());
    // The output it is linked to, else the one it would go to.
    let linked = own.and_then(|o| links.iter().find(|(a, _)| *a == o).map(|(_, b)| *b));
    let default_name = of_type(":Metadata")
        .filter(|m| m.get("props").str_or("metadata.name", "") == "default")
        .flat_map(|m| m.get("metadata").as_array().into_iter().flatten())
        .find(|e| e.str_or("key", "") == "default.audio.sink")
        .map(|e| e.get("value").str_or("name", "").to_string());
    let sink = of_type(":Node").find(|n| match linked {
        Some(id) => n.get("id").as_u64() == Some(id),
        None => default_name
            .as_deref()
            .is_some_and(|d| n.get("info").get("props").str_or("node.name", "") == d),
    })?;
    let sink_id = sink.get("id").as_u64()?;
    let props = sink.get("info").get("props");
    let hardware = class_of(sink) == "Audio/Sink" && props.str_or("device.api", "") == "alsa";
    let card = number(props.get("alsa.card")).zip(number(props.get("alsa.device")));
    let volumes = sink.get("info").get("params").get("Props").at(0);
    let soft = match volumes.get("softVolumes").as_array() {
        Some(_) => volumes.get("softVolumes"),
        None => volumes.get("channelVolumes"),
    };
    let soft_volume = volumes.bool_or("softMute", false)
        || volumes.bool_or("mute", false)
        || soft
            .as_array()
            .into_iter()
            .flatten()
            .any(|x| x.as_f32().is_some_and(|x| (x - 1.0).abs() > 0.001));
    let mut others: Vec<u64> = links
        .iter()
        .filter(|(from, to)| *to == sink_id && Some(*from) != own)
        .map(|(from, _)| *from)
        .collect();
    others.sort_unstable();
    others.dedup();
    others.retain(|id| {
        of_type(":Node").any(|n| {
            n.get("id").as_u64() == Some(*id)
                && class_of(n) == "Stream/Output/Audio"
                && n.get("info").str_or("state", "") == "running"
        })
    });
    Some((
        SinkFacts {
            hardware,
            card_rate: None,
            card_bits: None,
            soft_volume,
            others: others.len() as u32,
        },
        card,
    ))
}

/// What a running card is doing, from the kernel's own report.
pub(crate) fn parse_hw_params(text: &str) -> (Option<u32>, Option<u8>) {
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim())
    };
    let rate = value("rate:").and_then(|v| v.split_whitespace().next()?.parse().ok());
    let bits = value("format:").and_then(|f| match f {
        "S16_LE" => Some(16),
        "S24_LE" | "S24_3LE" => Some(24),
        "S32_LE" => Some(32),
        _ => None,
    });
    (rate, bits)
}

/// Looks at the graph (runs `pw-dump`: on a background thread).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn probe() -> Option<SinkFacts> {
    let out = std::process::Command::new("pw-dump").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let dump = bl_json::parse_bytes(&out.stdout).ok()?;
    let (mut facts, card) = parse_dump(&dump, std::process::id() as u64)?;
    if let Some((card, device)) = card {
        let path = format!("/proc/asound/card{card}/pcm{device}p/sub0/hw_params");
        if let Ok(text) = std::fs::read_to_string(path) {
            (facts.card_rate, facts.card_bits) = parse_hw_params(&text);
        }
    }
    Some(facts)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn probe() -> Option<SinkFacts> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cut down from what `pw-dump` really writes.
    fn dump(others_state: &str, soft: &str, api: &str) -> Value {
        bl_json::parse(&format!(
            r#"[
          {{"id":32,"type":"PipeWire:Interface:Metadata","props":{{"metadata.name":"default"}},
            "metadata":[{{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{{"name":"alsa_output.card"}}}}]}},
          {{"id":34,"type":"PipeWire:Interface:Node","info":{{"state":"running",
            "props":{{"node.name":"alsa_output.card","media.class":"Audio/Sink","device.api":"{api}","alsa.card":1,"alsa.device":0}},
            "params":{{"Props":[{{"volume":1.0,"mute":false,"channelVolumes":[1.0,1.0],"softVolumes":[{soft},{soft}],"softMute":false}}]}}}}}},
          {{"id":70,"type":"PipeWire:Interface:Node","info":{{"state":"suspended",
            "props":{{"node.name":"other.sink","media.class":"Audio/Sink","device.api":"alsa"}}}}}},
          {{"id":29,"type":"PipeWire:Interface:Client","info":{{"props":{{"application.process.id":4242}}}}}},
          {{"id":30,"type":"PipeWire:Interface:Client","info":{{"props":{{"application.process.id":77}}}}}},
          {{"id":100,"type":"PipeWire:Interface:Node","info":{{"state":"running",
            "props":{{"media.class":"Stream/Output/Audio","client.id":29}}}}}},
          {{"id":101,"type":"PipeWire:Interface:Node","info":{{"state":"{others_state}",
            "props":{{"media.class":"Stream/Output/Audio","client.id":30}}}}}},
          {{"id":102,"type":"PipeWire:Interface:Node","info":{{"state":"running",
            "props":{{"media.class":"Stream/Output/Audio","client.id":30}}}}}},
          {{"id":200,"type":"PipeWire:Interface:Link","info":{{"output-node-id":100,"input-node-id":34,"state":"active"}}}},
          {{"id":201,"type":"PipeWire:Interface:Link","info":{{"output-node-id":100,"input-node-id":34,"state":"active"}}}},
          {{"id":202,"type":"PipeWire:Interface:Link","info":{{"output-node-id":101,"input-node-id":34,"state":"active"}}}},
          {{"id":203,"type":"PipeWire:Interface:Link","info":{{"output-node-id":101,"input-node-id":34,"state":"active"}}}},
          {{"id":204,"type":"PipeWire:Interface:Link","info":{{"output-node-id":102,"input-node-id":70,"state":"active"}}}}
        ]"#
        ))
        .unwrap()
    }

    #[test]
    fn alone_on_a_card_at_full_volume() {
        let (f, card) = parse_dump(&dump("paused", "1.0", "alsa"), 4242).unwrap();
        assert!(f.hardware);
        assert!(!f.soft_volume);
        assert_eq!(f.others, 0, "a paused stream adds nothing");
        assert_eq!(card, Some((1, 0)));
    }

    #[test]
    fn another_program_on_the_same_output_counts_once_and_one_elsewhere_not() {
        let (f, _) = parse_dump(&dump("running", "1.0", "alsa"), 4242).unwrap();
        assert_eq!(f.others, 1);
    }

    #[test]
    fn pipewires_own_volume_changes_samples() {
        let (f, _) = parse_dump(&dump("paused", "0.5", "alsa"), 4242).unwrap();
        assert!(f.soft_volume);
    }

    #[test]
    fn bluetooth_or_a_filter_is_no_card() {
        let (f, _) = parse_dump(&dump("paused", "1.0", "bluez5"), 4242).unwrap();
        assert!(!f.hardware);
    }

    #[test]
    fn before_our_stream_exists_it_is_the_default_output() {
        let (f, card) = parse_dump(&dump("running", "1.0", "alsa"), 9).unwrap();
        assert!(f.hardware);
        assert_eq!(card, Some((1, 0)));
        // All who are running there count then.
        assert!(f.others >= 1);
    }

    /// Against a real `pw-dump`: `BL_PW_DUMP=<file> cargo test real_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_dump() {
        let data = std::fs::read(std::env::var("BL_PW_DUMP").unwrap()).unwrap();
        let v = bl_json::parse_bytes(&data).unwrap();
        println!("{:?}", parse_dump(&v, 1));
    }

    #[test]
    fn nonsense_is_nothing() {
        assert_eq!(parse_dump(&bl_json::parse("{}").unwrap(), 1), None);
        assert_eq!(parse_dump(&bl_json::parse("[]").unwrap(), 1), None);
    }

    #[test]
    fn what_the_kernel_says_about_a_card() {
        let running = "access: MMAP_INTERLEAVED\nformat: S32_LE\nsubformat: STD\nchannels: 10\nrate: 48000 (48000/1)\nperiod_size: 512\n";
        assert_eq!(parse_hw_params(running), (Some(48000), Some(32)));
        assert_eq!(
            parse_hw_params("format: S24_3LE\nrate: 96000 (96000/1)\n"),
            (Some(96000), Some(24))
        );
        assert_eq!(parse_hw_params("closed\n"), (None, None));
    }
}
