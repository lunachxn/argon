use std::path::Path;

use log::{error, trace};
use rbx_dom_weak::types::Ref;

use crate::{
	core::{
		changes::Changes,
		meta::SourceKind,
		snapshot::{Snapshot, UpdatedSnapshot},
		tree::Tree,
	},
	middleware::{new_snapshot, project::new_snapshot_node},
	stats, util,
	vfs::Vfs,
};

pub fn process_changes(id: Ref, tree: &mut Tree, vfs: &Vfs) -> Option<Changes> {
	trace!("Processing changes for instance: {id:?}");

	let mut changes = Changes::new();

	let meta = tree.get_meta(id)?;
	let source = meta.source.get();

	let process_path = |path: &Path| -> Option<Option<Snapshot>> {
		match new_snapshot(path, &meta.context, vfs) {
			Ok(snapshot) => Some(snapshot),
			Err(err) => {
				error!("Failed to process changes: {err}, source: {source:?}");
				None
			}
		}
	};

	let snapshot = match source {
		SourceKind::Project(name, path, node, node_path) => {
			if node_path.is_root() {
				process_path(path)?
			} else {
				match new_snapshot_node(name, path, *node.clone(), node_path.clone(), &meta.context, vfs) {
					Ok(snapshot) => Some(snapshot),
					Err(err) => {
						error!("Failed to process changes: {err}, source: {source:?}");
						return Some(changes);
					}
				}
			}
		}
		SourceKind::Path(path) => process_path(path)?,
		SourceKind::None => panic!(
			"Fatal processing error: `SourceKind::None` should not be present in the tree! Id: {id:?}, meta: {meta:#?}"
		),
	};

	// Handle additions, modifications and child removals
	if let Some(snapshot) = snapshot {
		process_child_changes(id, snapshot, &mut changes, tree);
	// Handle regular removals
	} else {
		tree.remove_instance(id);
		changes.remove(id);
	}

	Some(changes)
}

fn process_child_changes(id: Ref, mut snapshot: Snapshot, changes: &mut Changes, tree: &mut Tree) {
	if let Err(err) = remap_imported_refs(&mut snapshot, id, tree) {
		error!("Refusing model change with unresolved reference: {err}");
		return;
	}
	// Process instance changes
	let mut updated_snapshot = UpdatedSnapshot::new(id);

	updated_snapshot.meta = if snapshot.meta != *tree.get_meta(id).expect("Instance meta not found") {
		tree.update_meta(id, snapshot.meta.clone());
		Some(snapshot.meta)
	} else {
		None
	};

	let instance = tree.get_instance_mut(id).unwrap();

	updated_snapshot.name = if snapshot.name != instance.name {
		instance.name.clone_from(&snapshot.name);
		Some(snapshot.name)
	} else {
		None
	};

	updated_snapshot.class = if snapshot.class != instance.class {
		instance.class.clone_from(&snapshot.class);
		Some(snapshot.class)
	} else {
		None
	};

	updated_snapshot.properties = if snapshot.properties != instance.properties {
		instance.properties.clone_from(&snapshot.properties);
		Some(snapshot.properties)
	} else {
		None
	};

	if !updated_snapshot.is_empty() {
		// Track `lines_synced` stat
		if let Some(properties) = &updated_snapshot.properties {
			let loc = util::count_loc_from_properties(properties);
			stats::lines_synced(loc as u32);
		}

		changes.update(updated_snapshot);
	}

	let mut hydrated = vec![false; snapshot.children.len()];

	// Pair instances and find removed children
	#[allow(clippy::unnecessary_to_owned)]
	for child_id in instance.children().to_owned() {
		let instance = tree.get_instance(child_id).unwrap();

		let snapshot = snapshot.children.iter_mut().enumerate().find(|(index, child)| {
			if hydrated[*index] {
				return false;
			}

			if child.name == instance.name && child.class == instance.class {
				hydrated[*index] = true;
				return true;
			}

			false
		});

		if let Some((_, child)) = snapshot {
			child.set_id(child_id);
		} else {
			tree.remove_instance(child_id);
			changes.remove(child_id);
		}
	}

	// Process child changes and find new children
	for child in snapshot.children {
		if child.id.is_some() {
			process_child_changes(child.id, child, changes, tree);
		} else {
			let mut child = child;

			insert_children(&mut child, id, tree);

			// Track `lines_synced` stat
			let loc = util::count_loc_from_properties(&child.properties);
			stats::lines_synced(loc as u32);

			changes.add(child, id);
		}
	}
}

