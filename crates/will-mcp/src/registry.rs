use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;
use will_core::{ToolPolicy, ToolProvider, ToolSpec, WillTool};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolListing {
    pub name: String,
    pub family: String,
    pub description: String,
    pub default_policy: ToolPolicy,
    /// Política efetiva (override ou padrão).
    pub policy: ToolPolicy,
    pub overridden: bool,
    pub available: bool,
    pub input_schema: serde_json::Value,
}

/// Ferramentas de todos os módulos + políticas.
#[derive(Default)]
pub struct ToolRegistry {
    providers: RwLock<Vec<Arc<dyn ToolProvider>>>,
    overrides: RwLock<BTreeMap<String, ToolPolicy>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_provider(&self, p: Arc<dyn ToolProvider>) {
        let mut g = self.providers.write();
        let replaced = g.iter().any(|x| x.family() == p.family());
        g.retain(|x| x.family() != p.family());
        tracing::debug!(
            family = p.family(),
            tools = p.tools().len(),
            available = p.available(),
            replaced,
            "provedor de ferramentas registrado"
        );
        g.push(p);
    }

    pub fn remove_family(&self, family: &str) {
        let mut g = self.providers.write();
        let before = g.len();
        g.retain(|x| x.family() != family);
        if g.len() != before {
            tracing::debug!(family, "provedor de ferramentas removido");
        }
    }

    pub fn set_overrides(&self, m: BTreeMap<String, ToolPolicy>) {
        tracing::debug!(overrides = m.len(), "políticas de ferramentas carregadas");
        *self.overrides.write() = m;
    }

    pub fn set_policy(&self, tool: &str, policy: Option<ToolPolicy>) {
        let mut g = self.overrides.write();
        match policy {
            Some(p) => {
                g.insert(tool.to_string(), p);
            }
            None => {
                g.remove(tool);
            }
        }
        tracing::info!(
            tool,
            policy = policy.map(|p| p.as_str()).unwrap_or("default"),
            "política aplicada à ferramenta"
        );
    }

    pub fn policy_for(&self, spec: &ToolSpec) -> (ToolPolicy, bool) {
        match self.overrides.read().get(&spec.name) {
            Some(p) => (*p, true),
            None => (spec.default_policy, false),
        }
    }

    /// Todas as ferramentas conhecidas (para a UI), disponíveis ou não.
    pub fn listing(&self) -> Vec<ToolListing> {
        let mut out = vec![];
        for p in self.providers.read().iter() {
            let available = p.available();
            for t in p.tools() {
                let spec = t.spec();
                let (policy, overridden) = self.policy_for(&spec);
                out.push(ToolListing {
                    name: spec.name.clone(),
                    family: spec.family.clone(),
                    description: spec.description.clone(),
                    default_policy: spec.default_policy,
                    policy,
                    overridden,
                    available,
                    input_schema: spec.input_schema.clone(),
                });
            }
        }
        out
    }

    /// Ferramentas servíveis agora: módulo disponível e política != off.
    pub fn active(&self) -> Vec<(ToolSpec, Arc<dyn WillTool>)> {
        let mut out = vec![];
        for p in self.providers.read().iter() {
            if !p.available() {
                continue;
            }
            for t in p.tools() {
                let spec = t.spec();
                let (policy, _) = self.policy_for(&spec);
                if policy != ToolPolicy::Off {
                    out.push((spec, t));
                }
            }
        }
        out
    }

    pub fn find(&self, name: &str) -> Option<(ToolSpec, Arc<dyn WillTool>, bool)> {
        for p in self.providers.read().iter() {
            for t in p.tools() {
                let spec = t.spec();
                if spec.name == name {
                    return Some((spec, t, p.available()));
                }
            }
        }
        None
    }
}
