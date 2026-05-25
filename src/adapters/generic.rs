use super::Adapter;

pub struct GenericAdapter {
    canonical_agent_kind: &'static str,
    aliases: &'static [&'static str],
}

impl GenericAdapter {
    pub const fn new(canonical_agent_kind: &'static str, aliases: &'static [&'static str]) -> Self {
        Self {
            canonical_agent_kind,
            aliases,
        }
    }
}

impl Adapter for GenericAdapter {
    fn canonical_agent_kind(&self) -> &'static str {
        self.canonical_agent_kind
    }

    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }
}
