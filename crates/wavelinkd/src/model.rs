//! Mixer model: channels × mixes with a gain per crosspoint.
//! Mirrors the `channels` / `mixes` / `channelToMix` shape of Wave Link 3's
//! `config.json` (see docs/wavelink3-architecture.md).

/// Where a channel's audio comes from.
pub enum Source {
    /// A capture device, matched by node.name prefix (e.g. the Wave:3 mic).
    Device { node_prefix: &'static str },
    /// A virtual sink ("Wave Link Music") that apps play into.
    Virtual,
}

pub struct Channel {
    pub id: &'static str,
    pub label: &'static str,
    pub source: Source,
}

/// What a mix publishes.
#[derive(PartialEq)]
pub enum MixKind {
    /// A virtual source that OBS / Zoom record from.
    Source,
    /// A playback stream that the session manager routes to the default output.
    Monitor,
}

pub struct Mix {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: MixKind,
}

pub struct Model {
    pub channels: Vec<Channel>,
    pub mixes: Vec<Mix>,
    /// gains[mix][channel], linear (1.0 = 0 dB).
    pub gains: Vec<Vec<f32>>,
}

/// The built-in filter-graph mixer has 8 inputs.
pub const MAX_CHANNELS: usize = 8;

impl Model {
    pub fn default_layout() -> Self {
        let channels = vec![
            Channel { id: "mic", label: "Microphone", source: Source::Device { node_prefix: "alsa_input.usb-Elgato_Systems_Elgato_Wave_3" } },
            Channel { id: "system", label: "System", source: Source::Virtual },
            Channel { id: "music", label: "Music", source: Source::Virtual },
            Channel { id: "browser", label: "Browser", source: Source::Virtual },
            Channel { id: "voicechat", label: "Voice Chat", source: Source::Virtual },
        ];
        let mixes = vec![
            Mix { id: "personal", label: "Personal Mix", kind: MixKind::Monitor },
            Mix { id: "stream", label: "Stream Mix", kind: MixKind::Source },
            Mix { id: "chat", label: "Chat Mix", kind: MixKind::Source },
        ];
        // Everything at 0 dB, except: don't monitor your own mic, and don't
        // send voice chat back into the chat mix (echo for the other side).
        let gains = mixes
            .iter()
            .map(|m| {
                channels
                    .iter()
                    .map(|c| match (m.id, c.id) {
                        ("personal", "mic") | ("chat", "voicechat") => 0.0,
                        _ => 1.0,
                    })
                    .collect()
            })
            .collect();
        Self { channels, mixes, gains }
    }

    pub fn sink_node_name(ch: &Channel) -> String {
        format!("wavelink.channel.{}", ch.id)
    }
    pub fn mix_in_node_name(m: &Mix) -> String {
        format!("wavelink.mix.{}.in", m.id)
    }
    pub fn mix_out_node_name(m: &Mix) -> String {
        format!("wavelink.mix.{}", m.id)
    }

    /// Capture port of mix input for channel `i`, side 0 = L, 1 = R.
    pub fn mix_in_port(i: usize, side: usize) -> String {
        format!("input_AUX{}", 2 * i + side)
    }

    /// SPA-JSON args for libpipewire-module-filter-chain implementing one mix:
    /// two builtin mixers (L, R), input 2i/2i+1 = channel i.
    pub fn filter_chain_args(&self, mi: usize) -> String {
        let m = &self.mixes[mi];
        let n = self.channels.len();
        assert!(n <= MAX_CHANNELS);
        let controls: String =
            (0..n).map(|i| format!("\"Gain {}\" = {} ", i + 1, self.gains[mi][i])).collect();
        let inputs: String =
            (1..=n).map(|i| format!("\"mixL:In {i}\" \"mixR:In {i}\" ")).collect();
        let positions: String = (0..2 * n).map(|i| format!("AUX{i} ")).collect();
        let playback = match m.kind {
            MixKind::Source => format!(
                "node.name = \"{}\" node.description = \"Wave Link {}\" media.class = \"Audio/Source\"",
                Self::mix_out_node_name(m),
                m.label
            ),
            MixKind::Monitor => format!(
                "node.name = \"{}\" node.description = \"Wave Link {}\"",
                Self::mix_out_node_name(m),
                m.label
            ),
        };
        format!(
            r#"{{
  node.description = "Wave Link {label}"
  media.name = "Wave Link {label}"
  filter.graph = {{
    nodes = [
      {{ name = mixL type = builtin label = mixer control = {{ {controls}}} }}
      {{ name = mixR type = builtin label = mixer control = {{ {controls}}} }}
    ]
    inputs = [ {inputs}]
    outputs = [ "mixL:Out" "mixR:Out" ]
  }}
  capture.props = {{
    node.name = "{in_name}"
    node.description = "Wave Link {label} (input)"
    audio.position = [ {positions}]
    node.autoconnect = false
    stream.dont-remix = true
  }}
  playback.props = {{
    {playback}
    audio.position = [ FL FR ]
  }}
}}"#,
            label = m.label,
            in_name = Self::mix_in_node_name(m),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_shape() {
        let m = Model::default_layout();
        let a = m.filter_chain_args(1);
        assert!(a.contains("\"mixL:In 5\" \"mixR:In 5\""));
        assert!(a.contains("AUX9 ]"));
        assert!(a.contains("media.class = \"Audio/Source\""));
        let p = m.filter_chain_args(0);
        assert!(p.contains("\"Gain 1\" = 0 "), "mic muted in personal: {p}");
        assert!(!p.contains("media.class"));
    }
}
