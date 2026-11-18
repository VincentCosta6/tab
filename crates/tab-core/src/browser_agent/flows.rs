//! Saved flows: a plan that ran cleanly, kept per site with its values
//! turned into parameters ("Cloudflare: add DNS record {name, target}").
//! Next time the agent only fills in the parameters. A flow is just a plan
//! template: what it makes still goes through the plan card and the gate.
//! (Not "recipes": that word is taken by connection manifests.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::plan::Plan;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub id: String,
    pub name: String,
    /// The write site it's for.
    pub site: String,
    /// Parameter names; steps refer to them as `{name}`.
    pub params: Vec<String>,
    pub plan: Plan,
    #[serde(default)]
    pub runs: u32,
}

pub struct Flows {
    path: PathBuf,
}

fn valid_param(p: &str) -> bool {
    !p.is_empty() && p.len() <= 40 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Flow {
    /// The plan with `values` filled in. Every parameter must be given, and
    /// only parameters: a flow can't be made to say more than it did.
    pub fn instantiate(&self, values: &BTreeMap<String, String>) -> anyhow::Result<Plan> {
        for p in &self.params {
            anyhow::ensure!(values.contains_key(p), "the flow needs {p}");
        }
        for k in values.keys() {
            anyhow::ensure!(self.params.contains(k), "the flow has no parameter {k}");
        }
        let fill = |s: &str| {
            let mut s = s.to_string();
            for (k, v) in values {
                s = s.replace(&format!("{{{k}}}"), v);
            }
            s
        };
        let mut plan = self.plan.clone();
        for step in &mut plan.steps {
            step.value = step.value.as_deref().map(fill);
            step.url = step.url.as_deref().map(fill);
        }
        plan.goal = fill(&plan.goal);
        plan.validate()
    }
}

impl Flows {
    pub fn at(dir: &Path) -> Self {
        Self {
            path: dir.join("browser-flows.json"),
        }
    }

    pub fn list(&self) -> Vec<Flow> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, flows: &[Flow]) -> anyhow::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(flows)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn for_site(&self, site: &str) -> Vec<Flow> {
        self.list().into_iter().filter(|f| f.site == site).collect()
    }

    pub fn get(&self, id: &str) -> Option<Flow> {
        self.list().into_iter().find(|f| f.id == id)
    }

    /// Saves `plan` as a flow, turning each value named in `params` (value →
    /// parameter name) into `{name}`.
    pub fn save(
        &self,
        name: &str,
        plan: &Plan,
        params: &BTreeMap<String, String>,
    ) -> anyhow::Result<Flow> {
        anyhow::ensure!(!name.trim().is_empty(), "name the flow");
        let site = plan
            .write
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("only plans that change a site can be saved"))?;
        let mut template = plan.clone();
        let mut names: Vec<String> = Vec::new();
        for (value, param) in params {
            anyhow::ensure!(
                valid_param(param),
                "parameter names are letters, digits and _"
            );
            let mut used = false;
            for step in &mut template.steps {
                if step.value.as_deref() == Some(value.as_str()) {
                    step.value = Some(format!("{{{param}}}"));
                    used = true;
                }
                if step.url.as_deref() == Some(value.as_str()) {
                    step.url = Some(format!("{{{param}}}"));
                    used = true;
                }
            }
            anyhow::ensure!(used, "{value} isn't a value in the plan");
            if !names.contains(param) {
                names.push(param.clone());
            }
        }
        let flow = Flow {
            id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            name: name.trim().to_string(),
            site,
            params: names,
            plan: template,
            runs: 0,
        };
        let mut all = self.list();
        all.push(flow.clone());
        self.write(&all)?;
        Ok(flow)
    }

    pub fn ran(&self, id: &str) {
        let mut all = self.list();
        if let Some(f) = all.iter_mut().find(|f| f.id == id) {
            f.runs += 1;
            let _ = self.write(&all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        serde_json::from_value::<Plan>(serde_json::json!({
            "goal": "Point www at the app",
            "write": ["dash.cloudflare.com"],
            "steps": [
                { "do": "click", "what": "Add record" },
                { "do": "type", "field": "Name", "value": "www" },
                { "do": "type", "field": "Target", "value": "app.example.com" },
                { "do": "submit", "what": "Save" }
            ]
        }))
        .unwrap()
        .validate()
        .unwrap()
    }

    #[test]
    fn save_and_fill_a_flow() {
        let dir = std::env::temp_dir().join(format!("tab-flows-{}", uuid::Uuid::new_v4()));
        let flows = Flows::at(&dir);
        let params = BTreeMap::from([
            ("www".to_string(), "name".to_string()),
            ("app.example.com".to_string(), "target".to_string()),
        ]);
        let f = flows.save("Add DNS record", &plan(), &params).unwrap();
        assert_eq!(f.site, "cloudflare.com");
        assert_eq!(flows.for_site("cloudflare.com").len(), 1);
        let filled = f
            .instantiate(&BTreeMap::from([
                ("name".to_string(), "docs".to_string()),
                ("target".to_string(), "docs-host.example".to_string()),
            ]))
            .unwrap();
        assert_eq!(filled.steps[1].value.as_deref(), Some("docs"));
        assert_eq!(filled.steps[2].value.as_deref(), Some("docs-host.example"));
        assert_eq!(filled.write, ["cloudflare.com"]);
        // Every parameter, and only parameters.
        assert!(
            f.instantiate(&BTreeMap::from([("name".to_string(), "x".to_string())]))
                .is_err()
        );
        assert!(
            f.instantiate(&BTreeMap::from([
                ("name".to_string(), "x".to_string()),
                ("target".to_string(), "y".to_string()),
                ("site".to_string(), "evil.example".to_string()),
            ]))
            .is_err()
        );
        assert!(
            flows
                .save("x", &plan(), &BTreeMap::from([("nope".into(), "p".into())]))
                .is_err()
        );
        flows.ran(&f.id);
        assert_eq!(flows.get(&f.id).unwrap().runs, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
