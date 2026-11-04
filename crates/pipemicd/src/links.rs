//! Registry-driven graph keeper:
//! - creates the wanted port-to-port links whenever their ports exist
//!   (re-creating them after e.g. a mic replug);
//! - binds the mix input nodes so gains can be pushed at runtime;
//! - removes feedback links (a mix playing into a Pipemic channel);
//! - routes app streams to channels via the `default` metadata
//!   (`target.object`, same as pavucontrol), once per stream.

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::model::{Config, Router};

pub enum NodeMatch {
    Exact(String),
    Prefix(String),
}

impl NodeMatch {
    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(n) => name == n,
            Self::Prefix(p) => name.starts_with(p.as_str()),
        }
    }
}

/// One wanted link. `out_ports` are candidates; the first that exists wins
/// (e.g. `capture_MONO`, else `capture_FL`).
pub struct Want {
    pub out_node: NodeMatch,
    pub out_ports: Vec<String>,
    pub in_node: String,
    pub in_port: String,
}

struct Port {
    node: u32,
    name: String,
    out: bool,
}


struct NodeInfo {
    serial: Option<String>,
    is_app_stream: bool,
    /// Hardware (non Pipemic) Audio/Sink: a possible mix output.
    is_output: bool,
    description: Option<String>,
    /// Display name, then application.name, application.process.binary,
    /// node.name, portal app id.
    keys: Vec<String>,
    icon: Option<String>,
    /// Categories= of the app's desktop entry.
    categories: Vec<String>,
}

/// A running app (all its streams), as shown in the app picker.
pub struct LiveApp {
    pub name: String,
    pub keys: Vec<String>,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    /// Any of its streams is making sound.
    pub playing: bool,
}

/// Name to show for a stream: application.name without the
/// "PipeWire ALSA [x]" wrapper, else the binary, else node.name.
fn display_name(app: Option<&str>, binary: Option<&str>, node: &str) -> String {
    if let Some(a) = app {
        let a = a.strip_prefix("PipeWire ALSA [").and_then(|r| r.strip_suffix(']')).unwrap_or(a);
        if !a.is_empty() {
            return a.to_owned();
        }
    }
    binary.unwrap_or(node).to_owned()
}

/// Desktop helpers that only play UI sounds; not worth a place in the picker.
const HIDDEN_APPS: &[&str] = &["kded6", "org_kde_powerdevil", "polkit-kde-authentication-agent-1", "plasmashell"];

#[derive(Default)]
struct State {
    nodes: HashMap<u32, String>,
    info: HashMap<u32, NodeInfo>,
    /// stream id -> sink node name we sent it to
    moved: HashMap<u32, String>,
    metadata: Option<pw::metadata::Metadata>,
    ports: HashMap<u32, Port>,
    /// (out port id, in port id) -> link proxy
    links: HashMap<(u32, u32), pw::link::Link>,
    /// node.name -> bound proxy, for nodes we push params to
    bound: HashMap<String, (u32, pw::node::Node)>,
    router: Router,
    desktops: Vec<desktop_apps::Entry>,
    scanned: Option<std::time::Instant>,
    output_fallback: bool,
    /// Stream node.name -> sink node.name it should play to (None = default).
    pins: HashMap<String, Option<String>>,
    /// stream id -> sink serial we last set for its pin
    pinned: HashMap<u32, Option<String>>,
    /// App-meter slot -> app stream node id linked into it.
    slots: Vec<Option<u32>>,
    app_peaks: Option<std::sync::Arc<Vec<std::sync::atomic::AtomicU32>>>,
}

