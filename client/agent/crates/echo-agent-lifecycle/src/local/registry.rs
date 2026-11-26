use std::collections::HashMap;
use std::rc::Rc;

use crate::local::contributors::{
    LocalCommandContributor, LocalSessionLifecycleContributor, LocalTurnInputContributor,
    LocalTurnLifecycleContributor,
};

/// Mutable registry used while hosts register typed runtime contributions.
#[derive(Default)]
pub struct LocalExtensionRegistryBuilder {
    turn_lifecycle_contributors: Vec<Rc<dyn LocalTurnLifecycleContributor>>,
    session_lifecycle_contributors: Vec<Rc<dyn LocalSessionLifecycleContributor>>,
    turn_input_contributors: Vec<Rc<dyn LocalTurnInputContributor>>,
    command_contributors: Vec<Rc<dyn LocalCommandContributor>>,
}

impl LocalExtensionRegistryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn turn_lifecycle_contributor(
        &mut self,
        contributor: Rc<dyn LocalTurnLifecycleContributor>,
    ) {
        self.turn_lifecycle_contributors.push(contributor);
    }

    pub fn session_lifecycle_contributor(
        &mut self,
        contributor: Rc<dyn LocalSessionLifecycleContributor>,
    ) {
        self.session_lifecycle_contributors.push(contributor);
    }

    pub fn turn_input_contributor(&mut self, contributor: Rc<dyn LocalTurnInputContributor>) {
        self.turn_input_contributors.push(contributor);
    }

    pub fn command_contributor(&mut self, contributor: Rc<dyn LocalCommandContributor>) {
        self.command_contributors.push(contributor);
    }

    /// Routes each advertised command to its one owner. Duplicate names are a composition bug:
    /// first registration wins, panics in debug builds, logs in release.
    pub fn build(self) -> LocalExtensionRegistry {
        let mut command_handlers: HashMap<String, Rc<dyn LocalCommandContributor>> = HashMap::new();
        for contributor in &self.command_contributors {
            for spec in contributor.advertised_commands() {
                if command_handlers.contains_key(&spec.name) {
                    debug_assert!(false, "duplicate command contributed: /{}", spec.name);
                    tracing::error!(command = %spec.name, "Duplicate command contributed; first registration wins");
                    continue;
                }
                command_handlers.insert(spec.name, contributor.clone());
            }
        }

        LocalExtensionRegistry {
            turn_lifecycle_contributors: self.turn_lifecycle_contributors,
            session_lifecycle_contributors: self.session_lifecycle_contributors,
            turn_input_contributors: self.turn_input_contributors,
            command_contributors: self.command_contributors,
            command_handlers,
        }
    }
}

/// Immutable typed registry produced after extensions are installed.
#[derive(Default)]
pub struct LocalExtensionRegistry {
    turn_lifecycle_contributors: Vec<Rc<dyn LocalTurnLifecycleContributor>>,
    session_lifecycle_contributors: Vec<Rc<dyn LocalSessionLifecycleContributor>>,
    turn_input_contributors: Vec<Rc<dyn LocalTurnInputContributor>>,
    command_contributors: Vec<Rc<dyn LocalCommandContributor>>,
    command_handlers: HashMap<String, Rc<dyn LocalCommandContributor>>,
}

impl LocalExtensionRegistry {
    pub fn turn_lifecycle_contributors(&self) -> &[Rc<dyn LocalTurnLifecycleContributor>] {
        &self.turn_lifecycle_contributors
    }

    pub fn session_lifecycle_contributors(&self) -> &[Rc<dyn LocalSessionLifecycleContributor>] {
        &self.session_lifecycle_contributors
    }

    pub fn turn_input_contributors(&self) -> &[Rc<dyn LocalTurnInputContributor>] {
        &self.turn_input_contributors
    }

    pub fn command_contributors(&self) -> &[Rc<dyn LocalCommandContributor>] {
        &self.command_contributors
    }

    /// The one contributor owning `name`, or `None` when no extension advertised it.
    pub fn command_handler(&self, name: &str) -> Option<&Rc<dyn LocalCommandContributor>> {
        self.command_handlers.get(name)
    }
}
