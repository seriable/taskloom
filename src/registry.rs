//! Task catalogs: each crate (or module) lists the labels of its tasks and
//! checks them in a test ([`check_catalog`]). The label is a contract twice
//! over: the call ledger counts by it, and the gateway reads the task's
//! budget by it (longest matching prefix). A renamed or misspelled label
//! silently falls back to the default budget; the catalog check makes that a
//! test failure.

use crate::gateway::TaskRegistry;

/// One task of a catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The label as the task sends it (for a label with a variable suffix,
    /// one representative label).
    pub label: &'static str,
    /// `true` when the task deliberately has no budget entry and runs under
    /// the gateway's default budget in observe mode.
    pub default_budget: bool,
}

impl CatalogEntry {
    /// A task the gateway's task registry must know.
    pub const fn budgeted(label: &'static str) -> Self {
        Self {
            label,
            default_budget: false,
        }
    }

    /// A task that runs under the default budget on purpose.
    pub const fn default_budget(label: &'static str) -> Self {
        Self {
            label,
            default_budget: true,
        }
    }
}

/// Checks a catalog against `tasks`: labels are unique and non-blank, a
/// budgeted task has a registry entry, and a default-budget task has none
/// (so a later budget entry is noticed and the catalog updated).
pub fn check_catalog(entries: &[CatalogEntry], tasks: &TaskRegistry) -> Result<(), String> {
    let mut problems = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let label = entry.label.trim();
        if label.is_empty() {
            problems.push(format!("entry {index} has a blank label"));
            continue;
        }
        if entries[..index]
            .iter()
            .any(|earlier| earlier.label == entry.label)
        {
            problems.push(format!("{label} is listed twice"));
        }
        let registered = tasks.profile(label).registered;
        match (entry.default_budget, registered) {
            (false, false) => problems.push(format!("{label} has no task budget entry")),
            (true, true) => problems.push(format!(
                "{label} now has a task budget entry; list it as budgeted"
            )),
            _ => {}
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::TaskEntry;

    #[test]
    fn a_catalog_must_match_the_registry() {
        let tasks = TaskRegistry::new([TaskEntry::bounded_text("summary_", 1_000, 0.2)]);
        assert!(check_catalog(
            &[
                CatalogEntry::budgeted("summary_chapter"),
                CatalogEntry::default_budget("title")
            ],
            &tasks
        )
        .is_ok());
        let problems = check_catalog(
            &[
                CatalogEntry::budgeted("titel"),
                CatalogEntry::default_budget("summary_x"),
                CatalogEntry::budgeted("titel"),
            ],
            &tasks,
        )
        .unwrap_err();
        assert!(
            problems.contains("titel has no task budget entry"),
            "{problems}"
        );
        assert!(problems.contains("summary_x now has"), "{problems}");
        assert!(problems.contains("titel is listed twice"), "{problems}");
    }
}