// A fresh deserialize has fresh model ids; matched live children keep their session ids.
// Map the entire subtree first so forward and sibling references are independent of traversal order.
fn remap_imported_refs(snapshot: &mut Snapshot, id: Ref, tree: &Tree) -> anyhow::Result<()> {
	use std::collections::HashMap;
	use rbx_dom_weak::types::Variant;
	fn collect(node: &Snapshot, target: Option<Ref>, tree: &Tree, map: &mut HashMap<Ref, Ref>) {
		if node.source_id.is_some() {
			map.insert(node.source_id, target.unwrap_or(node.source_id));
		}
		let children = target.and_then(|id| tree.get_instance(id)).map(|n| n.children().to_vec()).unwrap_or_default();
		let mut used = vec![false; children.len()];
		for child in &node.children {
			let matched = children.iter().enumerate().find(|(i, id)| {
				let existing = tree.get_instance(**id).unwrap();
				!used[*i] && existing.name == child.name && existing.class == child.class
			}).map(|(i, id)| { used[i] = true; *id });
			collect(child, matched, tree, map);
		}
	}
	fn apply(node: &mut Snapshot, tree: &Tree, map: &HashMap<Ref, Ref>) -> anyhow::Result<()> {
		for (property, value) in &mut node.properties {
			if let Variant::Ref(reference) = value {
				if let Some(target) = map.get(reference) {
					*reference = *target;
				} else if reference.is_some() && tree.get_instance(*reference).is_none() {
					anyhow::bail!("{}.{} -> {:?}", node.name, property, reference);
				}
			}
		}
		for child in &mut node.children { apply(child, tree, map)?; }
		Ok(())
	}
	let mut map = HashMap::new();
	collect(snapshot, Some(id), tree, &mut map);
	apply(snapshot, tree, &map)
}

fn insert_children(snapshot: &mut Snapshot, parent: Ref, tree: &mut Tree) {
	let id = tree.insert_instance(snapshot.clone(), parent);

	snapshot.set_id(id);

	for child in snapshot.children.iter_mut() {
		insert_children(child, id, tree);
	}
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::Variant};
    fn fixture() -> Snapshot {
        let mut dom = WeakDom::new(InstanceBuilder::new("Model").with_name("Fixture"));
        let root = dom.root_ref();
        let part = dom.insert(root, InstanceBuilder::new("Part").with_name("Handle"));
        dom.get_by_ref_mut(root).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(part));
        crate::middleware::helpers::snapshot_from_dom(dom, root)
    }
    #[test]
    fn reload_retains_existing_reference_identity() {
        let mut tree = Tree::new(fixture());
        let root = tree.root_ref();
        let handle = tree.root().children()[0];
        let mut changes = Changes::new();
        process_child_changes(root, fixture(), &mut changes, &mut tree);
        assert_eq!(tree.root().children()[0], handle);
        assert_eq!(tree.root().properties[&"PrimaryPart".into()], Variant::Ref(handle));
        assert!(changes.additions.is_empty());
        assert!(changes.removals.is_empty());
    }

    #[test]
    fn reload_addition_can_reference_existing_sibling() {
        let mut tree = Tree::new(fixture());
        let root = tree.root_ref();
        let handle = tree.root().children()[0];
        let mut snapshot = fixture();
        let imported_handle = snapshot.children[0].source_id;
        let mut mount = Snapshot::new().with_name("Mount").with_class("Weld");
        mount.properties.insert("Part0".into(), Variant::Ref(imported_handle));
        mount.properties.insert("Part1".into(), Variant::Ref(imported_handle));
        snapshot.children.push(mount);
        let mut changes = Changes::new();
        process_child_changes(root, snapshot, &mut changes, &mut tree);
        let mount = tree.root().children().iter().find_map(|id| {
            let n = tree.get_instance(*id).unwrap(); if n.name == "Mount" {Some(n)} else {None}
        }).unwrap();
        assert_eq!(mount.properties[&"Part0".into()], Variant::Ref(handle));
        assert_eq!(mount.properties[&"Part1".into()], Variant::Ref(handle));
        tree.validate_references().unwrap();
    }

    #[test]
    fn bad_reload_preserves_previous_tree() {
        let mut tree = Tree::new(fixture());
        let root = tree.root_ref();
        let handle = tree.root().children()[0];
        let mut snapshot = fixture();
        snapshot.children.clear();
        snapshot.name = "Invalid".into();
        let mut changes = Changes::new();
        process_child_changes(root, snapshot, &mut changes, &mut tree);
        assert_eq!(tree.root().name, "Fixture");
        assert_eq!(tree.root().children(), &[handle]);
        assert!(changes.is_empty());
    }
}
