//! Registry-driven linker: keeps the wanted port-to-port links in place,
//! (re)creating them whenever the ports appear (e.g. the mic is replugged).

use pipewire as pw;
use pw::properties::properties;
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

#[derive(Default)]
struct State {
    nodes: HashMap<u32, String>,
    ports: HashMap<u32, Port>,
    /// (out port id, in port id) -> link proxy
    links: HashMap<(u32, u32), pw::link::Link>,
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
}

pub struct Linker {
    _listener: pw::registry::Listener,
    _registry: Rc<pw::registry::RegistryRc>,
}

impl Linker {
    pub fn start(core: &pw::core::CoreRc, wants: Vec<Want>) -> anyhow::Result<Self> {
        let registry = Rc::new(core.get_registry_rc()?);
        let state = Rc::new(RefCell::new(State::default()));
        let wants = Rc::new(wants);

        let reconcile = {
            let state = state.clone();
            let wants = wants.clone();
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
        let listener = registry
            .add_listener_local()
            .global(move |g| {
                let Some(props) = g.props else { return };
                let changed = match g.type_ {
                    ObjectType::Node => {
                        if let Some(n) = props.get("node.name") {
                            st_add.borrow_mut().nodes.insert(g.id, n.to_owned());
                        }
                        true
                    }
                    ObjectType::Port => {
                        let (Some(node), Some(name), Some(dir)) =
                            (props.get("node.id"), props.get("port.name"), props.get("port.direction"))
                        else {
                            return;
                        };
                        let Ok(node) = node.parse() else { return };
                        st_add.borrow_mut().ports.insert(g.id, Port { node, name: name.to_owned(), out: dir == "out" });
                        true
                    }
                    _ => false,
                };
                if changed {
                    reconcile();
                }
            })
            .global_remove(move |id| {
                let mut st = st_rm.borrow_mut();
                st.nodes.remove(&id);
                if st.ports.remove(&id).is_some() {
                    st.links.retain(|&(o, i), _| o != id && i != id);
                }
            })
            .register();

        Ok(Self { _listener: listener, _registry: registry })
    }
}
