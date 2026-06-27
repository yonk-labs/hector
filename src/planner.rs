use std::path::Path;

pub fn plan(_task: &str, _spec: Option<&str>) -> anyhow::Result<String> {
    anyhow::bail!("planner is specified but not implemented yet; see HECTOR_SPEC.md")
}

pub fn check(_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!("campaign checker is specified but not implemented yet; see HECTOR_SPEC.md")
}
