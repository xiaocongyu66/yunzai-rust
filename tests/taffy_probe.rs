use taffy::prelude::*;
#[test]
fn probe_column_align() {
    let mut t: taffy::TaffyTree<()> = TaffyTree::new();
    let body = t.new_with_children(
        Style { size: Size { width: length(420.0), height: auto() }, flex_direction: FlexDirection::Column, align_items: Some(AlignItems::FlexStart), ..Default::default() },
        &[],
    ).unwrap();
    let d1 = t.new_leaf(Style { size: Size { width: length(100.0), height: length(40.0) }, ..Default::default() }).unwrap();
    let d2 = t.new_leaf(Style { size: Size { width: length(50.0), height: length(40.0) }, ..Default::default() }).unwrap();
    t.set_children(body, &[d1, d2]).unwrap();
    t.compute_layout(body, Size { width: AvailableSpace::Definite(420.0), height: AvailableSpace::MaxContent }).unwrap();
    println!("d1 x = {}", t.layout(d1).unwrap().location.x);
    println!("d2 x = {}", t.layout(d2).unwrap().location.x);
}
