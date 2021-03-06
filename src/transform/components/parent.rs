use specs::prelude::{Component, DenseVecStorage, Entity, FlaggedStorage};

pub use specs_hierarchy::HierarchyEvent;
use specs_hierarchy::{Hierarchy, Parent as HParent};

/// An alias to tie `specs-hierarchy` `Hierarchy` structure to our `Parent` component.
pub type ParentHierarchy = Hierarchy<Parent>;

/// Component for defining a parent entity.
///
/// The entity with this component *has* a parent, rather than *is* a parent.
#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Parent {
    /// The parent entity
    pub entity: Entity,
}

impl Parent {
    pub fn new(entity: Entity) -> Self {
        Parent { entity }
    }
}
impl Component for Parent {
    type Storage = FlaggedStorage<Self, DenseVecStorage<Self>>;
}

impl HParent for Parent {
    fn parent_entity(&self) -> Entity {
        self.entity
    }
}
