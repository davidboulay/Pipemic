//! Registry-driven graph keeper:
//! - creates the wanted port-to-port links whenever their ports exist
//!   (re-creating them after e.g. a mic replug);
//! - binds the mix input nodes so gains can be pushed at runtime;
//! - removes feedback links (a mix playing into a Wave Link channel);
//! - routes app streams to channels via the `default` metadata
//!   (`target.object`, same as pavucontrol), once per stream.

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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

/// App-to-channel rule: sink node name + patterns (see model::Rule).
pub struct AppRule {
    pub sink: String,
    pub apps: Vec<String>,
}

struct NodeInfo {
    serial: Option<String>,
    is_app_stream: bool,
    /// application.name, application.process.binary, node.name, portal app id
    keys: Vec<String>,
}

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
}

impl State {
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

    fn route_streams(&mut self, rules: &[AppRule]) {
        let Some(md) = &self.metadata else { return };
        let mut newly = Vec::new();
        for (&id, info) in &self.info {
            if !info.is_app_stream || self.moved.contains_key(&id) {
                continue;
            }
            let Some(rule) =
                rules.iter().find(|r| r.apps.iter().any(|p| info.keys.iter().any(|k| crate::model::glob(p, k))))
            else {
                continue;
            };
            let sink_serial = self
                .nodes
                .iter()
                .find(|(_, n)| **n == rule.sink)
                .and_then(|(sid, _)| self.info.get(sid))
                .and_then(|i| i.serial.clone());
            let Some(serial) = sink_serial else { continue };
            md.set_property(id, "target.object", Some("Spa:Id"), Some(&serial));
            eprintln!("wavelinkd: {} -> {}", info.keys.first().map_or("?", |s| s.as_str()), rule.sink);
            newly.push((id, rule.sink.clone()));
        }
        self.moved.extend(newly);
    }
}

pub struct Graph {
    state: Rc<RefCell<State>>,
    _listener: pw::registry::Listener,
    _registry: Rc<pw::registry::RegistryRc>,
}

/// Is a link from node `from` into node `to` a feedback loop we must cut?
/// Any Wave Link mix output feeding a Wave Link node.
fn is_feedback(from: &str, to: &str) -> bool {
    from.starts_with("wavelink.mix.") && !from.ends_with(".in") && to.starts_with("wavelink.")
}

impl Graph {
    /// `bind_nodes`: node names to bind for `set_controls`.
    pub fn start(
        core: &pw::core::CoreRc,
        wants: Vec<Want>,
        bind_nodes: Vec<String>,
        rules: Vec<AppRule>,
    ) -> anyhow::Result<Self> {
        let registry = Rc::new(core.get_registry_rc()?);
        let state = Rc::new(RefCell::new(State::default()));
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
                        Err(e) => eprintln!("wavelinkd: link {} -> {}:{} failed: {e}", oport, w.in_node, w.in_port),
                    }
                }
            }
        };

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
                        let keys = ["application.name", "application.process.binary", "node.name", "pipewire.access.portal.app_id"]
                            .iter()
                            .filter_map(|k| props.get(k).map(str::to_owned))
                            .collect();
                        st.info.insert(
                            g.id,
                            NodeInfo {
                                serial: props.get("object.serial").map(str::to_owned),
                                is_app_stream: props.get("media.class") == Some("Stream/Output/Audio")
                                    && !name.starts_with("wavelink."),
                                keys,
                            },
                        );
                        if bind_nodes.iter().any(|b| b == name) {
                            match reg.bind::<pw::node::Node, _>(g) {
                                Ok(n) => {
                                    st.bound.insert(name.to_owned(), (g.id, n));
                                }
                                Err(e) => eprintln!("wavelinkd: bind {name} failed: {e}"),
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
                                Err(e) => eprintln!("wavelinkd: bind default metadata failed: {e}"),
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
                                eprintln!("wavelinkd: removing feedback link {from} -> {to}");
                                reg.destroy_global(g.id);
                            }
                        }
                        return;
                    }
                    _ => return,
                }
                reconcile();
                st_add.borrow_mut().route_streams(&rules);
            })
            .global_remove(move |id| {
                let mut st = st_rm.borrow_mut();
                st.nodes.remove(&id);
                st.info.remove(&id);
                st.moved.remove(&id);
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
            for &id in st.moved.keys() {
                md.set_property(id, "target.object", None, None);
            }
        }
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
        assert!(is_feedback("wavelink.mix.personal", "wavelink.channel.music"));
        assert!(is_feedback("wavelink.mix.stream", "wavelink.mix.chat.in"));
        assert!(!is_feedback("wavelink.mix.personal", "alsa_output.usb-CalDigit"));
        assert!(!is_feedback("wavelink.channel.music", "wavelink.mix.stream.in"));
        assert!(!is_feedback("wavelink.mix.stream.in", "wavelink.mix.stream"));
    }
}