impl State {
    /// Link each app stream's first output port into a free app-meter slot.
    fn probe_apps(&mut self, core: &pw::core::CoreRc) {
        let Some(meter) = self.nodes.iter().find(|(_, n)| *n == crate::meter::APP_NODE_NAME).map(|(&id, _)| id) else { return };
        if self.slots.is_empty() {
            self.slots = vec![None; crate::meter::APP_SLOTS];
        }
        let apps: Vec<u32> = self
            .info
            .iter()
            .filter(|(id, i)| i.is_app_stream && !self.slots.contains(&Some(**id)) && !i.keys.iter().any(|k| HIDDEN_APPS.contains(&k.as_str())))
            .map(|(&id, _)| id)
            .collect();
        for node in apps {
            let Some(slot) = self.slots.iter().position(Option::is_none) else { return };
            let mut outs: Vec<(&u32, &Port)> = self.ports.iter().filter(|(_, p)| p.node == node && p.out && !p.name.starts_with("monitor")).collect();
            outs.sort_by(|a, b| a.1.name.cmp(&b.1.name));
            let Some((&oport, _)) = outs.first() else { continue };
            let want = format!("input_AUX{slot}");
            let Some((&iport, _)) = self.ports.iter().find(|(_, p)| p.node == meter && !p.out && p.name == want) else { return };
            match core.create_object::<pw::link::Link>(
                "link-factory",
                &properties! {
                    "link.output.node" => node.to_string(),
                    "link.output.port" => oport.to_string(),
                    "link.input.node" => meter.to_string(),
                    "link.input.port" => iport.to_string(),
                    "object.linger" => "false",
                },
            ) {
                Ok(l) => {
                    self.links.insert((oport, iport), l);
                    self.slots[slot] = Some(node);
                    if let Some(p) = &self.app_peaks {
                        p[slot].store(0, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Err(e) => eprintln!("pipemicd: app meter link failed: {e}"),
            }
        }
    }

    /// Is stream `node` making sound right now?
    fn playing(&self, node: u32) -> bool {
        let (Some(slot), Some(p)) = (self.slots.iter().position(|s| *s == Some(node)), &self.app_peaks) else { return false };
        f32::from_bits(p[slot].load(std::sync::atomic::Ordering::Relaxed)) > crate::meter::PLAYING
    }

    fn find_port(&self, node: &NodeMatch, names: &[String], out: bool) -> Option<(u32, u32)> {
        for want in names {
            for (&pid, p) in &self.ports {
                if p.out == out && &p.name == want && self.nodes.get(&p.node).is_some_and(|n| node.matches(n)) {
                    return Some((p.node, pid));
                }
            }
        }
        None
    }

    fn serial_of(&self, node_name: &str) -> Option<String> {
        self.nodes.iter().find(|(_, n)| *n == node_name).and_then(|(id, _)| self.info.get(id)).and_then(|i| i.serial.clone())
    }

    /// Point pinned streams (the Personal mix) at their chosen output.
    fn apply_pins(&mut self) {
        let Some(md) = &self.metadata else { return };
        let mut done = Vec::new();
        for (&id, name) in &self.nodes {
            let Some(want) = self.pins.get(name) else { continue };
            let serial = want.as_deref().and_then(|s| self.serial_of(s));
            if want.is_some() && serial.is_none() && !self.output_fallback {
                continue; // chosen output not present (yet): leave it to the session manager
            }
            if self.pinned.get(&id) == Some(&serial) {
                continue;
            }
            md.set_property(id, "target.object", serial.as_ref().map(|_| "Spa:Id"), serial.as_deref());
            done.push((id, serial));
        }
        self.pinned.extend(done);
    }

    fn sink_for_stream(&self, info: &NodeInfo) -> Option<String> {
        self.router.route(&info.keys, &info.categories).map(|(c, _)| Config::sink_for(c))
    }

    fn route_streams(&mut self) {
        let Some(md) = &self.metadata else { return };
        let mut newly = Vec::new();
        for (&id, info) in &self.info {
            if !info.is_app_stream || self.moved.contains_key(&id) {
                continue;
            }
            let Some(sink) = self.sink_for_stream(info) else { continue };
            let Some(serial) = self.serial_of(&sink) else { continue };
            md.set_property(id, "target.object", Some("Spa:Id"), Some(&serial));
            eprintln!("pipemicd: {} -> {}", info.keys.first().map_or("?", |s| s.as_str()), sink);
            newly.push((id, sink));
        }
        self.moved.extend(newly);
    }

    /// Desktop entry for a stream; rescans (at most every 30 s) on a miss so
    /// freshly installed apps are found.
    fn desktop_for(&mut self, keys: &[String]) -> Option<desktop_apps::Entry> {
        if let Some(e) = desktop_apps::find(&self.desktops, keys) {
            return Some(e.clone());
        }
        if self.scanned.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(30)) {
            self.desktops = desktop_apps::scan();
            self.scanned = Some(std::time::Instant::now());
            return desktop_apps::find(&self.desktops, keys).cloned();
        }
        None
    }
}

pub struct Graph {
    state: Rc<RefCell<State>>,
    _listener: pw::registry::Listener,
    _registry: Rc<pw::registry::RegistryRc>,
}

/// Is a link from node `from` into node `to` a feedback loop we must cut?
/// Any Pipemic mix output feeding a Pipemic node.
fn is_feedback(from: &str, to: &str) -> bool {
    from.starts_with("pipemic.mix.") && !from.ends_with(".in") && to.starts_with("pipemic.") && to != crate::meter::NODE_NAME && to != crate::meter::APP_NODE_NAME
}

impl Graph {
    /// `bind_nodes`: node names to bind for `set_controls`.
    pub fn start(
        core: &pw::core::CoreRc,
        wants: Vec<Want>,
        bind_nodes: Vec<String>,
        router: Router,
        pins: HashMap<String, Option<String>>,
        output_fallback: bool,
        app_peaks: std::sync::Arc<Vec<std::sync::atomic::AtomicU32>>,
    ) -> anyhow::Result<Self> {
        let registry = Rc::new(core.get_registry_rc()?);
        let state = Rc::new(RefCell::new(State {
            router,
            pins,
            output_fallback,
            desktops: desktop_apps::scan(),
            scanned: Some(std::time::Instant::now()),
            app_peaks: Some(app_peaks),
            ..State::default()
        }));
        let wants = Rc::new(wants);

        let reconcile = {
            let state = state.clone();
            let core = core.clone();
            move || {
                let mut st = state.borrow_mut();
                for w in wants.iter() {
                    let Some((onode, oport)) = st.find_port(&w.out_node, &w.out_ports, true) else { continue };
                    let Some((inode, iport)) =
                        st.find_port(&NodeMatch::Exact(w.in_node.clone()), std::slice::from_ref(&w.in_port), false)
                    else {
                        continue;
                    };
                    if st.links.contains_key(&(oport, iport)) {
                        continue;
                    }
                    match core.create_object::<pw::link::Link>(
                        "link-factory",
                        &properties! {
                            "link.output.node" => onode.to_string(),
                            "link.output.port" => oport.to_string(),
                            "link.input.node" => inode.to_string(),
                            "link.input.port" => iport.to_string(),
                            "object.linger" => "false",
                        },
                    ) {
                        Ok(l) => {
                            st.links.insert((oport, iport), l);
                        }
                        Err(e) => eprintln!("pipemicd: link {} -> {}:{} failed: {e}", oport, w.in_node, w.in_port),
                    }
                }
            }
        };

        let probe_core = core.clone();
        let st_add = state.clone();
        let st_rm = state.clone();
        let reg = registry.clone();
        let listener = registry
            .add_listener_local()
            .global(move |g| {
                let Some(props) = g.props else { return };
                match g.type_ {
                    ObjectType::Node => {
                        let Some(name) = props.get("node.name") else { return };
                        let mut st = st_add.borrow_mut();
                        st.nodes.insert(g.id, name.to_owned());
                        let keys: Vec<String> = ["application.name", "application.process.binary", "node.name", "pipewire.access.portal.app_id"]
                            .iter()
                            .filter_map(|k| props.get(k).map(str::to_owned))
                            .collect();
                        let is_app_stream = props.get("media.class") == Some("Stream/Output/Audio")
                            && !name.starts_with("pipemic.")
                            && props.get("node.virtual") != Some("true")
                            && props.get("application.name").is_some()
                            // filter-chain / loopback halves, and streams the app pinned itself
                            && props.get("node.link-group").is_none()
                            && props.get("target.object").is_none();
                        let shown = display_name(props.get("application.name"), props.get("application.process.binary"), name);
                        let mut all: Vec<String> = std::iter::once(shown).chain(keys).collect();
                        let entry = if is_app_stream { st.desktop_for(&all) } else { None };
                        let (mut icon, mut categories) = (props.get("application.icon-name").map(str::to_owned), Vec::new());
                        if let Some(e) = entry {
                            // The desktop name leads: it's what the picker shows and rules store.
                            all.insert(0, e.name.clone());
                            all.extend(e.keys());
                            all.dedup();
                            if !e.icon.is_empty() {
                                icon = Some(e.icon.clone());
                            }
                            categories = e.categories.clone();
                        }
                        st.info.insert(
                            g.id,
                            NodeInfo {
                                serial: props.get("object.serial").map(str::to_owned),
                                is_app_stream,
                                is_output: props.get("media.class") == Some("Audio/Sink") && !name.starts_with("pipemic."),
                                description: props.get("node.description").map(str::to_owned),
                                keys: all,
                                icon,
                                categories,
                            },
                        );
                        if bind_nodes.iter().any(|b| b == name) {
                            match reg.bind::<pw::node::Node, _>(g) {
                                Ok(n) => {
                                    st.bound.insert(name.to_owned(), (g.id, n));
                                }
                                Err(e) => eprintln!("pipemicd: bind {name} failed: {e}"),
                            }
                        }
                    }
                    ObjectType::Port => {
                        let (Some(node), Some(name), Some(dir)) =
                            (props.get("node.id"), props.get("port.name"), props.get("port.direction"))
                        else {
                            return;
                        };
                        let Ok(node) = node.parse() else { return };
                        st_add.borrow_mut().ports.insert(g.id, Port { node, name: name.to_owned(), out: dir == "out" });
                    }
                    ObjectType::Metadata => {
                        if props.get("metadata.name") == Some("default") {
                            match reg.bind::<pw::metadata::Metadata, _>(g) {
                                Ok(m) => st_add.borrow_mut().metadata = Some(m),
                                Err(e) => eprintln!("pipemicd: bind default metadata failed: {e}"),
                            }
                        }
                    }
                    ObjectType::Link => {
                        let ids = (props.get("link.output.node"), props.get("link.input.node"));
                        let (Some(Ok(o)), Some(Ok(i))) = (ids.0.map(str::parse::<u32>), ids.1.map(str::parse::<u32>))
                        else {
                            return;
                        };
                        let st = st_add.borrow();
                        if let (Some(from), Some(to)) = (st.nodes.get(&o), st.nodes.get(&i)) {
                            if is_feedback(from, to) {
                                eprintln!("pipemicd: removing feedback link {from} -> {to}");
                                reg.destroy_global(g.id);
                            }
                        }
                        return;
                    }
                    _ => return,
                }
                reconcile();
                let mut st = st_add.borrow_mut();
                st.probe_apps(&probe_core);
                st.route_streams();
                st.apply_pins();
            })
            .global_remove(move |id| {
                let mut st = st_rm.borrow_mut();
                st.nodes.remove(&id);
                st.info.remove(&id);
                st.moved.remove(&id);
                st.pinned.remove(&id);
                for s in st.slots.iter_mut().filter(|s| **s == Some(id)) {
                    *s = None;
                }
                // A chosen output went away: fall back (see apply_pins).
                st.apply_pins();
                st.bound.retain(|_, (nid, _)| *nid != id);
                if st.ports.remove(&id).is_some() {
                    st.links.retain(|&(o, i), _| o != id && i != id);
                }
            })
            .register();

        Ok(Self { state, _listener: listener, _registry: registry })
    }

    /// Clear `target.object` on streams we moved, so they go back to the
    /// user's default output when the daemon stops. Needs a roundtrip after.
    pub fn release_streams(&self) {
        let st = self.state.borrow();
        if let Some(md) = &st.metadata {
            for &id in st.moved.keys().chain(st.pinned.keys()) {
                md.set_property(id, "target.object", None, None);
            }
        }
    }

    /// Running apps, one entry per display name, desktop helpers left out.
    pub fn live_apps(&self) -> Vec<LiveApp> {
        let st = self.state.borrow();
        let mut out: Vec<LiveApp> = Vec::new();
        for (&id, info) in st.info.iter().filter(|(_, i)| i.is_app_stream) {
            let name = &info.keys[0];
            let playing = st.playing(id);
            if info.keys.iter().any(|k| HIDDEN_APPS.contains(&k.as_str())) {
                continue;
            }
            match out.iter_mut().find(|a| &a.name == name) {
                Some(a) => {
                    for k in &info.keys {
                        if !a.keys.contains(k) {
                            a.keys.push(k.clone());
                        }
                    }
                    a.icon = a.icon.take().or_else(|| info.icon.clone());
                    a.playing |= playing;
                }
                None => out.push(LiveApp {
                    name: name.clone(),
                    keys: info.keys.clone(),
                    icon: info.icon.clone(),
                    categories: info.categories.clone(),
                    playing,
                }),
            }
        }
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    }

    /// Hardware outputs: (node.name, description).
    pub fn outputs(&self) -> Vec<(String, String)> {
        let st = self.state.borrow();
        let mut v: Vec<(String, String)> = st
            .info
            .iter()
            .filter(|(_, i)| i.is_output)
            .filter_map(|(id, i)| st.nodes.get(id).map(|n| (n.clone(), i.description.clone().unwrap_or_else(|| n.clone()))))
            .collect();
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    }

    /// Installed apps (visible desktop entries).
    pub fn installed(&self) -> Vec<desktop_apps::Entry> {
        self.state.borrow().desktops.iter().filter(|e| !e.hidden).cloned().collect()
    }

    /// Replace the routing and move running streams right away. Streams that
    /// keep their channel aren't touched (no relink blip).
    pub fn set_router(&self, router: Router) {
        let mut st = self.state.borrow_mut();
        st.router = router;
        let stale: Vec<u32> = st
            .moved
            .iter()
            .filter(|(id, sink)| st.info.get(id).and_then(|i| st.sink_for_stream(i)).as_ref() != Some(*sink))
            .map(|(&id, _)| id)
            .collect();
        for id in stale {
            st.moved.remove(&id);
            if let Some(md) = &st.metadata {
                md.set_property(id, "target.object", None, None);
            }
        }
        st.route_streams();
    }

    pub fn set_output_fallback(&self, on: bool) {
        let mut st = self.state.borrow_mut();
        st.output_fallback = on;
        st.pinned.clear();
        st.apply_pins();
    }

    /// Send stream `node_name` to output `sink` (node.name; None = default).
    pub fn set_pin(&self, node_name: &str, sink: Option<String>) {
        let mut st = self.state.borrow_mut();
        st.pins.insert(node_name.to_owned(), sink);
        let ids: Vec<u32> = st.nodes.iter().filter(|(_, n)| *n == node_name).map(|(id, _)| *id).collect();
        for id in ids {
            st.pinned.remove(&id);
        }
        st.apply_pins();
    }

    /// Push filter-graph control values (`"mixL:Gain 1"` = 0.5, …) to a bound node.
    pub fn set_controls(&self, node_name: &str, controls: &[(String, f32)]) -> anyhow::Result<()> {
        let st = self.state.borrow();
        let (_, node) = st.bound.get(node_name).ok_or_else(|| anyhow::anyhow!("{node_name} not bound yet"))?;
        let params: Vec<spa::pod::Value> = controls
            .iter()
            .flat_map(|(k, v)| [spa::pod::Value::String(k.clone()), spa::pod::Value::Float(*v)])
            .collect();
        let obj = spa::pod::Value::Object(spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Props,
            id: spa::sys::SPA_PARAM_Props,
            properties: vec![spa::pod::Property {
                key: spa::sys::SPA_PROP_params,
                flags: spa::pod::PropertyFlags::empty(),
                value: spa::pod::Value::Struct(params),
            }],
        });
        let bytes = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &obj)?.0.into_inner();
        let pod = spa::pod::Pod::from_bytes(&bytes).ok_or_else(|| anyhow::anyhow!("bad pod"))?;
        node.set_param(spa::param::ParamType::Props, 0, pod);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_feedback;

    #[test]
    fn feedback_rule() {
        assert!(is_feedback("pipemic.mix.personal", "pipemic.channel.music"));
        assert!(is_feedback("pipemic.mix.stream", "pipemic.mix.chat.in"));
        assert!(!is_feedback("pipemic.mix.personal", "alsa_output.usb-CalDigit"));
        assert!(!is_feedback("pipemic.channel.music", "pipemic.mix.stream.in"));
        assert!(!is_feedback("pipemic.mix.stream.in", "pipemic.mix.stream"));
        assert!(!is_feedback("pipemic.mix.stream", "pipemic.meter"));
    }
}
