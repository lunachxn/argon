pub use argon::core;
mod snapshot_convert {
    pub fn save_mesh(properties: &argon::Properties) -> Option<String> { let _ = properties; None }
    #[path = "../../src/middleware/helpers/snapshot.rs"]
    pub mod convert;
}
use argon::core::tree::Tree;
use snapshot_convert::convert::snapshot_from_dom;
use rbx_dom_weak::{types::Variant, InstanceBuilder, WeakDom};

#[test]
fn imported_model_retains_primarypart_and_weld_targets() {
    let mut dom = WeakDom::new(InstanceBuilder::new("Model").with_name("ReferenceFixture"));
    let root = dom.root_ref();
    let handle = dom.insert(root, InstanceBuilder::new("Part").with_name("Handle"));
    let blade = dom.insert(root, InstanceBuilder::new("Part").with_name("Blade"));
    let weld = dom.insert(root, InstanceBuilder::new("Weld").with_name("Mount"));
    dom.get_by_ref_mut(root).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(handle));
    dom.get_by_ref_mut(weld).unwrap().properties.insert("Part0".into(), Variant::Ref(handle));
    dom.get_by_ref_mut(weld).unwrap().properties.insert("Part1".into(), Variant::Ref(blade));
    let tree = Tree::new(snapshot_from_dom(dom, root));
    let fixture = tree.root();
    let find = |name: &str| *fixture.children().iter().find(|id| tree.get_instance(**id).unwrap().name == name).unwrap();
    let actual_handle = find("Handle");
    let actual_blade = find("Blade");
    let actual_weld = tree.get_instance(find("Mount")).unwrap();
    assert_eq!(fixture.properties[&"PrimaryPart".into()], Variant::Ref(actual_handle));
    assert_eq!(actual_weld.properties[&"Part0".into()], Variant::Ref(actual_handle));
    assert_eq!(actual_weld.properties[&"Part1".into()], Variant::Ref(actual_blade));
    tree.validate_references().unwrap();
}

#[test]
fn dangling_reference_fails_validation() {
    let mut dom = WeakDom::new(InstanceBuilder::new("Model").with_name("Fixture"));
    let root = dom.root_ref();
    let missing = dom.insert(root, InstanceBuilder::new("Part").with_name("Removed"));
    dom.destroy(missing);
    dom.get_by_ref_mut(root).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(missing));
    let tree = Tree::new(snapshot_from_dom(dom, root));
    assert!(tree.validate_references().is_err());
}
